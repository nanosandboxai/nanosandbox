"""Provider normalization for the models registry.

Loads the canonical/env maps from ``registries/models/providers.yaml`` when a
working PyYAML is importable; otherwise falls back to the embedded maps below
(identical content, kept in sync by ``test_providers.py``).
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

# Embedded source of truth (fallback when PyYAML is unavailable/broken).
# Mirrors registries/models/providers.yaml exactly.
_EMBEDDED_CANONICAL: dict[str, str] = {
    # Fireworks
    "fireworks-ai": "fireworks",
    "fireworks_ai": "fireworks",
    "fireworks": "fireworks",
    # Vertex AI
    "vertex_ai": "vertex_ai",
    "vertexai": "vertex_ai",
    # OpenCode
    "opencode_go": "opencode",
    "opencode-go": "opencode",
    "opencode": "opencode",
    "opencode_zen": "opencode",
    "opencode-zen": "opencode",
    "bakeryhub": "opencode",
    # Z.ai
    "zai": "zai",
    "zai-org": "zai",
    "zai_coding_plan": "zai",
    # MiniMax
    "minimax-china": "minimax",
    "minimax": "minimax",
    # Bedrock
    "bedrock-europe": "bedrock",
    "bedrock_converse": "bedrock",
    "bedrock": "bedrock",
    # Zhipu
    "zhipu-coding": "zhipu",
    "zhipu": "zhipu",
    # Hugging Face
    "hf": "huggingface",
    # OpenRouter (routers / gateways)
    "eurouter": "openrouter",
    "vercel_ai_gateway": "openrouter",
    "routerplex": "openrouter",
    "aigateway": "openrouter",
    "aimlapi": "openrouter",
    "trustedrouter": "openrouter",
    "orcarouter": "openrouter",
    "pleumrouter": "openrouter",
    "routstr": "openrouter",
    "ovhcloud": "openrouter",
    "futurmix": "openrouter",
    "meta": "openrouter",
    "openrouter": "openrouter",
    # Identity entries (variant == canonical) for prefixes used in the catalog
    "anthropic": "anthropic",
    "openai": "openai",
    "google": "google",
    "gemini": "gemini",
    "deepseek": "deepseek",
    "groq": "groq",
    "xai": "xai",
    "cerebras": "cerebras",
    "mistral": "mistral",
    "huggingface": "huggingface",
    "azure": "azure",
    "moonshot": "moonshot",
    "aihubmix": "aihubmix",
    "vercel": "vercel",
    "venice": "venice",
    "cortecs": "cortecs",
    "ionet": "ionet",
    "atlascloud": "atlascloud",
    "nebius": "nebius",
    "alibaba": "alibaba",
    "alibaba-us": "alibaba_us",
    "alibaba-singapore": "alibaba_singapore",
    "neuralwatt": "neuralwatt",
    "avian": "avian",
    "chutes": "chutes",
    "baseten": "baseten",
    "qiniucloud": "qiniucloud",
    "synthetic": "synthetic",
    "scaleway": "scaleway",
    "kimi-coding": "kimi_coding",
    "pioneer": "pioneer",
    "pareto": "pareto",
    "coralbricks": "coralbricks",
    "nvidia": "nvidia",
    "perplexity": "perplexity",
    "together": "together",
    "friendli": "friendli",
    "novita": "novita",
    "saladcloud": "saladcloud",
    "sakana": "sakana",
    "inception": "inception",
    "celeris": "celeris",
    "typesafe": "typesafe",
    "tanzu_ai": "tanzu_ai",
    "opper": "opper",
    "saygm": "saygm",
    "nearai": "nearai",
    "iflytek": "iflytek",
    "iflytek_astron": "iflytek_astron",
    "custom_deepseek": "custom_deepseek",
    "custom_tensorix": "custom_tensorix",
    "empiriolabs": "empiriolabs",
    # Additional prefixes found in the current catalog (zero unmapped)
    "cohere_chat": "cohere",
    "copilot": "copilot",
    # Strict provider sets for claude/codex adapters (documented provider surface)
    "foundry": "foundry",
    "mantle": "mantle",
    "custom": "custom",
}

_EMBEDDED_ENV: dict[str, list[str]] = {
    "anthropic": ["ANTHROPIC_API_KEY"],
    "openai": ["OPENAI_API_KEY"],
    "openrouter": ["OPENROUTER_API_KEY"],
    "google": ["GOOGLE_API_KEY"],
    "gemini": ["GEMINI_API_KEY"],
    "deepseek": ["DEEPSEEK_API_KEY"],
    "groq": ["GROQ_API_KEY"],
    "xai": ["XAI_API_KEY"],
    "cerebras": ["CEREBRAS_API_KEY"],
    "mistral": ["MISTRAL_API_KEY"],
    "huggingface": ["HF_TOKEN"],
    "fireworks": ["FIREWORKS_API_KEY"],
    "azure": ["AZURE_OPENAI_API_KEY"],
    "bedrock": ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"],
    "vertex_ai": ["GOOGLE_APPLICATION_CREDENTIALS"],
    "opencode": ["OPENCODE_API_KEY"],
    "zai": ["ZAI_API_KEY"],
    "zhipu": ["ZHIPU_API_KEY"],
    "minimax": ["MINIMAX_API_KEY"],
    "moonshot": ["MOONSHOT_API_KEY"],
    "aihubmix": ["AIHUBMIX_API_KEY"],
    "venice": ["VENICE_API_KEY"],
    "vercel": ["VERCEL_API_KEY"],
    "cortecs": ["CORTECS_API_KEY"],
    "ionet": ["IONET_API_KEY"],
    "atlascloud": ["ATLASCLOUD_API_KEY"],
    "nebius": ["NEBIUS_API_KEY"],
    "scaleway": ["SCW_SECRET_KEY"],
    "baseten": ["BASETEN_API_KEY"],
    "chutes": ["CHUTES_API_KEY"],
    "avian": ["AVIAN_API_KEY"],
    "neuralwatt": ["NEURALWATT_API_KEY"],
    "qiniucloud": ["QINIUCLOUD_API_KEY"],
    "synthetic": ["SYNTHETIC_API_KEY"],
    "kimi_coding": ["KIMI_CODING_API_KEY"],
    "pioneer": ["PIONEER_API_KEY"],
    "pareto": ["PARETO_API_KEY"],
    "coralbricks": ["CORALBRICKS_API_KEY"],
    "nvidia": ["NVIDIA_API_KEY"],
    "perplexity": ["PERPLEXITY_API_KEY"],
    "together": ["TOGETHER_API_KEY"],
    "friendli": ["FRIENDLI_TOKEN"],
    "novita": ["NOVITA_API_KEY"],
    "saladcloud": ["SALAD_API_KEY"],
    "sakana": ["SAKANA_API_KEY"],
    "inception": ["INCEPTION_API_KEY"],
    "celeris": ["CELERIS_API_KEY"],
    "typesafe": ["TYPESAFE_API_KEY"],
    "tanzu_ai": ["TANZU_API_KEY"],
    "opper": ["OPPER_API_KEY"],
    "saygm": ["SAYGM_API_KEY"],
    "nearai": ["NEARAI_API_KEY"],
    "iflytek": ["IFLYTEK_API_KEY"],
    "iflytek_astron": ["IFLYTEK_ASTRON_API_KEY"],
    "custom_deepseek": ["CUSTOM_DEEPSEEK_API_KEY"],
    "custom_tensorix": ["CUSTOM_TENSORIX_API_KEY"],
    "empiriolabs": ["EMPIRIOLABS_API_KEY"],
    "alibaba": ["ALIBABA_API_KEY"],
    "alibaba_singapore": ["ALIBABA_SINGAPORE_API_KEY"],
    "alibaba_us": ["ALIBABA_US_API_KEY"],
    "cohere": ["COHERE_API_KEY"],
    "foundry": ["FOUNDRY_API_KEY"],
    "mantle": ["MANTLE_API_KEY"],
    "custom": ["CUSTOM_API_KEY"],
    "cursor": ["CURSOR_API_KEY"],
    # OAuth-authenticated; no env var (sentinel empty list)
    "copilot": [],
}

# Mirrors the `agents:` block of registries/models/providers.yaml exactly.
_EMBEDDED_AGENTS: dict[str, dict[str, Any]] = {
    "claude": {
        "mode": "single",
        "default": "anthropic",
        "alt": ["CLAUDE_CODE_OAUTH_TOKEN"],
        "providers": ["anthropic"],
    },
    "codex": {
        "mode": "single",
        "default": "openai",
        "alt": ["CODEX_API_KEY"],
        "providers": ["openai"],
    },
    "goose": {
        "mode": "collapse",
        "case_sensitive": True,
        "collapse": {
            "anthropic": "anthropic",
            "openai": "openai",
            "google": "google",
            "gemini": "google",
            "openrouter": "openrouter",
        },
        "collapse_default": "openrouter",
        "providers": ["anthropic", "openai", "google", "openrouter"],
    },
    "cursor": {
        "mode": "single",
        "default": "cursor",
        "providers": [],
    },
    "opencode": {
        "mode": "derived",
        "overrides": {"bakeryhub": {"keys": ["BAKERYHUB_API_KEY"]}},
        "extra_keys": ["GOOGLE_API_KEY"],
    },
    "aider": {
        "mode": "derived",
        "overrides": {"azure": {"keys": ["AZURE_API_KEY"]}},
    },
    "gemini": {
        "mode": "single",
        "default": "gemini",
        "alt": ["GOOGLE_API_KEY"],
        "providers": ["gemini"],
    },
    "crush": {
        "mode": "derived",
        "overrides": {"azure": {"keys": ["AZURE_OPENAI_API_KEY"]}},
        "no_env": ["copilot"],
        "extra_keys": ["GOOGLE_API_KEY"],
    },
    "oh-my-openagent": {
        "mode": "derived",
    },
    "oh-my-codex": {
        "mode": "single",
        "default": "openai",
        "alt": ["CODEX_API_KEY"],
        "providers": ["openai"],
    },
    "oh-my-claude": {
        "mode": "single",
        "default": "anthropic",
        "alt": ["CLAUDE_CODE_OAUTH_TOKEN"],
        "providers": ["anthropic"],
    },
}


def _load_yaml_maps() -> tuple[dict[str, str], dict[str, Any], dict[str, Any]] | None:
    """Load canonical/env maps from providers.yaml via PyYAML.

    Returns ``None`` when PyYAML is missing or broken (e.g. an empty namespace
    package shadowing the real module), so callers fall back to the embedded
    maps.
    """
    try:
        import yaml  # type: ignore[import-not-found]

        if not hasattr(yaml, "safe_load"):
            return None
        path = (
            Path(__file__).resolve().parents[2]
            / "registries"
            / "models"
            / "providers.yaml"
        )
        with path.open(encoding="utf-8") as fh:
            data = yaml.safe_load(fh)
        if not isinstance(data, dict):
            return None
        canonical = data.get("canonical")
        env = data.get("env")
        agents = data.get("agents")
        if (
            not isinstance(canonical, dict)
            or not isinstance(env, dict)
            or not isinstance(agents, dict)
        ):
            return None
        return canonical, env, agents
    except Exception:
        return None


_yaml_maps = _load_yaml_maps()
if _yaml_maps is not None:
    _CANONICAL, _ENV_RAW, _AGENTS = _yaml_maps
else:
    _CANONICAL, _ENV_RAW, _AGENTS = (
        _EMBEDDED_CANONICAL,
        _EMBEDDED_ENV,
        _EMBEDDED_AGENTS,
    )

_ENV: dict[str, list[str]] = {
    key: ([value] if isinstance(value, str) else list(value))
    for key, value in _ENV_RAW.items()
}


def canonical_map() -> dict[str, str]:
    """Return the variant-prefix -> canonical-id map."""
    return dict(_CANONICAL)


def env_map() -> dict[str, list[str]]:
    """Return the canonical-id -> env-key-list map (strings normalized to lists)."""
    return {key: list(value) for key, value in _ENV.items()}


def _normalize_prefix(prefix: str) -> str:
    """Lowercase a prefix and strip any ``:*`` suffix (e.g. ``OpenRouter:free``)."""
    return prefix.split(":", 1)[0].lower()


def canonical_provider(prefix: str) -> str:
    """Map a provider prefix to its canonical id.

    The prefix is lowercased and stripped of any ``:*`` suffix before lookup.
    Unmapped prefixes fall back to the normalized prefix itself (identity), so
    callers can detect them via :func:`canonical_map` membership.
    """
    normalized = _normalize_prefix(prefix)
    return _CANONICAL.get(normalized, normalized)


def env_keys(canonical: str) -> list[str]:
    """Return the env var keys for a canonical provider id (``[]`` if none)."""
    return list(_ENV.get(canonical, []))


def model_provider(model: str) -> str | None:
    """Return the canonical provider for a model string.

    The provider is the first ``/``-segment of the model, lowercased, stripped
    of any ``:*`` suffix, and canonicalized. Returns ``None`` for bare model
    names (no ``/``). The ``:*`` suffix is stripped only for the provider
    decision; the model string itself is never modified.
    """
    if "/" not in model:
        return None
    prefix = model.split("/", 1)[0]
    return canonical_provider(prefix)


def agent_rules() -> dict[str, dict[str, Any]]:
    """Return a shallow copy of the per-agent resolution rules."""
    return {agent: dict(rule) for agent, rule in _AGENTS.items()}


def _provider_credential(provider: str, alt: list[str] | None = None) -> dict[str, Any]:
    keys = env_keys(provider)
    return {
        "provider": provider,
        "keys": keys + list(alt or []),
        "oauth": provider in _ENV and not _ENV[provider],
    }


def _override_credential(spec: dict[str, Any]) -> dict[str, Any]:
    return {
        "provider": None,
        "keys": list(spec.get("keys", [])),
        "oauth": bool(spec.get("oauth", False)),
    }


def resolve(agent: str, model: str) -> dict[str, Any] | None:
    """Resolve an (agent, model) pair to ``{provider, keys, oauth}``.

    Returns ``None`` when the model's provider is not resolvable for that agent.
    """
    rule = _AGENTS.get(agent)
    if rule is None:
        return None

    mode = rule.get("mode")
    alt = list(rule.get("alt", []))
    seg = model.split("/", 1)[0] if "/" in model else ""

    if seg == "":
        default = rule.get("default")
        if default is None and mode == "collapse":
            default = rule.get("collapse_default")
        if default is None:
            return {"provider": None, "keys": list(alt), "oauth": False}
        return _provider_credential(default, alt)

    if mode == "collapse":
        collapse = rule.get("collapse", {})
        key = seg if rule.get("case_sensitive") else seg.lower()
        provider = collapse.get(key) or rule.get("collapse_default")
        if provider is None:
            return None
        return _provider_credential(provider)

    canon = canonical_provider(seg)
    overrides = rule.get("overrides", {})
    seg_key = seg if rule.get("case_sensitive") else seg.lower()
    if seg_key in overrides:
        return _override_credential(overrides[seg_key])
    if canon in overrides:
        return _override_credential(overrides[canon])
    if canon in rule.get("no_env", []):
        return {"provider": canon, "keys": [], "oauth": True}
    if mode == "derived" or canon in rule.get("providers", []):
        if canon in _ENV or canon in rule.get("no_env", []):
            return _provider_credential(canon)
        return None
    return None


def providers_for(agent: str, models: list[str]) -> list[str]:
    """Return the credential-provider codomain for an agent."""
    rule = _AGENTS.get(agent)
    if rule is None:
        return []
    if rule.get("mode") in ("single", "collapse"):
        return sorted(set(rule.get("providers", [])))
    providers: set[str] = set()
    for model in models:
        if "/" not in model:
            continue
        providers.add(canonical_provider(model.split("/", 1)[0]))
    return sorted(providers)


def accepted_keys(agent: str, models: list[str]) -> list[str]:
    """Return every env key the agent can be credentialed with."""
    rule = _AGENTS.get(agent)
    if rule is None:
        return []
    overrides = rule.get("overrides", {})
    keys: set[str] = set()
    for provider in providers_for(agent, models):
        if provider in overrides:
            keys.update(overrides[provider].get("keys", []))
        else:
            keys.update(env_keys(provider))
    for spec in overrides.values():
        keys.update(spec.get("keys", []))
    if rule.get("default"):
        keys.update(env_keys(rule["default"]))
    keys.update(rule.get("alt", []))
    keys.update(rule.get("extra_keys", []))
    return sorted(keys)


def build_provider_catalog(models_by_agent: dict[str, list[str]]) -> dict[str, Any]:
    """Compile the provider catalog consumed by the Rust and web resolvers."""
    agents: dict[str, Any] = {}
    for agent, rule in _AGENTS.items():
        models = list(models_by_agent.get(agent, []))
        agents[agent] = {
            "mode": rule["mode"],
            "case_sensitive": bool(rule.get("case_sensitive", False)),
            "default": rule.get("default"),
            "alt": sorted(rule.get("alt", [])),
            "collapse": dict(rule.get("collapse", {})),
            "collapse_default": rule.get("collapse_default"),
            "overrides": {
                key: sorted(spec.get("keys", []))
                for key, spec in rule.get("overrides", {}).items()
            },
            "no_env": sorted(rule.get("no_env", [])),
            "extra_keys": sorted(rule.get("extra_keys", [])),
            "providers": providers_for(agent, models),
            "accepted_keys": accepted_keys(agent, models),
        }
    return {
        "version": 1,
        "canonical": canonical_map(),
        "credentials": {key: list(value) for key, value in sorted(_ENV.items())},
        "agents": agents,
    }
