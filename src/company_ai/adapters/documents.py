"""Shared interpretation of a bounded diagnostic document."""

from __future__ import annotations

from company_ai.contracts import AdapterResult, ContractError, decode_json, sha256_bytes, validate


def failed(code: str, message: str, byte_length: int = 0) -> AdapterResult:
    return AdapterResult(
        ok=False,
        error_code=code,
        message=message,
        payload=None,
        content_sha256=None,
        byte_length=byte_length,
        source={},
    )


def interpret_document(data: bytes, max_bytes: int) -> tuple[dict | None, AdapterResult | None, str | None]:
    """Return (payload, failure, content_hash). Failure is set when the bytes are not acceptable."""
    if len(data) > max_bytes:
        return None, failed("oversized", "Diagnostic document exceeds max_result_bytes", len(data)), None
    digest = sha256_bytes(data)
    try:
        value = decode_json(data.decode("utf-8"))
        validate("diagnostic", value)
    except (ContractError, UnicodeError):
        return None, failed("malformed", "Diagnostic document is not valid diagnostic JSON", len(data)), digest
    if not isinstance(value, dict):
        return None, failed("malformed", "Diagnostic document is not an object", len(data)), digest
    return value, None, digest
