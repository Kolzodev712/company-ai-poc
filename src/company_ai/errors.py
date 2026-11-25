"""Error types shared by the CLI and the four workflow modules."""

from __future__ import annotations


class CompanyAIError(Exception):
    """Expected failure with a stable code and a redacted operator message."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


class SetupError(CompanyAIError):
    """Live mode or local paths are not configured. Never a signal to switch modes."""

    def __init__(self, code: str, message: str, missing: list[str] | None = None):
        super().__init__(code, message)
        self.missing = list(missing or [])


class ConflictError(CompanyAIError):
    def __init__(self, message: str):
        super().__init__("conflict", message)
