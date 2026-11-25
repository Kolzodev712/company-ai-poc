"""Behavioral boundaries for the diagnostic workflow."""

from __future__ import annotations

import json
import socket
import threading
from pathlib import Path

import pytest

from company_ai.adapters.fixture import FixtureAdapter
from company_ai.adapters.s3 import S3DiagnosticsAdapter
from company_ai.cli import main
from company_ai.config import load_config, require_ready, settings_from_document
from company_ai.errors import ConflictError, SetupError
from company_ai.governance import load_bundle, verify_provenance
from company_ai.runtime import ManualClock, Runtime, build_worker_context, inspect_run, replay_run
from company_ai.store import Store
from company_ai.workers.scripted import ScriptedWorker
from tests.conftest import BUNDLE, EVENT, FIXTURES, HUB, ROOT, event, write_bundle

SECRET = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
ACCESS_KEY = "AKIAIOSFODNN7EXAMPLE"


class CountingAdapter:
    adapter_id = "fixture"
    transport = "local_files"

    def __init__(self, inner: FixtureAdapter, store: Store | None = None):
        self.inner = inner
        self.store = store
        self.calls: list[str] = []
        self.timeouts: list[float] = []
        self.saw_started = False

    def read(self, target, timeout, max_bytes):
        self.calls.append(target.resource_alias)
        self.timeouts.append(timeout)
        if self.store is not None:
            row = self.store.connection.execute(
                "SELECT status FROM steps WHERE kind='provider_attempt' ORDER BY seq DESC LIMIT 1"
            ).fetchone()
            self.saw_started = row is not None and row["status"] == "started"
        return self.inner.read(target, timeout, max_bytes)


def _runtime(tmp_path: Path, bundle_path: Path, worker, adapter, **kwargs) -> tuple[Runtime, Store]:
    store = kwargs.pop("store", None) or Store.open(tmp_path / "state.sqlite")
    runtime = Runtime(
        store=store,
        bundle=load_bundle(bundle_path),
        adapter=adapter,
        worker=worker,
        reports_dir=tmp_path / "reports",
        identity=kwargs.get("identity", "investigator"),
        worker_mode=kwargs.get("worker_mode", "scripted"),
        service_mode=kwargs.get("service_mode", "fixture"),
        live=kwargs.get("live", False),
        model_id=kwargs.get("model_id"),
        clock=kwargs.get("clock"),
    )
    return runtime, store


def _fixture_runtime(tmp_path: Path, bundle_path: Path, worker, **kwargs):
    store = Store.open(tmp_path / "state.sqlite")
    adapter = CountingAdapter(FixtureAdapter(FIXTURES), store)
    runtime, _ = _runtime(tmp_path, bundle_path, worker, adapter, store=store, **kwargs)
    return runtime, store, adapter


def _allow(document: dict, identity: str, aliases: list[str]) -> None:
    document["policy"]["identities"][identity]["allow"] = [
        {"operation": "diagnostics.read", "resources": aliases}
    ]


def test_01_fixture_command_writes_an_evidence_backed_report(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(socket, "create_connection", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("network")))
    output = tmp_path / "demo"
    assert main(["demo", "--output", str(output), "--bundle", str(BUNDLE), "--event", str(EVENT), "--fixture-dir", str(FIXTURES)]) == 0
    reports = list((output / "reports").glob("*.json"))
    document = json.loads(reports[0].read_text(encoding="utf-8"))
    assert document["status"] == "succeeded"
    assert document["worker_mode"] == "scripted"
    assert document["service_mode"] == "fixture"
    assert document["model_id"] is None
    assert document["adapters"]["worker"] == "scripted"
    assert document["adapters"]["service"] == "fixture"
    assert document["governance"]["policy_version"] == "2026.11.25.1"
    assert document["governance"]["cursor_hub_commit"] == "c5328edae2ac54df38244d7a4ef9353de1553a84"
    assert document["completion"]["passed"] is True
    assert document["authorization_decisions"][0]["allowed"] is True
    assert document["recorded_facts"][0]["payload"]["service"] == "demo-service-a"
    assert document["recorded_facts"][0]["run_id"] == document["run_id"]
    assert document["model_report"]["hypotheses"]
    assert document["model_report"]["missing"]
    assert "do not prove that the diagnosis is true" in " ".join(document["limitations"])
    markdown = Path(document["run_id"] and reports[0].with_suffix(".md")).read_text(encoding="utf-8")
    assert "Model hypotheses" in markdown and "Recorded facts" in markdown
    second = main(["demo", "--output", str(output), "--bundle", str(BUNDLE), "--event", str(EVENT), "--fixture-dir", str(FIXTURES)])
    assert second == 0
    store = Store.open(output / "state.sqlite", readonly=True)
    try:
        runs = store.connection.execute("SELECT COUNT(*) AS n FROM runs").fetchone()["n"]
        evidence = store.connection.execute("SELECT COUNT(*) AS n FROM evidence").fetchone()["n"]
    finally:
        store.close()
    assert runs == 1 and evidence == 1


