#!/usr/bin/env python3
"""Merge model lists from Anthropic and OpenAI APIs into models.yaml.

Reads API responses from /tmp/anthropic-models.json and /tmp/openai-models.json,
filters relevant models, and merges them into the existing models.yaml while
preserving any manually-added entries.
"""

import json
import re
import sys
from pathlib import Path

import yaml

MODELS_PATH = Path(__file__).resolve().parent.parent / "models.yaml"

# Patterns for filtering relevant models from each API.
ANTHROPIC_PATTERN = re.compile(r"^claude-")
OPENAI_PATTERN = re.compile(r"^(gpt-4|o3|o4)")

# OpenAI model families to exclude (not useful for code agents).
OPENAI_EXCLUDE = re.compile(
    r"(embedding|tts|whisper|dall-e|davinci|babbage|realtime|audio|search|moderation|instruct)"
)

# Which API feeds which agent types.
AGENT_SOURCES = {
    "claude": ["anthropic"],
    "codex": ["openai"],
    "goose": ["anthropic", "openai"],
    "cursor": ["anthropic", "openai"],
}


def load_api_models(path: str) -> list[str]:
    """Load model IDs from an API response JSON file."""
    try:
        with open(path) as f:
            data = json.load(f)
    except (FileNotFoundError, json.JSONDecodeError) as e:
        print(f"Warning: could not read {path}: {e}", file=sys.stderr)
        return []

    # Both APIs return {"data": [{"id": "model-name", ...}, ...]}
    if "data" not in data:
        print(f"Warning: unexpected format in {path}", file=sys.stderr)
        return []

    return [m["id"] for m in data["data"] if "id" in m]


def filter_anthropic(models: list[str]) -> list[str]:
    """Keep only Claude models."""
    return sorted(m for m in models if ANTHROPIC_PATTERN.match(m))


def filter_openai(models: list[str]) -> list[str]:
    """Keep GPT-4.x and o3/o4 models, exclude non-code families."""
    return sorted(
        m
        for m in models
        if OPENAI_PATTERN.match(m) and not OPENAI_EXCLUDE.search(m)
    )


def main():
    # Load current models.yaml.
    with open(MODELS_PATH) as f:
        config = yaml.safe_load(f)

    agents = config.get("agents", {})

    # Load and filter API models.
    raw_anthropic = load_api_models("/tmp/anthropic-models.json")
    raw_openai = load_api_models("/tmp/openai-models.json")

    anthropic_models = filter_anthropic(raw_anthropic)
    openai_models = filter_openai(raw_openai)

    print(f"Anthropic models found: {len(anthropic_models)}")
    print(f"OpenAI models found: {len(openai_models)}")

    changed = False

    for agent_key, sources in AGENT_SOURCES.items():
        existing = set(agents.get(agent_key, {}).get("models", []))
        new_models = set()

        if "anthropic" in sources:
            new_models.update(anthropic_models)
        if "openai" in sources:
            new_models.update(openai_models)

        # Merge: keep existing manual entries, add new API-discovered ones.
        merged = existing | new_models
        if merged != existing:
            changed = True
            added = merged - existing
            print(f"  {agent_key}: +{len(added)} new models: {sorted(added)}")

        if agent_key not in agents:
            agents[agent_key] = {}
        agents[agent_key]["models"] = sorted(merged)

    config["agents"] = agents

    if changed:
        with open(MODELS_PATH, "w") as f:
            yaml.dump(config, f, default_flow_style=False, sort_keys=False)
        print("models.yaml updated.")
    else:
        print("No new models found. models.yaml unchanged.")


if __name__ == "__main__":
    main()
