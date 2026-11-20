"""aider model catalog adapter.

SOURCE: aider/models.py + model-settings.yml
The committed catalog is the curated snapshot; ``fetch`` falls back to it until
a programmatic refresh source is wired in (keeps the pipeline offline-safe and
never drops a provider).
"""

from __future__ import annotations

from . import make_snapshot

SOURCE = "aider/models.py + model-settings.yml"
STRICT_PROVIDERS = frozenset(frozenset())


def snapshot(current_models: list[str]) -> dict:
    return make_snapshot(current_models, STRICT_PROVIDERS)


def fetch(current_models: list[str]) -> dict:
    return snapshot(current_models)
