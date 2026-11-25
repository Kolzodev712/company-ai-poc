"""Bounded event loop. The model proposes; the gateway executes."""

from __future__ import annotations

import json
import logging
import time
from pathlib import Path
from typing import Any, Mapping

from company_ai.broker import SecurityBroker
from company_ai.contracts import ContractError, RunOutcome, canonical, fingerprint, validate
from company_ai.errors import CompanyAIError, SetupError
from company_ai.gateway import ToolGateway
from company_ai.governance import LoadedBundle
from company_ai.redact import redact
from company_ai.reporting import build_report, write_report
from company_ai.store import Store

logger = logging.getLogger("company_ai.runtime")

_SUBMIT_ARGUMENTS: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["observations", "hypotheses", "missing", "evidence_ids"],
    "properties": {
        "observations": {"type": "array", "items": {"type": "string"}},
        "hypotheses": {"type": "array", "items": {"type": "string"}},
        "missing": {"type": "array", "items": {"type": "string"}},
        "evidence_ids": {"type": "array", "items": {"type": "string"}},
    },
}


class SystemClock:
    def __init__(self) -> None:
        self._start = time.monotonic()

    def elapsed(self) -> float:
        return time.monotonic() - self._start


class ManualClock:
    def __init__(self, elapsed: float = 0) -> None:
        self.elapsed_value = elapsed

    def elapsed(self) -> float:
        return self.elapsed_value


def build_worker_context(
    bundle: LoadedBundle,
    event: Mapping[str, Any],
    evidence: list[dict[str, Any]],
) -> dict[str, Any]:
    """Worker-visible task. Policy, credentials, and provider locations are omitted."""
    operation = bundle.document["operations"]["diagnostics.read"]
    return {
        "mission_objective": bundle.mission["objective"],
        "workflow_id": bundle.workflow["workflow_id"],
        "skill_id": bundle.skill_id,
        "skill_instructions": bundle.skill_text,
        "event": {
            "event_id": event["event_id"],
            "kind": event["kind"],
            "resource_alias": event["resource_alias"],
            "observed_at": event["observed_at"],
            "summary": event["summary"],
        },
        "tools": [
            {
                "name": "diagnostics.read",
                "description": operation["description"],
                "arguments": operation["arguments"],
            },
            {
                "name": "submit_report",
                "description": (
                    "Submit observations, hypotheses, missing information, and evidence ids from this run."
                ),
                "arguments": _SUBMIT_ARGUMENTS,
            },
        ],
        "evidence": evidence,
    }


