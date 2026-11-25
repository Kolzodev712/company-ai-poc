from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest

from company_ai.contracts import sha256_bytes

ROOT = Path(__file__).resolve().parents[1]
BUNDLE = ROOT / "governance" / "bundle.json"
FIXTURES = ROOT / "governance" / "fixtures"
EVENT = ROOT / "examples" / "events" / "diagnostic-demo-service-a.json"
HUB = ROOT.parent / "cursor-hub"


def event(event_id: str, alias: str = "demo-service-a", summary: str = "Synthetic diagnostic request.") -> dict:
    return {
        "schema_version": 1,
        "event_id": event_id,
        "kind": "diagnostic.requested",
        "resource_alias": alias,
        "observed_at": "2026-11-25T12:04:00Z",
        "summary": summary,
    }


def write_bundle(tmp_path: Path, mutate=None, skill_text: str | None = None) -> Path:
    document = json.loads(BUNDLE.read_text(encoding="utf-8"))
    root = tmp_path / "governance"
    skill_dir = root / "skills"
    skill_dir.mkdir(parents=True)
    if skill_text is None:
        shutil.copy(ROOT / "governance" / "skills" / "diagnostic-investigation.md", skill_dir / "diagnostic-investigation.md")
    else:
        target = skill_dir / "diagnostic-investigation.md"
        target.write_text(skill_text, encoding="utf-8")
        document["skill"]["sha256"] = sha256_bytes(target.read_bytes())
    if mutate is not None:
        mutate(document)
    path = root / "bundle.json"
    path.write_text(json.dumps(document), encoding="utf-8")
    return path


@pytest.fixture
def project_root() -> Path:
    return ROOT
