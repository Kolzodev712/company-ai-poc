use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::adapters::{interpret_document, success_source};
use crate::broker::{AuthorizedRead, FixtureBehavior, ProviderTarget};
use crate::gateway::{AdapterResult, DiagnosticsSource};
use crate::governance::contained;

pub struct FixtureAdapter {
    root: PathBuf,
}

impl FixtureAdapter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl DiagnosticsSource for FixtureAdapter {
    fn adapter_id(&self) -> &'static str {
        "fixture"
    }

    fn transport(&self) -> &'static str {
        "local_files"
    }

    fn read(&self, authorized: &AuthorizedRead, _timeout: Duration) -> AdapterResult {
        let ProviderTarget::Fixture {
            relative_path,
            behavior,
        } = &authorized.target().provider
        else {
            return AdapterResult::failure(
                "invalid_target",
                "Fixture adapter received a non-fixture target",
            );
        };
        let path = match contained(&self.root, relative_path) {
            Ok(path) => path,
            Err(_) => {
                return AdapterResult::failure(
                    "invalid_target",
                    "Fixture path is outside the trusted directory",
                )
            }
        };
        if !path.is_file() {
            return AdapterResult::failure("not_found", "Fixture file does not exist");
        }
        match behavior {
            FixtureBehavior::Timeout => {
                return AdapterResult::failure("timeout", "Fixture read timed out")
            }
            FixtureBehavior::Unavailable => {
                return AdapterResult::failure("unavailable", "Fixture source was unavailable")
            }
            FixtureBehavior::Read => {}
        }
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                return AdapterResult::failure("provider_error", "Fixture file could not be opened")
            }
        };
        let mut data = Vec::new();
        if file
            .take(authorized.max_bytes() as u64 + 1)
            .read_to_end(&mut data)
            .is_err()
        {
            return AdapterResult::failure("provider_error", "Fixture file could not be read");
        }
        match interpret_document(&data, authorized.max_bytes()) {
            Ok((document, digest)) => AdapterResult {
                ok: true,
                error_code: None,
                message: "ok".to_string(),
                payload: Some(document),
                content_sha256: Some(digest.clone()),
                byte_length: data.len(),
                source: success_source(
                    self.adapter_id(),
                    json!({
                        "resource_alias": authorized.target().resource_alias,
                        "relative_path": relative_path,
                        "behavior": "read",
                        "authenticity": "content hash is not authentication",
                    }),
                    &digest,
                    data.len(),
                ),
            },
            Err(mut failure) => {
                if failure.byte_length == 0 {
                    failure.byte_length = data.len();
                }
                *failure
            }
        }
    }
}
