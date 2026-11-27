"""oh-my-openagent model catalog adapter.

SOURCE: vendored pi-ai builtin provider snapshot (senpi-ai@2026.11.23)
The committed catalog is the vendored ``builtinProviders()`` snapshot from the
pinned OmO engine (``@code-yeongyu/senpi-ai@2026.11.23``, via ``getModels()``).
``snapshot`` returns it unconditionally so the vendored list is authoritative
and the old curated seed can never win.
"""

from __future__ import annotations

import json
from pathlib import Path

from . import make_snapshot

SOURCE = "vendored pi-ai builtin provider snapshot (senpi-ai@2026.11.23)"
STRICT_PROVIDERS = frozenset()
MODELS = json.loads(
    Path(__file__).with_name("oh_my_openagent_models.json").read_text(encoding="utf-8")
)["models"]
DEFAULT_MODELS = MODELS


def snapshot(current_models: list[str]) -> dict:
    return make_snapshot(MODELS, STRICT_PROVIDERS)


def fetch(current_models: list[str]) -> dict:
    return snapshot(current_models)