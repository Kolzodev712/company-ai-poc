"""Closed contracts for events, policy, proposals, evidence, and reports.

JSON Schema draft 2020-12 is the only schema validator. Schemas are defined here
and reused by validation. User input cannot supply a schema or a remote reference.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Any, Mapping

from company_ai.errors import CompanyAIError
from company_ai.redact import redact

MAX_BUNDLE_BYTES = 256_000
MAX_EVENT_BYTES = 32_768
MAX_SKILL_BYTES = 16_000
MAX_MODEL_RESPONSE_BYTES = 65_536
MAX_STORED_TEXT = 2_000

ALIAS = {"type": "string", "minLength": 1, "maxLength": 64, "pattern": "^[a-z][a-z0-9-]{0,63}$"}
ID = {"type": "string", "minLength": 1, "maxLength": 128, "pattern": "^[A-Za-z0-9][A-Za-z0-9_.:-]*$"}
HASH = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
GIT_COMMIT = {"type": "string", "pattern": "^[0-9a-f]{40}$"}
STAMP = {"type": "string", "pattern": r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$"}
REL_PATH = {"type": "string", "minLength": 1, "maxLength": 200, "pattern": "^[A-Za-z0-9_./-]+$"}


class ContractError(CompanyAIError):
    """Invalid input or a closed-schema rejection."""


def canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False)


def fingerprint(value: Any) -> str:
    return hashlib.sha256(canonical(value).encode("utf-8")).hexdigest()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError("validation", f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def _bad_constant(value: str) -> Any:
    raise ContractError("validation", f"Non-finite JSON number: {value}")


def decode_json(text: str) -> Any:
    try:
        value = json.loads(text, object_pairs_hook=_unique_object, parse_constant=_bad_constant)
        canonical(value)
        return value
    except CompanyAIError:
        raise
    except (ValueError, RecursionError, TypeError) as exc:
        raise ContractError("validation", f"Invalid JSON: {exc}") from exc


def load_json_bytes(raw: bytes, limit: int, kind: str) -> Any:
    if len(raw) > limit:
        raise ContractError("validation", f"{kind} exceeds {limit} bytes")
    try:
        text = raw.decode("utf-8")
    except UnicodeError as exc:
        raise ContractError("validation", f"{kind} must be UTF-8") from exc
    return decode_json(text)


def load_json_file(path: str | Path, limit: int, kind: str) -> Any:
    file_path = Path(path)
    with file_path.open("rb") as stream:
        raw = stream.read(limit + 1)
    return load_json_bytes(raw, limit, kind)


def obj(**properties: Any) -> dict[str, Any]:
    return {
        "type": "object",
        "properties": properties,
        "required": list(properties),
        "additionalProperties": False,
    }


def text(max_length: int = 500) -> dict[str, Any]:
    return {"type": "string", "minLength": 1, "maxLength": max_length}


def number(maximum: int, minimum: int = 0) -> dict[str, Any]:
    return {"type": "integer", "minimum": minimum, "maximum": maximum}


def array(items: Any, maximum: int = 32, minimum: int = 0) -> dict[str, Any]:
    return {"type": "array", "items": items, "minItems": minimum, "maxItems": maximum}


READ_ARGUMENTS: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["resource_alias"],
    "properties": {"resource_alias": ALIAS},
}

DIAGNOSTIC_SCHEMA: dict[str, Any] = obj(
    service=ALIAS,
    timestamp=STAMP,
    status={"type": "string", "minLength": 1, "maxLength": 32, "pattern": "^[a-z][a-z0-9_-]*$"},
    recent_error_count=number(1_000_000),
    recent_deployment_id={
        "type": "string",
        "minLength": 1,
        "maxLength": 65,
        "pattern": "^[A-Za-z0-9][A-Za-z0-9._:-]*$",
    },
)
DIAGNOSTIC_SCHEMA = {
    **DIAGNOSTIC_SCHEMA,
    "properties": {
        **DIAGNOSTIC_SCHEMA["properties"],
        "log_excerpt": {"type": "string", "minLength": 1, "maxLength": 2_000},
    },
}

LIMITS = obj(
    max_result_bytes=number(1_000_000, 256),
    max_model_turns=number(16, 1),
    max_tool_proposals=number(16, 1),
    max_provider_attempts=number(16, 1),
    max_duration_seconds=number(600, 1),
    request_timeout_seconds=number(120, 1),
    max_input_bytes=number(200_000, 1_024),
    max_transient_retries=number(2, 0),
    max_output_tokens=number(2_048, 16),
)

ALLOW_RULE = obj(operation={"const": "diagnostics.read"}, resources=array(ALIAS, maximum=16, minimum=1))
IDENTITY = obj(allow=array(ALLOW_RULE, maximum=4, minimum=0))
FIXTURE_MAP = obj(path=REL_PATH, behavior={"enum": ["read", "timeout", "unavailable"]})
S3_MAP = obj(
    bucket=text(63),
    key=text(256),
)
RESOURCE = obj(fixture=FIXTURE_MAP, s3=S3_MAP)
PROVENANCE_SOURCE = obj(
    path=text(200),
    sha256=HASH,
    role={"enum": ["concept", "runtime_skill_source", "developer_guidance"]},
    note=text(300),
)

DEFINITIONS: dict[str, Any] = {
    "event": obj(
        schema_version={"const": 1},
        event_id=ID,
        kind={"const": "diagnostic.requested"},
        resource_alias=ALIAS,
        observed_at=STAMP,
        summary=text(500),
    ),
    "diagnostic": DIAGNOSTIC_SCHEMA,
    "tool_proposal": obj(
        kind={"const": "tool_proposal"},
        operation=text(64),
        arguments={"type": "object"},
    ),
    "final_report": obj(
        kind={"const": "final_report"},
        observations=array(text(1_000), maximum=8, minimum=1),
        hypotheses=array(text(1_000), maximum=8, minimum=1),
        missing=array(text(1_000), maximum=8, minimum=1),
        evidence_ids=array(ID, maximum=8, minimum=0),
    ),
    "bundle": obj(
        schema_version={"const": 1},
        bundle_id=ID,
        bundle_version=text(32),
        mission=obj(mission_id=ID, objective=text(1_000)),
        workflow=obj(
            workflow_id=ID,
            skill_id=ID,
            required_operation={"const": "diagnostics.read"},
            report_required={"const": True},
        ),
        skill=obj(skill_id=ID, path=REL_PATH, sha256=HASH),
        operations={
            "type": "object",
            "additionalProperties": False,
            "required": ["diagnostics.read"],
            "properties": {
                "diagnostics.read": obj(
                    description=text(400),
                    arguments={"const": READ_ARGUMENTS},
                )
            },
        },
        policy=obj(
            policy_version=text(64),
            identities={
                "type": "object",
                "minProperties": 1,
                "maxProperties": 8,
                "propertyNames": ALIAS,
                "additionalProperties": IDENTITY,
            },
            limits=LIMITS,
        ),
        resources={
            "type": "object",
            "minProperties": 1,
            "maxProperties": 16,
            "propertyNames": ALIAS,
            "additionalProperties": RESOURCE,
        },
        provenance=obj(
            repository=text(200),
            commit=GIT_COMMIT,
            sources=array(PROVENANCE_SOURCE, maximum=32, minimum=1),
        ),
    ),
    "config": {
        "type": "object",
        "additionalProperties": False,
        "required": ["runtime", "worker", "service"],
        "properties": {
            "runtime": obj(
                identity=ALIAS,
                bundle=text(300),
                database=text(300),
                reports_dir=text(300),
            ),
            "hub": obj(path=text(300)),
            "worker": obj(
                mode={"enum": ["scripted", "ollama"]},
                script={
                    "enum": [
                        "investigate",
                        "forbidden",
                        "spoof_identity",
                        "bucket_override",
                        "hostile_followup",
                        "malformed",
                        "unknown_tool",
                        "loop",
                        "premature",
                    ]
                },
                endpoint=text(300),
                model={"type": "string", "maxLength": 128},
                num_predict=number(2048, 16),
                allow_remote={"type": "boolean"},
            ),
            "service": obj(
                mode={"enum": ["fixture", "s3"]},
                fixture_dir=text(300),
                s3=obj(
                    profile={"type": "string", "maxLength": 128},
                    region={"type": "string", "maxLength": 64},
                    expected_bucket_owner={"type": "string", "maxLength": 32},
                ),
            ),
        },
    },
}


def _validator(kind: str) -> Any:
    if kind not in DEFINITIONS:
        raise ContractError("validation", f"Unknown contract: {kind}")
    return _compiled(kind)


@lru_cache(maxsize=16)
def _compiled(kind: str) -> Any:
    try:
        from jsonschema import Draft202012Validator
    except ImportError as exc:
        raise ContractError(
            "setup",
            "jsonschema is not installed. Install the project with pip install -e '.[dev]'.",
        ) from exc
    spec = {"$schema": "https://json-schema.org/draft/2020-12/schema", **DEFINITIONS[kind]}
    Draft202012Validator.check_schema(spec)
    return Draft202012Validator(spec)


def _error_code(error: Any) -> str:
    if error.validator == "additionalProperties":
        return "rejected_untrusted_field"
    return "validation"


def validate(kind: str, value: Any) -> None:
    error = next(_validator(kind).iter_errors(value), None)
    if error is None:
        return
    path = ".".join(str(part) for part in error.absolute_path) or "$"
    message = redact(error.message)
    if len(message) > 300:
        message = message[:300] + "..."
    raise ContractError(_error_code(error), f"{kind} {path}: {message}")


def validate_instance(schema: Mapping[str, Any], value: Any, kind: str) -> None:
    """Validate one closed document against a schema constant owned by this package."""
    try:
        from jsonschema import Draft202012Validator
    except ImportError as exc:
        raise ContractError("setup", "jsonschema is not installed") from exc
    if "$ref" in schema or schema.get("type") != "object":
        raise ContractError("validation", f"{kind} schema is not a local object schema")
    error = next(Draft202012Validator(dict(schema)).iter_errors(value), None)
    if error is None:
        return
    path = ".".join(str(part) for part in error.absolute_path) or "$"
    message = redact(error.message)
    if len(message) > 300:
        message = message[:300] + "..."
    raise ContractError(_error_code(error), f"{kind} {path}: {message}")


@dataclass(frozen=True)
class ResolvedTarget:
    resource_alias: str
    provider: str
    relative_path: str | None = None
    behavior: str | None = None
    bucket: str | None = None
    key: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "resource_alias": self.resource_alias,
            "provider": self.provider,
            "relative_path": self.relative_path,
            "behavior": self.behavior,
            "bucket": self.bucket,
            "key": self.key,
        }


@dataclass(frozen=True)
class AdapterResult:
    ok: bool
    error_code: str | None
    message: str
    payload: dict[str, Any] | None
    content_sha256: str | None
    byte_length: int
    source: dict[str, Any]


@dataclass(frozen=True)
class RunOutcome:
    run_id: str
    status: str
    reused: bool
    failure_code: str | None
    report_md_path: str | None
    report_json_path: str | None
    event_id: str
