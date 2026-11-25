"""Operator configuration. Live mode is never implied by a missing value."""

from __future__ import annotations

import urllib.parse
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from company_ai.contracts import MAX_EVENT_BYTES, load_json_file, validate
from company_ai.errors import SetupError
from company_ai.governance import LoadedBundle, is_placeholder

try:
    import tomllib
except ModuleNotFoundError as exc:  # pragma: no cover - Python 3.11+ always has tomllib
    raise SetupError("setup", "Python 3.11 or newer is required") from exc


@dataclass(frozen=True)
class Settings:
    identity: str
    bundle_path: Path
    database: Path
    reports_dir: Path
    hub_path: Path | None
    worker_mode: str
    script: str
    endpoint: str
    model: str
    num_predict: int
    allow_remote: bool
    service_mode: str
    fixture_dir: Path
    aws_profile: str
    aws_region: str
    expected_bucket_owner: str
    live: bool
    config_dir: Path


def _path(base: Path, value: str) -> Path:
    candidate = Path(value)
    if not candidate.is_absolute():
        candidate = base / candidate
    return candidate


def load_config(path: str | Path) -> dict[str, Any]:
    file_path = Path(path)
    raw = file_path.read_bytes()
    if len(raw) > 64_000:
        raise SetupError("validation", "Configuration file exceeds 64000 bytes")
    try:
        document = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeError, tomllib.TOMLDecodeError) as exc:
        raise SetupError("validation", f"Configuration file is not valid TOML ({exc.__class__.__name__})") from exc
    try:
        validate("config", document)
    except Exception as exc:
        code = getattr(exc, "code", "validation")
        message = getattr(exc, "message", None) or exc.__class__.__name__
        raise SetupError(code, message) from exc
    return document


def settings_from_document(
    document: dict[str, Any],
    *,
    config_dir: Path,
    live: bool,
    identity: str | None = None,
    bundle: str | None = None,
    database: str | None = None,
    reports_dir: str | None = None,
    hub: str | None = None,
    worker: str | None = None,
    service: str | None = None,
    fixture_dir: str | None = None,
) -> Settings:
    runtime = document["runtime"]
    worker_doc = document["worker"]
    service_doc = document["service"]
    hub_value = hub if hub is not None else (document.get("hub") or {}).get("path")
    return Settings(
        identity=identity or runtime["identity"],
        bundle_path=_path(config_dir, bundle or runtime["bundle"]),
        database=_path(config_dir, database or runtime["database"]),
        reports_dir=_path(config_dir, reports_dir or runtime["reports_dir"]),
        hub_path=None if hub_value is None else _path(config_dir, hub_value),
        worker_mode=worker or worker_doc["mode"],
        script=worker_doc["script"],
        endpoint=worker_doc["endpoint"],
        model=worker_doc["model"],
        num_predict=int(worker_doc["num_predict"]),
        allow_remote=bool(worker_doc["allow_remote"]),
        service_mode=service or service_doc["mode"],
        fixture_dir=_path(config_dir, fixture_dir or service_doc["fixture_dir"]),
        aws_profile=service_doc["s3"]["profile"],
        aws_region=service_doc["s3"]["region"],
        expected_bucket_owner=service_doc["s3"]["expected_bucket_owner"],
        live=live,
        config_dir=config_dir,
    )


def load_event(path: str | Path) -> dict[str, Any]:
    event = load_json_file(path, MAX_EVENT_BYTES, "event")
    validate("event", event)
    return event


def _endpoint_problems(endpoint: str, allow_remote: bool) -> list[str]:
    parsed = urllib.parse.urlparse(endpoint)
    if parsed.username or parsed.password:
        return ["worker.endpoint must not include credentials"]
    if parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.path not in {"", "/"}:
        return ["worker.endpoint"]
    if parsed.hostname not in {"127.0.0.1", "localhost", "::1"} and not allow_remote:
        return ["worker.allow_remote"]
    return []


def _allowed_aliases(bundle: LoadedBundle, identity: str) -> list[str]:
    policy = bundle.document["policy"]["identities"].get(identity)
    if policy is None:
        return []
    aliases: list[str] = []
    for rule in policy["allow"]:
        for alias in rule["resources"]:
            if alias not in aliases:
                aliases.append(alias)
    return aliases


def live_problems(settings: Settings, bundle: LoadedBundle, event: dict[str, Any] | None) -> list[str]:
    """Return the exact settings that block a live run. This never selects a fallback mode."""
    problems: list[str] = []
    needs_live = settings.worker_mode == "ollama" or settings.service_mode == "s3"
    if needs_live and not settings.live:
        problems.append("--live")
    if settings.identity not in bundle.document["policy"]["identities"]:
        problems.append("runtime.identity")
    if settings.worker_mode == "ollama":
        if not settings.model.strip() or is_placeholder(settings.model):
            problems.append("worker.model")
        problems.extend(_endpoint_problems(settings.endpoint, settings.allow_remote))
        if settings.num_predict > bundle.limits["max_output_tokens"]:
            problems.append("worker.num_predict exceeds policy max_output_tokens")
    if settings.service_mode == "s3":
        if not settings.aws_profile.strip() or is_placeholder(settings.aws_profile):
            problems.append("service.s3.profile")
        if not settings.aws_region.strip() or is_placeholder(settings.aws_region):
            problems.append("service.s3.region")
        if event is not None:
            aliases = [str(event["resource_alias"])]
        else:
            aliases = _allowed_aliases(bundle, settings.identity)
        for alias in aliases:
            resource = bundle.document["resources"].get(alias)
            if resource is None:
                problems.append(f"resources.{alias}")
                continue
            if is_placeholder(resource["s3"]["bucket"]):
                problems.append(f"resources.{alias}.s3.bucket")
            if is_placeholder(resource["s3"]["key"]):
                problems.append(f"resources.{alias}.s3.key")
    return problems


def require_ready(settings: Settings, bundle: LoadedBundle, event: dict[str, Any] | None) -> None:
    problems = live_problems(settings, bundle, event)
    if not problems:
        return
    joined = ", ".join(problems)
    raise SetupError(
        "setup",
        f"Live configuration is incomplete. Fixture mode was not substituted. Missing or invalid settings: {joined}",
        missing=problems,
    )
