mod ollama;
mod scripted;

pub use ollama::{ChatTransport, OllamaWorker, ReqwestChat};
pub use scripted::{Script, ScriptedWorker};

use std::time::Duration;

use serde_json::Value;

use crate::error::Result;

pub trait Worker {
    fn adapter_id(&self) -> &'static str;
    fn transport_name(&self) -> &'static str;
    fn prompts(&self) -> &[Value];
    fn set_request_timeout(&mut self, timeout: Duration);
    fn turn(&mut self, context: &Value, tool_result: Option<&Value>) -> Result<Value>;
}
