#!/usr/bin/env python3
"""Validate the models registry catalog (v2 schema + provider consistency).

Usage:
    python scripts/models/validate.py [path]

Default path: ``registries/models/models.json`` (relative to the repo root).

Checks:
  (a) JSON parses; ``version == 2``; every agent has ``models`` + ``providers``
      arrays of strings (schema shape).
  (b) no intra-agent duplicate models.
  (c) every prefixed model's canonical provider is in that agent's ``providers``.
  (d) every provider prefix in the catalog is mapped in ``providers.yaml``.
  (e) every ``providers`` entry is a known canonical id with an ``env`` entry
      (or the ``[]`` sentinel).

Exits non-zero with a report on any violation.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

try:
    from . import providers
except ImportError:  # running as a plain script
    import providers


def _is_string_array(value: object) -> bool:
    return isinstance(value, list) and all(isinstance(x, str) for x in value)


def validate_catalog(
    data: object,
    canonical: dict[str, str],
    env: dict[str, list[str]],
) -> list[str]:
    """Return a list of violation strings (empty when the catalog is valid)."""
    violations: list[str] = []

    if not isinstance(data, dict):
        return ["catalog is not a JSON object"]

    # (a) version + schema shape
    if data.get("version") != 2:
        violations.append(f"version is {data.get('version')!r}, expected 2")

    agents = data.get("agents")
    if not isinstance(agents, dict):
        violations.append("'agents' is missing or not an object")
        return violations

    for agent, info in agents.items():
        if not isinstance(info, dict):
            violations.append(f"agent '{agent}': value is not an object")
            continue
        if "models" not in info:
            violations.append(f"agent '{agent}': missing 'models'")
        elif not _is_string_array(info["models"]):
            violations.append(f"agent '{agent}': 'models' must be an array of strings")
        if "providers" not in info:
            violations.append(f"agent '{agent}': missing 'providers'")
        elif not _is_string_array(info["providers"]):
            violations.append(
                f"agent '{agent}': 'providers' must be an array of strings"
            )

    # (b) no intra-agent duplicate models
    for agent, info in agents.items():
        if not isinstance(info, dict) or not _is_string_array(info.get("models")):
            continue
        seen: set[str] = set()
        for model in info["models"]:
            if model in seen:
                violations.append(f"agent '{agent}': duplicate model '{model}'")
            seen.add(model)

    # (c) every prefixed model must resolve to a provider the agent lists
    for agent, info in agents.items():
        if not isinstance(info, dict) or not _is_string_array(info.get("models")):
            continue
        if not _is_string_array(info.get("providers")):
            continue
        allowed = set(info["providers"])
        for model in info["models"]:
            if "/" not in model:
                continue  # bare model name, no provider constraint
            resolved = providers.resolve(agent, model)
            if resolved is None:
                violations.append(
                    f"agent '{agent}': model '{model}' does not resolve "
                    "to a known provider"
                )
            elif resolved["provider"] and resolved["provider"] not in allowed:
                violations.append(
                    f"agent '{agent}': model '{model}' provider "
                    f"'{resolved['provider']}' not in providers {sorted(allowed)}"
                )

    # (d) every provider prefix in the catalog is mapped in providers.yaml
    for agent, info in agents.items():
        if not isinstance(info, dict) or not _is_string_array(info.get("models")):
            continue
        for model in info["models"]:
            if "/" not in model:
                continue
            prefix = model.split("/", 1)[0]
            normalized = prefix.split(":", 1)[0].lower()
            if normalized not in canonical:
                violations.append(
                    f"agent '{agent}': unmapped provider prefix '{prefix}' "
                    f"(model '{model}')"
                )

    # (e) every providers entry is a known canonical id with an env entry
    for agent, info in agents.items():
        if not isinstance(info, dict) or not _is_string_array(info.get("providers")):
            continue
        for provider in info["providers"]:
            if provider not in env:
                violations.append(
                    f"agent '{agent}': providers entry '{provider}' is not a "
                    "known canonical id with an env entry"
                )

    # (f) every catalog agent has a resolution rule
    rules = providers.agent_rules()
    for agent in agents:
        if agent not in rules:
            violations.append(f"agent '{agent}' has no provider resolution rule")

    return violations


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if args:
        path = Path(args[0])
    else:
        path = (
            Path(__file__).resolve().parents[2]
            / "registries"
            / "models"
            / "models.json"
        )

    try:
        with path.open(encoding="utf-8") as fh:
            data = json.load(fh)
    except FileNotFoundError:
        print(f"ERROR: catalog not found: {path}")
        return 1
    except json.JSONDecodeError as exc:
        print(f"ERROR: {path} is not valid JSON: {exc}")
        return 1

    violations = validate_catalog(data, providers.canonical_map(), providers.env_map())

    if violations:
        print(f"FAIL: {path} has {len(violations)} violation(s):")
        for violation in violations:
            print(f"  - {violation}")
        return 1
    print(f"OK: {path} is a valid v2 catalog")
    return 0


if __name__ == "__main__":
    sys.exit(main())
