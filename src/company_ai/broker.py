"""Deny-by-default authorization for one normalized operation.

Decisions are not reusable credentials. The broker authorizes the exact target
the adapter will receive. It does not read credentials and cannot grant a
provider permission the provider has not already granted.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import Any

from company_ai.contracts import ResolvedTarget
from company_ai.governance import LoadedBundle, is_placeholder


@dataclass(frozen=True)
class Decision:
    decision_id: str
    allowed: bool
    reason: str
    policy_version: str
    operation: str
    resource_alias: str
    constraints: dict[str, int]
    target: ResolvedTarget | None

    def as_dict(self) -> dict[str, Any]:
        return {
            "decision_id": self.decision_id,
            "allowed": self.allowed,
            "reason": self.reason,
            "policy_version": self.policy_version,
            "operation": self.operation,
            "resource_alias": self.resource_alias,
            "constraints": dict(self.constraints),
            "target": None if self.target is None else self.target.as_dict(),
        }


class SecurityBroker:
    """Evaluate trusted run context plus one normalized request."""

    def __init__(self, bundle: LoadedBundle):
        self.bundle = bundle

    def evaluate(
        self,
        *,
        identity: str,
        operation: str,
        arguments: dict[str, Any],
        service_mode: str,
    ) -> Decision:
        policy_version = self.bundle.policy_version
        limits = self.bundle.limits
        alias = str(arguments.get("resource_alias", ""))
        decision_id = "dec_" + uuid.uuid4().hex

        def deny(reason: str, target: ResolvedTarget | None = None) -> Decision:
            return Decision(
                decision_id=decision_id,
                allowed=False,
                reason=reason,
                policy_version=policy_version,
                operation=operation,
                resource_alias=alias,
                constraints=limits,
                target=target,
            )

        identities = self.bundle.document["policy"]["identities"]
        identity_policy = identities.get(identity)
        if identity_policy is None:
            return deny("unknown_identity")
        if operation not in self.bundle.document["operations"]:
            return deny("unknown_operation")
        resource = self.bundle.document["resources"].get(alias)
        if resource is None:
            return deny("unknown_resource")
        permitted = any(
            rule["operation"] == operation and alias in rule["resources"]
            for rule in identity_policy["allow"]
        )
        if not permitted:
            return deny("resource_not_allowed")
        if service_mode == "fixture":
            fixture = resource["fixture"]
            target = ResolvedTarget(
                resource_alias=alias,
                provider="fixture",
                relative_path=fixture["path"],
                behavior=fixture["behavior"],
            )
        elif service_mode == "s3":
            s3 = resource["s3"]
            if is_placeholder(s3["bucket"]) or is_placeholder(s3["key"]):
                return deny("unconfigured_target")
            target = ResolvedTarget(
                resource_alias=alias,
                provider="s3",
                bucket=s3["bucket"],
                key=s3["key"],
            )
        else:
            return deny("unknown_service_mode")
        return Decision(
            decision_id=decision_id,
            allowed=True,
            reason="allowed",
            policy_version=policy_version,
            operation=operation,
            resource_alias=alias,
            constraints=limits,
            target=target,
        )
