"""Service adapters for diagnostics.read."""

from company_ai.adapters.fixture import FixtureAdapter
from company_ai.adapters.s3 import S3DiagnosticsAdapter

__all__ = ["FixtureAdapter", "S3DiagnosticsAdapter"]
