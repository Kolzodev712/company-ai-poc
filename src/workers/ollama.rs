use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::runtime_io::block_on;
use crate::util::{canonical, reject_duplicate_keys};
use crate::workers::Worker;

const MAX_MODEL_RESPONSE_BYTES: usize = 65_536;

pub trait ChatTransport: Send + Sync {
    fn chat(&self, url: &str, body: &Value, timeout: Duration) -> Result<Value>;
}

pub struct ReqwestChat;

impl ChatTransport for ReqwestChat {
    fn chat(&self, url: &str, body: &Value, timeout: Duration) -> Result<Value> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|error| {
                Error::app(
                    "model_unavailable",
                    format!("Ollama client setup failed: {error}"),
                )
            })?;
        let url = url.to_string();
        let body = body.clone();
        block_on(async move {
            let response = client.post(url).json(&body).send().await.map_err(|error| {
                Error::app(
                    "model_unavailable",
                    format!("Ollama endpoint unavailable: {}", error),
                )
            })?;
            let status = response.status();
            let mut bytes = Vec::new();
            let mut response = response;
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                Error::app(
                    "model_unavailable",
                    format!("Ollama endpoint unavailable: {error}"),
                )
            })? {
                bytes.extend_from_slice(&chunk);
                if bytes.len() > MAX_MODEL_RESPONSE_BYTES {
                    return Err(Error::app(
                        "malformed_model_output",
                        "Model response exceeds the size limit",
                    ));
                }
            }
            if !status.is_success() {
                let detail = String::from_utf8_lossy(&bytes);
                let code = if status.as_u16() == 400 || status.as_u16() == 404 {
                    "model_rejected"
                } else {
                    "model_unavailable"
                };
                let mut message = crate::redact::redact(&format!("Ollama HTTP {status}: {detail}"));
                message.truncate(300);
                return Err(Error::app(code, message));
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                Error::app("malformed_model_output", "Model response was not UTF-8")
            })?;
            reject_duplicate_keys(text)?;
            serde_json::from_str(text)
                .map_err(|_| Error::app("malformed_model_output", "Model response was not JSON"))
        })?
    }
}

pub struct OllamaWorker<T: ChatTransport> {
    endpoint: String,
    model: String,
    num_predict: u32,
    timeout: Duration,
    transport: T,
    transport_label: &'static str,
    messages: Vec<Value>,
    pending_tool: Option<String>,
    prompts: Vec<Value>,
}

impl<T: ChatTransport> OllamaWorker<T> {
    pub fn new(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        num_predict: u32,
        transport: T,
        injected: bool,
    ) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            model: model.into(),
            num_predict,
            timeout: Duration::from_secs(5),
            transport,
            transport_label: if injected { "injected" } else { "reqwest" },
            messages: Vec::new(),
            pending_tool: None,
            prompts: Vec::new(),
        }
    }
}

impl<T: ChatTransport> Worker for OllamaWorker<T> {
    fn adapter_id(&self) -> &'static str {
        "ollama"
    }

    fn transport_name(&self) -> &'static str {
        self.transport_label
    }

    fn prompts(&self) -> &[Value] {
        &self.prompts
    }

    fn set_request_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    fn turn(&mut self, context: &Value, tool_result: Option<&Value>) -> Result<Value> {
        if self.messages.is_empty() {
            self.messages.push(json!({
                "role": "system",
                "content": context["skill_instructions"].as_str().unwrap_or("")
            }));
            self.messages
                .push(json!({"role": "user", "content": task_text(context)?}));
        } else if let (Some(result), Some(name)) = (tool_result, self.pending_tool.clone()) {
            let mut content = canonical(result)?;
            content.truncate(8_000);
            self.messages
                .push(json!({"role": "tool", "tool_name": name, "content": content}));
            self.pending_tool = None;
        }
        let payload = json!({
            "model": self.model,
            "stream": false,
            "think": false,
            "messages": self.messages,
            "tools": tools(context),
            "options": {"num_predict": self.num_predict}
        });
        self.prompts.push(payload.clone());
        let raw = self.transport.chat(
            &format!("{}/api/chat", self.endpoint),
            &payload,
            self.timeout,
        )?;
        let message = raw.get("message").cloned().unwrap_or(Value::Null);
        let message = message.as_object().ok_or_else(|| {
            Error::app(
                "malformed_model_output",
                "Model response did not contain a message",
            )
        })?;
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut assistant = json!({"role": "assistant", "content": content});
        if !calls.is_empty() {
            if calls.len() != 1 {
                return Err(Error::app(
                    "malformed_model_output",
                    "Expected exactly one tool call",
                ));
            }
            let function = calls[0].get("function").cloned().unwrap_or(Value::Null);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                return Err(Error::app(
                    "malformed_model_output",
                    "Tool call is missing a function name",
                ));
            }
            let arguments = normalize_arguments(function.get("arguments"))?;
            assistant["tool_calls"] = json!([{
                "type": "function",
                "function": {"name": name, "arguments": arguments}
            }]);
            self.messages.push(assistant);
            self.pending_tool = Some(name.clone());
            if name == "submit_report" {
                let mut report = arguments.clone();
                if let Some(object) = report.as_object_mut() {
                    object.insert("kind".to_string(), json!("final_report"));
                }
                return Ok(report);
            }
            return Ok(json!({"kind": "tool_proposal", "operation": name, "arguments": arguments}));
        }
        self.messages.push(assistant);
        let trimmed = content.trim();
        if trimmed.starts_with('{') {
            reject_duplicate_keys(trimmed)?;
            let parsed: Value = serde_json::from_str(trimmed)
                .map_err(|_| Error::app("malformed_model_output", "Model report was not JSON"))?;
            if parsed.get("kind").and_then(Value::as_str) == Some("final_report") {
                return Ok(parsed);
            }
        }
        Err(Error::app(
            "malformed_model_output",
            "Model returned neither a tool call nor a final report",
        ))
    }
}

fn task_text(context: &Value) -> Result<String> {
    canonical(&json!({
        "mission_objective": context["mission_objective"],
        "workflow_id": context["workflow_id"],
        "event": context["event"],
    }))
}

fn tools(context: &Value) -> Value {
    let tools = context["tools"].as_array();
    let read = tools.and_then(|items| items.iter().find(|item| item["name"] == "diagnostics.read"));
    let submit = tools.and_then(|items| items.iter().find(|item| item["name"] == "submit_report"));
    json!([
        {
            "type": "function",
            "function": {
                "name": "diagnostics.read",
                "description": read.and_then(|item| item["description"].as_str()).unwrap_or(""),
                "parameters": read.and_then(|item| item.get("arguments")).cloned().unwrap_or(json!({}))
            }
        },
        {
            "type": "function",
            "function": {
                "name": "submit_report",
                "description": submit.and_then(|item| item["description"].as_str()).unwrap_or(""),
                "parameters": submit.and_then(|item| item.get("arguments")).cloned().unwrap_or(json!({}))
            }
        }
    ])
}

fn normalize_arguments(value: Option<&Value>) -> Result<Value> {
    match value {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::String(text)) => {
            reject_duplicate_keys(text)?;
            serde_json::from_str(text).map_err(|_| {
                Error::app("malformed_model_output", "Tool arguments must be an object")
            })
        }
        Some(Value::Object(_)) => Ok(value.cloned().unwrap_or(json!({}))),
        _ => Err(Error::app(
            "malformed_model_output",
            "Tool arguments must be an object",
        )),
    }
}
