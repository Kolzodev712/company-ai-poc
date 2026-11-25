"""Opt-in live checks. Skipped unless the operator opts in and live.toml exists."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from company_ai.cli import main

pytestmark = pytest.mark.live


def test_live_smoke_when_explicitly_configured():
    if os.environ.get("COMPANY_AI_LIVE") != "1":
        pytest.skip("COMPANY_AI_LIVE=1 is not set")
    config = Path("live.toml")
    if not config.is_file():
        pytest.skip("live.toml is not present")
    assert main(["live", "check", "--config", str(config)]) in {0, 2}
