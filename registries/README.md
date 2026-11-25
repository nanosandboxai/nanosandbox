# Registries

Open-source registry sources and agent VM images for [nanosandbox](https://github.com/nanosandboxai/nanosandbox).

## Overview

This directory is the registry entry point for the nanosandbox runtime and TUI. It holds
the open-source registries that agents, skills, and MCP servers are discovered from — it no
longer carries any hand-maintained agent, skill, or MCP definitions.

- **MCP registry sources** — the public, open-source MCP registries nanosandbox resolves
  servers from (see [`mcps/sources.yaml`](mcps/sources.yaml)).
- **Agent model catalog** — the curated per-agent model lists shown by the docs site's
  agent configurator (see [`models/models.json`](models/models.json)).
- **Agent VM images** — the Dockerfiles that build the per-agent microVM images
  (see [`docker/`](docker)).

Agent personas, skills, and MCP selections are configured per sandbox in `sandbox.yml`; the
runtime resolves them through the open-source registries listed here (and, for skills, the
public skills catalogs).

## MCP Registry Sources

[`mcps/sources.yaml`](mcps/sources.yaml) lists the open-source MCP registries nanosandbox can
resolve servers from:

| Registry | URL | Type |
|----------|-----|------|
| official | https://registry.modelcontextprotocol.io | Canonical MCP registry (Linux Foundation) |
| smithery | https://smithery.ai | Registry + managed hosting |
| glama | https://glama.ai/mcp/servers | MCP directory |
| pulsemcp | https://www.pulsemcp.com/servers | Quality-controlled directory |
| mcp-run | https://www.mcp.run | WebAssembly MCP hosting |
| mcp-get | https://mcp-get.com | MCP package-manager CLI |
| opentools | https://opentools.com/registry | Curated discovery |
| github-mcp | https://github.com/mcp | GitHub's integrated MCP registry |

See [`mcps/sources.yaml`](mcps/sources.yaml) for the full source metadata (API endpoints, auth).

## Agent Model Catalog

[`models/models.json`](models/models.json) is the **committed, CI-refreshed** catalog of the
models each agent type accepts, plus the canonical **providers** each agent can be
credentialed against. Each model list is taken from that agent's **own** source — its CLI
catalog, model picker, or provider database — not a third-party aggregator. The docs site's
agent configurator reads it to populate its model dropdown, and the runtime embeds it.

Schema (v2):

```json
{
  "version": 2,
  "agents": {
    "claude": {
      "models": ["claude-opus-4-1-20250805", "..."],
      "providers": ["anthropic", "bedrock", "foundry", "mantle", "vertex_ai"]
    }
  }
}
```

- `models` — the model IDs the agent accepts. Model strings are **never rewritten**; the
  provider of a model is derived from its `/`-prefix by the consumer.
- `providers` — the **credential providers** the agent can actually be configured with (the
  codomain of the per-agent resolution function). For agents whose models carry prefixes
  (opencode, aider, crush) it is derived from the model list; for the rest it is declared
  (claude → `anthropic`, codex → `openai`, gemini → `gemini`, goose → its four collapse
  targets). Agents that use only their own key (cursor) have an empty list.

Schema: [`models/models.schema.json`](models/models.schema.json).

The per-agent resolution rules (single / collapse / derived, per-agent overrides, OAuth
providers) live in [`models/providers.yaml`](models/providers.yaml) and are compiled into
[`models/provider-catalog.json`](models/provider-catalog.json) — the artifact every consumer
reads (Rust `sandbox::providers`, the docs-site configurator, the Python validator).

| Agent | Models | Providers | Source of truth |
|-------|-------:|----------:|-----------------|
| claude | 44 | 1 | Claude Code `/model` (aliases + full IDs) |
| codex | 8 | 1 | `codex debug models` catalog |
| goose | 455 | 4 | Goose provider definitions + per-provider defaults |
| cursor | 43 | 0 | Cursor docs model list (`cursor-agent --list-models` needs auth; snapshot) |
| opencode | 662 | 5 | `opencode models` catalog (models.dev) |
| aider | 379 | 12 | `aider/models.py` aliases + `model-settings.yml` |
| gemini | 16 | 1 | Gemini CLI `models.ts` (aliases + IDs) |
| crush | 1764 | 39 | Crush's Catwalk provider database |
| oh-my-openagent | 4 | 2 | snapshot |
| oh-my-codex | 8 | 1 | snapshot |
| oh-my-claude | 44 | 1 | snapshot |

Four agents (cursor, goose, opencode, crush) expose their catalogs dynamically at runtime,
so their lists here are point-in-time snapshots rather than fixed allow-lists. The three
`oh-my-*` agents are likewise recorded as point-in-time snapshots.

### Syncing the catalog

The catalog is regenerated from per-agent source adapters under
[`../scripts/models/sources/`](../scripts/models/sources) by
[`../scripts/models/sync.py`](../scripts/models/sync.py):

```bash
python3 scripts/models/sync.py            # refresh from upstream (network)
python3 scripts/models/sync.py --offline  # derive from the committed snapshot only
python3 scripts/models/sync.py --check    # verify the committed file is in sync (CI)
python3 scripts/models/update-models.py   # CI entrypoint (delegates to sync.py)
python3 scripts/models/validate.py        # schema + provider-prefix validation
```

`sync.py` preserves model strings, computes each agent's credential `providers`, validates
the result, writes `models.json` **and** `provider-catalog.json` only when they changed
(`--check` covers both), and refreshes the counts table above; it is idempotent.
[`.github/workflows/update-models.yml`](../.github/workflows/update-models.yml) runs it on a
weekly schedule and opens a PR when the catalog changes, while the `models` job in
[`ci.yml`](../.github/workflows/ci.yml) validates the committed catalog on every push.

## Usage with nanosandbox

Select an MCP server from an open-source registry in your `sandbox.yml`:

```yaml
defaults:
  image: ghcr.io/nanosandboxai/nanosandbox/claude:latest

sandboxes:
  my-env:
    type: claude
    mcp:
      github:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-github"]
        env:
          GITHUB_PERSONAL_ACCESS_TOKEN: "${GITHUB_TOKEN}"
```

Skills are resolved from the public skills catalogs and applied per sandbox:

```yaml
sandboxes:
  my-env:
    type: claude
    skills: [git-workflow, tdd]
```

## Docker Images

This directory also contains the Docker images for running agents inside nanosandbox VMs.

### Image Architecture

```
Dockerfile.base (node:26-slim + minimal system deps + python3/uv)
    |
    ├── Dockerfile.claude   → ghcr.io/nanosandboxai/nanosandbox/claude
    ├── Dockerfile.codex    → ghcr.io/nanosandboxai/nanosandbox/codex
    ├── Dockerfile.goose    → ghcr.io/nanosandboxai/nanosandbox/goose
    ├── Dockerfile.cursor   → ghcr.io/nanosandboxai/nanosandbox/cursor
    ├── Dockerfile.opencode → ghcr.io/nanosandboxai/nanosandbox/opencode
    ├── Dockerfile.aider    → ghcr.io/nanosandboxai/nanosandbox/aider
    ├── Dockerfile.gemini   → ghcr.io/nanosandboxai/nanosandbox/gemini
    ├── Dockerfile.crush    → ghcr.io/nanosandboxai/nanosandbox/crush
    ├── Dockerfile.oh-my-openagent → ghcr.io/nanosandboxai/nanosandbox/oh-my-openagent
    ├── Dockerfile.oh-my-codex     → ghcr.io/nanosandboxai/nanosandbox/oh-my-codex
    └── Dockerfile.oh-my-claude    → ghcr.io/nanosandboxai/nanosandbox/oh-my-claude
```

### Base Image Contents

- `node:26-slim` with minimal system deps (curl, ca-certificates, git, bash)
- Python 3 + uv/uvx (runtime for Python-based MCP servers like mcp-server-fetch)
- Pure installation: no user/group, no `/workspace`, no `CMD`/`ENTRYPOINT`
- No nanosb runtime code: sandbox setup happens at the microVM layer

### Available Images

| Image | Agent | Image reference |
|-------|-------|-----------------|
| claude | Claude Code | `ghcr.io/nanosandboxai/nanosandbox/claude:latest` |
| codex | OpenAI Codex | `ghcr.io/nanosandboxai/nanosandbox/codex:latest` |
| goose | Goose | `ghcr.io/nanosandboxai/nanosandbox/goose:latest` |
| cursor | Cursor Agent | `ghcr.io/nanosandboxai/nanosandbox/cursor:latest` |
| opencode | OpenCode | `ghcr.io/nanosandboxai/nanosandbox/opencode:latest` |
| aider | Aider | `ghcr.io/nanosandboxai/nanosandbox/aider:latest` |
| gemini | Gemini CLI | `ghcr.io/nanosandboxai/nanosandbox/gemini:latest` |
| crush | Crush | `ghcr.io/nanosandboxai/nanosandbox/crush:latest` |
| oh-my-openagent | Oh-My-OpenAgent (OmO) | `ghcr.io/nanosandboxai/nanosandbox/oh-my-openagent:latest` |
| oh-my-codex | Oh-My-Codex (OMX) | `ghcr.io/nanosandboxai/nanosandbox/oh-my-codex:latest` |
| oh-my-claude | Oh-My-Claude (OMC) | `ghcr.io/nanosandboxai/nanosandbox/oh-my-claude:latest` |

`latest` is published by the release pipeline on a clean `v*` tag. Between
releases, CI/`workflow_dispatch` builds publish the semver, `sha-<sha>` and
optional RC tags (for example `:rc2`), so use an explicit tag until the next
release exists.

### Building Locally

```bash
cd docker
docker build -f Dockerfile.base -t base:local .
docker build -f Dockerfile.claude -t claude:local .
```

The `oh-my-*` Dockerfiles (`Dockerfile.oh-my-openagent`,
`Dockerfile.oh-my-codex`, `Dockerfile.oh-my-claude`) are built the same way,
each from `Dockerfile.base`:

```bash
docker build -f Dockerfile.oh-my-openagent -t oh-my-openagent:local .
docker build -f Dockerfile.oh-my-codex -t oh-my-codex:local .
docker build -f Dockerfile.oh-my-claude -t oh-my-claude:local .
```

### CI build and registry publishing

[`.github/workflows/registry-docker-build.yml`](../.github/workflows/registry-docker-build.yml)
builds and publishes every image to GHCR:

- **Triggers** — pushes to `main` that touch `registries/docker/**`, `v*` tags, and
  manual `workflow_dispatch` (optionally a single `agent`).
- **Order** — `base` first, then all eleven agent images in parallel, for
  `linux/amd64` and `linux/arm64`.
- **Target** — `ghcr.io/nanosandboxai/nanosandbox/<agent>`. `latest` is pushed only
  on a clean `v*` tag; other runs publish the semver/sha tags.
- **Registry-side verification** — each agent leg pulls its own just-pushed digest
  back from GHCR and runs `<binary> --version`, so a broken published artifact
  fails the workflow instead of shipping.

To run an agent from the registry instead of a local build:

```bash
nanosb pull ghcr.io/nanosandboxai/nanosandbox/oh-my-codex:latest
nanosb run ghcr.io/nanosandboxai/nanosandbox/oh-my-codex:latest
```

A bare agent name in `sandbox.yml` (`agent: oh-my-codex`) resolves to the same
`ghcr.io/nanosandboxai/nanosandbox/<name>:latest` reference. The live registry
E2E suite exercises all eleven agents against GHCR:

```bash
NANOSB_E2E_TAG=latest cargo test -p runtime --test agents_e2e_test \
  --features integration-tests -- --ignored --nocapture
```

### Version updates (Dependabot)

The Dockerfiles install the agent CLIs and pin their versions. For npm-installed
CLIs the canonical pin lives in [`docker/package.json`](docker/package.json), and
the build workflow
([`../.github/workflows/registry-docker-build.yml`](../.github/workflows/registry-docker-build.yml))
reads those versions and passes them as build-args (`CLAUDE_VERSION`,
`CODEX_VERSION`, `GEMINI_VERSION`, `CRUSH_VERSION`, `OMO_VERSION`, `OMX_VERSION`,
`OMC_VERSION`). A break in any link of the pin → manifest → build-arg chain means
a Dependabot bump never reaches the image.

[`../.github/dependabot.yml`](../.github/dependabot.yml) runs the npm ecosystem
weekly on `/registries/docker` against `docker/package.json`, with
`open-pull-requests-limit: 10` (the default of 5 can starve the queue when
several agent bumps are in flight). Dependabot's npm ecosystem reads only
`package.json`; it does not see `npm install -g` inside a Dockerfile. The docker
ecosystem only parses literal `FROM image:tag` lines (it cannot resolve
`FROM ${BASE_IMAGE}` and ignores `:latest`), so the shared base is tracked
through [`docker/Dockerfile.base`](docker/Dockerfile.base)'s `FROM node:26-slim`.

| Image | Version source | Version updates |
|-------|----------------|-----------------|
| claude, codex, gemini, crush, oh-my-openagent, oh-my-codex, oh-my-claude | `docker/package.json` (npm) | Dependabot (npm ecosystem, weekly) |
| aider | PyPI `aider-chat`, installed with `uv tool` | Manual |
| goose | GitHub release tarball | Manual |
| opencode | curl installer with `--version` | Manual |
| cursor | curl installer, unpinned | Manual |

[`../scripts/check-agent-image-deps.sh`](../scripts/check-agent-image-deps.sh),
run in CI by the `agent-image-deps` job, enforces the contract: every npm package
installed by a Dockerfile must be a `package.json` dependency, every dependency
must be installed by a Dockerfile, each `ARG *_VERSION` default must equal the
manifest value, and every such ARG must be passed as a build-arg. Images with no
`npm install -g` are skipped automatically, which is how the four manual images
are excluded.

**Adding a new npm-installed agent image.**

1. Add the package to [`docker/package.json`](docker/package.json).
2. Declare `ARG <NAME>_VERSION=<same version>` and install with
   `npm install -g "<pkg>@${<NAME>_VERSION}"` in the Dockerfile.
3. Add the version to the `node -p` resolution step and the `build-args:` block
   in [`registry-docker-build.yml`](../.github/workflows/registry-docker-build.yml).
   The build-arg is named `<NAME>_VERSION` and must be wired to
   `${{ steps.versions.outputs.<name> }}`, where `<name>` is the variable name
   without `_VERSION`, lowercased.
4. Run `bash scripts/check-agent-image-deps.sh` and require exit 0.

### Attribution & license risk

The `oh-my-openagent` image installs and runs
[oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent), which is
**not** distributed under an OSI-approved license. Upstream is available under
the **Sustainable Use License, Version 1.0 (SUL-1.0)**, a non-commercial,
source-available license: you may use or modify it for your own internal
business purposes or for non-commercial or personal use, and may distribute it
only free of charge for non-commercial purposes. See
[`LICENSES/SUL-1.0-NOTICE.md`](../LICENSES/SUL-1.0-NOTICE.md) for the full
attribution and risk note. **Evaluate or obtain a commercial license from the
upstream authors before any commercial redistribution** of this image. The
sibling `oh-my-codex` and `oh-my-claude` images are MIT-licensed.

## Contributing

1. Add or update open-source MCP registry entries in [`mcps/sources.yaml`](mcps/sources.yaml),
   the agent model catalog in [`models/models.json`](models/models.json), or agent image
   definitions under [`docker/`](docker).
2. Submit a pull request.
