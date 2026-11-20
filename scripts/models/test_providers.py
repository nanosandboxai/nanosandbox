"""Tests for provider normalization (scripts/models/providers.py)."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from . import providers

REPO_ROOT = Path(__file__).resolve().parents[2]
CATALOG_PATH = REPO_ROOT / "registries" / "models" / "models.json"
PROVIDERS_YAML = REPO_ROOT / "registries" / "models" / "providers.yaml"


class TestCanonicalProvider:
    def test_fireworks_variants(self) -> None:
        assert providers.canonical_provider("fireworks-ai") == "fireworks"
        assert providers.canonical_provider("fireworks_ai") == "fireworks"
        assert providers.canonical_provider("fireworks") == "fireworks"

    def test_vertex_variants(self) -> None:
        assert providers.canonical_provider("vertex_ai") == "vertex_ai"
        assert providers.canonical_provider("vertexai") == "vertex_ai"

    def test_opencode_variants(self) -> None:
        assert providers.canonical_provider("opencode_go") == "opencode"
        assert providers.canonical_provider("opencode-go") == "opencode"
        assert providers.canonical_provider("opencode") == "opencode"
        assert providers.canonical_provider("opencode_zen") == "opencode"
        assert providers.canonical_provider("bakeryhub") == "opencode"

    def test_zai_variants(self) -> None:
        assert providers.canonical_provider("zai") == "zai"
        assert providers.canonical_provider("zai-org") == "zai"
        assert providers.canonical_provider("zai_coding_plan") == "zai"

    def test_minimax_variants(self) -> None:
        assert providers.canonical_provider("minimax-china") == "minimax"
        assert providers.canonical_provider("minimax") == "minimax"

    def test_bedrock_variants(self) -> None:
        assert providers.canonical_provider("bedrock-europe") == "bedrock"
        assert providers.canonical_provider("bedrock_converse") == "bedrock"
        assert providers.canonical_provider("bedrock") == "bedrock"

    def test_zhipu_variants(self) -> None:
        assert providers.canonical_provider("zhipu-coding") == "zhipu"
        assert providers.canonical_provider("zhipu") == "zhipu"

    def test_hf_maps_to_huggingface(self) -> None:
        assert providers.canonical_provider("hf") == "huggingface"

    def test_openrouter_variants(self) -> None:
        for variant in (
            "eurouter",
            "vercel_ai_gateway",
            "routerplex",
            "aigateway",
            "aimlapi",
            "trustedrouter",
            "orcarouter",
            "pleumrouter",
            "routstr",
            "ovhcloud",
            "futurmix",
            "meta",
        ):
            assert providers.canonical_provider(variant) == "openrouter", variant

    def test_alibaba_variants(self) -> None:
        assert providers.canonical_provider("alibaba-us") == "alibaba_us"
        assert providers.canonical_provider("alibaba-singapore") == "alibaba_singapore"
        assert providers.canonical_provider("alibaba") == "alibaba"

    def test_kimi_coding(self) -> None:
        assert providers.canonical_provider("kimi-coding") == "kimi_coding"

    def test_catalog_found_additions(self) -> None:
        assert providers.canonical_provider("cohere_chat") == "cohere"
        assert providers.canonical_provider("copilot") == "copilot"

    def test_identity_entries(self) -> None:
        for prefix in (
            "anthropic",
            "openai",
            "google",
            "gemini",
            "deepseek",
            "groq",
            "xai",
            "cerebras",
            "mistral",
            "huggingface",
            "azure",
            "moonshot",
            "aihubmix",
            "vercel",
            "venice",
            "cortecs",
            "ionet",
            "atlascloud",
            "nebius",
            "neuralwatt",
            "avian",
            "chutes",
            "baseten",
            "qiniucloud",
            "synthetic",
            "scaleway",
            "pioneer",
            "pareto",
            "coralbricks",
            "nvidia",
            "perplexity",
            "together",
            "friendli",
            "novita",
            "saladcloud",
            "sakana",
            "inception",
            "celeris",
            "typesafe",
            "tanzu_ai",
            "opper",
            "saygm",
            "nearai",
            "iflytek",
            "iflytek_astron",
            "custom_deepseek",
            "custom_tensorix",
            "empiriolabs",
        ):
            assert providers.canonical_provider(prefix) == prefix, prefix

    def test_case_insensitive(self) -> None:
        assert providers.canonical_provider("OpenRouter") == "openrouter"
        assert providers.canonical_provider("ANTHROPIC") == "anthropic"

    def test_strips_colon_suffix(self) -> None:
        assert providers.canonical_provider("openrouter:free") == "openrouter"
        assert providers.canonical_provider("openrouter:beta") == "openrouter"

    def test_unmapped_prefix_falls_back_to_identity(self) -> None:
        assert providers.canonical_provider("totally-unknown") == "totally-unknown"


class TestEnvKeys:
    def test_single_key(self) -> None:
        assert providers.env_keys("anthropic") == ["ANTHROPIC_API_KEY"]
        assert providers.env_keys("huggingface") == ["HF_TOKEN"]

    def test_multi_key(self) -> None:
        assert providers.env_keys("bedrock") == [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
        ]

    def test_oauth_sentinel_empty(self) -> None:
        assert providers.env_keys("copilot") == []

    def test_unknown_canonical_empty(self) -> None:
        assert providers.env_keys("not-a-provider") == []


class TestModelProvider:
    def test_simple_prefixed(self) -> None:
        assert providers.model_provider("anthropic/claude-sonnet-5.5") == "anthropic"
        assert providers.model_provider("openai/gpt-5.6-luna") == "openai"

    def test_variant_prefix_normalized(self) -> None:
        assert (
            providers.model_provider(
                "fireworks-ai/accounts/fireworks/models/deepseek-v4-flash"
            )
            == "fireworks"
        )
        assert providers.model_provider("opencode_go/beam-free") == "opencode"
        assert providers.model_provider("vertexai/claude-opus-4-6") == "vertex_ai"
        assert providers.model_provider("bakeryhub/deepseek-v4-flash") == "opencode"
        assert providers.model_provider("zai_coding_plan/glm-5.3") == "zai"

    def test_colon_suffix_on_last_segment_ignored(self) -> None:
        assert (
            providers.model_provider(
                "openrouter/baidu/ernie-5.5-flash-preview:free"
            )
            == "openrouter"
        )
        assert (
            providers.model_provider("huggingface/openai/gpt-oss-120b:cerebras")
            == "huggingface"
        )

    def test_colon_suffix_on_first_segment_stripped(self) -> None:
        assert providers.model_provider("openrouter:free/foo") == "openrouter"

    def test_bare_name_returns_none(self) -> None:
        assert providers.model_provider("claude-opus-4-6") is None
        assert providers.model_provider("gpt-5.6-luna") is None
        assert providers.model_provider("auto") is None

    def test_dotted_bedrock_style_name_returns_none(self) -> None:
        # No '/', so no provider decision even though it looks prefixed.
        assert providers.model_provider("anthropic.claude-opus-4-20250514-v1:0") is None

    def test_empty_string_returns_none(self) -> None:
        assert providers.model_provider("") is None


class TestCatalogCoverage:
    def test_all_catalog_prefixes_are_mapped(self) -> None:
        """Zero current prefixes may be unmapped."""
        catalog = json.loads(CATALOG_PATH.read_text(encoding="utf-8"))
        canonical = providers.canonical_map()
        unmapped: set[str] = set()
        for info in catalog["agents"].values():
            for model in info["models"]:
                if "/" in model:
                    prefix = model.split("/", 1)[0].split(":", 1)[0].lower()
                    if prefix not in canonical:
                        unmapped.add(prefix)
        assert unmapped == set()

    def test_yaml_matches_embedded_maps(self) -> None:
        """providers.yaml and the embedded fallback must never drift."""
        yaml = pytest.importorskip("yaml")
        if not hasattr(yaml, "safe_load"):
            pytest.skip("PyYAML is broken (no safe_load)")
        data = yaml.safe_load(PROVIDERS_YAML.read_text(encoding="utf-8"))
        assert data["canonical"] == providers._EMBEDDED_CANONICAL
        # YAML stores scalar env keys; the embedded maps store normalized lists.
        normalized_env = {
            key: ([value] if isinstance(value, str) else list(value))
            for key, value in data["env"].items()
        }
        assert normalized_env == providers._EMBEDDED_ENV
        assert data["agents"] == providers._EMBEDDED_AGENTS


class TestAgentResolution:
    def _models(self) -> dict[str, list[str]]:
        catalog = json.loads(CATALOG_PATH.read_text(encoding="utf-8"))
        return {agent: info["models"] for agent, info in catalog["agents"].items()}

    def _provider(self, agent: str, model: str) -> str | None:
        resolved = providers.resolve(agent, model)
        return resolved["provider"] if resolved else None

    def test_every_agent_has_a_rule(self) -> None:
        assert set(providers.agent_rules()) == set(self._models())

    def test_target_providers(self) -> None:
        models = self._models()
        assert providers.providers_for("claude", models["claude"]) == ["anthropic"]
        assert providers.providers_for("codex", models["codex"]) == ["openai"]
        assert providers.providers_for("gemini", models["gemini"]) == ["gemini"]
        assert providers.providers_for("cursor", models["cursor"]) == []
        assert providers.providers_for("goose", models["goose"]) == [
            "anthropic",
            "google",
            "openai",
            "openrouter",
        ]

    def test_every_prefixed_model_resolves(self) -> None:
        for agent, agent_models in self._models().items():
            for model in agent_models:
                if "/" in model:
                    assert providers.resolve(agent, model) is not None, (agent, model)

    def test_provider_credentials_are_accepted(self) -> None:
        models = self._models()
        rules = providers.agent_rules()
        for agent in rules:
            accepted = set(providers.accepted_keys(agent, models[agent]))
            overrides = rules[agent].get("overrides", {})
            for provider in providers.providers_for(agent, models[agent]):
                keys = (
                    set(overrides[provider]["keys"])
                    if provider in overrides
                    else set(providers.env_keys(provider))
                )
                assert keys <= accepted, (agent, provider, keys - accepted)

    def test_goose_collapse_is_case_sensitive(self) -> None:
        assert self._provider("goose", "anthropic/x") == "anthropic"
        assert self._provider("goose", "Anthropic/x") == "openrouter"
        assert self._provider("goose", "zai/glm-5.3") == "openrouter"

    def test_overrides_win_over_base_env(self) -> None:
        assert providers.resolve("aider", "azure/gpt-4o")["keys"] == ["AZURE_API_KEY"]
        assert providers.resolve("crush", "azure/gpt-4o")["keys"] == [
            "AZURE_OPENAI_API_KEY"
        ]
        assert providers.resolve("opencode", "bakeryhub/x")["keys"] == [
            "BAKERYHUB_API_KEY"
        ]
