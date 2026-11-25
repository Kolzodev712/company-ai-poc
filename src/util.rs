use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::new_v4().simple())
}

pub fn canonical(value: &Value) -> Result<String> {
    serde_json::to_string(value).map_err(Error::from)
}

pub fn fingerprint(value: &Value) -> Result<String> {
    Ok(sha256_hex(canonical(value)?.as_bytes()))
}

pub fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_utc(secs)
}

pub fn format_unix_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

/// Reject objects that repeat a key. `serde_json` would otherwise keep the last value.
pub fn reject_duplicate_keys(input: &str) -> Result<()> {
    let bytes = input.as_bytes();
    let mut index = 0;
    let mut frames: Vec<Frame> = Vec::new();
    while index < bytes.len() {
        skip_ws(bytes, &mut index);
        if index >= bytes.len() {
            break;
        }
        match bytes[index] {
            b'{' => {
                frames.push(Frame::object());
                index += 1;
            }
            b'[' => {
                frames.push(Frame::array());
                index += 1;
            }
            b'}' | b']' => {
                frames.pop();
                index += 1;
            }
            b',' => {
                if let Some(frame) = frames.last_mut() {
                    if frame.in_object {
                        frame.expect_key = true;
                    }
                }
                index += 1;
            }
            b':' => index += 1,
            b'"' => {
                let (text, next) = parse_string(bytes, index)?;
                if let Some(frame) = frames.last_mut() {
                    if frame.in_object && frame.expect_key {
                        if !frame.keys.insert(text) {
                            return Err(Error::app("validation", "Duplicate JSON key"));
                        }
                        frame.expect_key = false;
                    }
                }
                index = next;
            }
            _ => index += 1,
        }
    }
    Ok(())
}

struct Frame {
    in_object: bool,
    expect_key: bool,
    keys: HashSet<String>,
}

impl Frame {
    fn object() -> Self {
        Self {
            in_object: true,
            expect_key: true,
            keys: HashSet::new(),
        }
    }

    fn array() -> Self {
        Self {
            in_object: false,
            expect_key: false,
            keys: HashSet::new(),
        }
    }
}

fn skip_ws(bytes: &[u8], index: &mut usize) {
    while *index < bytes.len() && matches!(bytes[*index], b' ' | b'\n' | b'\r' | b'\t') {
        *index += 1;
    }
}

fn parse_string(bytes: &[u8], start: usize) -> Result<(String, usize)> {
    let mut index = start + 1;
    let mut out = String::new();
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok((out, index + 1)),
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    break;
                }
                let escaped = match bytes[index] {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        if index + 4 >= bytes.len() {
                            return Err(Error::app("validation", "Invalid JSON string escape"));
                        }
                        let hex = std::str::from_utf8(&bytes[index + 1..index + 5])
                            .map_err(|_| Error::app("validation", "Invalid JSON string escape"))?;
                        let code = u32::from_str_radix(hex, 16)
                            .map_err(|_| Error::app("validation", "Invalid JSON string escape"))?;
                        index += 4;
                        char::from_u32(code).unwrap_or('\u{FFFD}')
                    }
                    _ => return Err(Error::app("validation", "Invalid JSON string escape")),
                };
                out.push(escaped);
                index += 1;
            }
            byte => {
                out.push(byte as char);
                index += 1;
            }
        }
    }
    Err(Error::app("validation", "Unterminated JSON string"))
}

pub fn is_alias(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn is_event_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && (1..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

pub fn is_stamp(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit())
}

pub fn is_placeholder(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty()
        || trimmed.to_ascii_uppercase().starts_with("YOUR_")
        || trimmed.to_ascii_uppercase().starts_with("PLACEHOLDER")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_formats_as_utc() {
        assert_eq!(format_unix_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        assert!(reject_duplicate_keys(r#"{"a":1,"a":2}"#).is_err());
        assert!(reject_duplicate_keys(r#"{"a":1,"b":{"c":2}}"#).is_ok());
    }
}
