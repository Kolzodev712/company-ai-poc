mod fixture;
mod s3;

pub use fixture::FixtureAdapter;
pub use s3::{
    classify_provider_error, s3_retry_attempts, ObjectBody, ObjectReader, ReaderFailure,
    S3Diagnostics,
};

use serde_json::Value;

use crate::gateway::AdapterResult;
use crate::model::{validate_diagnostic, Diagnostic};
use crate::util::sha256_hex;

pub fn interpret_document(
    data: &[u8],
    max_bytes: usize,
) -> Result<(Diagnostic, String), Box<AdapterResult>> {
    if data.len() > max_bytes {
        return Err(Box::new(AdapterResult::failure(
            "oversized",
            "Diagnostic document exceeds max_result_bytes",
        )));
    }
    let digest = sha256_hex(data);
    let text = match std::str::from_utf8(data) {
        Ok(text) => text,
        Err(_) => {
            return Err(Box::new(failed_with_len(
                "malformed",
                "Diagnostic document is not valid diagnostic JSON",
                data.len(),
            )))
        }
    };
    if crate::util::reject_duplicate_keys(text).is_err() {
        return Err(Box::new(failed_with_len(
            "malformed",
            "Diagnostic document is not valid diagnostic JSON",
            data.len(),
        )));
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => {
            return Err(Box::new(failed_with_len(
                "malformed",
                "Diagnostic document is not valid diagnostic JSON",
                data.len(),
            )))
        }
    };
    let document: Diagnostic = match serde_json::from_value(value) {
        Ok(document) => document,
        Err(_) => {
            return Err(Box::new(failed_with_len(
                "malformed",
                "Diagnostic document is not valid diagnostic JSON",
                data.len(),
            )))
        }
    };
    if let Err(error) = validate_diagnostic(&document) {
        return Err(Box::new(failed_with_len(
            "malformed",
            error.message(),
            data.len(),
        )));
    }
    Ok((document, digest))
}

fn failed_with_len(code: &str, message: impl Into<String>, len: usize) -> AdapterResult {
    let mut result = AdapterResult::failure(code, message);
    result.byte_length = len;
    result
}

pub fn success_source(adapter: &str, fields: Value, digest: &str, len: usize) -> Value {
    let mut source = fields;
    if let Some(object) = source.as_object_mut() {
        object.insert("adapter".to_string(), Value::String(adapter.to_string()));
        object.insert("byte_length".to_string(), Value::from(len));
        object.insert(
            "content_sha256".to_string(),
            Value::String(digest.to_string()),
        );
    }
    source
}
