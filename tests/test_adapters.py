"""Adapter contract tests. Network clients are fakes unless a live test is opted in."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from company_ai.adapters.documents import interpret_document
from company_ai.adapters.fixture import FixtureAdapter
from company_ai.adapters.s3 import S3DiagnosticsAdapter, classify_provider_exception, s3_client_config
from company_ai.contracts import ResolvedTarget, sha256_bytes
from company_ai.workers.ollama import OllamaWorker, WorkerError
from tests.conftest import FIXTURES


class _Body:
    def __init__(self, data: bytes):
        self.data = data

    def read(self, amount: int | None = None) -> bytes:
        if amount is None:
            return self.data
        return self.data[:amount]

    def close(self) -> None:
        return None


class _Client:
    def __init__(self, data: bytes, error: Exception | None = None):
        self.data = data
        self.error = error
        self.calls: list[dict] = []

    def get_object(self, **kwargs):
        self.calls.append(kwargs)
        if self.error is not None:
            raise self.error
        return {
            "Body": _Body(self.data),
            "ETag": '"example-etag"',
            "ContentType": "application/json",
            "ContentLength": len(self.data),
        }


def test_fixture_and_s3_return_the_same_diagnostic_contract():
    raw = (FIXTURES / "service-a.json").read_bytes()
    fixture = FixtureAdapter(FIXTURES).read(
        ResolvedTarget("demo-service-a", "fixture", relative_path="service-a.json", behavior="read"),
        timeout_seconds=2,
        max_bytes=8192,
    )
    client = _Client(raw)
    s3 = S3DiagnosticsAdapter(profile="test", region="eu-central-1", client=client).read(
        ResolvedTarget("demo-service-a", "s3", bucket="demo-bucket", key="diagnostics/demo-service-a.json"),
        timeout_seconds=2,
        max_bytes=8192,
    )
    assert fixture.ok and s3.ok
    assert fixture.payload == s3.payload
    assert fixture.content_sha256 == s3.content_sha256 == sha256_bytes(raw)
    assert client.calls[0]["Bucket"] == "demo-bucket"
    assert client.calls[0]["Key"] == "diagnostics/demo-service-a.json"
    assert client.calls[0]["Range"] == "bytes=0-8192"
    assert s3.source["provider_metadata"]["ETag"] == '"example-etag"'
    assert "authentication" in s3.source["authenticity"]


def test_fixture_cases_are_bounded_and_contained(tmp_path: Path):
    adapter = FixtureAdapter(FIXTURES)
    malformed = adapter.read(
        ResolvedTarget("demo-malformed", "fixture", relative_path="malformed.txt", behavior="read"),
        1,
        8192,
    )
    oversized = adapter.read(
        ResolvedTarget("demo-oversized", "fixture", relative_path="oversized.json", behavior="read"),
        1,
        8192,
    )
    timeout = adapter.read(
        ResolvedTarget("demo-timeout", "fixture", relative_path="timeout.txt", behavior="timeout"),
        1,
        8192,
    )
    unavailable = adapter.read(
        ResolvedTarget("demo-unavailable", "fixture", relative_path="unavailable.txt", behavior="unavailable"),
        1,
        8192,
    )
    assert (malformed.error_code, oversized.error_code, timeout.error_code, unavailable.error_code) == (
        "malformed",
        "oversized",
        "timeout",
        "unavailable",
    )
    assert oversized.payload is None and oversized.byte_length > 8192
    outside = tmp_path / "secret.txt"
    outside.write_text("secret-outside", encoding="utf-8")
    escaped = adapter.read(
        ResolvedTarget("demo-service-a", "fixture", relative_path="../secret.txt", behavior="read"),
        1,
        8192,
    )
    assert escaped.error_code == "invalid_target"
    assert "secret-outside" not in escaped.message


def test_s3_access_denied_is_distinct_and_redacted():
    secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
    error = Exception(f"AccessDenied AWS_SECRET_ACCESS_KEY={secret}")
    error.response = {"Error": {"Code": "AccessDenied"}, "ResponseMetadata": {"HTTPStatusCode": 403}}
    client = _Client(b"", error=error)
    result = S3DiagnosticsAdapter(profile="test", region="eu-central-1", client=client).read(
        ResolvedTarget("demo-service-a", "s3", bucket="demo-bucket", key="diagnostics/demo-service-a.json"),
        1,
        1024,
    )
    assert result.ok is False
    assert result.error_code == "provider_denied"
    assert secret not in result.message
    assert result.payload is None


def test_s3_refuses_a_fixture_target():
    result = S3DiagnosticsAdapter(profile="test", region="eu-central-1", client=_Client(b"{}")).read(
        ResolvedTarget("demo-service-a", "fixture", relative_path="service-a.json", behavior="read"),
        1,
        100,
    )
    assert result.error_code == "invalid_target"
    assert result.ok is False


def test_sdk_retry_configuration_disables_hidden_retries():
    pytest.importorskip("botocore")
    config = s3_client_config(1.5)
    assert config.retries["max_attempts"] == 1
    assert config.connect_timeout == 1.5
    assert config.read_timeout == 1.5


def test_provider_exception_classes():
    timeout = TimeoutError("timed out")
    code, message = classify_provider_exception(timeout)
    assert code == "timeout"
    denied = Exception("no")
    denied.response = {"Error": {"Code": "NoSuchKey"}, "ResponseMetadata": {"HTTPStatusCode": 404}}
    assert classify_provider_exception(denied)[0] == "not_found"


def test_ollama_worker_uses_documented_chat_shape_and_drops_thinking():
    seen = []

    def transport(url, payload, timeout):
        seen.append((url, payload, timeout))
        assert "thinking" not in json.dumps(payload)
        if len(seen) == 1:
            return {
                "message": {
                    "role": "assistant",
                    "content": "",
                    "thinking": "SECRET_REASONING",
                    "tool_calls": [
                        {"function": {"name": "diagnostics.read", "arguments": {"resource_alias": "demo-service-a"}}}
                    ],
                }
            }
        tool = json.loads(payload["messages"][-1]["content"])
        return {
            "message": {
                "role": "assistant",
                "content": "",
                "thinking": "SECRET_REASONING",
                "tool_calls": [
                    {
                        "function": {
                            "name": "submit_report",
                            "arguments": {
                                "observations": [f"service={tool['diagnostic']['service']}"],
                                "hypotheses": ["Unproven."],
                                "missing": ["No baseline."],
                                "evidence_ids": [tool["evidence_id"]],
                            },
                        }
                    }
                ],
            }
        }

    worker = OllamaWorker(endpoint="http://127.0.0.1:11434", model="configured-model", num_predict=64, transport=transport)
    context = {
        "mission_objective": "Investigate one alias.",
        "workflow_id": "diagnostic-investigation",
        "skill_id": "diagnostic-investigation",
        "skill_instructions": "SKILL_SENTINEL read the alias.",
        "event": {
            "event_id": "evt-1",
            "kind": "diagnostic.requested",
            "resource_alias": "demo-service-a",
            "observed_at": "2026-11-25T12:04:00Z",
            "summary": "Synthetic.",
        },
        "tools": [
            {
                "name": "diagnostics.read",
                "description": "Read the diagnostic JSON document for one configured resource alias.",
                "arguments": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["resource_alias"],
                    "properties": {"resource_alias": {"type": "string"}},
                },
            },
            {
                "name": "submit_report",
                "description": "Submit the report.",
                "arguments": {"type": "object"},
            },
        ],
        "evidence": [],
    }
    proposal = worker.turn(context, None)
    assert proposal["kind"] == "tool_proposal"
    assert seen[0][0] == "http://127.0.0.1:11434/api/chat"
    assert seen[0][1]["stream"] is False
    assert seen[0][1]["think"] is False
    assert seen[0][1]["options"]["num_predict"] == 64
    assert "SKILL_SENTINEL" in seen[0][1]["messages"][0]["content"]
    report = worker.turn(
        context,
        {
            "status": "ok",
            "evidence_id": "ev_123",
            "diagnostic": {
                "service": "demo-service-a",
                "status": "degraded",
                "recent_error_count": 3,
                "recent_deployment_id": "deploy-1007",
                "timestamp": "2026-11-25T12:00:00Z",
            },
        },
    )
    assert report["kind"] == "final_report"
    assert report["evidence_ids"] == ["ev_123"]
    assert "SECRET_REASONING" not in json.dumps(seen)


def test_ollama_malformed_and_unavailable_fail_clearly():
    def bad_transport(url, payload, timeout):
        return {"message": {"role": "assistant", "content": "not a report"}}

    worker = OllamaWorker(endpoint="http://127.0.0.1:11434", model="configured-model", num_predict=32, transport=bad_transport)
    context = {
        "mission_objective": "Investigate.",
        "workflow_id": "diagnostic-investigation",
        "skill_instructions": "skill",
        "event": {
            "event_id": "evt-1",
            "kind": "diagnostic.requested",
            "resource_alias": "demo-service-a",
            "observed_at": "2026-11-25T12:04:00Z",
            "summary": "Synthetic.",
        },
        "tools": [
            {"name": "diagnostics.read", "description": "read", "arguments": {"type": "object"}},
            {"name": "submit_report", "description": "submit", "arguments": {"type": "object"}},
        ],
    }
    with pytest.raises(WorkerError) as caught:
        worker.turn(context, None)
    assert caught.value.code == "malformed_model_output"

    def down(url, payload, timeout):
        raise WorkerError("model_unavailable", "Ollama endpoint unavailable: URLError")

    worker = OllamaWorker(endpoint="http://127.0.0.1:11434", model="missing-model", num_predict=32, transport=down)
    with pytest.raises(WorkerError) as caught:
        worker.turn(context, None)
    assert caught.value.code == "model_unavailable"


def test_document_interpreter_rejects_non_json():
    payload, failure, _digest = interpret_document(b"{", 100)
    assert payload is None and failure is not None and failure.error_code == "malformed"
