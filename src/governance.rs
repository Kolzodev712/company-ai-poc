use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::{validate_limits, Bundle, READ_ARGUMENTS};
use crate::util::{fingerprint, is_alias, reject_duplicate_keys, sha256_hex};

const MAX_BUNDLE_BYTES: usize = 256_000;
const MAX_SKILL_BYTES: usize = 16_000;

#[derive(Debug, Clone)]
pub struct LoadedBundle {
    pub path: PathBuf,
    pub root: PathBuf,
    pub document: Bundle,
    pub fingerprint: String,
    pub skill_text: String,
}

impl LoadedBundle {
    pub fn policy_version(&self) -> &str {
        &self.document.policy.policy_version
    }

    pub fn limits(&self) -> &crate::model::Limits {
        &self.document.policy.limits
    }
}

pub fn load_bundle(path: impl AsRef<Path>) -> Result<LoadedBundle> {
    let path = path.as_ref();
    let bytes = read_limited(path, MAX_BUNDLE_BYTES, "bundle")?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::app("validation", "bundle must be UTF-8"))?;
    reject_duplicate_keys(text)?;
    let value: Value = serde_json::from_str(text)?;
    let document: Bundle = serde_json::from_value(value.clone())?;
    semantic(&document, path)?;
    let skill_text = load_skill(path.parent().unwrap_or(Path::new(".")), &document)?;
    let expected: Value = serde_json::from_str(READ_ARGUMENTS)
        .map_err(|_| Error::app("validation", "built-in argument schema is invalid"))?;
    if document.operations.diagnostics_read.arguments != expected {
        return Err(Error::app(
            "validation",
            "diagnostics.read arguments schema does not match the closed contract",
        ));
    }
    Ok(LoadedBundle {
        path: path.to_path_buf(),
        root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        fingerprint: fingerprint(&value)?,
        document,
        skill_text,
    })
}

fn semantic(document: &Bundle, bundle_path: &Path) -> Result<()> {
    if document.schema_version != 1 {
        return Err(Error::app("validation", "bundle schema_version must be 1"));
    }
    if document.workflow.required_operation != "diagnostics.read"
        || !document.workflow.report_required
    {
        return Err(Error::app(
            "validation",
            "workflow must require diagnostics.read and a report",
        ));
    }
    if document.workflow.skill_id != document.skill.skill_id {
        return Err(Error::app(
            "validation",
            "workflow.skill_id must match skill.skill_id",
        ));
    }
    if document.mission.objective.is_empty() || document.mission.objective.len() > 1_000 {
        return Err(Error::app(
            "validation",
            "mission objective length is out of bounds",
        ));
    }
    if document.operations.diagnostics_read.description.is_empty()
        || document.operations.diagnostics_read.description.len() > 400
    {
        return Err(Error::app(
            "validation",
            "operation description length is out of bounds",
        ));
    }
    validate_limits(&document.policy.limits)?;
    if document.policy.identities.is_empty() || document.policy.identities.len() > 8 {
        return Err(Error::app(
            "validation",
            "policy must name 1 to 8 identities",
        ));
    }
    for (identity, policy) in &document.policy.identities {
        if !is_alias(identity) {
            return Err(Error::app(
                "validation",
                format!("identity {identity} has an invalid name"),
            ));
        }
        for rule in &policy.allow {
            if rule.operation != "diagnostics.read" {
                return Err(Error::app(
                    "validation",
                    "allow rules may only name diagnostics.read",
                ));
            }
            if rule.resources.is_empty() || rule.resources.len() > 16 {
                return Err(Error::app(
                    "validation",
                    "allow rule resource count is out of bounds",
                ));
            }
            if rule.resources.len()
                != rule
                    .resources
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            {
                return Err(Error::app(
                    "validation",
                    format!("Duplicate resource in allow list for {identity}"),
                ));
            }
            for alias in &rule.resources {
                if !document.resources.contains_key(alias) {
                    return Err(Error::app(
                        "validation",
                        format!("Allow list references unknown resource {alias}"),
                    ));
                }
            }
        }
    }
    let root = bundle_path.parent().unwrap_or(Path::new("."));
    for (alias, resource) in &document.resources {
        if !is_alias(alias) {
            return Err(Error::app(
                "validation",
                format!("resource {alias} has an invalid name"),
            ));
        }
        if !matches!(
            resource.fixture.behavior.as_str(),
            "read" | "timeout" | "unavailable"
        ) {
            return Err(Error::app(
                "validation",
                format!("resource {alias} has an unknown fixture behavior"),
            ));
        }
        lexical_relative(&resource.fixture.path)?;
        let _ = contained(root, &resource.fixture.path)?;
        validate_object_key(alias, &resource.s3.key)?;
        if resource.s3.bucket.is_empty()
            || resource.s3.bucket.len() > 63
            || resource.s3.bucket.contains('/')
            || resource.s3.bucket == "."
            || resource.s3.bucket == ".."
        {
            return Err(Error::app(
                "validation",
                format!("Resource {alias} has an unsafe bucket name"),
            ));
        }
    }
    if document.provenance.commit.len() != 40
        || !document
            .provenance
            .commit
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    {
        return Err(Error::app(
            "validation",
            "provenance commit must be a git commit id",
        ));
    }
    if document.provenance.sources.is_empty() || document.provenance.sources.len() > 32 {
        return Err(Error::app(
            "validation",
            "provenance source count is out of bounds",
        ));
    }
    for source in &document.provenance.sources {
        if !matches!(
            source.role.as_str(),
            "concept" | "runtime_skill_source" | "developer_guidance"
        ) {
            return Err(Error::app(
                "validation",
                "provenance role is not recognized",
            ));
        }
        if source.sha256.len() != 64 || !source.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::app("validation", "provenance hash is invalid"));
        }
        lexical_relative(&source.path)?;
    }
    Ok(())
}

