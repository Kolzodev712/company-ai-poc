use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::runtime::ManualClock;
use crate::workers::Worker;

#[derive(Clone)]
pub enum Script {
    Investigate,
    Forbidden,
    SpoofIdentity,
    BucketOverride,
    HostileFollowup,
    Malformed,
    UnknownTool,
    Loop,
    Premature,
    Cite(String),
    Expire(Arc<ManualClock>),
    DenyLoop,
}

pub struct ScriptedWorker {
    script: Script,
    prompts: Vec<Value>,
    success: Option<Value>,
}

impl ScriptedWorker {
    pub fn new(script: Script) -> Self {
        Self {
            script,
            prompts: Vec::new(),
            success: None,
        }
    }

    pub fn seen(&self) -> &[Value] {
        &self.prompts
    }

    pub fn parse(name: &str) -> Result<Script> {
        Ok(match name {
            "investigate" => Script::Investigate,
            "forbidden" => Script::Forbidden,
            "spoof_identity" => Script::SpoofIdentity,
            "bucket_override" => Script::BucketOverride,
            "hostile_followup" => Script::HostileFollowup,
            "malformed" => Script::Malformed,
            "unknown_tool" => Script::UnknownTool,
            "loop" => Script::Loop,
            "premature" => Script::Premature,
            "deny_loop" => Script::DenyLoop,
            _ => return Err(Error::app("validation", format!("Unknown script {name}"))),
        })
    }
}

impl Worker for ScriptedWorker {
    fn adapter_id(&self) -> &'static str {
        "scripted"
    }

    fn transport_name(&self) -> &'static str {
        "script"
    }

    fn prompts(&self) -> &[Value] {
        &self.prompts
    }

    fn set_request_timeout(&mut self, _timeout: Duration) {}

    fn turn(&mut self, context: &Value, tool_result: Option<&Value>) -> Result<Value> {
        self.prompts.push(context.clone());
        let alias = context["event"]["resource_alias"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let value = match &self.script {
            Script::Investigate => match tool_result {
                None => proposal(&alias),
                Some(result) if result.get("status").and_then(Value::as_str) == Some("ok") => {
                    report_from(result)
                }
                Some(_) => empty_report(),
            },
            Script::Forbidden => {
                if tool_result.is_none() {
                    proposal("demo-service-b")
                } else {
                    empty_report()
                }
            }
            Script::SpoofIdentity => json!({
                "kind": "tool_proposal",
                "operation": "diagnostics.read",
                "arguments": {"resource_alias": "demo-service-b"},
                "identity": "auditor"
            }),
            Script::BucketOverride => json!({
                "kind": "tool_proposal",
                "operation": "diagnostics.read",
                "arguments": {"resource_alias": alias, "bucket": "other-bucket", "key": "other-key"}
            }),
            Script::HostileFollowup => {
                if tool_result.is_none() {
                    proposal(&alias)
                } else if tool_result
                    .and_then(|item| item.get("status"))
                    .and_then(Value::as_str)
                    == Some("ok")
                    && self.success.is_none()
                {
                    self.success = tool_result.cloned();
                    proposal("demo-service-b")
                } else if let Some(success) = &self.success {
                    report_from(success)
                } else {
                    empty_report()
                }
            }
            Script::Malformed => json!({"kind": "dance"}),
            Script::UnknownTool => {
                json!({"kind": "tool_proposal", "operation": "shell.exec", "arguments": {}})
            }
            Script::Loop | Script::DenyLoop => {
                if matches!(self.script, Script::DenyLoop) {
                    proposal("demo-service-b")
                } else {
                    proposal(&alias)
                }
            }
            Script::Premature => empty_report(),
            Script::Cite(evidence_id) => {
                let mut report = empty_report();
                report["observations"] =
                    json!(["Cited an evidence identifier supplied outside this run."]);
                report["evidence_ids"] = json!([evidence_id]);
                report
            }
            Script::Expire(clock) => {
                clock.set(Duration::from_secs(10_000));
                proposal(&alias)
            }
        };
        Ok(value)
    }
}

fn proposal(alias: &str) -> Value {
    json!({
        "kind": "tool_proposal",
        "operation": "diagnostics.read",
        "arguments": {"resource_alias": alias}
    })
}

fn empty_report() -> Value {
    json!({
        "kind": "final_report",
        "observations": ["No diagnostic document was read."],
        "hypotheses": ["No hypothesis is supported without the required evidence."],
        "missing": ["The required diagnostic snapshot is missing."],
        "evidence_ids": []
    })
}

fn report_from(result: &Value) -> Value {
    let diagnostic = &result["diagnostic"];
    let mut observations = vec![
        format!("service={}", diagnostic["service"].as_str().unwrap_or("")),
        format!("status={}", diagnostic["status"].as_str().unwrap_or("")),
        format!("recent_error_count={}", diagnostic["recent_error_count"]),
        format!(
            "recent_deployment_id={}",
            diagnostic["recent_deployment_id"].as_str().unwrap_or("")
        ),
        format!(
            "timestamp={}",
            diagnostic["timestamp"].as_str().unwrap_or("")
        ),
    ];
    if !diagnostic
        .get("log_excerpt")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        observations
            .push("A log excerpt was present and was treated as untrusted data.".to_string());
    }
    json!({
        "kind": "final_report",
        "observations": observations,
        "hypotheses": ["The error count may be associated with the recorded deployment. The snapshot does not establish that link."],
        "missing": ["No host metrics, change ticket, or previous baseline were in the snapshot."],
        "evidence_ids": [result["evidence_id"].as_str().unwrap_or("")]
    })
}
