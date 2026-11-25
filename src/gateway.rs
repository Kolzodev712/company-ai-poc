use std::time::Duration;

use serde_json::{json, Value};
use tracing::info;

use crate::broker::{AuthorizedRead, Decision, ProviderTarget, SecurityBroker, ServiceProvider};
use crate::error::Result;
use crate::governance::LoadedBundle;
use crate::model::{parse_worker_output, validate_diagnostic, Diagnostic, ValidatedTurn};
use crate::redact::redact;
use crate::store::{Budget, Store};

const TRANSIENT: &[&str] = &["timeout", "unavailable"];
const FORBIDDEN_SOURCE_KEYS: &[&str] = &[
    "access_key",
    "secret",
    "secret_access_key",
    "session_token",
    "password",
    "credential",
    "authorization",
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "token",
];

pub struct AdapterResult {
    pub ok: bool,
    pub error_code: Option<String>,
    pub message: String,
    pub payload: Option<Diagnostic>,
    pub content_sha256: Option<String>,
    pub byte_length: usize,
    pub source: Value,
}

impl AdapterResult {
    pub fn failure(code: &str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error_code: Some(code.to_string()),
            message: message.into(),
            payload: None,
            content_sha256: None,
            byte_length: 0,
            source: json!({}),
        }
    }
}

pub trait DiagnosticsSource: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn transport(&self) -> &'static str;
    fn read(&self, authorized: &AuthorizedRead, timeout: Duration) -> AdapterResult;
}

#[derive(Debug, Clone)]
pub struct ToolView {
    pub status: &'static str,
    pub error_code: Option<String>,
    pub message: String,
    pub operation: Option<String>,
    pub resource_alias: Option<String>,
    pub evidence_id: Option<String>,
    pub diagnostic: Option<Value>,
}

pub struct ToolGateway<'a> {
    bundle: &'a LoadedBundle,
    adapter: &'a dyn DiagnosticsSource,
    store: &'a mut Store,
    service: ServiceProvider,
}

impl<'a> ToolGateway<'a> {
    pub fn new(
        bundle: &'a LoadedBundle,
        adapter: &'a dyn DiagnosticsSource,
        store: &'a mut Store,
        service: ServiceProvider,
    ) -> Self {
        Self {
            bundle,
            adapter,
            store,
            service,
        }
    }

    pub fn invoke(
        &mut self,
        run_id: &str,
        identity: &str,
        proposal: &Value,
        budget: &mut Budget,
        remaining: Duration,
    ) -> Result<ToolView> {
        let parsed = match parse_worker_output(proposal) {
            Ok(ValidatedTurn::Proposal(proposal)) => proposal,
            Ok(ValidatedTurn::Report(_)) => {
                return self.invalid(
                    run_id,
                    "malformed_model_output",
                    "Gateway received a report",
                );
            }
            Err(error) => return self.invalid(run_id, error.code(), &error.message()),
        };
        let decision = SecurityBroker::new(self.bundle).evaluate(
            identity,
            &parsed.operation,
            &parsed.resource_alias,
            self.service,
        )?;
        self.store.save_decision(run_id, &decision)?;
        info!(
            run_id,
            allowed = decision.allowed,
            reason = %decision.reason,
            policy = %decision.policy_version,
            operation = %decision.operation,
            resource = %decision.resource_alias,
            "authorization"
        );
        if !decision.allowed {
            return Ok(ToolView {
                status: "denied",
                error_code: Some(decision.reason.clone()),
                message: decision.reason,
                operation: Some(parsed.operation),
                resource_alias: Some(parsed.resource_alias),
                evidence_id: None,
                diagnostic: None,
            });
        }
        let Some(authorized) = decision.authorized.as_ref() else {
            return self.invalid(run_id, "provider_error", "Permit was missing a target");
        };
        self.execute(run_id, authorized, budget, remaining, &decision)
    }

    fn invalid(&mut self, run_id: &str, code: &str, message: &str) -> Result<ToolView> {
        let decision = Decision {
            decision_id: crate::util::new_id("dec_"),
            allowed: false,
            reason: code.to_string(),
            policy_version: self.bundle.policy_version().to_string(),
            operation: "unknown".to_string(),
            resource_alias: "unknown".to_string(),
            limits: self.bundle.limits().clone(),
            authorized: None,
            target_audit: None,
        };
        self.store.save_decision(run_id, &decision)?;
        Ok(ToolView {
            status: "invalid",
            error_code: Some(code.to_string()),
            message: message.to_string(),
            operation: None,
            resource_alias: None,
            evidence_id: None,
            diagnostic: None,
        })
    }

