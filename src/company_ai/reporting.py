"""Local JSON and Markdown investigation reports."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from company_ai import __version__
from company_ai.contracts import canonical

LIMITATION = (
    "Structural validation and resolvable evidence references do not prove that the diagnosis is true. "
    "Recorded facts are the diagnostic fields that were stored. Hypotheses are model output and are not facts. "
    "A content hash, an S3 ETag, or a fixture filename is not authentication. "
    "Configured identities are trusted-runtime labels, not cryptographic workload credentials."
)


def _loads(value: str | None) -> Any:
    if not value:
        return None
    return json.loads(value)


def build_report(
    *,
    run: dict[str, Any],
    decisions: list[dict[str, Any]],
    evidence_rows: list[dict[str, Any]],
    provenance: dict[str, Any],
    completion: dict[str, Any],
    model_report: dict[str, Any] | None,
    service_transport: str,
    worker_transport: str,
) -> dict[str, Any]:
    recorded = []
    for row in evidence_rows:
        recorded.append(
            {
                "evidence_id": row["evidence_id"],
                "run_id": row["run_id"],
                "operation": row["operation"],
                "resource_alias": row["resource_alias"],
                "captured_at": row["captured_at"],
                "content_sha256": row["content_sha256"],
                "source": _loads(row["source_json"]),
                "payload": _loads(row["payload_json"]),
            }
        )
    authorization = []
    for row in decisions:
        authorization.append(
            {
                "decision_id": row["decision_id"],
                "allowed": bool(row["allowed"]),
                "reason": row["reason"],
                "policy_version": row["policy_version"],
                "operation": row["operation"],
                "resource_alias": row["resource_alias"],
                "target": _loads(row["target_json"]),
            }
        )
    notes = [LIMITATION]
    if service_transport == "injected":
        notes.append("The service client was injected. This record does not prove a live AWS call.")
    if worker_transport == "injected":
        notes.append("The model transport was injected. This record does not prove a live Ollama call.")
    if run["service_mode"] == "fixture":
        notes.append("Service mode is fixture. This run does not prove live service connectivity.")
    elif service_transport == "boto3":
        notes.append(
            "Service mode is live S3. A successful read shows that the configured object was fetched. "
            "It does not prove monitoring or diagnosis of a real incident. The object in the example is synthetic."
        )
    if run["worker_mode"] == "scripted":
        notes.append("Worker mode is scripted. This run does not prove model participation.")
    elif worker_transport == "urllib":
        notes.append("Worker mode used the Ollama HTTP API. Fixture service data does not prove a live service.")
    if run["worker_mode"] == "real_model" and run["service_mode"] == "live" and run["status"] == "succeeded":
        if worker_transport == "urllib" and service_transport == "boto3":
            notes.append("This run is a connected real-model and live-service execution for this workflow.")
    return {
        "company_ai_version": __version__,
        "run_id": run["run_id"],
        "event_id": run["event_id"],
        "status": run["status"],
        "failure_code": run["failure_code"],
        "failure_detail": run["failure_detail"],
        "identity": run["identity"],
        "worker_mode": run["worker_mode"],
        "service_mode": run["service_mode"],
        "model_id": run["model_id"],
        "adapters": {
            "worker": run["worker_adapter"],
            "service": run["service_adapter"],
            "worker_transport": worker_transport,
            "service_transport": service_transport,
        },
        "governance": {
            "bundle_id": run["bundle_id"],
            "bundle_version": run["bundle_version"],
            "bundle_fingerprint": run["bundle_fingerprint"],
            "policy_version": run["policy_version"],
            "mission_id": run["mission_id"],
            "workflow_id": run["workflow_id"],
            "cursor_hub_repository": provenance["repository"],
            "cursor_hub_commit": provenance["commit"],
        },
        "completion": completion,
        "authorization_decisions": authorization,
        "recorded_facts": recorded,
        "model_report": None
        if model_report is None
        else {
            "observations": model_report["observations"],
            "hypotheses": model_report["hypotheses"],
            "missing": model_report["missing"],
            "evidence_ids": model_report["evidence_ids"],
        },
        "limitations": notes,
    }


def render_markdown(report: dict[str, Any]) -> str:
    governance = report["governance"]
    lines = [
        f"# Investigation {report['run_id']}",
        "",
        f"- Status: `{report['status']}`",
        f"- Failure: `{report['failure_code'] or 'none'}`",
        f"- Event: `{report['event_id']}`",
        f"- Identity: `{report['identity']}`",
        f"- Worker mode: `{report['worker_mode']}`",
        f"- Service mode: `{report['service_mode']}`",
        f"- Model: `{report['model_id'] or 'none'}`",
        f"- Worker adapter: `{report['adapters']['worker']}` ({report['adapters']['worker_transport']})",
        f"- Service adapter: `{report['adapters']['service']}` ({report['adapters']['service_transport']})",
        f"- Policy: `{governance['policy_version']}`",
        f"- Bundle: `{governance['bundle_id']}` `{governance['bundle_version']}` `{governance['bundle_fingerprint']}`",
        f"- cursor-hub: `{governance['cursor_hub_commit']}`",
        "",
        "## Completion checks",
        "",
    ]
    for check in report["completion"]["checks"]:
        lines.append(f"- {check['name']}: {'passed' if check['passed'] else 'failed'}")
    lines.extend(["", "## Authorization decisions", ""])
    if not report["authorization_decisions"]:
        lines.append("None.")
    for decision in report["authorization_decisions"]:
        lines.append(
            f"- `{decision['decision_id']}` allowed={str(decision['allowed']).lower()} "
            f"reason=`{decision['reason']}` policy=`{decision['policy_version']}` "
            f"operation=`{decision['operation']}` resource=`{decision['resource_alias']}`"
        )
    lines.extend(["", "## Recorded facts", ""])
    if not report["recorded_facts"]:
        lines.append("No evidence was stored.")
    for item in report["recorded_facts"]:
        lines.append(
            f"### {item['evidence_id']}",
        )
        lines.append("")
        lines.append(f"- Operation: `{item['operation']}`")
        lines.append(f"- Resource: `{item['resource_alias']}`")
        lines.append(f"- Captured: `{item['captured_at']}`")
        lines.append(f"- Content SHA-256: `{item['content_sha256']}`")
        lines.append("")
        lines.append("```json")
        lines.append(json.dumps(item["payload"], indent=2, sort_keys=True))
        lines.append("```")
        lines.append("")
    model = report["model_report"]
    lines.extend(["## Model hypotheses", ""])
    if model is None:
        lines.append("The worker did not submit a report.")
    else:
        for item in model["hypotheses"]:
            lines.append(f"- {item}")
        lines.extend(["", "## Model observations", ""])
        for item in model["observations"]:
            lines.append(f"- {item}")
        lines.extend(["", "## Model missing-information list", ""])
        for item in model["missing"]:
            lines.append(f"- {item}")
        lines.extend(["", "## Evidence ids cited by the model", ""])
        if model["evidence_ids"]:
            for item in model["evidence_ids"]:
                lines.append(f"- `{item}`")
        else:
            lines.append("None.")
    lines.extend(["", "## Limitations", ""])
    for note in report["limitations"]:
        lines.append(f"- {note}")
    lines.append("")
    return "\n".join(lines)


def write_report(reports_dir: Path, run_id: str, document: dict[str, Any]) -> tuple[Path, Path]:
    reports_dir.mkdir(parents=True, exist_ok=True)
    json_path = reports_dir / f"{run_id}.json"
    md_path = reports_dir / f"{run_id}.md"
    json_path.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    md_path.write_text(render_markdown(document), encoding="utf-8")
    # Touch canonical form so a stable fingerprint of the report can be recomputed by operators.
    canonical(document)
    return json_path, md_path