class Runtime:
    def __init__(
        self,
        *,
        store: Store,
        bundle: LoadedBundle,
        adapter: Any,
        worker: Any,
        reports_dir: Path,
        identity: str,
        worker_mode: str,
        service_mode: str,
        live: bool,
        model_id: str | None = None,
        clock: SystemClock | ManualClock | None = None,
    ):
        self._reject_mismatch(adapter, worker, worker_mode, service_mode, live)
        self.store = store
        self.bundle = bundle
        self.adapter = adapter
        self.worker = worker
        self.reports_dir = Path(reports_dir)
        self.identity = identity
        self.worker_mode = worker_mode
        self.service_mode = service_mode
        self.live = live
        self.model_id = model_id
        self.clock = clock or SystemClock()
        provider = "s3" if service_mode == "live" else "fixture"
        self.gateway = ToolGateway(
            bundle=bundle,
            broker=SecurityBroker(bundle),
            adapter=adapter,
            store=store,
            service_mode=provider,
        )

    @staticmethod
    def _reject_mismatch(adapter: Any, worker: Any, worker_mode: str, service_mode: str, live: bool) -> None:
        if service_mode == "live" and adapter.adapter_id != "s3":
            raise SetupError("adapter_mismatch", "Live service mode cannot use the fixture adapter.", ["service.mode"])
        if service_mode == "fixture" and adapter.adapter_id != "fixture":
            raise SetupError("adapter_mismatch", "Fixture service mode cannot use the S3 adapter.", ["service.mode"])
        if worker_mode == "real_model" and worker.adapter_id != "ollama":
            raise SetupError("adapter_mismatch", "Real-model mode cannot use the scripted worker.", ["worker.mode"])
        if worker_mode == "scripted" and worker.adapter_id != "scripted":
            raise SetupError("adapter_mismatch", "Scripted mode cannot use the Ollama worker.", ["worker.mode"])
        if adapter.adapter_id == "s3" and not live:
            raise SetupError(
                "live_opt_in",
                "S3 was selected without --live. Fixture mode was not substituted.",
                ["--live"],
            )
        if worker.adapter_id == "ollama" and not live:
            raise SetupError(
                "live_opt_in",
                "Ollama was selected without --live. The scripted worker was not substituted.",
                ["--live"],
            )

    def handle_event(self, event: dict[str, Any]) -> RunOutcome:
        validate("event", event)
        budget = {
            "limits": self.bundle.limits,
            "used": {"model_turns": 0, "tool_proposals": 0, "provider_attempts": 0},
        }
        run_id, reused = self.store.admit(
            event=event,
            event_fingerprint=fingerprint(event),
            identity=self.identity,
            workflow_id=self.bundle.workflow["workflow_id"],
            mission_id=self.bundle.mission["mission_id"],
            policy_version=self.bundle.policy_version,
            bundle_id=self.bundle.bundle_id,
            bundle_version=self.bundle.bundle_version,
            bundle_fingerprint=self.bundle.fingerprint,
            worker_mode=self.worker_mode,
            service_mode=self.service_mode,
            worker_adapter=self.worker.adapter_id,
            service_adapter=self.adapter.adapter_id,
            model_id=self.model_id,
            budget=budget,
        )
        if reused:
            logger.info("event reused run=%s event=%s", run_id, event["event_id"])
            return self._outcome(run_id, True)
        try:
            self._execute(run_id, event, budget)
        except CompanyAIError as exc:
            self._fail(run_id, event, exc.code, exc.message, None)
        except Exception as exc:
            self._fail(run_id, event, "internal_error", redact(f"{exc.__class__.__name__}")[:300], None)
        return self._outcome(run_id, False)

    def _execute(self, run_id: str, event: dict[str, Any], budget: dict[str, Any]) -> None:
        last_tool: dict[str, Any] | None = None
        while True:
            if self._remaining() <= 0:
                self._fail(run_id, event, "deadline_exceeded", "Run duration budget is exhausted", None)
                return
            limits = budget["limits"]
            if budget["used"]["model_turns"] >= limits["max_model_turns"]:
                self._fail(run_id, event, "budget_exhausted", "Model turn budget is exhausted", None)
                return
            context = build_worker_context(self.bundle, event, self._evidence_view(run_id))
            if len(canonical(context).encode("utf-8")) > limits["max_input_bytes"]:
                self._fail(run_id, event, "input_too_large", "Worker input exceeds max_input_bytes", None)
                return
            step_id = self.store.start_step(run_id, "model_turn")
            budget["used"]["model_turns"] += 1
            self.store.update_budget(run_id, budget)
            if getattr(self.worker, "adapter_id", None) == "ollama":
                self.worker.timeout_seconds = min(float(limits["request_timeout_seconds"]), max(self._remaining(), 0.1))
            try:
                raw = self.worker.turn(context, last_tool)
            except CompanyAIError as exc:
                self.store.finish_step(step_id, "failed", {"error_code": exc.code})
                self._fail(run_id, event, exc.code, exc.message, None)
                return
            except Exception as exc:
                self.store.finish_step(step_id, "failed", {"error_code": "malformed_model_output"})
                self._fail(run_id, event, "malformed_model_output", redact(f"{exc.__class__.__name__}")[:300], None)
                return
            if not isinstance(raw, dict):
                self.store.finish_step(step_id, "failed", {"error_code": "malformed_model_output"})
                self._fail(run_id, event, "malformed_model_output", "Worker output was not an object", None)
                return
            kind = raw.get("kind")
            if kind == "final_report":
                try:
                    validate("final_report", raw)
                except ContractError as exc:
                    self.store.finish_step(step_id, "failed", {"error_code": exc.code})
                    self._fail(run_id, event, exc.code, exc.message, None)
                    return
                self.store.finish_step(step_id, "succeeded", {"kind": "final_report"})
                self._complete(run_id, event, raw)
                return
            if kind != "tool_proposal":
                self.store.finish_step(step_id, "failed", {"error_code": "malformed_model_output"})
                self._fail(run_id, event, "malformed_model_output", "Worker output was not a proposal or report", None)
                return
            if budget["used"]["tool_proposals"] >= limits["max_tool_proposals"]:
                self.store.finish_step(step_id, "failed", {"error_code": "budget_exhausted"})
                self._fail(run_id, event, "budget_exhausted", "Tool proposal budget is exhausted", None)
                return
            budget["used"]["tool_proposals"] += 1
            self.store.update_budget(run_id, budget)
            operation = raw.get("operation") if isinstance(raw.get("operation"), str) else "unknown"
            self.store.finish_step(step_id, "succeeded", {"kind": "tool_proposal", "operation": operation})
            if self._remaining() <= 0:
                self._fail(run_id, event, "deadline_exceeded", "Run duration budget is exhausted", None)
                return
            result = self.gateway.invoke(
                run_id=run_id,
                identity=self.identity,
                proposal=raw,
                budget=budget,
                remaining_seconds=self._remaining(),
            )
            if result["status"] in {"invalid", "error"}:
                self._fail(run_id, event, str(result["error_code"]), str(result["message"]), None)
                return
            last_tool = result

    def _complete(self, run_id: str, event: dict[str, Any], model_report: dict[str, Any]) -> None:
        completion = self._assess(run_id, event, model_report)
        if not completion["passed"]:
            self._fail(run_id, event, completion["failure_code"], completion["failure_code"], model_report)
            return
        self._write(run_id, event, "succeeded", None, None, model_report, completion)

    def _fail(
        self,
        run_id: str,
        event: dict[str, Any],
        code: str,
        detail: str,
        model_report: dict[str, Any] | None,
    ) -> None:
        completion = {
            "passed": False,
            "failure_code": code,
            "checks": [
                {"name": "same_run_evidence_references", "passed": False},
                {"name": "required_diagnostics_read", "passed": False},
            ],
        }
        if model_report is not None:
            assessed = self._assess(run_id, event, model_report)
            completion["checks"] = assessed["checks"]
        self._write(run_id, event, "failed", code, detail, model_report, completion)

    def _write(
        self,
        run_id: str,
        event: dict[str, Any],
        status: str,
        failure_code: str | None,
        failure_detail: str | None,
        model_report: dict[str, Any] | None,
        completion: dict[str, Any],
    ) -> None:
        current = self.store.get_run(run_id)
        if current["status"] != "running":
            return
        # Status is written after the files so a crash leaves the run unfinished rather than successful.
        preview = dict(current)
        preview["status"] = status
        preview["failure_code"] = failure_code
        preview["failure_detail"] = failure_detail
        document = build_report(
            run=preview,
            decisions=self.store.decisions(run_id),
            evidence_rows=self.store.evidence(run_id),
            provenance=self.bundle.provenance,
            completion=completion,
            model_report=model_report,
            service_transport=str(getattr(self.adapter, "transport", "unknown")),
            worker_transport=str(getattr(self.worker, "transport_name", "unknown")),
        )
        json_path, md_path = write_report(self.reports_dir, run_id, document)
        self.store.finish_run(
            run_id,
            status,
            failure_code,
            failure_detail,
            report_json=json.dumps(document, sort_keys=True),
            report_md_path=str(md_path),
            report_json_path=str(json_path),
        )
        logger.info("run finished run=%s status=%s failure=%s", run_id, status, failure_code or "none")

    def _assess(self, run_id: str, event: Mapping[str, Any], model_report: Mapping[str, Any]) -> dict[str, Any]:
        references_ok = True
        for evidence_id in model_report["evidence_ids"]:
            row = self.store.find_evidence(str(evidence_id))
            if row is None or row["run_id"] != run_id:
                references_ok = False
        required_ok = False
        for row in self.store.evidence(run_id):
            if (
                row["operation"] == "diagnostics.read"
                and row["resource_alias"] == event["resource_alias"]
                and row["evidence_id"] in model_report["evidence_ids"]
            ):
                required_ok = True
        failure = None
        if not references_ok:
            failure = "invalid_evidence"
        elif not required_ok:
            failure = "missing_evidence"
        return {
            "passed": failure is None,
            "failure_code": failure,
            "checks": [
                {"name": "same_run_evidence_references", "passed": references_ok},
                {"name": "required_diagnostics_read", "passed": required_ok},
            ],
        }

    def _evidence_view(self, run_id: str) -> list[dict[str, Any]]:
        view = []
        for row in self.store.evidence(run_id):
            view.append(
                {
                    "evidence_id": row["evidence_id"],
                    "operation": row["operation"],
                    "resource_alias": row["resource_alias"],
                    "diagnostic": json.loads(row["payload_json"]),
                }
            )
        return view

    def _remaining(self) -> float:
        return float(self.bundle.limits["max_duration_seconds"]) - float(self.clock.elapsed())

    def _outcome(self, run_id: str, reused: bool) -> RunOutcome:
        run = self.store.get_run(run_id)
        return RunOutcome(
            run_id=run_id,
            status=str(run["status"]),
            reused=reused,
            failure_code=run["failure_code"],
            report_md_path=run["report_md_path"],
            report_json_path=run["report_json_path"],
            event_id=str(run["event_id"]),
        )


