"""Load and check the operator-owned governance bundle.

The bundle is versioned configuration. Model output and provider payloads are
never written back into it.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from company_ai.contracts import (
    MAX_BUNDLE_BYTES,
    MAX_SKILL_BYTES,
    ContractError,
    fingerprint,
    load_json_file,
    sha256_bytes,
    validate,
)

PLACEHOLDER_PREFIXES = ("YOUR_", "PLACEHOLDER")


def is_placeholder(value: str | None) -> bool:
    if value is None:
        return True
    text = value.strip()
    if not text:
        return True
    upper = text.upper()
    return upper.startswith(PLACEHOLDER_PREFIXES)


def contained_path(root: Path, relative: str) -> Path:
    parts = Path(relative).parts
    if (
        not relative
        or relative.startswith(("/", "\\"))
        or ".." in parts
        or any(part in ("", ".") for part in parts)
    ):
        raise ContractError("validation", "Relative path is not contained in the trusted directory")
    root_resolved = root.resolve()
    candidate = (root_resolved / relative).resolve()
    if candidate != root_resolved and root_resolved not in candidate.parents:
        raise ContractError("validation", "Relative path escapes the trusted directory")
    return candidate


@dataclass
class LoadedBundle:
    path: Path
    root: Path
    document: dict[str, Any]
    fingerprint: str
    skill_text: str

    @property
    def bundle_id(self) -> str:
        return str(self.document["bundle_id"])

    @property
    def bundle_version(self) -> str:
        return str(self.document["bundle_version"])

    @property
    def policy_version(self) -> str:
        return str(self.document["policy"]["policy_version"])

    @property
    def limits(self) -> dict[str, int]:
        return dict(self.document["policy"]["limits"])

    @property
    def mission(self) -> dict[str, Any]:
        return dict(self.document["mission"])

    @property
    def workflow(self) -> dict[str, Any]:
        return dict(self.document["workflow"])

    @property
    def skill_id(self) -> str:
        return str(self.document["skill"]["skill_id"])

    @property
    def provenance(self) -> dict[str, Any]:
        return dict(self.document["provenance"])


def _semantic(document: dict[str, Any], bundle_path: Path) -> str:
    workflow = document["workflow"]
    skill = document["skill"]
    if workflow["skill_id"] != skill["skill_id"]:
        raise ContractError("validation", "workflow.skill_id must match skill.skill_id")
    resources = document["resources"]
    for identity, policy in document["policy"]["identities"].items():
        for rule in policy["allow"]:
            if len(set(rule["resources"])) != len(rule["resources"]):
                raise ContractError("validation", f"Duplicate resource in allow list for {identity}")
            for alias in rule["resources"]:
                if alias not in resources:
                    raise ContractError("validation", f"Allow list references unknown resource {alias}")
    for alias, resource in resources.items():
        relative = resource["fixture"]["path"]
        contained_path(bundle_path.parent, relative)
        bucket = resource["s3"]["bucket"]
        key = resource["s3"]["key"]
        if "/" not in key or key.startswith("/") or ".." in Path(key).parts:
            raise ContractError("validation", f"Resource {alias} has an unsafe object key")
        if bucket in {".", ".."} or "/" in bucket:
            raise ContractError("validation", f"Resource {alias} has an unsafe bucket name")
    limits = document["policy"]["limits"]
    if limits["request_timeout_seconds"] > limits["max_duration_seconds"]:
        raise ContractError("validation", "request_timeout_seconds exceeds max_duration_seconds")
    skill_path = contained_path(bundle_path.parent, skill["path"])
    if not skill_path.is_file():
        raise ContractError("validation", f"Skill file is missing: {skill['path']}")
    skill_bytes = skill_path.read_bytes()
    if len(skill_bytes) > MAX_SKILL_BYTES:
        raise ContractError("validation", "Skill file exceeds the size limit")
    digest = sha256_bytes(skill_bytes)
    if digest != skill["sha256"]:
        raise ContractError("validation", "Skill file hash does not match the governance bundle")
    try:
        skill_text = skill_bytes.decode("utf-8")
    except UnicodeError as exc:
        raise ContractError("validation", "Skill file must be UTF-8") from exc
    if not skill_text.strip():
        raise ContractError("validation", "Skill file is empty")
    return skill_text


def load_bundle(path: str | Path) -> LoadedBundle:
    bundle_path = Path(path)
    document = load_json_file(bundle_path, MAX_BUNDLE_BYTES, "bundle")
    validate("bundle", document)
    skill_text = _semantic(document, bundle_path)
    return LoadedBundle(
        path=bundle_path,
        root=bundle_path.parent,
        document=document,
        fingerprint=fingerprint(document),
        skill_text=skill_text,
    )


def check_fixture_files(bundle: LoadedBundle, fixture_dir: Path) -> None:
    root = fixture_dir.resolve()
    if not root.is_dir():
        raise ContractError("validation", f"Fixture directory does not exist: {fixture_dir}")
    for alias, resource in bundle.document["resources"].items():
        relative = resource["fixture"]["path"]
        candidate = contained_path(root, relative)
        if not candidate.is_file():
            raise ContractError("validation", f"Fixture for {alias} is missing: {relative}")


def verify_provenance(bundle: LoadedBundle, hub_path: Path) -> list[str]:
    """Recompute recorded cursor-hub hashes. Empty list means the record matches."""
    errors: list[str] = []
    hub = hub_path.resolve()
    if not hub.is_dir():
        return [f"cursor-hub path does not exist: {hub}"]
    recorded = bundle.provenance["commit"]
    try:
        revision = subprocess.check_output(
            ["git", "-C", str(hub), "rev-parse", "HEAD"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        errors.append(f"could not read cursor-hub commit: {exc.__class__.__name__}")
    else:
        if revision != recorded:
            errors.append(f"cursor-hub commit is {revision}, provenance records {recorded}")
    for source in bundle.provenance["sources"]:
        relative = source["path"]
        if ".." in Path(relative).parts or relative.startswith("/"):
            errors.append(f"unsafe provenance path: {relative}")
            continue
        file_path = hub / relative
        if not file_path.is_file():
            errors.append(f"missing hub file: {relative}")
            continue
        digest = sha256_bytes(file_path.read_bytes())
        if digest != source["sha256"]:
            errors.append(f"hash mismatch: {relative}")
    return errors
