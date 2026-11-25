use std::time::Duration;

use crate::error::{Error, Result};
use crate::governance::LoadedBundle;
use crate::model::Limits;
use crate::util::{is_placeholder, new_id};

/// A permit for one read. Only the broker can construct it.
#[derive(Debug, Clone)]
pub struct AuthorizedRead {
    decision_id: String,
    policy_version: String,
    target: ResolvedTarget,
    limits: Limits,
}

impl AuthorizedRead {
    pub fn decision_id(&self) -> &str {
        &self.decision_id
    }

    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }

    pub fn target(&self) -> &ResolvedTarget {
        &self.target
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub fn max_bytes(&self) -> usize {
        self.limits.max_result_bytes as usize
    }

    pub fn timeout(&self, remaining: Duration) -> Duration {
        remaining.min(Duration::from_secs(self.limits.request_timeout_seconds))
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub resource_alias: String,
    pub provider: ProviderTarget,
}

#[derive(Debug, Clone)]
pub enum ProviderTarget {
    Fixture {
        relative_path: String,
        behavior: FixtureBehavior,
    },
    S3 {
        bucket: String,
        key: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureBehavior {
    Read,
    Timeout,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub decision_id: String,
    pub allowed: bool,
    pub reason: String,
    pub policy_version: String,
    pub operation: String,
    pub resource_alias: String,
    pub limits: Limits,
    pub authorized: Option<AuthorizedRead>,
    pub target_audit: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceProvider {
    Fixture,
    S3,
}

pub struct SecurityBroker<'a> {
    bundle: &'a LoadedBundle,
}

impl<'a> SecurityBroker<'a> {
    pub fn new(bundle: &'a LoadedBundle) -> Self {
        Self { bundle }
    }

    pub fn evaluate(
        &self,
        identity: &str,
        operation: &str,
        resource_alias: &str,
        service: ServiceProvider,
    ) -> Result<Decision> {
        let policy_version = self.bundle.policy_version().to_string();
        let limits = self.bundle.limits().clone();
        let decision_id = new_id("dec_");
        let deny = |reason: &str| Decision {
            decision_id: decision_id.clone(),
            allowed: false,
            reason: reason.to_string(),
            policy_version: policy_version.clone(),
            operation: operation.to_string(),
            resource_alias: resource_alias.to_string(),
            limits: limits.clone(),
            authorized: None,
            target_audit: None,
        };
        let Some(identity_policy) = self.bundle.document.policy.identities.get(identity) else {
            return Ok(deny("unknown_identity"));
        };
        if operation != "diagnostics.read" {
            return Ok(deny("unknown_operation"));
        }
        let Some(resource) = self.bundle.document.resources.get(resource_alias) else {
            return Ok(deny("unknown_resource"));
        };
        let permitted = identity_policy.allow.iter().any(|rule| {
            rule.operation == operation && rule.resources.iter().any(|item| item == resource_alias)
        });
        if !permitted {
            return Ok(deny("resource_not_allowed"));
        }
        let target = match service {
            ServiceProvider::Fixture => {
                let behavior = match resource.fixture.behavior.as_str() {
                    "read" => FixtureBehavior::Read,
                    "timeout" => FixtureBehavior::Timeout,
                    "unavailable" => FixtureBehavior::Unavailable,
                    _ => return Err(Error::app("validation", "unknown fixture behavior")),
                };
                ResolvedTarget {
                    resource_alias: resource_alias.to_string(),
                    provider: ProviderTarget::Fixture {
                        relative_path: resource.fixture.path.clone(),
                        behavior,
                    },
                }
            }
            ServiceProvider::S3 => {
                if is_placeholder(&resource.s3.bucket) || is_placeholder(&resource.s3.key) {
                    return Ok(deny("unconfigured_target"));
                }
                ResolvedTarget {
                    resource_alias: resource_alias.to_string(),
                    provider: ProviderTarget::S3 {
                        bucket: resource.s3.bucket.clone(),
                        key: resource.s3.key.clone(),
                    },
                }
            }
        };
        let target_audit = Some(audit_target(&target));
        let authorized = AuthorizedRead {
            decision_id: decision_id.clone(),
            policy_version: policy_version.clone(),
            target,
            limits: limits.clone(),
        };
        Ok(Decision {
            decision_id,
            allowed: true,
            reason: "allowed".to_string(),
            policy_version,
            operation: operation.to_string(),
            resource_alias: resource_alias.to_string(),
            limits,
            authorized: Some(authorized),
            target_audit,
        })
    }
}

fn audit_target(target: &ResolvedTarget) -> serde_json::Value {
    match &target.provider {
        ProviderTarget::Fixture {
            relative_path,
            behavior,
        } => serde_json::json!({
            "resource_alias": target.resource_alias,
            "provider": "fixture",
            "relative_path": relative_path,
            "behavior": match behavior {
                FixtureBehavior::Read => "read",
                FixtureBehavior::Timeout => "timeout",
                FixtureBehavior::Unavailable => "unavailable",
            },
        }),
        ProviderTarget::S3 { bucket, key } => serde_json::json!({
            "resource_alias": target.resource_alias,
            "provider": "s3",
            "bucket": bucket,
            "key": key,
        }),
    }
}
