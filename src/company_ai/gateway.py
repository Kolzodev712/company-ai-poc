"""The only execution path for diagnostics.read."""

from __future__ import annotations

import logging
import uuid
from typing import Any, Protocol

from company_ai.broker import SecurityBroker
from company_ai.contracts import (
    READ_ARGUMENTS,
    AdapterResult,
    ContractError,
    ResolvedTarget,
    validate,
    validate_instance,
)
from company_ai.governance import LoadedBundle
from company_ai.redact import redact
from company_ai.store import Store

logger = logging.getLogger("company_ai.gateway")

TRANSIENT = {"timeout", "unavailable"}
FORBIDDEN_SOURCE_KEYS = {
    "access_key",
    "secret",
    "secret_access_key",
    "session_token",
    "password",
    "credential",
    "authorization",
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "token",
}


class ServiceAdapter(Protocol):
    adapter_id: str

    def read(self, target: ResolvedTarget, timeout_seconds: float, max_bytes: int) -> AdapterResult:
        """Read exactly the authorized target or return a structured failure."""


class ToolGateway:
    def __init__(
        self,
        *,
        bundle: LoadedBundle,
        broker: SecurityBroker,
        adapter: ServiceAdapter,
        store: Store,
        service_mode: str,
    ):
        self.bundle = bundle
        self.broker = broker
        self.adapter = adapter
        self.store = store
        self.service_mode = service_mode
        self.adapter_calls = 0

    def invoke(
        self,
        *,
        run_id: str,
        identity: str,
        proposal: Any,
        budget: dict[str, Any],
        remaining_seconds: float,
    ) -> dict[str, Any]:
        normalized, invalid = self._normalize(proposal)
        if invalid is not None:
            self._audit_rejection(run_id, invalid)
            return invalid
        assert normalized is not None
        operation = normalized["operation"]
        arguments = normalized["arguments"]
        decision = self.broker.evaluate(
            identity=identity,
            operation=operation,
            arguments=arguments,
            service_mode=self.service_mode,
        )
        self.store.save_decision(run_id, decision.as_dict())
        logger.info(
            "authorization run=%s allowed=%s reason=%s policy=%s operation=%s resource=%s",
            run_id,
            decision.allowed,
            decision.reason,
            decision.policy_version,
            decision.operation,
            decision.resource_alias,
        )
        if not decision.allowed or decision.target is None:
            return {
                "status": "denied",
                "error_code": decision.reason,
                "message": decision.reason,
                "operation": operation,
                "resource_alias": arguments.get("resource_alias"),
            }
        return self._execute_authorized(
            run_id=run_id,
            decision_target=decision.target,
            operation=operation,
            budget=budget,
            remaining_seconds=remaining_seconds,
            max_attempts=decision.constraints["max_provider_attempts"],
            max_retries=decision.constraints["max_transient_retries"],
            timeout_cap=float(decision.constraints["request_timeout_seconds"]),
            max_bytes=int(decision.constraints["max_result_bytes"]),
        )

    def _normalize(self, proposal: Any) -> tuple[dict[str, Any] | None, dict[str, Any] | None]:
        try:
            validate("tool_proposal", proposal)
        except ContractError as exc:
            return None, self._invalid(exc.code, exc.message, proposal)
        operation = str(proposal["operation"])
        if operation not in self.bundle.document["operations"]:
            return None, self._invalid("unknown_tool", f"Unknown tool {operation}", proposal)
        try:
            validate_instance(READ_ARGUMENTS, proposal["arguments"], "arguments")
        except ContractError as exc:
            return None, self._invalid(exc.code, exc.message, proposal)
        return {"operation": operation, "arguments": dict(proposal["arguments"])}, None

    def _invalid(self, code: str, message: str, proposal: Any) -> dict[str, Any]:
        operation = proposal.get("operation") if isinstance(proposal, dict) else None
        arguments = proposal.get("arguments") if isinstance(proposal, dict) else None
        alias = arguments.get("resource_alias") if isinstance(arguments, dict) else None
        return {
            "status": "invalid",
            "error_code": code,
            "message": message,
            "operation": operation if isinstance(operation, str) else None,
            "resource_alias": alias if isinstance(alias, str) else None,
        }

    def _audit_rejection(self, run_id: str, invalid: dict[str, Any]) -> None:
        self.store.save_decision(
            run_id,
            {
                "decision_id": "dec_" + uuid.uuid4().hex,
                "allowed": False,
                "reason": invalid["error_code"],
                "policy_version": self.bundle.policy_version,
                "operation": invalid["operation"] or "unknown",
                "resource_alias": invalid["resource_alias"] or "unknown",
                "constraints": self.bundle.limits,
                "target": None,
            },
        )

    def _execute_authorized(
        self,
        *,
        run_id: str,
        decision_target: ResolvedTarget,
        operation: str,
        budget: dict[str, Any],
        remaining_seconds: float,
        max_attempts: int,
        max_retries: int,
        timeout_cap: float,
        max_bytes: int,
    ) -> dict[str, Any]:
        attempts = 0
        last_code = "provider_error"
        last_message = "No adapter attempt was made"
        while True:
            used = int(budget["used"]["provider_attempts"])
            if used >= max_attempts:
                return self._error("budget_exhausted", "Provider attempt budget is exhausted", decision_target)
            timeout = min(timeout_cap, remaining_seconds)
            if timeout <= 0:
                return self._error("deadline_exceeded", "Run duration budget is exhausted", decision_target)
            if attempts > max_retries:
                return self._error(last_code, last_message, decision_target)
            budget["used"]["provider_attempts"] = used + 1
            self.store.update_budget(run_id, budget)
            step_id = self.store.start_step(
                run_id,
                "provider_attempt",
                {
                    "operation": operation,
                    "resource_alias": decision_target.resource_alias,
                    "provider": decision_target.provider,
                    "timeout_seconds": timeout,
                },
            )
            attempts += 1
            result = self._call_adapter(decision_target, timeout, max_bytes)
            if result.ok and self._accepted(result, decision_target, max_bytes):
                evidence_id = self.store.add_evidence(
                    run_id=run_id,
                    operation=operation,
                    resource_alias=decision_target.resource_alias,
                    source=result.source,
                    payload=result.payload or {},
                    content_sha256=result.content_sha256 or "",
                )
                self.store.finish_step(
                    step_id,
                    "succeeded",
                    {"evidence_id": evidence_id, "byte_length": result.byte_length, "content_sha256": result.content_sha256},
                )
                return {
                    "status": "ok",
                    "operation": operation,
                    "resource_alias": decision_target.resource_alias,
                    "evidence_id": evidence_id,
                    "diagnostic": result.payload,
                }
            last_code = result.error_code or "provider_error"
            last_message = redact(result.message)[:400]
            self.store.finish_step(
                step_id,
                "failed",
                {"error_code": last_code, "byte_length": result.byte_length},
            )
            if last_code not in TRANSIENT:
                return self._error(last_code, last_message, decision_target)
            if attempts > max_retries:
                return self._error(last_code, last_message, decision_target)

    def _call_adapter(self, target: ResolvedTarget, timeout: float, max_bytes: int) -> AdapterResult:
        self.adapter_calls += 1
        try:
            result = self.adapter.read(target, timeout, max_bytes)
        except Exception as exc:
            return AdapterResult(
                ok=False,
                error_code="provider_error",
                message=redact(f"{exc.__class__.__name__}")[:400],
                payload=None,
                content_sha256=None,
                byte_length=0,
                source={},
            )
        if not isinstance(result, AdapterResult):
            return AdapterResult(
                ok=False,
                error_code="provider_error",
                message="Adapter returned an unexpected result",
                payload=None,
                content_sha256=None,
                byte_length=0,
                source={},
            )
        return result

    def _accepted(self, result: AdapterResult, target: ResolvedTarget, max_bytes: int) -> bool:
        if result.byte_length > max_bytes or result.payload is None or result.content_sha256 is None:
            result_error = "oversized" if result.byte_length > max_bytes else "malformed"
            return self._reject(result, result_error)
        lowered = {key.lower() for key in result.source}
        if lowered & FORBIDDEN_SOURCE_KEYS:
            return self._reject(result, "adapter_scope_mismatch")
        if result.source.get("adapter") != self.adapter.adapter_id:
            return self._reject(result, "adapter_scope_mismatch")
        if result.source.get("resource_alias") != target.resource_alias:
            return self._reject(result, "adapter_scope_mismatch")
        if target.provider == "fixture" and result.source.get("relative_path") != target.relative_path:
            return self._reject(result, "adapter_scope_mismatch")
        if target.provider == "s3" and (
            result.source.get("bucket") != target.bucket or result.source.get("key") != target.key
        ):
            return self._reject(result, "adapter_scope_mismatch")
        try:
            validate("diagnostic", result.payload)
        except ContractError:
            return self._reject(result, "malformed")
        return True

    def _reject(self, result: AdapterResult, code: str) -> bool:
        # AdapterResult is frozen, so the caller reads a replaced failure via the return value.
        # Returning False lets _execute_authorized use the original error when one was set.
        if result.ok:
            object.__setattr__(result, "ok", False)
            object.__setattr__(result, "error_code", code)
            object.__setattr__(result, "message", code)
            object.__setattr__(result, "payload", None)
        return False

    def _error(self, code: str, message: str, target: ResolvedTarget) -> dict[str, Any]:
        return {
            "status": "error",
            "error_code": code,
            "message": message,
            "operation": "diagnostics.read",
            "resource_alias": target.resource_alias,
        }
