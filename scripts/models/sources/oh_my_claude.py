"""oh-my-claude model catalog adapter.

SOURCE: delegated to the claude catalog in sync.py
The committed catalog is the curated snapshot; ``fetch`` falls back to it until
a programmatic refresh source is wired in (keeps the pipeline offline-safe and
never drops a provider).
"""

from __future__ import annotations

from . import make_snapshot

SOURCE = "delegated to the claude catalog in sync.py"
STRICT_PROVIDERS = frozenset({"anthropic", "bedrock", "vertex_ai"})
DEFAULT_MODELS = []


def snapshot(current_models: list[str]) -> dict:
    return make_snapshot(list(current_models) or DEFAULT_MODELS, STRICT_PROVIDERS)


def fetch(current_models: list[str]) -> dict:
    return snapshot(current_models)
