"""gemini model catalog adapter.

SOURCE: Gemini CLI models.ts (aliases + IDs)
The committed catalog is the curated snapshot; ``fetch`` falls back to it until
a programmatic refresh source is wired in (keeps the pipeline offline-safe and
never drops a provider).
"""

from __future__ import annotations

from . import make_snapshot

SOURCE = "Gemini CLI models.ts (aliases + IDs)"
STRICT_PROVIDERS = frozenset({"gemini", "vertex_ai"})


def snapshot(current_models: list[str]) -> dict:
    return make_snapshot(current_models, STRICT_PROVIDERS)


def fetch(current_models: list[str]) -> dict:
    return snapshot(current_models)
