"""Workers that propose tools. They do not call providers."""

from company_ai.workers.ollama import OllamaWorker
from company_ai.workers.scripted import ScriptedWorker

__all__ = ["OllamaWorker", "ScriptedWorker"]
