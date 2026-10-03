#!/usr/bin/env python3
"""Sync model lists from OpenRouter into models.json.

Fetches the public (no-auth) OpenRouter model catalog and extracts models
by provider, then maps them to each code agent type.

Cursor uses its own model naming convention that differs from provider IDs,
so cursor models are maintained manually and preserved as-is.
"""

import json
import re
import sys
from pathlib import Path

MODELS_PATH = Path(__file__).resolve().parent.parent / "models" / "models.json"
SUMMARY_PATH = Path("/tmp/models-update-summary.md")
OPENROUTER_FILE = "/tmp/openrouter-models.json"

# ---------------------------------------------------------------------------
# OpenRouter provider prefixes → our provider keys
# ---------------------------------------------------------------------------
PROVIDER_MAP = {
    "anthropic": "anthropic",
    "openai": "openai",
    "google": "gemini",
}

# ---------------------------------------------------------------------------
# Patterns for filtering relevant models per provider
# ---------------------------------------------------------------------------
ANTHROPIC_PATTERN = re.compile(r"^claude-")
OPENAI_PATTERN = re.compile(r"^(gpt-|o[0-9]|codex-)")
GEMINI_PATTERN = re.compile(r"^gemini-")

OPENAI_EXCLUDE = re.compile(
    r"(embedding|tts|whisper|dall-e|davinci|babbage|realtime|audio|search|moderation|instruct)"
)
GEMINI_EXCLUDE = re.compile(
    r"(embedding|aqa|imagen|veo|chirp|thinking-exp)"
)

# ---------------------------------------------------------------------------
# Which providers feed each agent type.
# "cursor" is special — maintained manually, not synced from OpenRouter.
# ---------------------------------------------------------------------------
AGENT_SOURCES = {
    "claude": ["anthropic"],
    "codex": ["openai"],
    "goose": ["anthropic", "openai", "gemini"],
    "cursor": [],
}

SOURCE_PATTERNS = {
    "anthropic": ANTHROPIC_PATTERN,
    "openai": OPENAI_PATTERN,
    "gemini": GEMINI_PATTERN,
}


def is_api_sourced(model, sources):
    """Check if a model matches any provider pattern for the given sources."""
    for src in sources:
        pattern = SOURCE_PATTERNS.get(src)
        if pattern and pattern.match(model):
            return True
    return False


def contributing_source(model, sources):
    """Determine which provider source contributed this model."""
    if len(sources) == 1:
        return sources[0]
    if ANTHROPIC_PATTERN.match(model) and "anthropic" in sources:
        return "anthropic"
    if GEMINI_PATTERN.match(model) and "gemini" in sources:
        return "gemini"
    if OPENAI_PATTERN.match(model) and "openai" in sources:
        return "openai"
    return None


def load_openrouter(path):
    """Load and split OpenRouter response into per-provider model lists.

    OpenRouter returns {"data": [{"id": "anthropic/claude-opus-4-6", ...}, ...]}.
    We strip the provider prefix and group by provider.
    """
    try:
        with open(path) as f:
            data = json.load(f)
    except (FileNotFoundError, json.JSONDecodeError) as e:
        print("Error: could not read %s: %s" % (path, e), file=sys.stderr)
        return None

    if "data" not in data:
        print("Error: unexpected format in %s" % path, file=sys.stderr)
        return None

    providers = {}
    for entry in data["data"]:
        model_id = entry.get("id", "")
        if "/" not in model_id:
            continue
        prefix, name = model_id.split("/", 1)
        provider_key = PROVIDER_MAP.get(prefix)
        if provider_key:
            providers.setdefault(provider_key, []).append(name)

    return providers


def expand_anthropic_aliases(name):
    """Expand an OpenRouter Anthropic name into both OpenRouter and Anthropic API formats.

    OpenRouter uses dots for versions: claude-opus-4.6
    Anthropic API uses dashes:         claude-opus-4-6

    Returns a list containing both formats (deduplicated if they're the same).
    """
    # Strip :thinking suffix (OpenRouter uses claude-3.7-sonnet:thinking)
    clean = name.replace(":thinking", "")

    # Convert dots to dashes in the version part: claude-opus-4.6 -> claude-opus-4-6
    # Pattern: digit.digit -> digit-digit
    api_name = re.sub(r"(\d)\.(\d)", r"\1-\2", clean)

    names = {name}  # OpenRouter format
    if api_name != name:
        names.add(api_name)  # Anthropic API format

    # Also add :thinking variant as -thinking for the API format
    if ":thinking" in name:
        names.add(clean + "-thinking")
        if api_name != clean:
            names.add(api_name + "-thinking")

    return names


def filter_models(provider, models):
    """Apply provider-specific filters and expand aliases."""
    if provider == "anthropic":
        result = set()
        for m in models:
            if ANTHROPIC_PATTERN.match(m):
                result.update(expand_anthropic_aliases(m))
        return sorted(result)
    if provider == "openai":
        return sorted(
            m for m in models
            if OPENAI_PATTERN.match(m) and not OPENAI_EXCLUDE.search(m)
        )
    if provider == "gemini":
        return sorted(
            m for m in models
            if GEMINI_PATTERN.match(m) and not GEMINI_EXCLUDE.search(m)
        )
    return sorted(models)


def main():
    with open(MODELS_PATH) as f:
        config = json.load(f)

    agents = config.get("agents", {})

    # Load OpenRouter catalog.
    raw_providers = load_openrouter(OPENROUTER_FILE)
    if raw_providers is None:
        print("OpenRouter data unavailable. No changes made.", file=sys.stderr)
        sys.exit(1)

    # Filter each provider's models.
    provider_models = {}
    for provider, models in raw_providers.items():
        filtered = filter_models(provider, models)
        provider_models[provider] = filtered
        print("%s: %d models" % (provider.capitalize(), len(filtered)))

    changed = False
    summary_lines = []

    for agent_key, sources in AGENT_SOURCES.items():
        existing = set(agents.get(agent_key, {}).get("models", []))

        # Cursor is manually maintained — skip sync.
        if not sources:
            continue

        # Build set of models from providers.
        api_models = set()
        for src in sources:
            api_models.update(provider_models.get(src, []))

        # Separate existing into API-sourced and manual.
        manual_models = set()
        existing_api_models = set()
        for m in existing:
            if is_api_sourced(m, sources):
                existing_api_models.add(m)
            else:
                manual_models.add(m)

        result = set(api_models) | manual_models

        added = result - existing
        removed = existing - result

        if added or removed:
            changed = True
            agent_summary = "**%s**:" % agent_key
            if added:
                agent_summary += " +%d (%s)" % (len(added), ", ".join(sorted(added)))
                print("  %s: +%d added: %s" % (agent_key, len(added), sorted(added)))
            if removed:
                agent_summary += " -%d (%s)" % (len(removed), ", ".join(sorted(removed)))
                print("  %s: -%d removed: %s" % (agent_key, len(removed), sorted(removed)))
            summary_lines.append(agent_summary)

        if agent_key not in agents:
            agents[agent_key] = {}
        agents[agent_key]["models"] = sorted(result)

    config["agents"] = agents

    if changed:
        with open(MODELS_PATH, "w") as f:
            json.dump(config, f, indent=2)
            f.write("\n")
        print("models.json updated.")

        summary = "### Changes\n\n" + "\n".join("- %s" % line for line in summary_lines)
        SUMMARY_PATH.write_text(summary)
    else:
        print("No changes detected. models.json unchanged.")
        SUMMARY_PATH.write_text("No changes detected.")


if __name__ == "__main__":
    main()