def test_02_allowed_read_is_recorded_before_execution(tmp_path: Path):
    runtime, store, adapter = _fixture_runtime(tmp_path, BUNDLE, ScriptedWorker("investigate"))
    outcome = runtime.handle_event(event("evt-allowed"))
    assert outcome.status == "succeeded"
    assert adapter.calls == ["demo-service-a"]
    assert adapter.saw_started is True
    evidence = store.evidence(outcome.run_id)
    assert evidence[0]["resource_alias"] == "demo-service-a"
    assert json.loads(evidence[0]["payload_json"])["recent_error_count"] == 3
    assert len(json.loads(evidence[0]["payload_json"])) <= 8192


def test_03_forbidden_resource_does_not_call_the_adapter(tmp_path: Path):
    runtime, store, adapter = _fixture_runtime(tmp_path, BUNDLE, ScriptedWorker("forbidden"))
    outcome = runtime.handle_event(event("evt-forbidden", "demo-service-a"))
    assert outcome.status == "failed"
    assert outcome.failure_code == "missing_evidence"
    assert adapter.calls == []
    decisions = store.decisions(outcome.run_id)
    assert decisions[0]["allowed"] == 0
    assert decisions[0]["reason"] == "resource_not_allowed"
    assert decisions[0]["resource_alias"] == "demo-service-b"
    assert [row for row in store.steps(outcome.run_id) if row["kind"] == "provider_attempt"] == []


def test_04_identity_changes_permission_and_spoofing_does_not(tmp_path: Path):
    denied, _, denied_adapter = _fixture_runtime(
        tmp_path / "inv", BUNDLE, ScriptedWorker("investigate"), identity="investigator"
    )
    denied_outcome = denied.handle_event(event("evt-b-inv", "demo-service-b"))
    assert denied_outcome.status != "succeeded"
    assert denied_adapter.calls == []

    allowed, store, allowed_adapter = _fixture_runtime(
        tmp_path / "aud", BUNDLE, ScriptedWorker("investigate"), identity="auditor"
    )
    allowed_outcome = allowed.handle_event(event("evt-b-aud", "demo-service-b"))
    assert allowed_outcome.status == "succeeded"
    assert allowed_adapter.calls == ["demo-service-b"]
    assert store.get_run(allowed_outcome.run_id)["identity"] == "auditor"

    spoof, spoof_store, spoof_adapter = _fixture_runtime(
        tmp_path / "spoof", BUNDLE, ScriptedWorker("spoof_identity"), identity="investigator"
    )
    spoof_outcome = spoof.handle_event(event("evt-spoof", "demo-service-a"))
    assert spoof_outcome.status == "failed"
    assert spoof_outcome.failure_code == "rejected_untrusted_field"
    assert spoof_adapter.calls == []
    assert spoof_store.get_run(spoof_outcome.run_id)["identity"] == "investigator"
    assert all(row["allowed"] == 0 for row in spoof_store.decisions(spoof_outcome.run_id))


