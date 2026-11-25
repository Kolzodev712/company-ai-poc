use std::sync::Arc;
use std::time::Duration;

use aws_config::retry::RetryConfig;
use aws_sdk_s3::config::{Builder, ProvideCredentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::error::ProvideErrorMetadata;
use serde_json::json;

use crate::adapters::{interpret_document, success_source};
use crate::broker::{AuthorizedRead, ProviderTarget};
use crate::gateway::{AdapterResult, DiagnosticsSource};
use crate::redact::redact;
use crate::runtime_io::block_on;

pub struct ObjectBody {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
    pub content_type: Option<String>,
}

pub struct ReaderFailure {
    pub code: Option<String>,
    pub message: String,
}

pub trait ObjectReader: Send + Sync {
    fn get(
        &self,
        bucket: &str,
        key: &str,
        range: &str,
    ) -> std::result::Result<ObjectBody, ReaderFailure>;
}

pub struct S3Diagnostics {
    profile: String,
    region: String,
    expected_owner: String,
    reader: Option<Arc<dyn ObjectReader>>,
    transport_name: &'static str,
}

impl S3Diagnostics {
    pub fn live(
        profile: impl Into<String>,
        region: impl Into<String>,
        expected_owner: impl Into<String>,
    ) -> Self {
        Self {
            profile: profile.into(),
            region: region.into(),
            expected_owner: expected_owner.into(),
            reader: None,
            transport_name: "aws-sdk-s3",
        }
    }

    pub fn injected(reader: Arc<dyn ObjectReader>) -> Self {
        Self {
            profile: String::new(),
            region: String::new(),
            expected_owner: String::new(),
            reader: Some(reader),
            transport_name: "injected",
        }
    }
}

impl DiagnosticsSource for S3Diagnostics {
    fn adapter_id(&self) -> &'static str {
        "s3"
    }

    fn transport(&self) -> &'static str {
        self.transport_name
    }

    fn read(&self, authorized: &AuthorizedRead, timeout: Duration) -> AdapterResult {
        let ProviderTarget::S3 { bucket, key } = &authorized.target().provider else {
            return AdapterResult::failure("invalid_target", "S3 adapter received a non-S3 target");
        };
        let range = format!("bytes=0-{}", authorized.max_bytes());
        let fetched = if let Some(reader) = &self.reader {
            reader.get(bucket, key, &range)
        } else {
            live_get(
                &self.profile,
                &self.region,
                &self.expected_owner,
                bucket,
                key,
                &range,
                timeout,
            )
        };
        let body = match fetched {
            Ok(body) => body,
            Err(failure) => {
                let (code, message) =
                    classify_provider_error(failure.code.as_deref(), &failure.message);
                return AdapterResult::failure(code, message);
            }
        };
        match interpret_document(&body.bytes, authorized.max_bytes()) {
            Ok((document, digest)) => {
                let mut metadata = serde_json::Map::new();
                if let Some(etag) = body.etag {
                    metadata.insert("ETag".to_string(), json!(truncate(&etag)));
                }
                if let Some(content_type) = body.content_type {
                    metadata.insert("ContentType".to_string(), json!(truncate(&content_type)));
                }
                AdapterResult {
                    ok: true,
                    error_code: None,
                    message: "ok".to_string(),
                    payload: Some(document),
                    content_sha256: Some(digest.clone()),
                    byte_length: body.bytes.len(),
                    source: success_source(
                        self.adapter_id(),
                        json!({
                            "resource_alias": authorized.target().resource_alias,
                            "bucket": bucket,
                            "key": key,
                            "provider_metadata": metadata,
                            "authenticity": "S3 ETag, version id, and content hash are not authentication",
                            "transport": self.transport_name,
                        }),
                        &digest,
                        body.bytes.len(),
                    ),
                }
            }
            Err(failure) => *failure,
        }
    }
}

pub fn s3_retry_attempts() -> u32 {
    retry_config().max_attempts()
}

