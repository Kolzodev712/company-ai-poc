use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::util::{is_alias, is_event_id, is_stamp};

pub const READ_ARGUMENTS: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["resource_alias"],
  "properties": {
    "resource_alias": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[a-z][a-z0-9-]{0,63}$"
    }
  }
}"#;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub schema_version: u32,
    pub bundle_id: String,
    pub bundle_version: String,
    pub mission: Mission,
    pub workflow: Workflow,
    pub skill: SkillRef,
    pub operations: Operations,
    pub policy: Policy,
    pub resources: BTreeMap<String, Resource>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mission {
    pub mission_id: String,
    pub objective: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub workflow_id: String,
    pub skill_id: String,
    pub required_operation: String,
    pub report_required: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillRef {
    pub skill_id: String,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operations {
    #[serde(rename = "diagnostics.read")]
    pub diagnostics_read: OperationDef,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationDef {
    pub description: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub policy_version: String,
    pub identities: BTreeMap<String, IdentityPolicy>,
    pub limits: Limits,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityPolicy {
    pub allow: Vec<AllowRule>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AllowRule {
    pub operation: String,
    pub resources: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub fixture: FixtureMapping,
    pub s3: S3Mapping,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FixtureMapping {
    pub path: String,
    pub behavior: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct S3Mapping {
    pub bucket: String,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub repository: String,
    pub commit: String,
    pub sources: Vec<ProvenanceSource>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceSource {
    pub path: String,
    pub sha256: String,
    pub role: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_result_bytes: u64,
    pub max_model_turns: u32,
    pub max_tool_proposals: u32,
    pub max_provider_attempts: u32,
    pub max_duration_seconds: u64,
    pub request_timeout_seconds: u64,
    pub max_input_bytes: u64,
    pub max_transient_retries: u32,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub schema_version: u32,
    pub event_id: String,
    pub kind: String,
    pub resource_alias: String,
    pub observed_at: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub service: String,
    pub timestamp: String,
    pub status: String,
    pub recent_error_count: u64,
    pub recent_deployment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalReport {
    pub observations: Vec<String>,
    pub hypotheses: Vec<String>,
    pub missing: Vec<String>,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ValidatedTurn {
    Proposal(ValidatedProposal),
    Report(FinalReport),
}

#[derive(Debug, Clone)]
pub struct ValidatedProposal {
    pub operation: String,
    pub resource_alias: String,
}

pub fn validate_event(event: &Event) -> Result<()> {
    if event.schema_version != 1 {
        return Err(Error::app("validation", "event schema_version must be 1"));
    }
    if event.kind != "diagnostic.requested" {
        return Err(Error::app(
            "validation",
            "event kind must be diagnostic.requested",
        ));
    }
    if !is_event_id(&event.event_id) {
        return Err(Error::app(
            "validation",
            "event_id is not a valid identifier",
        ));
    }
    if !is_alias(&event.resource_alias) {
        return Err(Error::app(
            "validation",
            "resource_alias is not a configured alias shape",
        ));
    }
    if !is_stamp(&event.observed_at) {
        return Err(Error::app(
            "validation",
            "observed_at must be a UTC timestamp",
        ));
    }
    if event.summary.is_empty() || event.summary.len() > 500 {
        return Err(Error::app(
            "validation",
            "event summary length is out of bounds",
        ));
    }
    Ok(())
}

pub fn validate_diagnostic(document: &Diagnostic) -> Result<()> {
    if !is_alias(&document.service) {
        return Err(Error::app("malformed", "diagnostic service is invalid"));
    }
    if !is_stamp(&document.timestamp) {
        return Err(Error::app("malformed", "diagnostic timestamp is invalid"));
    }
    if !is_status(&document.status) {
        return Err(Error::app("malformed", "diagnostic status is invalid"));
    }
    if document.recent_error_count > 1_000_000 {
        return Err(Error::app(
            "malformed",
            "diagnostic error count is out of bounds",
        ));
    }
    if !is_deployment(&document.recent_deployment_id) {
        return Err(Error::app(
            "malformed",
            "diagnostic deployment id is invalid",
        ));
    }
    if let Some(excerpt) = &document.log_excerpt {
        if excerpt.is_empty() || excerpt.len() > 2_000 {
            return Err(Error::app(
                "malformed",
                "diagnostic log excerpt is out of bounds",
            ));
        }
    }
    Ok(())
}

pub fn parse_worker_output(value: &Value) -> Result<ValidatedTurn> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::app("malformed_model_output", "Worker output was not an object"))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::app("malformed_model_output", "Worker output is missing kind"))?;
    match kind {
        "tool_proposal" => {
            reject_extra(object, &["kind", "operation", "arguments"])?;
            let operation = required_string(object, "operation")?;
            let arguments = object.get("arguments").ok_or_else(|| {
                Error::app(
                    "malformed_model_output",
                    "Tool proposal is missing arguments",
                )
            })?;
            if operation != "diagnostics.read" {
                return Err(Error::app(
                    "unknown_tool",
                    format!("Unknown tool {operation}"),
                ));
            }
            let alias = parse_read_arguments(arguments)?;
            Ok(ValidatedTurn::Proposal(ValidatedProposal {
                operation,
                resource_alias: alias,
            }))
        }
        "final_report" => {
            reject_extra(
                object,
                &[
                    "kind",
                    "observations",
                    "hypotheses",
                    "missing",
                    "evidence_ids",
                ],
            )?;
            let mut body = value.clone();
            if let Some(fields) = body.as_object_mut() {
                fields.remove("kind");
            }
            let report: FinalReport = serde_json::from_value(body)?;
            validate_report(&report)?;
            Ok(ValidatedTurn::Report(report))
        }
        _ => Err(Error::app(
            "malformed_model_output",
            "Worker output was not a tool proposal or final report",
        )),
    }
}

fn parse_read_arguments(value: &Value) -> Result<String> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::app("validation", "arguments must be an object"))?;
    reject_extra(object, &["resource_alias"])?;
    let alias = required_string(object, "resource_alias")?;
    if !is_alias(&alias) {
        return Err(Error::app(
            "validation",
            "resource_alias is not a configured alias shape",
        ));
    }
    Ok(alias)
}

fn validate_report(report: &FinalReport) -> Result<()> {
    for (name, items) in [
        ("observations", &report.observations),
        ("hypotheses", &report.hypotheses),
        ("missing", &report.missing),
    ] {
        if items.is_empty() || items.len() > 8 {
            return Err(Error::app(
                "validation",
                format!("{name} must contain 1 to 8 items"),
            ));
        }
        if items
            .iter()
            .any(|item| item.is_empty() || item.len() > 1_000)
        {
            return Err(Error::app(
                "validation",
                format!("{name} item length is out of bounds"),
            ));
        }
    }
    if report.evidence_ids.len() > 8 {
        return Err(Error::app(
            "validation",
            "evidence_ids length is out of bounds",
        ));
    }
    if report.evidence_ids.iter().any(|id| !is_event_id(id)) {
        return Err(Error::app(
            "validation",
            "evidence id is not a valid identifier",
        ));
    }
    Ok(())
}

fn reject_extra(object: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::app(
            "rejected_untrusted_field",
            format!("Rejected untrusted field {key}"),
        ));
    }
    Ok(())
}

fn required_string(object: &serde_json::Map<String, Value>, key: &str) -> Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::app("malformed_model_output", format!("Missing {key}")))
}

fn is_status(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn is_deployment(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && value.len() <= 65
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

pub fn validate_limits(limits: &Limits) -> Result<()> {
    let ok = (256..=1_000_000).contains(&limits.max_result_bytes)
        && (1..=16).contains(&limits.max_model_turns)
        && (1..=16).contains(&limits.max_tool_proposals)
        && (1..=16).contains(&limits.max_provider_attempts)
        && (1..=600).contains(&limits.max_duration_seconds)
        && (1..=120).contains(&limits.request_timeout_seconds)
        && (1_024..=200_000).contains(&limits.max_input_bytes)
        && limits.max_transient_retries <= 2
        && (16..=2_048).contains(&limits.max_output_tokens)
        && limits.request_timeout_seconds <= limits.max_duration_seconds;
    if ok {
        Ok(())
    } else {
        Err(Error::app(
            "validation",
            "policy limits are outside the closed bounds",
        ))
    }
}