def test_05_hostile_provider_text_cannot_change_policy(tmp_path: Path):
    original = BUNDLE.read_bytes()

    def mutate(document: dict) -> None:
        _allow(document, "investigator", ["demo-service-a", "demo-hostile"])

    bundle_path = write_bundle(tmp_path, mutate)
    before = load_bundle(bundle_path)
    runtime, store, adapter = _fixture_runtime(tmp_path, bundle_path, ScriptedWorker("hostile_followup"))
    outcome = runtime.handle_event(event("evt-hostile", "demo-hostile"))
    after = load_bundle(bundle_path)
    assert outcome.status == "succeeded"
    assert adapter.calls == ["demo-hostile"]
    decisions = store.decisions(outcome.run_id)
    assert any(row["resource_alias"] == "demo-service-b" and row["allowed"] == 0 for row in decisions)
    assert after.policy_version == before.policy_version
    assert after.limits == before.limits
    assert store.get_run(outcome.run_id)["identity"] == "investigator"
    report = json.loads(Path(outcome.report_json_path).read_text(encoding="utf-8"))
    assert "treated as untrusted data" in " ".join(report["model_report"]["observations"])
    assert "auditor" not in " ".join(report["model_report"]["observations"])
    assert BUNDLE.read_bytes() == original


def test_06_policy_change_applies_to_a_new_run_only(tmp_path: Path):
    worker_type = ScriptedWorker
    first, _, first_adapter = _fixture_runtime(tmp_path / "v1", BUNDLE, worker_type("investigate"))
    first_outcome = first.handle_event(event("evt-policy-v1"))
    assert first_outcome.status == "succeeded" and first_adapter.calls == ["demo-service-a"]

    def mutate(document: dict) -> None:
        document["policy"]["policy_version"] = "2026.11.25.2"
        _allow(document, "investigator", ["demo-service-b"])

    changed = write_bundle(tmp_path / "v2", mutate)
    second, store, second_adapter = _fixture_runtime(tmp_path / "v2", changed, worker_type("investigate"))
    second_outcome = second.handle_event(event("evt-policy-v2"))
    assert second_adapter.calls == []
    assert second_outcome.status != "succeeded"
    assert store.decisions(second_outcome.run_id)[0]["policy_version"] == "2026.11.25.2"
    assert store.decisions(second_outcome.run_id)[0]["reason"] == "resource_not_allowed"
    assert first_outcome.run_id != second_outcome.run_id


def test_08_duplicate_event_is_reused_and_conflict_is_rejected(tmp_path: Path):
    runtime, store, adapter = _fixture_runtime(tmp_path, BUNDLE, ScriptedWorker("investigate"))
    payload = event("evt-dup")
    first = runtime.handle_event(payload)
    calls = len(adapter.calls)
    second = runtime.handle_event(dict(payload))
    assert second.reused is True
    assert second.run_id == first.run_id
    assert len(adapter.calls) == calls
    with pytest.raises(ConflictError):
        runtime.handle_event(event("evt-dup", summary="Different synthetic content."))
    assert store.connection.execute("SELECT COUNT(*) AS n FROM runs").fetchone()["n"] == 1


def test_09_malformed_unknown_invalid_and_oversized_fail(tmp_path: Path):
    malformed, _, adapter = _fixture_runtime(tmp_path / "bad", BUNDLE, ScriptedWorker("malformed"))
    outcome = malformed.handle_event(event("evt-malformed-model"))
    assert outcome.status == "failed"
    assert outcome.failure_code == "malformed_model_output"
    assert adapter.calls == []

    unknown, _, unknown_adapter = _fixture_runtime(tmp_path / "tool", BUNDLE, ScriptedWorker("unknown_tool"))
    unknown_outcome = unknown.handle_event(event("evt-unknown-tool"))
    assert unknown_outcome.failure_code == "unknown_tool"
    assert unknown_adapter.calls == []

    def widen(document: dict) -> None:
        document["policy"]["allow_all"] = True

    with pytest.raises(Exception) as caught:
        load_bundle(write_bundle(tmp_path / "policy", widen))
    assert getattr(caught.value, "code", "") in {"validation", "rejected_untrusted_field"}

    def allow_bad(document: dict) -> None:
        _allow(document, "investigator", ["demo-service-a", "demo-malformed", "demo-oversized"])

    bundle_path = write_bundle(tmp_path / "docs", allow_bad)
    bad_doc, _, _ = _fixture_runtime(tmp_path / "docs", bundle_path, ScriptedWorker("investigate"))
    malformed_doc = bad_doc.handle_event(event("evt-bad-doc", "demo-malformed"))
    oversized, _, oversized_adapter = _fixture_runtime(tmp_path / "big", bundle_path, ScriptedWorker("investigate"))
    oversized_outcome = oversized.handle_event(event("evt-big-doc", "demo-oversized"))
    assert malformed_doc.failure_code == "malformed"
    assert oversized_outcome.failure_code == "oversized"
    assert oversized_adapter.calls == ["demo-oversized"]
    assert oversized.store.evidence(oversized_outcome.run_id) == []
    assert malformed_doc.status != "succeeded" and oversized_outcome.status != "succeeded"


