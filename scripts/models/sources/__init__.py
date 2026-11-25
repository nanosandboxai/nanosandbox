"""Per-agent model catalog source adapters.

Each adapter module exposes:
  ``fetch(current_models)``  — best-effort refresh from the agent's documented
      source; on ANY failure falls back to a snapshot of ``current_models``.
  ``snapshot(current_models)`` — always offline, no network.
  ``STRICT_PROVIDERS`` — frozenset of canonical provider ids the agent supports
      (empty for agents whose providers are derived purely from their models).

Both ``fetch`` and ``snapshot`` return ``{"models": [...], "providers": [...],
"note": "refreshed"|"snapshot"}``.
"""

from __future__ import annotations

import importlib
import urllib.request

try:
    from .. import providers
except ImportError:  # running as a plain script
    import providers

AGENTS = (
    "claude",
    "codex",
    "goose",
    "cursor",
    "opencode",
    "aider",
    "gemini",
    "crush",
    "oh-my-openagent",
    "oh-my-codex",
    "oh-my-claude",
)

# Agent ids contain hyphens, which are not valid Python module names. Map each
# hyphenated agent id to its underscore module name.
MODULE_ALIASES = {
    "oh-my-openagent": "oh_my_openagent",
    "oh-my-codex": "oh_my_codex",
    "oh-my-claude": "oh_my_claude",
}


def module_name(agent: str) -> str:
    return MODULE_ALIASES.get(agent, agent)


TIMEOUT = 5


def fetch_url(url: str, timeout: int = TIMEOUT) -> str:
    """Fetch a URL with a short timeout and no auth. Raises on any failure."""
    request = urllib.request.Request(
        url, headers={"User-Agent": "nanosandbox-models-sync/1.0"}
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read().decode("utf-8", errors="replace")


def derive_providers(models: list[str], strict: frozenset[str] = frozenset()) -> list[str]:
    """Sorted unique canonical providers present in ``models``, plus ``strict``.

    Bare model names (no ``/``) do not contribute a provider.
    """
    derived = {
        provider
        for model in models
        if (provider := providers.model_provider(model)) is not None
    }
    return sorted(derived | set(strict))


def make_snapshot(
    current_models: list[str], strict: frozenset[str] = frozenset()
) -> dict:
    """Offline snapshot: keep the current models, derive providers."""
    return {
        "models": list(current_models),
        "providers": derive_providers(current_models, strict),
        "note": "snapshot",
    }


def strict_providers(agent: str) -> frozenset[str]:
    module = importlib.import_module(f".{module_name(agent)}", __name__)
    return frozenset(getattr(module, "STRICT_PROVIDERS", ()))


def fetch_agent(agent: str, current_models: list[str], offline: bool = False) -> dict:
    module = importlib.import_module(f".{module_name(agent)}", __name__)
    if offline:
        return module.snapshot(current_models)
    return module.fetch(current_models)