fn load_skill(root: &Path, document: &Bundle) -> Result<String> {
    let path = contained(root, &document.skill.path)?;
    if !path.is_file() {
        return Err(Error::app(
            "validation",
            format!("Skill file is missing: {}", document.skill.path),
        ));
    }
    let bytes = read_limited(&path, MAX_SKILL_BYTES, "skill")?;
    if sha256_hex(&bytes) != document.skill.sha256 {
        return Err(Error::app(
            "validation",
            "Skill file hash does not match the governance bundle",
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::app("validation", "Skill file must be UTF-8"))?;
    if text.trim().is_empty() {
        return Err(Error::app("validation", "Skill file is empty"));
    }
    Ok(text)
}

pub fn check_fixture_files(bundle: &LoadedBundle, fixture_dir: &Path) -> Result<()> {
    if !fixture_dir.is_dir() {
        return Err(Error::app(
            "validation",
            format!(
                "Fixture directory does not exist: {}",
                fixture_dir.display()
            ),
        ));
    }
    for (alias, resource) in &bundle.document.resources {
        let path = contained(fixture_dir, &resource.fixture.path)?;
        if !path.is_file() {
            return Err(Error::app(
                "validation",
                format!("Fixture for {alias} is missing: {}", resource.fixture.path),
            ));
        }
    }
    Ok(())
}

pub fn verify_provenance(bundle: &LoadedBundle, hub: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    if !hub.is_dir() {
        return vec![format!("cursor-hub path does not exist: {}", hub.display())];
    }
    match Command::new("git")
        .arg("-C")
        .arg(hub)
        .args(["rev-parse", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if revision != bundle.document.provenance.commit {
                errors.push(format!(
                    "cursor-hub commit is {revision}, provenance records {}",
                    bundle.document.provenance.commit
                ));
            }
        }
        _ => errors.push("could not read cursor-hub commit".to_string()),
    }
    for source in &bundle.document.provenance.sources {
        if lexical_relative(&source.path).is_err() {
            errors.push(format!("unsafe provenance path: {}", source.path));
            continue;
        }
        let file = hub.join(&source.path);
        if !file.is_file() {
            errors.push(format!("missing hub file: {}", source.path));
            continue;
        }
        match fs::read(&file) {
            Ok(bytes) if sha256_hex(&bytes) == source.sha256 => {}
            Ok(_) => errors.push(format!("hash mismatch: {}", source.path)),
            Err(_) => errors.push(format!("missing hub file: {}", source.path)),
        }
    }
    errors
}

pub fn contained(root: &Path, relative: &str) -> Result<PathBuf> {
    lexical_relative(relative)?;
    let root = if root.exists() {
        root.canonicalize()?
    } else {
        root.to_path_buf()
    };
    let candidate = root.join(relative);
    let resolved = if candidate.exists() {
        candidate.canonicalize()?
    } else {
        candidate
    };
    if resolved != root && !resolved.starts_with(&root) {
        return Err(Error::app(
            "validation",
            "Relative path escapes the trusted directory",
        ));
    }
    Ok(resolved)
}

fn lexical_relative(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.starts_with('\\')
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::CurDir
            )
        })
    {
        return Err(Error::app(
            "validation",
            "Relative path is not contained in the trusted directory",
        ));
    }
    Ok(())
}

fn validate_object_key(alias: &str, key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 256
        || !key.contains('/')
        || key.starts_with('/')
        || Path::new(key)
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::app(
            "validation",
            format!("Resource {alias} has an unsafe object key"),
        ));
    }
    Ok(())
}

fn read_limited(path: &Path, limit: usize, kind: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    if bytes.len() > limit {
        return Err(Error::app(
            "validation",
            format!("{kind} exceeds {limit} bytes"),
        ));
    }
    Ok(bytes)
}
