use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::Url;

use crate::error::{Error, Result};
use crate::governance::LoadedBundle;
use crate::model::{validate_event, Event};
use crate::util::{is_placeholder, reject_duplicate_keys};

const MAX_EVENT_BYTES: usize = 32_768;

#[derive(Debug, Clone)]
pub struct Settings {
    pub identity: String,
    pub bundle_path: PathBuf,
    pub database: PathBuf,
    pub reports_dir: PathBuf,
    pub hub_path: Option<PathBuf>,
    pub worker_mode: String,
    pub script: String,
    pub endpoint: String,
    pub model: String,
    pub num_predict: u32,
    pub allow_remote: bool,
    pub service_mode: String,
    pub fixture_dir: PathBuf,
    pub aws_profile: String,
    pub aws_region: String,
    pub expected_bucket_owner: String,
    pub live: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    runtime: RuntimeFile,
    #[serde(default)]
    hub: Option<HubFile>,
    worker: WorkerFile,
    service: ServiceFile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFile {
    identity: String,
    bundle: String,
    database: String,
    reports_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubFile {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerFile {
    mode: String,
    script: String,
    endpoint: String,
    model: String,
    num_predict: u32,
    allow_remote: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceFile {
    mode: String,
    fixture_dir: String,
    s3: S3File,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3File {
    profile: String,
    region: String,
    expected_bucket_owner: String,
}

pub fn load_config(path: &Path) -> Result<FileConfig> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > 64_000 {
        return Err(Error::app(
            "validation",
            "Configuration file exceeds 64000 bytes",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::app("validation", "Configuration file is not valid UTF-8"))?;
    toml::from_str(text).map_err(|error| {
        let message = crate::redact::redact(&error.to_string());
        Error::app(
            "validation",
            format!("Configuration file is invalid: {message}"),
        )
    })
}

pub struct Overrides<'a> {
    pub live: bool,
    pub identity: Option<&'a str>,
    pub bundle: Option<&'a str>,
    pub database: Option<&'a str>,
    pub reports_dir: Option<&'a str>,
    pub hub: Option<&'a str>,
    pub worker: Option<&'a str>,
    pub service: Option<&'a str>,
    pub fixture_dir: Option<&'a str>,
}

pub fn settings_from_config(
    document: FileConfig,
    base: &Path,
    overrides: Overrides<'_>,
) -> Result<Settings> {
    if !matches!(document.worker.mode.as_str(), "scripted" | "ollama") {
        return Err(Error::app(
            "validation",
            "worker.mode must be scripted or ollama",
        ));
    }
    if !matches!(document.service.mode.as_str(), "fixture" | "s3") {
        return Err(Error::app(
            "validation",
            "service.mode must be fixture or s3",
        ));
    }
    let known = [
        "investigate",
        "forbidden",
        "spoof_identity",
        "bucket_override",
        "hostile_followup",
        "malformed",
        "unknown_tool",
        "loop",
        "premature",
    ];
    if !known.contains(&document.worker.script.as_str()) {
        return Err(Error::app(
            "validation",
            "worker.script is not a known script",
        ));
    }
    let hub_value = overrides
        .hub
        .or(document.hub.as_ref().map(|hub| hub.path.as_str()));
    Ok(Settings {
        identity: overrides
            .identity
            .unwrap_or(&document.runtime.identity)
            .to_string(),
        bundle_path: resolve(base, overrides.bundle.unwrap_or(&document.runtime.bundle)),
        database: resolve(
            base,
            overrides.database.unwrap_or(&document.runtime.database),
        ),
        reports_dir: resolve(
            base,
            overrides
                .reports_dir
                .unwrap_or(&document.runtime.reports_dir),
        ),
        hub_path: hub_value.map(|value| resolve(base, value)),
        worker_mode: overrides
            .worker
            .unwrap_or(&document.worker.mode)
            .to_string(),
        script: document.worker.script,
        endpoint: document.worker.endpoint,
        model: document.worker.model,
        num_predict: document.worker.num_predict,
        allow_remote: document.worker.allow_remote,
        service_mode: overrides
            .service
            .unwrap_or(&document.service.mode)
            .to_string(),
        fixture_dir: resolve(
            base,
            overrides
                .fixture_dir
                .unwrap_or(&document.service.fixture_dir),
        ),
        aws_profile: document.service.s3.profile,
        aws_region: document.service.s3.region,
        expected_bucket_owner: document.service.s3.expected_bucket_owner,
        live: overrides.live,
    })
}

pub fn load_event(path: &Path) -> Result<Event> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(Error::app("validation", "event exceeds 32768 bytes"));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| Error::app("validation", "event must be UTF-8"))?;
    reject_duplicate_keys(text)?;
    let event: Event = serde_json::from_str(text)?;
    validate_event(&event)?;
    Ok(event)
}

pub fn live_problems(
    settings: &Settings,
    bundle: &LoadedBundle,
    event: Option<&Event>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let needs_live = settings.worker_mode == "ollama" || settings.service_mode == "s3";
    if needs_live && !settings.live {
        problems.push("--live".to_string());
    }
    if !bundle
        .document
        .policy
        .identities
        .contains_key(&settings.identity)
    {
        problems.push("runtime.identity".to_string());
    }
    if settings.worker_mode == "ollama" {
        if settings.model.trim().is_empty() || is_placeholder(&settings.model) {
            problems.push("worker.model".to_string());
        }
        problems.extend(endpoint_problems(&settings.endpoint, settings.allow_remote));
        if settings.num_predict > bundle.limits().max_output_tokens {
            problems.push("worker.num_predict exceeds policy max_output_tokens".to_string());
        }
    }
    if settings.service_mode == "s3" {
        if settings.aws_profile.trim().is_empty() || is_placeholder(&settings.aws_profile) {
            problems.push("service.s3.profile".to_string());
        }
        if settings.aws_region.trim().is_empty() || is_placeholder(&settings.aws_region) {
            problems.push("service.s3.region".to_string());
        }
        let aliases = if let Some(event) = event {
            vec![event.resource_alias.clone()]
        } else {
            allowed_aliases(bundle, &settings.identity)
        };
        for alias in aliases {
            match bundle.document.resources.get(&alias) {
                None => problems.push(format!("resources.{alias}")),
                Some(resource) => {
                    if is_placeholder(&resource.s3.bucket) {
                        problems.push(format!("resources.{alias}.s3.bucket"));
                    }
                    if is_placeholder(&resource.s3.key) {
                        problems.push(format!("resources.{alias}.s3.key"));
                    }
                }
            }
        }
    }
    problems
}

pub fn require_ready(
    settings: &Settings,
    bundle: &LoadedBundle,
    event: Option<&Event>,
) -> Result<()> {
    let problems = live_problems(settings, bundle, event);
    if problems.is_empty() {
        return Ok(());
    }
    let joined = problems.join(", ");
    Err(Error::setup(
        format!("Live configuration is incomplete. Fixture mode was not substituted. Missing or invalid settings: {joined}"),
        problems,
    ))
}

fn endpoint_problems(endpoint: &str, allow_remote: bool) -> Vec<String> {
    let Ok(url) = Url::parse(endpoint) else {
        return vec!["worker.endpoint".to_string()];
    };
    if !url.username().is_empty() || url.password().is_some() {
        return vec!["worker.endpoint must not include credentials".to_string()];
    }
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || url.path() != "/" {
        return vec!["worker.endpoint".to_string()];
    }
    let host = url.host_str().unwrap_or("");
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") && !allow_remote {
        return vec!["worker.allow_remote".to_string()];
    }
    Vec::new()
}

fn allowed_aliases(bundle: &LoadedBundle, identity: &str) -> Vec<String> {
    let Some(policy) = bundle.document.policy.identities.get(identity) else {
        return Vec::new();
    };
    let mut aliases = Vec::new();
    for rule in &policy.allow {
        for alias in &rule.resources {
            if !aliases.contains(alias) {
                aliases.push(alias.clone());
            }
        }
    }
    aliases
}

fn resolve(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
