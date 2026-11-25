"""Remove recognizable credential material from persisted text."""

from __future__ import annotations

import re

_PATTERNS = (
    re.compile(r"AKIA[0-9A-Z]{16}"),
    re.compile(r"ASIA[0-9A-Z]{16}"),
    re.compile(r"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
    re.compile(r"(?i)(aws_)?secret_access_key\s*[=:]\s*\S+"),
    re.compile(r"(?i)aws_access_key_id\s*[=:]\s*\S+"),
    re.compile(r"(?i)aws_session_token\s*[=:]\s*\S+"),
    re.compile(r"(?i)authorization\s*:\s*\S+"),
    re.compile(r"X-Amz-Credential=[^&\s]+"),
    re.compile(r"X-Amz-Signature=[^&\s]+"),
    re.compile(r"X-Amz-Security-Token=[^&\s]+"),
)


def redact(value: str) -> str:
    text = value
    for pattern in _PATTERNS:
        text = pattern.sub("[redacted]", text)
    return text
