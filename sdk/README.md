# Nanosandbox SDKs

Eight protocol clients — **Rust, Python, TypeScript/Node, Go, Ruby, Java,
C#/.NET, Kotlin** — plus one language-agnostic conformance harness.

Every SDK is a **thin client over the versioned `nanosb-protocol` (v1.0.0)**:
it speaks NDJSON on a Unix stream socket to the `nanosb-runtime` helper, which
owns the microVM (boot, exec, fs, logs, metrics, volumes, secrets, snapshots,
pause/resume). No SDK hosts a VM itself, and none depends on a native addon or
the retired C ABI — so all eight share one wire contract and one test corpus.

```
   SDK client ──NDJSON / Unix socket──▶ nanosb-runtime (helper)
                                          ├─ supervisor ─▶ libkrun ─▶ microVM
                                          ├─ console / logs / metrics
                                          └─ control socket (exec / fs / attach)
```

## The SDKs

| SDK | Directory | Runtime | Entry point | Install / run |
|---|---|---|---|---|
| Rust | [`rust/`](rust/README.md) | Rust | `nanosandbox-sdk` (re-exports `nanosb-client`) | `cargo add nanosandbox-sdk` |
| Python | [`python/`](python/README.md) | Python ≥ 3.9 | `nanosandbox.SandboxClient` | `pip install nanosandbox-sdk` |
| TypeScript/Node | [`typescript/`](typescript/README.md) | Node ≥ 24 | `nanosandbox-sdk` (ESM) | `import { SandboxClient } from "nanosandbox-sdk"` |
| Go | [`go/`](go/README.md) | Go ≥ 1.27 | `github.com/nanosandboxai/nanosandbox/sdk/go/nanosandbox` | `go get github.com/nanosandboxai/nanosandbox/sdk/go` |
| Ruby | [`ruby/`](ruby/README.md) | Ruby ≥ 2.6 | `nanosandbox` | `gem install nanosandbox-sdk` |
| Java | [`java/`](java/README.md) | JDK ≥ 21 | `nanosandbox.SandboxClient` | `ai.nanosandbox:nanosandbox-sdk` (Maven Central) |
| C#/.NET | [`dotnet/`](dotnet/README.md) | .NET ≥ 8 | `SandboxClient` | `dotnet add package Nanosandbox.Sdk` |
| Kotlin | [`kotlin/`](kotlin/README.md) | JDK ≥ 21 | `nanosandbox.kt.KotlinClient` | `ai.nanosandbox:nanosandbox-sdk-kotlin` (Maven Central) |

Each README documents that SDK's install, full usage, and test commands.

## Quick start

Connect to a running helper:

```python
# Python (others are analogous — see each README)
from nanosandbox import SandboxClient
client = SandboxClient.connect("/path/to/helper.sock")
box = client.create({"image": "library/alpine:latest", "name": "demo"})
print(box.exec("echo", ["hello from a microVM"])["stdout"])
```

Or let the SDK **start the helper for you** via `ManagedRuntime` (locates/spawns
`nanosb-runtime serve` on a private socket, then tears it down):

| SDK | Launch |
|---|---|
| Rust | `let rt = ManagedRuntime::start()?; SandboxClient::connect(rt.socket())?` |
| Python | `with ManagedRuntime() as rt: SandboxClient.connect(str(rt.socket))` |
| TypeScript | `const rt = await ManagedRuntime.start(); await SandboxClient.connect(rt.socket)` |
| Go | `rt, _ := nanosandbox.StartRuntime(15*time.Second)` |
| Ruby | `Nanosandbox::ManagedRuntime.open { \|rt\| ... }` |
| Java | `try (var rt = ManagedRuntime.start()) { ... }` |
| Kotlin | `val rt = nanosandbox.ManagedRuntime.start()` |
| C#/.NET | `using (var rt = ManagedRuntime.start()) { ... }` |

Every `ManagedRuntime` — **in all eight SDKs, Rust included** — resolves the
helper via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime` → `PATH` →
the repo `target/debug/nanosb-runtime`. When the helper is missing, it
**auto-provisions** via the shared `scripts/sdk/ensure-runtime.sh` provisioner
(located via `NANOSB_PROVISIONER`, the package/repo, or an embedded copy shipped
with the SDK), downloading and verifying the per-platform runtime bundle under
`$NANOSB_HOME`, pinned to the SDK's release version — so the package-manager
install is the only step needed.

Provisioning is configured by environment variables: `NANOSB_HOME` (install
root, default `~/.nanosandbox`), `NANOSB_VERSION` (runtime version to download,
pinned to the SDK's release version), `NANOSB_RUNTIME_URL` (release base URL),
`NANOSB_RUNTIME_SHA256` (expected sha256, verified), `NANOSB_OFFLINE=1` (never
download), and `NANOSB_PROVISIONER` (explicit provisioner path).

## Headless code agents

Use an agent image and its headless command instead of Alpine. Inject the
provider key as an environment variable, start the sandbox, and exec the
command.

| Agent | Image (under `ghcr.io/nanosandboxai/nanosandbox/`) | Headless command | Environment |
|---|---|---|---|
| Claude Code | `claude:latest` | `claude --print "<prompt>"` | `ANTHROPIC_API_KEY` |
| Codex | `codex:latest` | `codex exec --skip-git-repo-check --json "<prompt>" < /dev/null` | `CODEX_API_KEY` (and `OPENAI_API_KEY`) |
| Gemini CLI | `gemini:latest` | `gemini -p "<prompt>"` | `GEMINI_API_KEY` |
| Goose | `goose:latest` | `goose run --text "<prompt>"` | `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` |
| Aider | `aider:latest` | `aider --message "<prompt>" --yes --no-auto-commits` | provider key |
| OpenCode | `opencode:latest` | `opencode run "<prompt>"` | provider key |
| Crush | `crush:latest` | `crush run "<prompt>"` | provider key |

Verified Codex example (run against `v0.3.0-rc2`; the final event carries the
agent message `ok`):

```python
import os
from nanosandbox import ManagedRuntime, SandboxBuilder, SandboxClient