    fn execute(
        &mut self,
        run_id: &str,
        authorized: &AuthorizedRead,
        budget: &mut Budget,
        remaining: Duration,
        decision: &Decision,
    ) -> Result<ToolView> {
        let mut attempts = 0u32;
        loop {
            if budget.used.provider_attempts >= authorized.limits().max_provider_attempts {
                return Ok(error_view(
                    "budget_exhausted",
                    "Provider attempt budget is exhausted",
                    authorized,
                ));
            }
            let timeout = authorized.timeout(remaining);
            if timeout.is_zero() {
                return Ok(error_view(
                    "deadline_exceeded",
                    "Run duration budget is exhausted",
                    authorized,
                ));
            }
            budget.used.provider_attempts += 1;
            self.store.update_budget(run_id, budget)?;
            let step_id = self.store.start_step(
                run_id,
                "provider_attempt",
                Some(&json!({
                    "operation": decision.operation,
                    "resource_alias": decision.resource_alias,
                    "timeout_seconds": timeout.as_secs_f64(),
                })),
            )?;
            attempts += 1;
            let mut result = self.adapter.read(authorized, timeout);
            if result.ok && accept(&mut result, authorized, self.adapter.adapter_id()) {
                let Some(diagnostic) = result.payload.clone() else {
                    return Ok(error_view(
                        "malformed",
                        "Diagnostic payload was rejected",
                        authorized,
                    ));
                };
                let payload = serde_json::to_value(diagnostic)?;
                let evidence_id = self.store.add_evidence(
                    run_id,
                    &decision.operation,
                    &decision.resource_alias,
                    &result.source,
                    &payload,
                    result.content_sha256.as_deref().unwrap_or(""),
                )?;
                self.store.finish_step(
                    &step_id,
                    "succeeded",
                    &json!({
                        "evidence_id": evidence_id,
                        "byte_length": result.byte_length,
                        "content_sha256": result.content_sha256,
                    }),
                )?;
                return Ok(ToolView {
                    status: "ok",
                    error_code: None,
                    message: "ok".to_string(),
                    operation: Some(decision.operation.clone()),
                    resource_alias: Some(decision.resource_alias.clone()),
                    evidence_id: Some(evidence_id),
                    diagnostic: Some(payload),
                });
            }
            let code = result
                .error_code
                .clone()
                .unwrap_or_else(|| "provider_error".to_string());
            let mut message = redact(&result.message);
            message.truncate(400);
            self.store.finish_step(
                &step_id,
                "failed",
                &json!({"error_code": code, "byte_length": result.byte_length}),
            )?;
            let retry = TRANSIENT.contains(&code.as_str())
                && attempts <= authorized.limits().max_transient_retries;
            if !retry {
                return Ok(error_view(&code, &message, authorized));
            }
        }
    }
}

fn accept(result: &mut AdapterResult, authorized: &AuthorizedRead, adapter_id: &str) -> bool {
    if result.byte_length > authorized.max_bytes()
        || result.payload.is_none()
        || result.content_sha256.is_none()
    {
        result.ok = false;
        result.error_code = Some(
            if result.byte_length > authorized.max_bytes() {
                "oversized"
            } else {
                "malformed"
            }
            .to_string(),
        );
        result.payload = None;
        return false;
    }
    let Some(source) = result.source.as_object() else {
        result.ok = false;
        result.error_code = Some("adapter_scope_mismatch".to_string());
        return false;
    };
    if source.keys().any(|key| {
        let lower = key.to_ascii_lowercase();
        FORBIDDEN_SOURCE_KEYS.contains(&lower.as_str())
    }) {
        fail_scope(result);
        return false;
    }
    if source.get("adapter").and_then(Value::as_str) != Some(adapter_id)
        || source.get("resource_alias").and_then(Value::as_str)
            != Some(&authorized.target().resource_alias)
    {
        fail_scope(result);
        return false;
    }
    let matches = match &authorized.target().provider {
        ProviderTarget::Fixture { relative_path, .. } => {
            source.get("relative_path").and_then(Value::as_str) == Some(relative_path)
        }
        ProviderTarget::S3 { bucket, key } => {
            source.get("bucket").and_then(Value::as_str) == Some(bucket)
                && source.get("key").and_then(Value::as_str) == Some(key)
        }
    };
    if !matches {
        fail_scope(result);
        return false;
    }
    if let Some(payload) = &result.payload {
        if validate_diagnostic(payload).is_err() {
            result.ok = false;
            result.error_code = Some("malformed".to_string());
            result.payload = None;
            return false;
        }
    }
    true
}

fn fail_scope(result: &mut AdapterResult) {
    result.ok = false;
    result.error_code = Some("adapter_scope_mismatch".to_string());
    result.message = "adapter_scope_mismatch".to_string();
    result.payload = None;
}

fn error_view(code: &str, message: &str, authorized: &AuthorizedRead) -> ToolView {
    ToolView {
        status: "error",
        error_code: Some(code.to_string()),
        message: message.to_string(),
        operation: Some("diagnostics.read".to_string()),
        resource_alias: Some(authorized.target().resource_alias.clone()),
        evidence_id: None,
        diagnostic: None,
    }
}
