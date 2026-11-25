#!/usr/bin/env python3
"""Regenerate ``registries/models/models.json`` to schema v2.

Usage:
    python3 scripts/models/sync.py [--offline] [--check]

- Assembles ``{version: 2, agents: {<agent>: {models: [...], providers: [...]}}}``
  from the per-agent source adapters (``scripts/models/sources/*``), preserving
  the existing model lists and deriving each agent's canonical ``providers``.
- Runs the ``validate.py`` checks on the result and aborts on any violation.
- Writes the catalog only when it changed and refreshes the per-agent counts
  table in ``registries/README.md`` (best-effort).
- ``--offline`` forces snapshot-only (no network). ``--check`` validates the
  committed catalog for drift without writing (for CI).

Offline-safe and idempotent: running twice with ``--offline`` is a no-op.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

try:
    from . import providers, validate
    from .sources import AGENTS, fetch_agent
except ImportError:  # running as a plain script
    import providers
    import validate
    from sources import AGENTS, fetch_agent

REPO = Path(__file__).resolve().parents[2]
CATALOG = REPO / "registries" / "models" / "models.json"
PROVIDER_CATALOG = REPO / "registries" / "models" / "provider-catalog.json"
README = REPO / "registries" / "README.md"

# Agents whose model list is delegated to a sibling agent's catalog. AGENTS
# order puts each delegating agent after its sibling, so the sibling is already
# in ``agents`` when the delegation runs.
DELEGATE_MODELS = {"oh-my-codex": "codex", "oh-my-claude": "claude"}


def build(offline: bool) -> tuple[dict, dict[str, str]]:
    """Assemble the v2 catalog from the adapters. Returns (data, notes)."""
    if CATALOG.exists():
        current = json.loads(CATALOG.read_text(encoding="utf-8"))
    else:
        current = {"agents": {}}
    cur_agents = current.get("agents", {})

    agents: dict[str, dict[str, list[str]]] = {}
    notes: dict[str, str] = {}
    for agent in AGENTS:
        cur_models = list((cur_agents.get(agent) or {}).get("models", []))
        result = fetch_agent(agent, cur_models, offline=offline)
        seen: set[str] = set()
        models: list[str] = []
        for model in result["models"]:
            if model not in seen:
                seen.add(model)
                models.append(model)
        if not models and agent in DELEGATE_MODELS and DELEGATE_MODELS[agent] in agents:
            models = list(agents[DELEGATE_MODELS[agent]]["models"])
        agents[agent] = {
            "models": models,
            "providers": providers.providers_for(agent, models),
        }
        notes[agent] = result.get("note", "snapshot")
    return {"version": 2, "agents": agents}, notes


def update_readme_counts(data: dict) -> bool:
    """Refresh ``| <agent> | <count> |`` rows in registries/README.md."""
    if not README.exists():
        return False
    text = README.read_text(encoding="utf-8")
    original = text
    for agent, info in data["agents"].items():
        count = len(info["models"])
        pattern = re.compile(rf"^(\|\s*{re.escape(agent)}\s*\|\s*)\d+", re.MULTILINE)
        text = pattern.sub(rf"\g<1>{count}", text)
    if text != original:
        README.write_text(text, encoding="utf-8")
        return True
    return False


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    offline = "--offline" in args
    check = "--check" in args

    data, notes = build(offline)

    violations = validate.validate_catalog(
        data, providers.canonical_map(), providers.env_map()
    )
    if violations:
        print(f"FAIL: {len(violations)} violation(s):")
        for violation in violations:
            print(f"  - {violation}")
        return 1

    rendered = json.dumps(data, indent=2) + "\n"
    existing = CATALOG.read_text(encoding="utf-8") if CATALOG.exists() else ""

    catalog = providers.build_provider_catalog(
        {agent: data["agents"][agent]["models"] for agent in AGENTS}
    )
    catalog_rendered = json.dumps(catalog, indent=2, sort_keys=True) + "\n"
    existing_catalog = (
        PROVIDER_CATALOG.read_text(encoding="utf-8")
        if PROVIDER_CATALOG.exists()
        else ""
    )

    changed = rendered != existing or catalog_rendered != existing_catalog

    if check:
        if changed:
            print(f"DRIFT: {CATALOG} or {PROVIDER_CATALOG} would change")
            return 1
        print(f"OK: {CATALOG} and {PROVIDER_CATALOG} are up to date")
        return 0

    if rendered != existing:
        CATALOG.write_text(rendered, encoding="utf-8")
        readme = update_readme_counts(data)
        print(f"wrote {CATALOG}" + (" (+ README counts)" if readme else ""))
    else:
        print(f"unchanged {CATALOG}")

    if catalog_rendered != existing_catalog:
        PROVIDER_CATALOG.write_text(catalog_rendered, encoding="utf-8")
        print(f"wrote {PROVIDER_CATALOG}")
    else:
        print(f"unchanged {PROVIDER_CATALOG}")

    counts = ", ".join(f"{a}:{len(data['agents'][a]['models'])}" for a in AGENTS)
    print(f"models: {counts}")
    print(f"notes: {notes}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