def _history(store: Store, run_id: str) -> dict[str, Any]:
    run = store.get_run(run_id)
    return {
        "run_id": run["run_id"],
        "event_id": run["event_id"],
        "status": run["status"],
        "failure_code": run["failure_code"],
        "failure_detail": run["failure_detail"],
        "identity": run["identity"],
        "worker_mode": run["worker_mode"],
        "service_mode": run["service_mode"],
        "model_id": run["model_id"],
        "policy_version": run["policy_version"],
        "bundle_fingerprint": run["bundle_fingerprint"],
        "report_md_path": run["report_md_path"],
        "report_json_path": run["report_json_path"],
        "budget": json.loads(run["budget_json"]),
        "steps": [
            {
                "step_id": row["step_id"],
                "seq": row["seq"],
                "kind": row["kind"],
                "status": row["status"],
                "started_at": row["started_at"],
                "finished_at": row["finished_at"],
            }
            for row in store.steps(run_id)
        ],
        "decisions": [
            {
                "decision_id": row["decision_id"],
                "allowed": bool(row["allowed"]),
                "reason": row["reason"],
                "policy_version": row["policy_version"],
                "operation": row["operation"],
                "resource_alias": row["resource_alias"],
            }
            for row in store.decisions(run_id)
        ],
        "evidence_ids": [row["evidence_id"] for row in store.evidence(run_id)],
    }


def inspect_run(store: Store, run_id: str) -> dict[str, Any]:
    """Mark unfinished runs interrupted, then read history. Does not call providers."""
    if store.readonly:
        raise CompanyAIError("validation", "Inspect needs a writable state file so unfinished runs can be marked")
    store.reconcile_interrupted()
    history = _history(store, run_id)
    history["view"] = "inspect"
    return history


def replay_run(store: Store, run_id: str) -> dict[str, Any]:
    """Read recorded history only. Does not mark state and does not call providers or models."""
    if not store.readonly:
        raise CompanyAIError("validation", "Replay must open the state file read-only")
    history = _history(store, run_id)
    history["view"] = "replay"
    history["external_calls"] = 0
    history["note"] = (
        "Replay reads recorded history and performs no provider or model calls. "
        "It does not reproduce nondeterministic model output. "
        "A status of running means the writer had not finished; inspect marks that run interrupted."
    )
    return history
