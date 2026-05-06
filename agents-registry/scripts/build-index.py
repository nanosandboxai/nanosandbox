#!/usr/bin/env python3
"""Generate index.json from agent definitions, skills, and MCP sources."""

import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path


def parse_yaml_frontmatter(content: str) -> dict:
    """Extract YAML frontmatter from markdown content.

    Simple parser that handles key: value and key: [list] syntax.
    Avoids requiring PyYAML dependency.
    Note: Does not support multiline YAML values (| or >).
    """
    match = re.match(r"^---\s*\n(.*?)\n---", content, re.DOTALL)
    if not match:
        return {}
    frontmatter = {}
    for line in match.group(1).strip().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        key = key.strip()
        value = value.strip()
        # Handle YAML lists [a, b, c]
        if value.startswith("[") and value.endswith("]"):
            items = [item.strip().strip("\"'") for item in value[1:-1].split(",")]
            frontmatter[key] = [i for i in items if i]
        # Handle quoted strings
        elif value.startswith('"') and value.endswith('"'):
            frontmatter[key] = value[1:-1]
        elif value.startswith("'") and value.endswith("'"):
            frontmatter[key] = value[1:-1]
        # Handle bare booleans
        elif value == "true":
            frontmatter[key] = True
        elif value == "false":
            frontmatter[key] = False
        else:
            frontmatter[key] = value
    return frontmatter


def parse_yaml_simple(content: str) -> dict:
    """Simple YAML parser for agent definition files.

    Handles the specific structure of agent YAML files without
    requiring PyYAML. Supports nested objects, arrays, and multiline strings.
    """
    result = {}
    lines = content.splitlines()
    i = 0

    def parse_value(val: str):
        val = val.strip()
        if val.startswith("[") and val.endswith("]"):
            return [item.strip().strip("\"'") for item in val[1:-1].split(",") if item.strip()]
        if val.startswith('"') and val.endswith('"'):
            return val[1:-1]
        if val.startswith("'") and val.endswith("'"):
            return val[1:-1]
        if val == "true":
            return True
        if val == "false":
            return False
        return val

    # Extract just metadata and top-level spec fields we need
    metadata = {}
    spec_skills = []
    spec_mcps = []

    in_metadata = False
    in_spec = False
    in_skills = False
    in_mcps = False
    in_mcp_item = False
    current_mcp = {}

    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue

        indent = len(line) - len(line.lstrip())

        if stripped.startswith("metadata:"):
            in_metadata = True
            in_spec = False
            continue
        if stripped.startswith("spec:"):
            in_spec = True
            in_metadata = False
            continue
        if stripped.startswith("apiVersion:") or stripped.startswith("kind:"):
            in_metadata = False
            in_spec = False
            continue

        if in_metadata and indent >= 2:
            if ":" in stripped:
                key, value = stripped.split(":", 1)
                key = key.strip()
                value = value.strip()
                if value:
                    metadata[key] = parse_value(value)

        if in_spec:
            if stripped == "skills:":
                in_skills = True
                in_mcps = False
                continue
            if stripped == "mcps:":
                in_mcps = True
                in_skills = False
                if current_mcp:
                    spec_mcps.append(current_mcp)
                    current_mcp = {}
                continue
            if stripped.startswith("prompt:"):
                in_skills = False
                in_mcps = False
                continue

            if in_skills and stripped.startswith("- "):
                spec_skills.append(stripped[2:].strip())

            if in_mcps:
                if stripped.startswith("- name:"):
                    if current_mcp:
                        spec_mcps.append(current_mcp)
                    current_mcp = {"name": stripped.split(":", 1)[1].strip()}
                elif current_mcp and ":" in stripped:
                    key, value = stripped.split(":", 1)
                    key = key.strip()
                    value = value.strip()
                    if key in ("source", "package", "path") and value:
                        current_mcp[key] = value

    if current_mcp:
        spec_mcps.append(current_mcp)

    return {
        "metadata": metadata,
        "skills": spec_skills,
        "mcps": [m.get("name", "") for m in spec_mcps if m.get("name")],
    }


def scan_agents(agents_dir: Path) -> list:
    """Scan agents/ directory and extract metadata from each YAML file."""
    agents = []
    if not agents_dir.exists():
        return agents

    for f in sorted(agents_dir.glob("*.yaml")):
        content = f.read_text()
        parsed = parse_yaml_simple(content)
        meta = parsed["metadata"]
        agents.append({
            "name": meta.get("name", f.stem),
            "description": meta.get("description", ""),
            "tags": meta.get("tags", []),
            "skills": parsed["skills"],
            "mcps": parsed["mcps"],
            "path": f"agents/{f.name}",
        })

    return agents


def scan_skills(skills_dir: Path) -> list:
    """Scan skills/ directory and extract frontmatter from each markdown file."""
    skills = []
    if not skills_dir.exists():
        return skills

    for f in sorted(skills_dir.glob("*.md")):
        content = f.read_text()
        fm = parse_yaml_frontmatter(content)
        skill_entry = {
            "name": fm.get("name", f.stem),
            "description": fm.get("description", ""),
            "tags": fm.get("tags", []),
            "path": f"skills/{f.name}",
        }
        # Include optional enrichment fields when present
        for opt_field in ("when_to_use", "allowed_tools", "user_invocable", "paths"):
            if opt_field in fm:
                skill_entry[opt_field] = fm[opt_field]
        skills.append(skill_entry)

    return skills


def scan_mcps(mcps_dir: Path) -> dict:
    """Scan mcps/ directory for custom MCPs and sources."""
    custom = []
    sources = []

    custom_dir = mcps_dir / "custom"
    if custom_dir.exists():
        for entry in sorted(custom_dir.iterdir()):
            if entry.is_dir() and entry.name != "__pycache__":
                custom.append({
                    "name": entry.name,
                    "path": f"mcps/custom/{entry.name}",
                })

    sources_file = mcps_dir / "sources.yaml"
    if sources_file.exists():
        content = sources_file.read_text()
        # Extract registry names from sources.yaml
        for line in content.splitlines():
            stripped = line.strip()
            if stripped.endswith(":") and not stripped.startswith(("apiVersion", "kind", "registries", "url", "type", "description", "#")):
                sources.append(stripped[:-1])

    return {"custom": custom, "sources": sources}


def scan_models(models_dir: Path):
    """Load the models registry from models/models.json."""
    models_file = models_dir / "models.json"
    if not models_file.exists():
        return None

    content = models_file.read_text()
    try:
        data = json.loads(content)
        return data.get("agents", {})
    except json.JSONDecodeError:
        print(f"Warning: could not parse {models_file}", file=sys.stderr)
        return None


def main():
    repo_root = Path(__file__).parent.parent

    agents = scan_agents(repo_root / "agents")
    skills = scan_skills(repo_root / "skills")
    mcps = scan_mcps(repo_root / "mcps")
    models = scan_models(repo_root / "models")

    index = {
        "version": "1.0",
        "updated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "registry_url": "https://github.com/devdone-labs/agents-registry",
        "agents": agents,
        "skills": skills,
        "mcps": mcps,
    }

    if models is not None:
        index["models"] = models

    output_path = repo_root / "index.json"
    output_path.write_text(json.dumps(index, indent=2) + "\n")

    models_count = sum(len(m.get("models", [])) for m in models.values()) if models else 0
    print(f"Generated {output_path} with {len(agents)} agents, {len(skills)} skills, {models_count} models")


if __name__ == "__main__":
    main()
