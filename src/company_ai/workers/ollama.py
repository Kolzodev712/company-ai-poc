"""Ollama chat/tool worker.

Calls the documented HTTP API: POST {endpoint}/api/chat with stream disabled,
thinking disabled, and a finite options.num_predict. This module does not install
Ollama or download a model. Private thinking text is dropped before the next
request and is not returned to the runtime.
"""

from __future__ import annotations

import urllib.error
import urllib.request
from typing import Any, Callable, Mapping

from company_ai.contracts import MAX_MODEL_RESPONSE_BYTES, canonical, load_json_bytes
from company_ai.errors import CompanyAIError
from company_ai.redact import redact

Transport = Callable[[str, Mapping[str, Any], float], Any]


class WorkerError(CompanyAIError):
    """The model endpoint failed or returned a document this worker will not trust."""


def urllib_transport(url: str, payload: Mapping[str, Any], timeout: float) -> Any:
    body = canonical(payload).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read(MAX_MODEL_RESPONSE_BYTES + 1)
    except urllib.error.HTTPError as exc:
        detail = redact(exc.read(1_000).decode("utf-8", errors="replace"))
        code = "model_rejected" if exc.code in {400, 404} else "model_unavailable"
        raise WorkerError(code, f"Ollama HTTP {exc.code}: {detail[:300]}") from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise WorkerError("model_unavailable", f"Ollama endpoint unavailable: {exc.__class__.__name__}") from exc
    if len(raw) > MAX_MODEL_RESPONSE_BYTES:
        raise WorkerError("malformed_model_output", "Model response exceeds the size limit")
    try:
        return load_json_bytes(raw, MAX_MODEL_RESPONSE_BYTES, "model response")
    except CompanyAIError as exc:
        raise WorkerError("malformed_model_output", exc.message) from exc


def _task_text(context: Mapping[str, Any]) -> str:
    event = context["event"]
    return canonical(
        {
            "mission_objective": context["mission_objective"],
            "workflow_id": context["workflow_id"],
            "event": {
                "event_id": event["event_id"],
                "kind": event["kind"],
                "resource_alias": event["resource_alias"],
                "observed_at": event["observed_at"],
                "summary": event["summary"],
            },
        }
    )


def _tools(context: Mapping[str, Any]) -> list[dict[str, Any]]:
    described = {tool["name"]: tool for tool in context["tools"]}
    read = described["diagnostics.read"]
    submit = described["submit_report"]
    return [
        {
            "type": "function",
            "function": {
                "name": "diagnostics.read",
                "description": read["description"],
                "parameters": read["arguments"],
            },
        },
        {
            "type": "function",
            "function": {
                "name": "submit_report",
                "description": submit["description"],
                "parameters": submit["arguments"],
            },
        },
    ]


class OllamaWorker:
    adapter_id = "ollama"

    def __init__(
        self,
        *,
        endpoint: str,
        model: str,
        num_predict: int,
        transport: Transport | None = None,
        timeout_seconds: float = 5,
    ):
        self.endpoint = endpoint.rstrip("/")
        self.model = model
        self.num_predict = num_predict
        self.transport = transport or urllib_transport
        self.transport_name = "injected" if transport is not None else "urllib"
        self.timeout_seconds = timeout_seconds
        self.messages: list[dict[str, Any]] = []
        self._pending_tool: str | None = None
        self.payloads: list[dict[str, Any]] = []

    def turn(self, context: Mapping[str, Any], tool_result: Mapping[str, Any] | None) -> dict[str, Any]:
        if not self.messages:
            self.messages.append({"role": "system", "content": str(context["skill_instructions"])})
            self.messages.append({"role": "user", "content": _task_text(context)})
        elif tool_result is not None and self._pending_tool is not None:
            self.messages.append(
                {
                    "role": "tool",
                    "tool_name": self._pending_tool,
                    "content": canonical(dict(tool_result))[:8_000],
                }
            )
            self._pending_tool = None
        payload = {
            "model": self.model,
            "stream": False,
            "think": False,
            "messages": self.messages,
            "tools": _tools(context),
            "options": {"num_predict": self.num_predict},
        }
        self.payloads.append(payload)
        raw = self.transport(self.endpoint + "/api/chat", payload, self.timeout_seconds)
        message = raw.get("message") if isinstance(raw, dict) else None
        if not isinstance(message, dict):
            raise WorkerError("malformed_model_output", "Model response did not contain a message")
        content = message.get("content") if isinstance(message.get("content"), str) else ""
        tool_calls = message.get("tool_calls") or []
        assistant: dict[str, Any] = {"role": "assistant", "content": content}
        if tool_calls:
            if not isinstance(tool_calls, list) or len(tool_calls) != 1:
                raise WorkerError("malformed_model_output", "Expected exactly one tool call")
            function = tool_calls[0].get("function") if isinstance(tool_calls[0], dict) else None
            if not isinstance(function, dict) or not isinstance(function.get("name"), str):
                raise WorkerError("malformed_model_output", "Tool call is missing a function name")
            arguments = function.get("arguments") if function.get("arguments") is not None else {}
            if isinstance(arguments, str):
                try:
                    from company_ai.contracts import decode_json

                    arguments = decode_json(arguments)
                except CompanyAIError as exc:
                    raise WorkerError("malformed_model_output", exc.message) from exc
            if not isinstance(arguments, dict):
                raise WorkerError("malformed_model_output", "Tool arguments must be an object")
            name = function["name"]
            assistant["tool_calls"] = [
                {"type": "function", "function": {"name": name, "arguments": arguments}}
            ]
            self.messages.append(assistant)
            self._pending_tool = name
            if name == "submit_report":
                return {"kind": "final_report", **arguments}
            return {"kind": "tool_proposal", "operation": name, "arguments": arguments}
        self.messages.append(assistant)
        if content.strip().startswith("{"):
            try:
                from company_ai.contracts import decode_json

                parsed = decode_json(content)
            except CompanyAIError as exc:
                raise WorkerError("malformed_model_output", exc.message) from exc
            if isinstance(parsed, dict) and parsed.get("kind") == "final_report":
                return parsed
        raise WorkerError("malformed_model_output", "Model returned neither a tool call nor a final report")