def test_10_timeout_denial_and_budget_do_not_succeed(tmp_path: Path):
    def allow_timeout(document: dict) -> None:
        _allow(document, "investigator", ["demo-timeout"])

    bundle_path = write_bundle(tmp_path / "timeout", allow_timeout)
    runtime, store, adapter = _fixture_runtime(tmp_path / "timeout", bundle_path, ScriptedWorker("investigate"))
    outcome = runtime.handle_event(event("evt-timeout", "demo-timeout"))
    assert outcome.status == "failed"
    assert outcome.failure_code == "timeout"
    assert adapter.calls == ["demo-timeout", "demo-timeout"]
    assert store.evidence(outcome.run_id) == []
    budget = json.loads(store.get_run(outcome.run_id)["budget_json"])
    assert budget["used"]["provider_attempts"] == 2

    denied_runtime, denied_store, denied_adapter = _fixture_runtime(
        tmp_path / "budget", BUNDLE, ScriptedWorker("deny_loop")
    )
    denied = denied_runtime.handle_event(event("evt-budget"))
    assert denied.status == "failed"
    assert denied.failure_code == "budget_exhausted"
    assert denied_adapter.calls == []
    assert denied_store.evidence(denied.run_id) == []

    clock = ManualClock()
    expired, _, expired_adapter = _fixture_runtime(
        tmp_path / "clock", BUNDLE, ScriptedWorker("expire", clock=clock), clock=clock
    )
    expired_outcome = expired.handle_event(event("evt-expire"))
    assert expired_outcome.failure_code == "deadline_exceeded"
    assert expired_adapter.calls == []


def test_11_interrupted_work_is_inspectable_and_replay_is_local(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(socket, "create_connection", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("network")))
    bundle = load_bundle(BUNDLE)
    store = Store.open(tmp_path / "state.sqlite")
    payload = event("evt-interrupt")
    from company_ai.contracts import fingerprint

    run_id, reused = store.admit(
        event=payload,
        event_fingerprint=fingerprint(payload),
        identity="investigator",
        workflow_id=bundle.workflow["workflow_id"],
        mission_id=bundle.mission["mission_id"],
        policy_version=bundle.policy_version,
        bundle_id=bundle.bundle_id,
        bundle_version=bundle.bundle_version,
        bundle_fingerprint=bundle.fingerprint,
        worker_mode="scripted",
        service_mode="fixture",
        worker_adapter="scripted",
        service_adapter="fixture",
        model_id=None,
        budget={"limits": bundle.limits, "used": {"model_turns": 0, "tool_proposals": 0, "provider_attempts": 0}},
    )
    assert reused is False
    store.start_step(run_id, "provider_attempt", {"operation": "diagnostics.read"})
    store.close()

    inspected = Store.open(tmp_path / "state.sqlite")
    history = inspect_run(inspected, run_id)
    assert history["status"] == "interrupted"
    assert history["evidence_ids"] == []
    inspected.close()

    replay_store = Store.open(tmp_path / "state.sqlite", readonly=True)
    replay = replay_run(replay_store, run_id)
    assert replay["external_calls"] == 0
    assert replay["status"] == "interrupted"
    replay_store.close()

    original = Store.open(tmp_path / "state.sqlite")
    counter = CountingAdapter(FixtureAdapter(FIXTURES), original)
    resumed, _ = _runtime(tmp_path, BUNDLE, ScriptedWorker("investigate"), counter, store=original)
    outcome = resumed.handle_event(payload)
    assert outcome.reused is True
    assert outcome.status == "interrupted"
    assert counter.calls == []


