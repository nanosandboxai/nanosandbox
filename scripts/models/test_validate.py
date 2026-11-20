"""Tests for the models catalog validator (scripts/models/validate.py)."""

from __future__ import annotations

import json
from pathlib import Path

from . import providers
from . import validate

REPO_ROOT = Path(__file__).resolve().parents[2]
CATALOG_PATH = REPO_ROOT / "registries" / "models" / "models.json"


def _valid_v2_catalog() -> dict:
    return {
        "version": 2,
        "agents": {
            "claude": {
                "models": ["claude-opus-4-6", "claude-sonnet-4-5"],
                "providers": ["anthropic"],
            },
            "goose": {
                "models": [
                    "anthropic/claude-sonnet-5.5",
                    "openrouter/deepseek/deepseek-v4-pro",
                    "bare-model",
                ],
                "providers": ["anthropic", "openrouter"],
            },
        },
    }


def _synthesize_v2_from_current_catalog() -> dict:
    """Build the v2 shape from the current catalog without editing the file."""
    catalog = json.loads(CATALOG_PATH.read_text(encoding="utf-8"))
    v2: dict = {"version": 2, "agents": {}}
    for agent, info in catalog["agents"].items():
        agent_providers = {
            provider
            for model in info["models"]
            if (provider := providers.model_provider(model)) is not None
        }
        v2["agents"][agent] = {
            "models": info["models"],
            "providers": sorted(agent_providers),
        }
    return v2


def _violations(catalog: dict) -> list[str]:
    return validate.validate_catalog(
        catalog, providers.canonical_map(), providers.env_map()
    )


class TestValidCatalogs:
    def test_valid_v2_catalog_passes(self) -> None:
        assert _violations(_valid_v2_catalog()) == []

    def test_current_catalog_accepted_once_v2(self) -> None:
        """The real catalog must pass once it carries the v2 providers field."""
        assert _violations(_synthesize_v2_from_current_catalog()) == []


class TestSchemaShape:
    def test_version_mismatch_fails(self) -> None:
        catalog = _valid_v2_catalog()
        catalog["version"] = 1
        assert any("version is 1, expected 2" in v for v in _violations(catalog))

    def test_missing_providers_fails(self) -> None:
        catalog = _valid_v2_catalog()
        del catalog["agents"]["claude"]["providers"]
        assert any(
            "agent 'claude': missing 'providers'" in v for v in _violations(catalog)
        )

    def test_missing_models_fails(self) -> None:
        catalog = _valid_v2_catalog()
        del catalog["agents"]["claude"]["models"]
        assert any("agent 'claude': missing 'models'" in v for v in _violations(catalog))

    def test_non_string_models_fails(self) -> None:
        catalog = _valid_v2_catalog()
        catalog["agents"]["claude"]["models"] = ["ok", 42]
        assert any(
            "agent 'claude': 'models' must be an array of strings"
            in v
            for v in _violations(catalog)
        )

    def test_duplicate_models_fail(self) -> None:
        catalog = _valid_v2_catalog()
        catalog["agents"]["claude"]["models"].append("claude-opus-4-6")
        assert any(
            "agent 'claude': duplicate model 'claude-opus-4-6'" in v
            for v in _violations(catalog)
        )


class TestProviderConsistency:
    def test_planted_bad_provider_fails(self) -> None:
        """A model resolving to a provider the agent does not list must fail."""
        catalog = _valid_v2_catalog()
        catalog["agents"]["goose"]["providers"] = ["anthropic"]
        violations = _violations(catalog)
        assert any(
            "agent 'goose'" in v and "openrouter" in v and "not in providers" in v
            for v in violations
        )

    def test_unmapped_prefix_fails(self) -> None:
        catalog = _valid_v2_catalog()
        catalog["agents"]["goose"]["models"].append("brandnewprovider/foo")
        violations = _violations(catalog)
        assert any(
            "unmapped provider prefix 'brandnewprovider'" in v for v in violations
        )

    def test_unknown_providers_entry_fails(self) -> None:
        catalog = _valid_v2_catalog()
        catalog["agents"]["claude"]["providers"] = ["anthropic", "not-a-provider"]
        violations = _violations(catalog)
        assert any(
            "providers entry 'not-a-provider' is not a known canonical id" in v
            for v in violations
        )

    def test_agent_without_rule_fails(self) -> None:
        catalog = _valid_v2_catalog()
        catalog["agents"]["newagent"] = {"models": [], "providers": []}
        violations = _violations(catalog)
        assert any(
            "agent 'newagent' has no provider resolution rule" in v
            for v in violations
        )


class TestCli:
    def test_main_ok(self, tmp_path: Path) -> None:
        path = tmp_path / "models.json"
        path.write_text(json.dumps(_valid_v2_catalog()), encoding="utf-8")
        assert validate.main([str(path)]) == 0

    def test_main_fails_on_bad_provider(self, tmp_path: Path) -> None:
        catalog = _valid_v2_catalog()
        catalog["agents"]["goose"]["providers"] = ["anthropic"]
        path = tmp_path / "models.json"
        path.write_text(json.dumps(catalog), encoding="utf-8")
        assert validate.main([str(path)]) == 1

    def test_main_missing_file(self, tmp_path: Path) -> None:
        assert validate.main([str(tmp_path / "nope.json")]) == 1

    def test_main_invalid_json(self, tmp_path: Path) -> None:
        path = tmp_path / "models.json"
        path.write_text("{not json", encoding="utf-8")
        assert validate.main([str(path)]) == 1
