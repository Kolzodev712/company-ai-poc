"""Construct the worker and service adapter selected by the operator."""

from __future__ import annotations

from company_ai.adapters.fixture import FixtureAdapter
from company_ai.adapters.s3 import S3DiagnosticsAdapter
from company_ai.config import Settings
from company_ai.errors import SetupError
from company_ai.workers.ollama import OllamaWorker
from company_ai.workers.scripted import ScriptedWorker


def build_worker(settings: Settings, num_predict: int) -> ScriptedWorker | OllamaWorker:
    if settings.worker_mode == "scripted":
        return ScriptedWorker(script=settings.script)
    if settings.worker_mode == "ollama":
        return OllamaWorker(
            endpoint=settings.endpoint,
            model=settings.model,
            num_predict=num_predict,
        )
    raise SetupError("setup", f"Unknown worker mode {settings.worker_mode}", missing=["worker.mode"])


def build_service(settings: Settings) -> FixtureAdapter | S3DiagnosticsAdapter:
    if settings.service_mode == "fixture":
        return FixtureAdapter(settings.fixture_dir)
    if settings.service_mode == "s3":
        return S3DiagnosticsAdapter(
            profile=settings.aws_profile,
            region=settings.aws_region,
            expected_bucket_owner=settings.expected_bucket_owner,
        )
    raise SetupError("setup", f"Unknown service mode {settings.service_mode}", missing=["service.mode"])