def test_12_unknown_and_cross_run_evidence_cannot_complete(tmp_path: Path):
    first, store, _ = _fixture_runtime(tmp_path, BUNDLE, ScriptedWorker("investigate"))
    done = first.handle_event(event("evt-evidence-source"))
    evidence_id = store.evidence(done.run_id)[0]["evidence_id"]
    cross, _, cross_adapter = _fixture_runtime(
        tmp_path / "cross", BUNDLE, ScriptedWorker("cite", evidence_id=evidence_id)
    )
    # The cited id exists, but it belongs to the other database/run. This runtime has an empty store.
    cross_outcome = cross.handle_event(event("evt-cross"))
    assert cross_outcome.status == "failed"
    assert cross_outcome.failure_code == "invalid_evidence"
    assert cross_adapter.calls == []

    missing, _, _ = _fixture_runtime(tmp_path / "missing", BUNDLE, ScriptedWorker("cite", evidence_id="ev_missing"))
    missing_outcome = missing.handle_event(event("evt-missing-evidence"))
    assert missing_outcome.failure_code == "invalid_evidence"

    same_store_worker = ScriptedWorker("cite", evidence_id=evidence_id)
    # Reuse the first store but a new event so the foreign id is visible and still rejected.
    runtime, _ = _runtime(tmp_path, BUNDLE, same_store_worker, CountingAdapter(FixtureAdapter(FIXTURES), store), store=store)
    reused_db = runtime.handle_event(event("evt-cross-same-db"))
    assert reused_db.failure_code == "invalid_evidence"
    assert reused_db.status != "succeeded"


def test_13_live_modes_report_missing_settings_and_do_not_fall_back(tmp_path: Path, monkeypatch):
    constructed: list[str] = []

    class Guard:
        def __init__(self, *args, **kwargs):
            constructed.append("s3")

        def read(self, *args, **kwargs):
            raise AssertionError("live client was called")

    monkeypatch.setattr("company_ai.wiring.S3DiagnosticsAdapter", Guard)
    monkeypatch.setattr("company_ai.wiring.FixtureAdapter.read", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("fixture fallback")))
    config = ROOT / "examples" / "config.example.toml"
    code = main(
        [
            "run",
            "--config",
            str(config),
            "--event",
            str(EVENT),
            "--service",
            "s3",
            "--bundle",
            str(BUNDLE),
            "--database",
            str(tmp_path / "state.sqlite"),
            "--reports-dir",
            str(tmp_path / "reports"),
            "--fixture-dir",
            str(FIXTURES),
        ]
    )
    assert code == 2
    assert constructed == []
    document = load_config(config)
    settings = settings_from_document(
        document,
        config_dir=config.parent,
        live=True,
        service="s3",
        bundle=str(BUNDLE),
        database=str(tmp_path / "state.sqlite"),
        reports_dir=str(tmp_path / "reports"),
        fixture_dir=str(FIXTURES),
    )
    with pytest.raises(SetupError) as caught:
        require_ready(settings, load_bundle(BUNDLE), json.loads(EVENT.read_text(encoding="utf-8")))
    assert "--live" not in caught.value.missing  # live was set; other settings are still missing
    assert "service.s3.profile" in caught.value.missing
    assert "service.s3.region" in caught.value.missing
    assert "resources.demo-service-a.s3.bucket" in caught.value.missing
    assert "Fixture mode was not substituted" in caught.value.message

    ollama_settings = settings_from_document(
        document,
        config_dir=config.parent,
        live=False,
        worker="ollama",
        bundle=str(BUNDLE),
        database=str(tmp_path / "db.sqlite"),
        reports_dir=str(tmp_path / "reports"),
        fixture_dir=str(FIXTURES),
    )
    with pytest.raises(SetupError) as ollama_error:
        require_ready(ollama_settings, load_bundle(BUNDLE), json.loads(EVENT.read_text(encoding="utf-8")))
    assert "--live" in ollama_error.value.missing
    assert "worker.model" in ollama_error.value.missing


