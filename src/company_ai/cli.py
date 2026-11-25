"""Operator CLI. Live calls happen only from `run --live` and `live smoke`."""

from __future__ import annotations

import argparse
import json
import logging
from pathlib import Path

from company_ai import __version__
from company_ai.adapters.s3 import S3DiagnosticsAdapter
from company_ai.config import (
    live_problems,
    load_config,
    load_event,
    require_ready,
    settings_from_document,
)
from company_ai.contracts import ResolvedTarget
from company_ai.errors import CompanyAIError, SetupError
from company_ai.governance import check_fixture_files, load_bundle, verify_provenance
from company_ai.runtime import Runtime, inspect_run, replay_run
from company_ai.store import Store
from company_ai.wiring import build_service, build_worker
from company_ai.workers.ollama import urllib_transport

logger = logging.getLogger("company_ai")


def _print_outcome(outcome: object) -> None:
    print(f"run_id: {outcome.run_id}")
    print(f"event_id: {outcome.event_id}")
    print(f"status: {outcome.status}")
    print(f"reused: {str(outcome.reused).lower()}")
    print(f"failure_code: {outcome.failure_code or 'none'}")
    print(f"report_json: {outcome.report_json_path or 'none'}")
    print(f"report_md: {outcome.report_md_path or 'none'}")


def _status_code(status: str) -> int:
    return 0 if status == "succeeded" else 1


def _validate(bundle_path: Path, fixture_dir: Path, hub: Path | None, config_path: Path | None) -> int:
    bundle = load_bundle(bundle_path)
    check_fixture_files(bundle, fixture_dir)
    print(f"bundle: ok {bundle.bundle_id} {bundle.bundle_version}")
    print(f"policy_version: {bundle.policy_version}")
    print(f"fingerprint: {bundle.fingerprint}")
    print(f"skill: {bundle.skill_id}")
    if config_path is not None:
        load_config(config_path)
        print(f"config: ok {config_path}")
    if hub is not None:
        errors = verify_provenance(bundle, hub)
        if errors:
            print("provenance: mismatch")
            for error in errors:
                print(f"- {error}")
            return 2
        print(f"provenance: ok {bundle.provenance['commit']} ({len(bundle.provenance['sources'])} sources)")
    else:
        print("provenance: not checked (pass --hub to verify cursor-hub hashes)")
    return 0


def _execute(settings, bundle, event) -> int:
    require_ready(settings, bundle, event)
    if settings.service_mode == "fixture":
        check_fixture_files(bundle, settings.fixture_dir)
    worker_mode = "real_model" if settings.worker_mode == "ollama" else "scripted"
    service_mode = "live" if settings.service_mode == "s3" else "fixture"
    num_predict = min(settings.num_predict, bundle.limits["max_output_tokens"])
    worker = build_worker(settings, num_predict)
    adapter = build_service(settings)
    store = Store.open(settings.database)
    try:
        store.reconcile_interrupted()
        runtime = Runtime(
            store=store,
            bundle=bundle,
            adapter=adapter,
            worker=worker,
            reports_dir=settings.reports_dir,
            identity=settings.identity,
            worker_mode=worker_mode,
            service_mode=service_mode,
            live=settings.live,
            model_id=settings.model if settings.worker_mode == "ollama" else None,
        )
        outcome = runtime.handle_event(event)
    finally:
        store.close()
    _print_outcome(outcome)
    return _status_code(outcome.status)


def _demo(output: Path, bundle_path: Path, event_path: Path, fixture_dir: Path) -> int:
    bundle = load_bundle(bundle_path)
    check_fixture_files(bundle, fixture_dir)
    event = load_event(event_path)
    output.mkdir(parents=True, exist_ok=True)
    store = Store.open(output / "state.sqlite")
    try:
        store.reconcile_interrupted()
        runtime = Runtime(
            store=store,
            bundle=bundle,
            adapter=build_service(
                _fixture_settings(output, bundle_path, fixture_dir),
            ),
            worker=build_worker(_fixture_settings(output, bundle_path, fixture_dir), 64),
            reports_dir=output / "reports",
            identity="investigator",
            worker_mode="scripted",
            service_mode="fixture",
            live=False,
            model_id=None,
        )
        outcome = runtime.handle_event(event)
    finally:
        store.close()
    _print_outcome(outcome)
    return _status_code(outcome.status)


