"""Deterministic worker used by the fixture demonstration and tests."""

from __future__ import annotations

from typing import Any, Mapping


def proposal(resource_alias: str) -> dict[str, Any]:
    return {
        "kind": "tool_proposal",
        "operation": "diagnostics.read",
        "arguments": {"resource_alias": resource_alias},
    }


def empty_report() -> dict[str, Any]:
    return {
        "kind": "final_report",
        "observations": ["No diagnostic document was read."],
        "hypotheses": ["No hypothesis is supported without the required evidence."],
        "missing": ["The required diagnostic snapshot is missing."],
        "evidence_ids": [],
    }


def report_from_result(result: Mapping[str, Any]) -> dict[str, Any]:
    diagnostic = result["diagnostic"]
    observations = [
        f"service={diagnostic['service']}",
        f"status={diagnostic['status']}",
        f"recent_error_count={diagnostic['recent_error_count']}",
        f"recent_deployment_id={diagnostic['recent_deployment_id']}",
        f"timestamp={diagnostic['timestamp']}",
    ]
    if "log_excerpt" in diagnostic:
        observations.append("A log excerpt was present and was treated as untrusted data.")
    return {
        "kind": "final_report",
        "observations": observations,
        "hypotheses": [
            "The error count may be associated with the recorded deployment. The snapshot does not establish that link."
        ],
        "missing": ["No host metrics, change ticket, or previous baseline were in the snapshot."],
        "evidence_ids": [result["evidence_id"]],
    }


class ScriptedWorker:
    adapter_id = "scripted"
    transport_name = "script"

    def __init__(self, script: str = "investigate", evidence_id: str = "", clock: Any | None = None):
        self.script = script
        self.evidence_id = evidence_id
        self.clock = clock
        self.prompts: list[Mapping[str, Any]] = []
        self._success: Mapping[str, Any] | None = None

    def turn(self, context: Mapping[str, Any], tool_result: Mapping[str, Any] | None) -> Any:
        self.prompts.append(context)
        alias = str(context["event"]["resource_alias"])
        if self.script == "investigate":
            if tool_result is None:
                return proposal(alias)
            if tool_result.get("status") == "ok":
                return report_from_result(tool_result)
            return empty_report()
        if self.script == "forbidden":
            if tool_result is None:
                return proposal("demo-service-b")
            return empty_report()
        if self.script == "spoof_identity":
            return {
                "kind": "tool_proposal",
                "operation": "diagnostics.read",
                "arguments": {"resource_alias": "demo-service-b"},
                "identity": "auditor",
            }
        if self.script == "bucket_override":
            return {
                "kind": "tool_proposal",
                "operation": "diagnostics.read",
                "arguments": {"resource_alias": alias, "bucket": "other-bucket", "key": "other-key"},
            }
        if self.script == "hostile_followup":
            if tool_result is None:
                return proposal(alias)
            if tool_result.get("status") == "ok" and self._success is None:
                self._success = tool_result
                return proposal("demo-service-b")
            if self._success is not None:
                return report_from_result(self._success)
            return empty_report()
        if self.script == "malformed":
            return {"kind": "dance"}
        if self.script == "unknown_tool":
            return {"kind": "tool_proposal", "operation": "shell.exec", "arguments": {}}
        if self.script == "loop":
            return proposal(alias)
        if self.script == "premature":
            return empty_report()
        if self.script == "cite":
            report = empty_report()
            report["observations"] = ["Cited an evidence identifier supplied outside this run."]
            report["evidence_ids"] = [self.evidence_id]
            return report
        if self.script == "deny_loop":
            return proposal("demo-service-b")
        if self.script == "expire":
            if self.clock is not None:
                self.clock.elapsed_value = 10_000
            return proposal(alias)
        raise ValueError(f"Unknown script {self.script}")
