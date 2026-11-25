use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::Result;
use crate::model::FinalReport;
use crate::runtime::{Check, Completion};
use crate::store::{DecisionRow, EvidenceRow, RunRecord};

pub const LIMITATION: &str = "Structural validation and resolvable evidence references do not prove that the diagnosis is true. Recorded facts are the diagnostic fields that were stored. Hypotheses are model output and are not facts. A content hash, an S3 ETag, or a fixture filename is not authentication. Configured identities are trusted-runtime labels, not cryptographic workload credentials.";

pub struct ReportInput<'a> {
    pub run: &'a RunRecord,
    pub status: &'a str,
    pub failure_code: Option<&'a str>,
    pub failure_detail: Option<&'a str>,
    pub decisions: &'a [DecisionRow],
    pub evidence: &'a [EvidenceRow],
    pub provenance_repository: &'a str,
    pub provenance_commit: &'a str,
    pub completion: &'a Completion,
    pub model_report: Option<&'a FinalReport>,
    pub service_transport: &'a str,
    pub worker_transport: &'a str,
}

pub fn build_report(input: &ReportInput<'_>) -> Result<Value> {
    let recorded = input
        .evidence
        .iter()
        .map(|row| {
            Ok(json!({
                "evidence_id": row.evidence_id,
                "run_id": row.run_id,
                "operation": row.operation,
                "resource_alias": row.resource_alias,
                "captured_at": row.captured_at,
                "content_sha256": row.content_sha256,
                "source": serde_json::from_str::<Value>(&row.source_json)?,
                "payload": serde_json::from_str::<Value>(&row.payload_json)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let decisions = input
        .decisions
        .iter()
        .map(|row| {
            Ok(json!({
                "decision_id": row.decision_id,
                "allowed": row.allowed,
                "reason": row.reason,
                "policy_version": row.policy_version,
                "operation": row.operation,
                "resource_alias": row.resource_alias,
                "target": row.target_json.as_deref().map(serde_json::from_str::<Value>).transpose()?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut notes = vec![LIMITATION.to_string()];
    if input.service_transport == "injected" {
        notes.push(
            "The service client was injected. This record does not prove a live AWS call."
                .to_string(),
        );
    }
    if input.worker_transport == "injected" {
        notes.push(
            "The model transport was injected. This record does not prove a live Ollama call."
                .to_string(),
        );
    }
    if input.run.service_mode == "fixture" {
        notes.push(
            "Service mode is fixture. This run does not prove live service connectivity."
                .to_string(),
        );
    } else if input.service_transport == "aws-sdk-s3" {
        notes.push("Service mode is live S3. A successful read shows that the configured object was fetched. It does not prove monitoring or diagnosis of a real incident. The object in the example is synthetic.".to_string());
    }
    if input.run.worker_mode == "scripted" {
        notes.push(
            "Worker mode is scripted. This run does not prove model participation.".to_string(),
        );
    } else if input.worker_transport == "reqwest" {
        notes.push("Worker mode used the Ollama HTTP API. Fixture service data does not prove a live service.".to_string());
    }
    if input.run.worker_mode == "real_model"
        && input.run.service_mode == "live"
        && input.status == "succeeded"
        && input.worker_transport == "reqwest"
        && input.service_transport == "aws-sdk-s3"
    {
        notes.push(
            "This run is a connected real-model and live-service execution for this workflow."
                .to_string(),
        );
    }
    Ok(json!({
        "company_ai_version": env!("CARGO_PKG_VERSION"),
        "run_id": input.run.run_id,
        "event_id": input.run.event_id,
        "status": input.status,
        "failure_code": input.failure_code,
        "failure_detail": input.failure_detail,
        "identity": input.run.identity,
        "worker_mode": input.run.worker_mode,
        "service_mode": input.run.service_mode,
        "model_id": input.run.model_id,
        "adapters": {
            "worker": input.run.worker_adapter,
            "service": input.run.service_adapter,
            "worker_transport": input.worker_transport,
            "service_transport": input.service_transport,
        },
        "governance": {
            "bundle_id": input.run.bundle_id,
            "bundle_version": input.run.bundle_version,
            "bundle_fingerprint": input.run.bundle_fingerprint,
            "policy_version": input.run.policy_version,
            "mission_id": input.run.mission_id,
            "workflow_id": input.run.workflow_id,
            "cursor_hub_repository": input.provenance_repository,
            "cursor_hub_commit": input.provenance_commit,
        },
        "completion": {
            "passed": input.completion.passed,
            "failure_code": input.completion.failure_code,
            "checks": checks_json(&input.completion.checks),
        },
        "authorization_decisions": decisions,
        "recorded_facts": recorded,
        "model_report": input.model_report.map(report_json),
        "limitations": notes,
    }))
}

fn checks_json(checks: &[Check]) -> Vec<Value> {
    checks
        .iter()
        .map(|check| json!({"name": check.name, "passed": check.passed}))
        .collect()
}

fn report_json(report: &FinalReport) -> Value {
    json!({
        "observations": report.observations,
        "hypotheses": report.hypotheses,
        "missing": report.missing,
        "evidence_ids": report.evidence_ids,
    })
}

pub fn write_reports(dir: &Path, run_id: &str, document: &Value) -> Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(dir)?;
    let json_path = dir.join(format!("{run_id}.json"));
    let md_path = dir.join(format!("{run_id}.md"));
    fs::write(&json_path, serde_json::to_string_pretty(document)? + "\n")?;
    fs::write(&md_path, render_markdown(document))?;
    Ok((json_path, md_path))
}

fn render_markdown(report: &Value) -> String {
    let governance = &report["governance"];
    let mut lines = vec![
        format!(
            "# Investigation {}",
            report["run_id"].as_str().unwrap_or("")
        ),
        String::new(),
        format!("- Status: `{}`", report["status"].as_str().unwrap_or("")),
        format!(
            "- Failure: `{}`",
            report["failure_code"].as_str().unwrap_or("none")
        ),
        format!("- Event: `{}`", report["event_id"].as_str().unwrap_or("")),
        format!(
            "- Identity: `{}`",
            report["identity"].as_str().unwrap_or("")
        ),
        format!(
            "- Worker mode: `{}`",
            report["worker_mode"].as_str().unwrap_or("")
        ),
        format!(
            "- Service mode: `{}`",
            report["service_mode"].as_str().unwrap_or("")
        ),
        format!(
            "- Model: `{}`",
            report["model_id"].as_str().unwrap_or("none")
        ),
        format!(
            "- Worker adapter: `{}` ({})",
            report["adapters"]["worker"].as_str().unwrap_or(""),
            report["adapters"]["worker_transport"]
                .as_str()
                .unwrap_or("")
        ),
        format!(
            "- Service adapter: `{}` ({})",
            report["adapters"]["service"].as_str().unwrap_or(""),
            report["adapters"]["service_transport"]
                .as_str()
                .unwrap_or("")
        ),
        format!(
            "- Policy: `{}`",
            governance["policy_version"].as_str().unwrap_or("")
        ),
        format!(
            "- Bundle: `{}` `{}` `{}`",
            governance["bundle_id"].as_str().unwrap_or(""),
            governance["bundle_version"].as_str().unwrap_or(""),
            governance["bundle_fingerprint"].as_str().unwrap_or("")
        ),
        format!(
            "- cursor-hub: `{}`",
            governance["cursor_hub_commit"].as_str().unwrap_or("")
        ),
        String::new(),
        "## Completion checks".to_string(),
        String::new(),
    ];
    if let Some(checks) = report["completion"]["checks"].as_array() {
        for check in checks {
            let name = check["name"].as_str().unwrap_or("");
            let passed = if check["passed"].as_bool().unwrap_or(false) {
                "passed"
            } else {
                "failed"
            };
            lines.push(format!("- {name}: {passed}"));
        }
    }
    lines.extend([
        String::new(),
        "## Authorization decisions".to_string(),
        String::new(),
    ]);
    match report["authorization_decisions"].as_array() {
        Some(items) if !items.is_empty() => {
            for decision in items {
                lines.push(format!(
                    "- `{}` allowed={} reason=`{}` policy=`{}` operation=`{}` resource=`{}`",
                    decision["decision_id"].as_str().unwrap_or(""),
                    decision["allowed"].as_bool().unwrap_or(false),
                    decision["reason"].as_str().unwrap_or(""),
                    decision["policy_version"].as_str().unwrap_or(""),
                    decision["operation"].as_str().unwrap_or(""),
                    decision["resource_alias"].as_str().unwrap_or("")
                ));
            }
        }
        _ => lines.push("None.".to_string()),
    }
    lines.extend([
        String::new(),
        "## Recorded facts".to_string(),
        String::new(),
    ]);
    match report["recorded_facts"].as_array() {
        Some(items) if !items.is_empty() => {
            for item in items {
                lines.push(format!(
                    "### {}",
                    item["evidence_id"].as_str().unwrap_or("")
                ));
                lines.push(String::new());
                lines.push(format!(
                    "- Operation: `{}`",
                    item["operation"].as_str().unwrap_or("")
                ));
                lines.push(format!(
                    "- Resource: `{}`",
                    item["resource_alias"].as_str().unwrap_or("")
                ));
                lines.push(format!(
                    "- Captured: `{}`",
                    item["captured_at"].as_str().unwrap_or("")
                ));
                lines.push(format!(
                    "- Content SHA-256: `{}`",
                    item["content_sha256"].as_str().unwrap_or("")
                ));
                lines.push(String::new());
                lines.push("```json".to_string());
                lines.push(
                    serde_json::to_string_pretty(&item["payload"])
                        .unwrap_or_else(|_| "{}".to_string()),
                );
                lines.push("```".to_string());
                lines.push(String::new());
            }
        }
        _ => lines.push("No evidence was stored.".to_string()),
    }
    lines.extend([
        String::new(),
        "## Model hypotheses".to_string(),
        String::new(),
    ]);
    if report["model_report"].is_null() {
        lines.push("The worker did not submit a report.".to_string());
    } else {
        push_list(&mut lines, &report["model_report"]["hypotheses"]);
        lines.extend([
            String::new(),
            "## Model observations".to_string(),
            String::new(),
        ]);
        push_list(&mut lines, &report["model_report"]["observations"]);
        lines.extend([
            String::new(),
            "## Model missing-information list".to_string(),
            String::new(),
        ]);
        push_list(&mut lines, &report["model_report"]["missing"]);
        lines.extend([
            String::new(),
            "## Evidence ids cited by the model".to_string(),
            String::new(),
        ]);
        if report["model_report"]["evidence_ids"]
            .as_array()
            .map(|items| items.is_empty())
            .unwrap_or(true)
        {
            lines.push("None.".to_string());
        } else {
            push_codes(&mut lines, &report["model_report"]["evidence_ids"]);
        }
    }
    lines.extend([String::new(), "## Limitations".to_string(), String::new()]);
    if let Some(notes) = report["limitations"].as_array() {
        for note in notes {
            lines.push(format!("- {}", note.as_str().unwrap_or("")));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

fn push_list(lines: &mut Vec<String>, value: &Value) {
    if let Some(items) = value.as_array() {
        for item in items {
            lines.push(format!("- {}", item.as_str().unwrap_or("")));
        }
    }
}

fn push_codes(lines: &mut Vec<String>, value: &Value) {
    if let Some(items) = value.as_array() {
        for item in items {
            lines.push(format!("- `{}`", item.as_str().unwrap_or("")));
        }
    }
}