def test_14_credentials_do_not_enter_worker_context_or_audit(tmp_path: Path, monkeypatch, caplog):
    monkeypatch.setenv("AWS_SECRET_ACCESS_KEY", SECRET)
    monkeypatch.setenv("AWS_ACCESS_KEY_ID", ACCESS_KEY)
    caplog.set_level("INFO")
    runtime, store, _ = _fixture_runtime(tmp_path, BUNDLE, ScriptedWorker("investigate"))
    outcome = runtime.handle_event(event("evt-secrets"))
    prompts = json.dumps(runtime.worker.prompts)
    report = Path(outcome.report_json_path).read_text(encoding="utf-8")
    database = (tmp_path / "state.sqlite").read_bytes()
    logs = caplog.text
    for secret in (SECRET, ACCESS_KEY):
        assert secret not in prompts
        assert secret not in report
        assert secret.encode() not in database
        assert secret not in logs
    context = build_worker_context(load_bundle(BUNDLE), event("evt-context"), [])
    assert "Prefer incremental changes" not in json.dumps(context)
    assert "policy_version" not in context
    assert "YOUR_BUCKET" not in json.dumps(context)

    config = tmp_path / "secret.toml"
    config.write_text(
        (ROOT / "examples" / "config.example.toml").read_text(encoding="utf-8")
        + f'\nsecret_access_key = "{SECRET}"\n',
        encoding="utf-8",
    )
    with pytest.raises(SetupError) as caught:
        load_config(config)
    assert SECRET not in str(caught.value)


def test_15_provenance_matches_the_hub_and_skill_text_is_loaded(tmp_path: Path):
    bundle = load_bundle(BUNDLE)
    assert bundle.provenance["commit"] == "c5328edae2ac54df38244d7a4ef9353de1553a84"
    if not HUB.is_dir():
        pytest.skip("cursor-hub sibling checkout is not available")
    assert verify_provenance(bundle, HUB) == []
    alpha = write_bundle(tmp_path / "alpha", skill_text="ALPHA_SKILL_SENTINEL investigate the alias.\n")
    beta = write_bundle(tmp_path / "beta", skill_text="BETA_SKILL_SENTINEL investigate the alias.\n")
    alpha_worker = ScriptedWorker("investigate")
    beta_worker = ScriptedWorker("investigate")
    alpha_runtime, _, _ = _fixture_runtime(tmp_path / "alpha", alpha, alpha_worker)
    beta_runtime, _, _ = _fixture_runtime(tmp_path / "beta", beta, beta_worker)
    assert alpha_runtime.handle_event(event("evt-skill-alpha")).status == "succeeded"
    assert beta_runtime.handle_event(event("evt-skill-beta")).status == "succeeded"
    alpha_text = json.dumps(alpha_worker.prompts)
    beta_text = json.dumps(beta_worker.prompts)
    assert "ALPHA_SKILL_SENTINEL" in alpha_text and "BETA_SKILL_SENTINEL" not in alpha_text
    assert "BETA_SKILL_SENTINEL" in beta_text and "ALPHA_SKILL_SENTINEL" not in beta_text
    assert "Prefer incremental changes" not in alpha_text
    assert "Present drafts before writing" not in alpha_text


def test_request_timeout_is_capped_by_remaining_duration(tmp_path: Path):
    clock = ManualClock(elapsed=28)
    runtime, _, adapter = _fixture_runtime(tmp_path, BUNDLE, ScriptedWorker("investigate"), clock=clock)
    outcome = runtime.handle_event(event("evt-timeout-cap"))
    assert outcome.status == "succeeded"
    assert adapter.timeouts[0] == pytest.approx(2)


def test_bucket_override_is_rejected(tmp_path: Path):
    runtime, _, adapter = _fixture_runtime(tmp_path, BUNDLE, ScriptedWorker("bucket_override"))
    outcome = runtime.handle_event(event("evt-bucket"))
    assert outcome.failure_code == "rejected_untrusted_field"
    assert adapter.calls == []