fn retry_config() -> RetryConfig {
    // max_attempts includes the first request. 1 disables SDK retries.
    RetryConfig::standard().with_max_attempts(1)
}

fn live_get(
    profile: &str,
    region: &str,
    expected_owner: &str,
    bucket: &str,
    key: &str,
    range: &str,
    timeout: Duration,
) -> std::result::Result<ObjectBody, ReaderFailure> {
    if profile.is_empty() || region.is_empty() {
        return Err(ReaderFailure {
            code: Some("setup".to_string()),
            message: "S3 client requires service.s3.profile and service.s3.region. Fixture mode was not substituted.".to_string(),
        });
    }
    let provider = aws_config::profile::ProfileFileCredentialsProvider::builder()
        .profile_name(profile)
        .build();
    let loaded = block_on(provider.provide_credentials());
    if let Err(error) = loaded {
        return Err(ReaderFailure {
            code: Some("setup".to_string()),
            message: redact(&format!(
                "AWS profile {profile} could not be loaded: {error}"
            )),
        });
    }
    let shared = SharedCredentialsProvider::new(
        aws_config::profile::ProfileFileCredentialsProvider::builder()
            .profile_name(profile)
            .build(),
    );
    let timeout_config = aws_config::timeout::TimeoutConfig::builder()
        .connect_timeout(timeout)
        .read_timeout(timeout)
        .operation_attempt_timeout(timeout)
        .build();
    let config = Builder::new()
        .behavior_version_latest()
        .region(Region::new(region.to_string()))
        .credentials_provider(shared)
        .retry_config(retry_config())
        .timeout_config(timeout_config)
        .build();
    let client = aws_sdk_s3::Client::from_conf(config);
    let owner = if expected_owner.is_empty() {
        None
    } else {
        Some(expected_owner.to_string())
    };
    let send = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .range(range)
        .set_expected_bucket_owner(owner)
        .send();
    match block_on(send) {
        Ok(Ok(output)) => {
            let etag = output.e_tag().map(str::to_string);
            let content_type = output.content_type().map(str::to_string);
            match block_on(output.body.collect()) {
                Ok(Ok(collected)) => Ok(ObjectBody {
                    bytes: collected.into_bytes().to_vec(),
                    etag,
                    content_type,
                }),
                Ok(Err(error)) => Err(ReaderFailure {
                    code: Some("provider_error".to_string()),
                    message: redact(&error.to_string()),
                }),
                Err(error) => Err(ReaderFailure {
                    code: Some("provider_error".to_string()),
                    message: redact(&error.message()),
                }),
            }
        }
        Ok(Err(error)) => Err(ReaderFailure {
            code: ProvideErrorMetadata::code(&error).map(str::to_string),
            message: redact(&error.to_string()),
        }),
        Err(error) => Err(ReaderFailure {
            code: Some("setup".to_string()),
            message: redact(&error.message()),
        }),
    }
}

pub fn classify_provider_error(code: Option<&str>, display: &str) -> (&'static str, String) {
    let code = code.unwrap_or("");
    let mapped = match code {
        "AccessDenied"
        | "InvalidAccessKeyId"
        | "SignatureDoesNotMatch"
        | "AllAccessDisabled"
        | "403" => "provider_denied",
        "NoSuchKey" | "NoSuchBucket" | "404" => "not_found",
        "RequestTimeout" | "SlowDown" | "InternalError" | "ServiceUnavailable" => "unavailable",
        "setup" => "setup",
        _ if display.contains("Timeout") || display.contains("timed out") => "timeout",
        _ if display.contains("dispatch failure") || display.contains("connection") => {
            "unavailable"
        }
        _ => "provider_error",
    };
    let mut message = redact(&format!("{code}: {display}"));
    message.truncate(400);
    (mapped, message)
}

fn truncate(value: &str) -> String {
    value.chars().take(256).collect()
}
