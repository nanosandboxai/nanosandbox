"""oh-my-openagent model catalog adapter.

SOURCE: curated seed (provider-prefixed codex/claude ids)
The committed catalog is the curated snapshot; ``fetch`` falls back to it until
a programmatic refresh source is wired in (keeps the pipeline offline-safe and
never drops a provider).
"""

from __future__ import annotations

from . import make_snapshot

SOURCE = "curated seed (provider-prefixed codex/claude ids)"
STRICT_PROVIDERS = frozenset()
DEFAULT_MODELS = [
    "openai/gpt-5.6-luna",
    "openai/gpt-5.5",
    "anthropic/claude-opus-4-6",
    "anthropic/claude-sonnet-4-5",
]


def snapshot(current_models: list[str]) -> dict:
    return make_snapshot(list(current_models) or DEFAULT_MODELS, STRICT_PROVIDERS)


def fetch(current_models: list[str]) -> dict:
    return snapshot(current_models)