def _fixture_settings(output: Path, bundle_path: Path, fixture_dir: Path):
    from company_ai.config import Settings

    return Settings(
        identity="investigator",
        bundle_path=bundle_path,
        database=output / "state.sqlite",
        reports_dir=output / "reports",
        hub_path=None,
        worker_mode="scripted",
        script="investigate",
        endpoint="http://127.0.0.1:11434",
        model="",
        num_predict=64,
        allow_remote=False,
        service_mode="fixture",
        fixture_dir=fixture_dir,
        aws_profile="",
        aws_region="",
        expected_bucket_owner="",
        live=False,
        config_dir=Path.cwd(),
    )


def _smoke_ollama(settings, bundle) -> int:
    require_ready(settings, bundle, None)
    if settings.worker_mode != "ollama":
        raise SetupError("setup", "Ollama smoke requires worker.mode = ollama", missing=["worker.mode"])
    payload = {
        "model": settings.model,
        "stream": False,
        "think": False,
        "messages": [{"role": "user", "content": "Reply with the single word ok."}],
        "options": {"num_predict": min(32, bundle.limits["max_output_tokens"])},
    }
    try:
        body = urllib_transport(settings.endpoint.rstrip("/") + "/api/chat", payload, bundle.limits["request_timeout_seconds"])
    except CompanyAIError as exc:
        print(f"ollama_smoke: {exc.code}: {exc.message}")
        return 1
    message = body.get("message", {}) if isinstance(body, dict) else {}
    content = message.get("content") if isinstance(message, dict) else ""
    print(f"ollama_smoke: ok model={settings.model}")
    print(f"content_chars: {len(content) if isinstance(content, str) else 0}")
    return 0


def _smoke_s3(settings, bundle, alias: str) -> int:
    event = {
        "schema_version": 1,
        "event_id": "evt-live-smoke",
        "kind": "diagnostic.requested",
        "resource_alias": alias,
        "observed_at": "2026-11-25T00:00:00Z",
        "summary": "Bounded S3 smoke read.",
    }
    require_ready(settings, bundle, event)
    resource = bundle.document["resources"][alias]
    adapter = S3DiagnosticsAdapter(
        profile=settings.aws_profile,
        region=settings.aws_region,
        expected_bucket_owner=settings.expected_bucket_owner,
    )
    result = adapter.read(
        ResolvedTarget(
            resource_alias=alias,
            provider="s3",
            bucket=resource["s3"]["bucket"],
            key=resource["s3"]["key"],
        ),
        timeout_seconds=float(bundle.limits["request_timeout_seconds"]),
        max_bytes=min(1024, int(bundle.limits["max_result_bytes"])),
    )
    print(f"s3_smoke: {'ok' if result.ok else result.error_code}")
    print(f"bytes: {result.byte_length}")
    print(f"content_sha256: {result.content_sha256 or 'none'}")
    if result.message and not result.ok:
        print(f"message: {result.message}")
    return 0 if result.ok else 1


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="company-ai", description="Local company AI workflow proof of concept")
    parser.add_argument("--version", action="version", version=__version__)
    commands = parser.add_subparsers(dest="command", required=True)

    validate = commands.add_parser("validate", help="Validate a governance bundle and optional config")
    validate.add_argument("--bundle", default="governance/bundle.json")
    validate.add_argument("--fixture-dir", default="governance/fixtures")
    validate.add_argument("--config")
    validate.add_argument("--hub")

    demo = commands.add_parser("demo", help="Run the fixture investigation")
    demo.add_argument("--output", default="var/demo")
    demo.add_argument("--bundle", default="governance/bundle.json")
    demo.add_argument("--event", default="examples/events/diagnostic-demo-service-a.json")
    demo.add_argument("--fixture-dir", default="governance/fixtures")

    run = commands.add_parser("run", help="Run one event with the selected worker and service")
    run.add_argument("--config", required=True)
    run.add_argument("--event", required=True)
    run.add_argument("--live", action="store_true")
    run.add_argument("--identity")
    run.add_argument("--bundle")
    run.add_argument("--database")
    run.add_argument("--reports-dir")
    run.add_argument("--hub")
    run.add_argument("--worker", choices=["scripted", "ollama"])
    run.add_argument("--service", choices=["fixture", "s3"])
    run.add_argument("--fixture-dir")

    inspect = commands.add_parser("inspect", help="Show a run and mark unfinished work interrupted")
    inspect.add_argument("--database", required=True)
    inspect.add_argument("--run", required=True)

    replay = commands.add_parser("replay", help="Read a run without provider or model calls")
    replay.add_argument("--database", required=True)
    replay.add_argument("--run", required=True)

    export = commands.add_parser("export", help="Print the stored report paths")
    export.add_argument("--database", required=True)
    export.add_argument("--run", required=True)

    live = commands.add_parser("live", help="Check or smoke-test explicit live configuration")
    live_commands = live.add_subparsers(dest="live_command", required=True)
    check = live_commands.add_parser("check", help="Report missing live settings without network calls")
    check.add_argument("--config", required=True)
    check.add_argument("--event")
    smoke = live_commands.add_parser("smoke", help="Perform one bounded live call")
    smoke.add_argument("--config", required=True)
    smoke.add_argument("--target", required=True, choices=["ollama", "s3"])
    smoke.add_argument("--resource", default="demo-service-a")
    return parser