def test_same_event_does_not_run_twice_concurrently(tmp_path: Path):
    database = tmp_path / "state.sqlite"
    Store.open(database).close()
    started = threading.Event()
    release = threading.Event()

    class Blocking(CountingAdapter):
        def read(self, target, timeout, max_bytes):
            self.calls.append(target.resource_alias)
            started.set()
            assert release.wait(5)
            return self.inner.read(target, timeout, max_bytes)

    adapter = Blocking(FixtureAdapter(FIXTURES))
    second_worker = ScriptedWorker("investigate")
    payload = event("evt-race")
    box: dict = {}

    def run_first():
        store = Store.open(database)
        runtime, _ = _runtime(tmp_path, BUNDLE, ScriptedWorker("investigate"), adapter, store=store)
        try:
            box["first"] = runtime.handle_event(payload)
        finally:
            store.close()

    thread = threading.Thread(target=run_first)
    thread.start()
    assert started.wait(5)
    store_b = Store.open(database)
    second, _ = _runtime(tmp_path, BUNDLE, second_worker, adapter, store=store_b)
    reused = second.handle_event(payload)
    store_b.close()
    assert reused.reused is True
    assert second_worker.prompts == []
    assert adapter.calls == ["demo-service-a"]
    release.set()
    thread.join(5)
    assert box["first"].status == "succeeded"
    assert box["first"].run_id == reused.run_id


def test_s3_denied_provider_access_fails_without_evidence(tmp_path: Path):
    def concrete(document: dict) -> None:
        document["resources"]["demo-service-a"]["s3"] = {
            "bucket": "demo-bucket",
            "key": "diagnostics/demo-service-a.json",
        }

    bundle_path = write_bundle(tmp_path, concrete)
    error = Exception(f"AWS_SECRET_ACCESS_KEY={SECRET}")
    error.response = {"Error": {"Code": "AccessDenied"}, "ResponseMetadata": {"HTTPStatusCode": 403}}

    class Client:
        calls = 0

        def get_object(self, **kwargs):
            Client.calls += 1
            assert kwargs["Bucket"] == "demo-bucket"
            assert kwargs["Key"] == "diagnostics/demo-service-a.json"
            raise error

    adapter = S3DiagnosticsAdapter(profile="explicit-profile", region="eu-central-1", client=Client())
    store = Store.open(tmp_path / "state.sqlite")
    runtime, _ = _runtime(
        tmp_path,
        bundle_path,
        ScriptedWorker("investigate"),
        adapter,
        store=store,
        service_mode="live",
        live=True,
    )
    outcome = runtime.handle_event(event("evt-s3-denied"))
    assert outcome.status == "failed"
    assert outcome.failure_code == "provider_denied"
    assert Client.calls == 1
    assert store.evidence(outcome.run_id) == []
    assert SECRET.encode() not in (tmp_path / "state.sqlite").read_bytes()


def test_example_config_live_check_selects_nothing_live(monkeypatch, capsys):
    monkeypatch.chdir(ROOT)
    code = main(["live", "check", "--config", "examples/config.example.toml"])
    assert code == 0
    assert "no live adapter selected" in capsys.readouterr().out


def test_validate_command_checks_the_recorded_hub(capsys):
    if not HUB.is_dir():
        pytest.skip("cursor-hub sibling checkout is not available")
    code = main(["validate", "--bundle", str(BUNDLE), "--fixture-dir", str(FIXTURES), "--config", str(ROOT / "examples" / "config.example.toml"), "--hub", str(HUB)])
    captured = capsys.readouterr()
    assert code == 0
    assert "provenance: ok c5328edae2ac54df38244d7a4ef9353de1553a84" in captured.out


def test_live_s3_without_opt_in_is_rejected_by_the_runtime(tmp_path: Path):
    adapter = S3DiagnosticsAdapter(profile="explicit", region="eu-central-1", client=object())
    store = Store.open(tmp_path / "state.sqlite")
    with pytest.raises(SetupError) as caught:
        Runtime(
            store=store,
            bundle=load_bundle(BUNDLE),
            adapter=adapter,
            worker=ScriptedWorker(),
            reports_dir=tmp_path / "reports",
            identity="investigator",
            worker_mode="scripted",
            service_mode="live",
            live=False,
        )
    assert caught.value.missing == ["--live"]
    assert "not substituted" in caught.value.message
