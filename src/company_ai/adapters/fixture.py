"""Deterministic diagnostic documents from a trusted fixture directory."""

from __future__ import annotations

from pathlib import Path

from company_ai.adapters.documents import failed, interpret_document
from company_ai.contracts import AdapterResult, ContractError, ResolvedTarget
from company_ai.governance import contained_path


class FixtureAdapter:
    adapter_id = "fixture"
    transport = "local_files"

    def __init__(self, fixture_dir: str | Path):
        self.fixture_dir = Path(fixture_dir)

    def read(self, target: ResolvedTarget, timeout_seconds: float, max_bytes: int) -> AdapterResult:
        del timeout_seconds  # The runtime supplies the bound; this adapter does not sleep past it.
        if target.provider != "fixture" or not target.relative_path or not target.behavior:
            return failed("invalid_target", "Fixture adapter received a non-fixture target")
        try:
            path = contained_path(self.fixture_dir, target.relative_path)
        except ContractError:
            return failed("invalid_target", "Fixture path is outside the trusted directory")
        if not path.is_file():
            return failed("not_found", "Fixture file does not exist")
        if target.behavior == "timeout":
            return failed("timeout", "Fixture read timed out")
        if target.behavior == "unavailable":
            return failed("unavailable", "Fixture source was unavailable")
        if target.behavior != "read":
            return failed("invalid_target", "Unsupported fixture behavior")
        with path.open("rb") as handle:
            data = handle.read(max_bytes + 1)
        payload, failure, digest = interpret_document(data, max_bytes)
        if failure is not None:
            return failure
        assert payload is not None and digest is not None
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
                "relative_path": target.relative_path,
                "behavior": target.behavior,
                "byte_length": len(data),
                "content_sha256": digest,
                "authenticity": "content hash is not authentication",
            },
        )