def main(argv: list[str] | None = None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(name)s %(levelname)s %(message)s")
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return _dispatch(args)
    except (CompanyAIError, OSError) as exc:
        code = getattr(exc, "code", "error")
        message = getattr(exc, "message", None) or str(exc.__class__.__name__)
        missing = getattr(exc, "missing", None)
        print(f"{code}: {message}")
        if missing:
            for item in missing:
                print(f"missing: {item}")
        return 2


def _dispatch(args: argparse.Namespace) -> int:
    if args.command == "validate":
        return _validate(
            Path(args.bundle),
            Path(args.fixture_dir),
            None if args.hub is None else Path(args.hub),
            None if args.config is None else Path(args.config),
        )
    if args.command == "demo":
        return _demo(Path(args.output), Path(args.bundle), Path(args.event), Path(args.fixture_dir))
    if args.command == "run":
        document = load_config(args.config)
        settings = settings_from_document(
            document,
            config_dir=Path.cwd(),
            live=bool(args.live),
            identity=args.identity,
            bundle=args.bundle,
            database=args.database,
            reports_dir=args.reports_dir,
            hub=args.hub,
            worker=args.worker,
            service=args.service,
            fixture_dir=args.fixture_dir,
        )
        bundle = load_bundle(settings.bundle_path)
        event = load_event(args.event)
        return _execute(settings, bundle, event)
    if args.command == "inspect":
        store = Store.open(args.database)
        try:
            print(json.dumps(inspect_run(store, args.run), indent=2, sort_keys=True))
        finally:
            store.close()
        return 0
    if args.command == "replay":
        store = Store.open(args.database, readonly=True)
        try:
            print(json.dumps(replay_run(store, args.run), indent=2, sort_keys=True))
        finally:
            store.close()
        return 0
    if args.command == "export":
        store = Store.open(args.database, readonly=True)
        try:
            run = store.get_run(args.run)
        finally:
            store.close()
        print(f"status: {run['status']}")
        print(f"report_json: {run['report_json_path'] or 'none'}")
        print(f"report_md: {run['report_md_path'] or 'none'}")
        return 0 if run["report_json_path"] else 1
    if args.command == "live" and args.live_command == "check":
        document = load_config(args.config)
        settings = settings_from_document(
            document,
            config_dir=Path.cwd(),
            live=True,
        )
        bundle = load_bundle(settings.bundle_path)
        event = None if args.event is None else load_event(args.event)
        # check reports configuration gaps; passing live=True here only evaluates settings,
        # it does not construct a client. A missing --live on `run` is a separate guard.
        problems = [item for item in live_problems(settings, bundle, event) if item != "--live"]
        if settings.worker_mode != "ollama" and settings.service_mode != "s3":
            print("live_check: no live adapter selected")
            return 0
        if problems:
            print("live_check: incomplete. Fixture mode was not substituted.")
            for item in problems:
                print(f"missing: {item}")
            return 2
        print("live_check: ready")
        print(f"worker: {settings.worker_mode}")
        print(f"service: {settings.service_mode}")
        return 0
    if args.command == "live" and args.live_command == "smoke":
        document = load_config(args.config)
        settings = settings_from_document(
            document,
            config_dir=Path.cwd(),
            live=True,
        )
        bundle = load_bundle(settings.bundle_path)
        if args.target == "ollama":
            forced = settings
            if settings.worker_mode != "ollama":
                raise SetupError("setup", "Ollama smoke requires worker.mode = ollama", missing=["worker.mode"])
            return _smoke_ollama(forced, bundle)
        if settings.service_mode != "s3":
            raise SetupError("setup", "S3 smoke requires service.mode = s3", missing=["service.mode"])
        return _smoke_s3(settings, bundle, args.resource)
    raise SetupError("error", "Unknown command")


if __name__ == "__main__":
    raise SystemExit(main())
