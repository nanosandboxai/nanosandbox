"""Tests for the models catalog sync orchestrator and adapters."""

from __future__ import annotations

import json

import pytest

from . import providers, sync, validate
from .sources import AGENTS, fetch_agent

CURRENT = json.loads(sync.CATALOG.read_text(encoding="utf-8"))


def test_build_preserves_every_model() -> None:
    data, _ = sync.build(offline=True)
    for agent in AGENTS:
        before = set((CURRENT["agents"].get(agent) or {}).get("models", []))
        after = set(data["agents"][agent]["models"])
        assert before <= after, f"{agent} dropped models: {before - after}"


def test_build_is_idempotent() -> None:
    first, _ = sync.build(offline=True)
    second, _ = sync.build(offline=True)
    assert first == second


def test_build_emits_v2_with_providers() -> None:
    data, _ = sync.build(offline=True)
    assert data["version"] == 2
    for agent in AGENTS:
        info = data["agents"][agent]
        assert isinstance(info["models"], list)
        assert isinstance(info["providers"], list)
        assert info["providers"] == sorted(info["providers"])


def test_fixed_agent_providers_include_credential_provider() -> None:
    data, _ = sync.build(offline=True)
    assert "anthropic" in data["agents"]["claude"]["providers"]
    assert "openai" in data["agents"]["codex"]["providers"]
    assert "gemini" in data["agents"]["gemini"]["providers"]


def test_prefixed_agent_providers_derived_from_models() -> None:
    data, _ = sync.build(offline=True)
    # crush has aihubmix/* and openrouter/*; opencode has anthropic/openai/openrouter
    assert "aihubmix" in data["agents"]["crush"]["providers"]
    assert "openrouter" in data["agents"]["crush"]["providers"]
    assert {"anthropic", "openai", "openrouter"} <= set(
        data["agents"]["opencode"]["providers"]
    )


def test_built_catalog_passes_validation() -> None:
    data, _ = sync.build(offline=True)
    violations = validate.validate_catalog(
        data, providers.canonical_map(), providers.env_map()
    )
    assert violations == []


@pytest.mark.parametrize("agent", ["claude", "opencode", "crush"])
def test_offline_matches_snapshot(agent: str) -> None:
    models = list((CURRENT["agents"].get(agent) or {}).get("models", []))
    assert fetch_agent(agent, models, offline=True) == fetch_agent(
        agent, models, offline=False
    )


def test_main_is_idempotent_and_check(tmp_path, monkeypatch) -> None:
    catalog = tmp_path / "models.json"
    provider_catalog = tmp_path / "provider-catalog.json"
    readme = tmp_path / "README.md"
    catalog.write_text(sync.CATALOG.read_text(encoding="utf-8"), encoding="utf-8")
    readme.write_text("| claude | 0 |\n", encoding="utf-8")
    monkeypatch.setattr(sync, "CATALOG", catalog)
    monkeypatch.setattr(sync, "PROVIDER_CATALOG", provider_catalog)
    monkeypatch.setattr(sync, "README", readme)

    assert sync.main(["--offline"]) == 0  # writes v2
    after_first = catalog.read_text(encoding="utf-8")
    assert json.loads(after_first)["version"] == 2
    assert json.loads(provider_catalog.read_text(encoding="utf-8"))["version"] == 1

    assert sync.main(["--offline"]) == 0  # no change
    assert catalog.read_text(encoding="utf-8") == after_first

    assert sync.main(["--check"]) == 0  # up to date


def test_provider_catalog_is_deterministic() -> None:
    data, _ = sync.build(offline=True)
    models = {agent: data["agents"][agent]["models"] for agent in AGENTS}
    first = providers.build_provider_catalog(models)
    assert first == providers.build_provider_catalog(models)


def test_catalog_providers_match_rules() -> None:
    data, _ = sync.build(offline=True)
    for agent in AGENTS:
        models = data["agents"][agent]["models"]
        assert data["agents"][agent]["providers"] == providers.providers_for(
            agent, models
        )
