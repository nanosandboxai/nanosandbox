"""claude model catalog adapter.

SOURCE: Claude Code /model (aliases + full IDs)
The committed catalog is the curated snapshot; ``fetch`` falls back to it until
a programmatic refresh source is wired in (keeps the pipeline offline-safe and
never drops a provider).
"""

from __future__ import annotations

from . import make_snapshot

SOURCE = "Claude Code /model (aliases + full IDs)"
STRICT_PROVIDERS = frozenset({"anthropic", "bedrock", "vertex_ai", "foundry", "mantle"})


def snapshot(current_models: list[str]) -> dict:
    return make_snapshot(current_models, STRICT_PROVIDERS)


def fetch(current_models: list[str]) -> dict:
    return snapshot(current_models)