with ManagedRuntime() as rt:
    client = SandboxClient.connect(str(rt.socket))
    box = client.create_with(
        SandboxBuilder("ghcr.io/nanosandboxai/nanosandbox/codex:latest")
        .with_name("codex-agent")
        .with_env("OPENAI_API_KEY", os.environ["OPENAI_API_KEY"])
        .with_env("CODEX_API_KEY", os.environ["OPENAI_API_KEY"])
        .with_resources(2, 2048)
    )
    box.start()
    out = box.exec(
        "sh",
        ["-c", "codex exec --skip-git-repo-check --json 'Reply with exactly: ok' < /dev/null"],
    )
    print(out["stdout"])
    box.remove(force=True)
```

Codex waits for stdin when it is not a TTY, so redirect it from `/dev/null`;
this image uses `CODEX_API_KEY` for API-key auth. For Claude, use
`claude --print "<prompt>"` with `ANTHROPIC_API_KEY`. See
<https://nanosandbox.ai/docs/sdk/agents> for the full guide and per-language
adapters.

## Error handling

Wire errors are `{type:"error", code, message}` with stable codes:
`bad_request`, `incompatible_version`, `not_found`, `invalid_state`,
`unsupported`, `unauthorized`, `timeout`, `transport`, `io`, and `internal`.
Retry only `timeout`/`transport` for idempotent calls; treat `not_found` as a
missing sandbox; check `supports()` before optional verbs; re-raise unknown
errors. A non-zero `exec` exit code is a result, not a protocol error.

Each SDK exposes the code plus helpers (`is_not_found`/`isNotFound`, …). Full
table, accessors, and per-language handlers:
<https://nanosandbox.ai/docs/sdk/errors>.

## Conformance suite

All SDKs are validated by **one** corpus: [`conformance/`](conformance/README.md)
holds the harness contract (`SPEC.md`) and **40 scenarios** (lifecycle, exec /
exec_stream / shell / attach, fs incl. streamed write, logs, metrics, volumes,
secrets, snapshots, errors, timeouts, capability gating).

```bash
cargo build -p nanosb-conformance     # the harness
# each SDK runs the same corpus through its own driver, e.g.:
python3 sdk/python/conformance/run.py
node    sdk/typescript/conformance/run.mjs
go      run ./sdk/go/conformance/run.go
ruby    -I sdk/ruby/lib sdk/ruby/conformance/run.rb
java    -cp <build> Run
dotnet  run --project sdk/dotnet/conformance
java    -cp sdk/kotlin/nanosandbox-sdk-kotlin.jar nanosandbox.kt.RunKt
```

Expected: `40 passed, 0 failed`.

## Real microVM end-to-end

Beyond the scripted harness, each SDK has an **opt-in live test** that boots a
real microVM through the nanosandbox runtime and drives the same operations as
the Rust reference (`NANOSB_E2E=1`). See
[`docs/live-verification.md`](../docs/live-verification.md) and
[`docs/plans/2026-10-11-sdk-live-e2e-plan.md`](../docs/plans/2026-10-11-sdk-live-e2e-plan.md).

## References

- Protocol spec: [`docs/protocol/`](../docs/protocol/)
- Conformance contract: [`conformance/SPEC.md`](conformance/SPEC.md)
- Runtime setup / packaging: [`docs/runtime-setup.md`](../docs/runtime-setup.md)
- Live verification: [`docs/live-verification.md`](../docs/live-verification.md)
- CI/CD & publishing plan: [`docs/epics/epic-4-sdk-ci-cd-and-publishing.md`](../docs/epics/epic-4-sdk-ci-cd-and-publishing.md)

## Layout

```
sdk/
├── conformance/     # harness contract (SPEC.md) + 40 shared scenarios
├── rust/            # Rust client (re-exports nanosb-client) + ManagedRuntime
├── python/          # Python client + ManagedRuntime
├── typescript/      # TypeScript/Node client + ManagedRuntime
├── go/              # Go client + ManagedRuntime
├── ruby/            # Ruby client + ManagedRuntime
├── java/            # Java client + ManagedRuntime
├── dotnet/          # C#/.NET client + ManagedRuntime
└── kotlin/          # Kotlin facade over the Java SDK
```