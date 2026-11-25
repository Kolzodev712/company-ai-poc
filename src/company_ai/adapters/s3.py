"""Read one configured S3 object with GetObject.

The client is built only when a live call needs it. SDK retries are disabled:
botocore ``retries.max_attempts`` counts the first attempt, so 1 means no retry.
The runtime's own transient retry is a separate, persisted provider attempt.
A content hash or ETag recorded here is not authentication.
"""

from __future__ import annotations

from typing import Any

from company_ai.adapters.documents import failed, interpret_document
from company_ai.contracts import AdapterResult, ResolvedTarget
from company_ai.errors import SetupError
from company_ai.redact import redact


def s3_client_config(timeout_seconds: float) -> Any:
    from botocore.config import Config

    bounded = max(0.1, float(timeout_seconds))
    return Config(
        retries={"max_attempts": 1, "mode": "standard"},
        connect_timeout=bounded,
        read_timeout=bounded,
        signature_version="s3v4",
    )


def classify_provider_exception(exc: BaseException) -> tuple[str, str]:
    response = getattr(exc, "response", None)
    code = None
    status = None
    if isinstance(response, dict):
        error = response.get("Error") or {}
        if isinstance(error, dict):
            code = error.get("Code")
        metadata = response.get("ResponseMetadata") or {}
        if isinstance(metadata, dict):
            status = metadata.get("HTTPStatusCode")
    name = exc.__class__.__name__
    if code in {"AccessDenied", "InvalidAccessKeyId", "SignatureDoesNotMatch", "AllAccessDisabled"} or status == 403:
        mapped = "provider_denied"
    elif code in {"NoSuchKey", "NoSuchBucket"} or status == 404:
        mapped = "not_found"
    elif code in {"RequestTimeout", "SlowDown", "InternalError", "ServiceUnavailable"} or status in {500, 503}:
        mapped = "unavailable"
    elif "Timeout" in name:
        mapped = "timeout"
    elif "Endpoint" in name or "Connection" in name:
        mapped = "unavailable"
    else:
        mapped = "provider_error"
    return mapped, redact(f"{name}: {exc}")[:400]


class S3DiagnosticsAdapter:
    adapter_id = "s3"

    def __init__(
        self,
        *,
        profile: str | None,
        region: str | None,
        expected_bucket_owner: str | None = None,
        client: Any | None = None,
    ):
        self.profile = profile or ""
        self.region = region or ""
        self.expected_bucket_owner = expected_bucket_owner or ""
        self._client = client
        self.transport = "injected" if client is not None else "boto3"

    def read(self, target: ResolvedTarget, timeout_seconds: float, max_bytes: int) -> AdapterResult:
        if target.provider != "s3" or not target.bucket or not target.key:
            return failed("invalid_target", "S3 adapter received a non-S3 target")
        try:
            client = self._client if self._client is not None else self._build_client(timeout_seconds)
            kwargs: dict[str, Any] = {
                "Bucket": target.bucket,
                "Key": target.key,
                "Range": f"bytes=0-{max_bytes}",
            }
            if self.expected_bucket_owner:
                kwargs["ExpectedBucketOwner"] = self.expected_bucket_owner
            response = client.get_object(**kwargs)
            body = response["Body"]
            try:
                data = body.read(max_bytes + 1)
            finally:
                close = getattr(body, "close", None)
                if callable(close):
                    close()
        except SetupError:
            raise
        except Exception as exc:
            code, message = classify_provider_exception(exc)
            return failed(code, message)
        payload, failure, digest = interpret_document(data, max_bytes)
        if failure is not None or payload is None or digest is None:
            return failure if failure is not None else failed("malformed", "Diagnostic document was rejected")
        metadata = _public_metadata(response)
        return AdapterResult(
            ok=True,
            error_code=None,
            message="ok",
            payload=payload,
            content_sha256=digest,
            byte_length=len(data),
            source={
                "adapter": self.adapter_id,
                "resource_alias": target.resource_alias,
                "bucket": target.bucket,
                "key": target.key,
                "byte_length": len(data),
                "content_sha256": digest,
                "provider_metadata": metadata,
                "authenticity": "S3 ETag, version id, and content hash are not authentication",
                "transport": self.transport,
            },
        )

    def _build_client(self, timeout_seconds: float) -> Any:
        if not self.profile or not self.region:
            raise SetupError(
                "setup",
                "S3 client requires service.s3.profile and service.s3.region. Fixture mode was not substituted.",
                missing=["service.s3.profile", "service.s3.region"],
            )
        try:
            import boto3
        except ImportError as exc:
            raise SetupError(
                "setup",
                "boto3 is not installed. Install with pip install -e '.[live]'. Fixture mode was not substituted.",
                missing=["boto3"],
            ) from exc
        session = boto3.Session(profile_name=self.profile, region_name=self.region)
        return session.client("s3", config=s3_client_config(timeout_seconds))


def _public_metadata(response: dict[str, Any]) -> dict[str, Any]:
    kept: dict[str, Any] = {}
    for key in ("ETag", "ContentType", "ContentLength", "VersionId"):
        if key in response and response[key] is not None:
            kept[key] = response[key] if not isinstance(response[key], str) else response[key][:256]
    return kept
