# Spikes — oh-my-openagent / oh-my-codex / oh-my-claude images

Status: complete (decision record for [OMY-1](https://github.com/nanosandboxai/nanosandbox/issues/239), epic #238).
Date: 2026-11-20.

Probes ran in Docker on macOS (arm64 host), using `node:22-slim` and `node:24-slim`
and `--platform linux/arm64` for the multi-arch check. Every result below is an
observed command output, not an assumption.

## Versions probed

| Agent | Package | Version |
|---|---|---|
| oh-my-openagent (OmO) | `omo-ai` | 5.1.79 (`engine: senpi 2026.11.18-3`) |
| oh-my-codex (OMX) | `oh-my-codex` + `@openai/codex` | 0.22.11 (`oh-my-codex v0.22.11`) + 0.166.0 (`codex-cli 0.166.0`) |
| oh-my-claude (OMC) | `oh-my-claude-sisyphus` + `@anthropic-ai/claude-code` | 5.8.4 + 2.1.318 |

## Probe A — install + `--version`

| Package | node:22-slim | node:24-slim | linux/arm64 (node 24) |
|---|---|---|---|
| `omo-ai@5.1.79` | installs; `omo --help` works | installs; `omo 5.1.79 (engine: senpi 2026.11.18-3)` | installs; same |
| `oh-my-codex@0.22.11` (+ `@openai/codex@0.166.0`) | installs; `codex --version` = `codex-cli 0.166.0` | installs; `oh-my-codex v0.22.11` | installs; same |
| `oh-my-claude-sisyphus@5.8.4` (+ `@anthropic-ai/claude-code@2.1.318`) | installs; `omc` 5.8.4; `claude` 2.1.318 | installs; same | installs; same |

`omo-ai` declares `engines.node >=24` but npm only warns on node 22 and the binary
runs normally, so the shared `node:22-slim` base can stay unchanged (no rebuild of
the eight existing images).

## Probe B — headless behavior

Commands run with a throwaway `HOME` and `< /dev/null`.

| Agent | Command | Observed |
|---|---|---|
| OmO | `omo --model openai/gpt-4o -p --no-session "say hi"` | with `OPENAI_API_KEY=sk-test-not-real`: proceeds to the provider and returns `OpenAI API error (401): {"message":"Incorrect API key provided ..."}` — **the env key is honored**. Without a key: `No API key found for the selected model` and exit 1. |
| OMX | `omx exec --skip-git-repo-check -C /tmp "say hi"` | prints `Reading additional input from stdin...` then starts `OpenAI Codex v0.166.0`; exit 1 without a key. **stdin is read**, so the runtime's pipe must be redirected. |
| OMC | `omc ask claude "say hi"` | writes `~/.omc/artifacts/ask/claude-say-hi-<ts>.md`; exit 1 without a key. |

### Flags confirmed from `--help`

- **OmO**: `-p/--print`, `--no-session`, `--model <pattern>`, `--provider`,
  `--permission <tool=action>`, `--permission-preset {full-access, workspace, accept-edits, auto, read-only, ask}`, `--headless` (RPC host, not needed).
- **OMX**: `omx exec` proxies `codex exec` and accepts `-m/--model`, `--json`,
  `--oss`, plus the codex approval flags; `--yolo` and `--direct` exist at the
  `omx` command level.
- **OMC**: `omc ask <claude|codex|gemini|...> [-p|--print|--prompt] <prompt>`;
  `omc launch` is the interactive entrypoint; no `--model` flag on `ask`.

### Missing tools found

- `spawnSync ps ENOENT` from both OMX and OmO → install `procps`.
- OMX team runtime needs `tmux` → install `tmux`.
- With `procps` present, `omx --version` emitted no warning.

## Probe C — state directories

| Agent | Directories created under a fresh `HOME` |
|---|---|
| OmO | `~/.omo/{memory,lsp-daemon,agent/{logs,kill-guard,rpc,process-crashes,cache,extensions,runtime}}` |
| OMX | `~/.codex/{skills,sessions,shell_snapshots,thread-writer-locks,.tmp/plugins}`; project state `.omx/` lives in the workspace |
| OMC | `~/.omc/artifacts/ask`, `~/.claude/{backups,projects,sessions,plugins/store}`; user config `~/.config/claude-omc` is written by `omc setup` |

## Probe D — multi-arch

`docker run --platform linux/arm64 node:24-slim` installed all five packages and
printed every `--version` (`ARCH=aarch64`, `omo`, `oh-my-codex`, `codex-cli`,
`omc`, `claude`). All three images are viable on `linux/amd64` and `linux/arm64`.

## Decisions

1. **Base stays `node:22-slim`.** All three install and run on 22; OmO's `node>=24`
   engine is advisory in npm. This avoids rebuilding/risking the eight existing
   agent images.
2. **Headless argv** (all wrapped in `sh -c 'exec "$@" < /dev/null'` because OMX
   explicitly reads stdin and the runtime hands the agent a pipe):
   - OmO: `omo -p --no-session "<prompt>"`
   - OMX: `omx exec --skip-git-repo-check --json "<prompt>"` (+ `--model`, + approval flag)
   - OMC: `omc ask claude "<prompt>"`
3. **Interactive argv**: `omo`, `omx`, `omc launch`.
4. **Permission flags**: OmO `--permission-preset full-access` (allow-all) /
   `accept-edits` (accept-edits); OMX `--yolo` (interactive) /
   `--dangerously-bypass-approvals-and-sandbox` (headless); OMC `--madmax`
   (interactive and headless). *(Superseded the earlier "OMC none" MVP
   decision — `omc launch`/`omc ask` accept `--madmax`/`--yolo`.)*
5. **Model flag**: OmO `--model`, OMX `-m/--model`; OMC none (documented).
6. **API keys**: OmO `OPENAI_API_KEY` (required; `ANTHROPIC_API_KEY` optional),
   OMX `OPENAI_API_KEY` (required), OMC `ANTHROPIC_API_KEY` (required).
7. **State mounts**: OmO `~/.omo`; OMX `~/.codex` + `~/.agents`; OMC
   `~/.claude` + `~/.config/claude-omc` + `~/.omc`.
8. **Image extras**: `procps` (OMX, OmO), `tmux` (OMX).
9. **Licensing**: OmO is SUL-1.0 (non-commercial). Shipping it is an explicit
   product decision; the image carries a license label and a NOTICE pointer, and
   `registries/README.md` carries the risk note (OMY-6).

## Cleanup

All probes used `docker run --rm`; no containers, images, or temp contexts were
left behind. Temp scripts: `/tmp/spike-{omo,omx,omc,arm64,followup}.sh`.
