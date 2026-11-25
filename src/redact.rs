/// Remove recognizable credential material from text that may be stored or shown.
pub fn redact(input: &str) -> String {
    let mut text = input.to_string();
    text = replace_exact(&text, "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
    text = mask_prefixed(&text, &["AKIA", "ASIA"], 16, |c| {
        c.is_ascii_uppercase() || c.is_ascii_digit()
    });
    for marker in [
        "secret_access_key",
        "aws_secret_access_key",
        "aws_access_key_id",
        "aws_session_token",
        "authorization:",
        "X-Amz-Credential=",
        "X-Amz-Signature=",
        "X-Amz-Security-Token=",
    ] {
        text = mask_assignment(&text, marker);
    }
    text
}

fn replace_exact(input: &str, secret: &str) -> String {
    input.replace(secret, "[redacted]")
}

fn mask_prefixed(
    input: &str,
    prefixes: &[&str],
    extra: usize,
    ok: impl Fn(char) -> bool,
) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let mut matched = false;
        for prefix in prefixes {
            let prefix_chars: Vec<char> = prefix.chars().collect();
            if chars[index..].starts_with(&prefix_chars)
                && chars.len() >= index + prefix_chars.len() + extra
                && chars[index + prefix_chars.len()..index + prefix_chars.len() + extra]
                    .iter()
                    .copied()
                    .all(&ok)
            {
                out.push_str("[redacted]");
                index += prefix_chars.len() + extra;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push(chars[index]);
            index += 1;
        }
    }
    out
}

fn mask_assignment(input: &str, marker: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let marker_lower = marker.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(found) = lower[cursor..].find(&marker_lower) {
        let start = cursor + found;
        out.push_str(&input[cursor..start]);
        let mut end = start + marker.len();
        let bytes = input.as_bytes();
        while end < bytes.len() && matches!(bytes[end], b' ' | b'\t' | b'=' | b':') {
            end += 1;
        }
        while end < bytes.len()
            && !matches!(
                bytes[end],
                b' ' | b'\n' | b'\r' | b'\t' | b'&' | b'"' | b'\''
            )
        {
            end += 1;
        }
        out.push_str("[redacted]");
        cursor = end;
    }
    out.push_str(&input[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_example_credentials() {
        let raw = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY id=AKIAIOSFODNN7EXAMPLE";
        let cleaned = redact(raw);
        assert!(!cleaned.contains("wJalrXUtnFEMI"));
        assert!(!cleaned.contains("AKIAIOSFODNN7EXAMPLE"));
    }
}
