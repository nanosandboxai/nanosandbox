# Conformance Corpus

A language-agnostic, CI-safe protocol conformance suite for the nanosandbox
SDKs. The `nanosb-conformance` harness replays a scripted NDJSON exchange over a
Unix stream socket, so every SDK driver can be tested against a real, byte-level
protocol target without a VM or network.

```
sdk/conformance/
├── SPEC.md        # scenario schema + per-SDK driver contract (read this first)
├── README.md      # this file
└── scenarios/     # 40 scripted scenario JSON files
```

## What this verifies

Each scenario scripts a request/response exchange for one or more protocol
verbs. A **driver** (one per SDK) launches the harness, connects its SDK client,
drives the scripted operations, and evaluates the scenario's `assert` block.
The corpus covers:

| Group | Scenarios |
|---|---|
| lifecycle | create/get, list, start/stop, kill/remove, drain/touch, pause/resume, modify, full create→start→exec→stop→remove flow, rich spec (network/labels/ephemeral/hostname) |
| exec | buffered exec with stdout/stderr/exit code, env/cwd/user passthrough |
| exec_stream | streamed stdout + exit, stderr + non-zero exit |
| shell | multi-line script |
| attach | attach + attach_input + attach_resize |
| fs | write/read, read_to_string, write_stream (fs_data chunks), list/stat/exists, mkdir/remove_dir, copy/rename, copy_from_host/copy_to_host |
| logs | tail + cursor, follow (event stream), source filter |
| metrics | snapshot, metrics_stream |
| volumes | create/list/get/remove, mount_add |
| secrets | set/list/remove (values redacted) |
| snapshots | create/list/restore/fork/remove |
| errors | not_found, invalid_state, bad_request, unsupported |
| timeouts | per-step `timeout_ms` |
| capability-gating | verb whose capability is not advertised → `unsupported` |

## Running the harness

Build once:

```bash
cargo build -p nanosb-conformance
```

Serve a single scenario (prints `READY <socket-path>` and blocks):

```bash
cargo run -p nanosb-conformance -- serve --scenario sdk/conformance/scenarios/lifecycle_create_get.json
```

Optional flags:

```bash
# fixed socket path (default: a fresh temp path derived from the scenario id)
cargo run -p nanosb-conformance -- serve --scenario <file> --socket /tmp/conf.sock

# override the advertised capabilities for the hello handshake
cargo run -p nanosb-conformance -- serve --scenario <file> --capabilities lifecycle,exec
```

Every scenario in `scenarios/` must print `READY <path>` and not error. To check
all of them:

```bash
for f in sdk/conformance/scenarios/*.json; do
  target/debug/nanosb-conformance serve --scenario "$f" &
  pid=$!
  sleep 1
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
done
```

## Writing a driver

Read [`SPEC.md`](SPEC.md) — it defines the scenario schema, the server
semantics, the driver contract, and the `expect.type` → SDK-operation mapping
table. In short, a driver:

1. **Launches** `nanosb-conformance serve --scenario <file>` and reads the
   `READY <path>` line from stdout.
2. **Connects** its SDK client to that path (the client performs the `hello`
   handshake).
3. **Drives each step**: invokes the SDK operation named by `expect.type` with
   the other `expect` fields as arguments, consuming the `respond` frames
   (streaming steps use the SDK's streaming API).
4. **Evaluates `assert`** and fails the run on any violation.

If the SDK does not wrap a verb (e.g. `attach_input`, `snapshot_list`), send the
raw request frame on a fresh connection. See §4 and §5 of `SPEC.md`.

### Minimal driver skeleton (Python)

```python
import json, subprocess, sys
from nanosandbox import SandboxClient

proc = subprocess.Popen(
    ["target/debug/nanosb-conformance", "serve", "--scenario", scenario_file],
    stdout=subprocess.PIPE, text=True,
)
line = proc.stdout.readline().strip()
assert line.startswith("READY "), line
sock = line.split(" ", 1)[1]

client = SandboxClient.connect(sock)
# ... drive steps from the scenario JSON, then evaluate `assert` ...
proc.terminate()
```

## Coverage matrix

| Scenario file | Verbs exercised |
|---|---|
| `lifecycle_create_get.json` | create, get |
| `lifecycle_list.json` | list |
| `lifecycle_start_stop.json` | start, stop |
| `lifecycle_kill_remove.json` | kill, remove |
| `lifecycle_drain_touch.json` | drain, touch |
| `lifecycle_pause_resume.json` | pause, resume |
| `lifecycle_restart.json` | restart |
| `wait_terminal.json` | wait (exit code + terminal state) |
| `kill_signal.json` | kill with a signal selector (SIGTERM) |
| `lifecycle_modify.json` | modify |
| `lifecycle_full_flow.json` | create, start, exec, stop, remove (ordering) |
| `lifecycle_create_rich_spec.json` | create (network/labels/ephemeral/hostname) |
| `exec_basic.json` | exec |
| `exec_env_cwd.json` | exec (env/cwd/user) |
| `exec_stream_basic.json` | exec_stream |
| `exec_stream_error.json` | exec_stream (stderr, non-zero exit) |
| `exec_tty_stdin.json` | exec_stream with a PTY + live stdin |
| `shell_basic.json` | shell |
| `attach_interactive.json` | attach, attach_input, attach_resize |
| `fs_write_read.json` | fs_write, fs_read |
| `fs_read_to_string.json` | fs_read_to_string |
| `fs_write_stream.json` | fs_write_stream, fs_data |
| `fs_list_stat_exists.json` | fs_list, fs_stat, fs_exists |
| `fs_mkdir_remove_dir.json` | fs_mkdir, fs_remove_dir |
| `fs_copy_rename.json` | fs_copy, fs_rename |
| `fs_host_transfer.json` | fs_copy_from_host, fs_copy_to_host |
| `logs_tail_cursor.json` | logs (tail, cursor) |
| `logs_follow.json` | logs (follow) |
| `logs_source.json` | logs (source) |
| `metrics_snapshot.json` | metrics |
| `metrics_stream.json` | metrics_stream |
| `volumes_crud.json` | volume_create, volume_list, volume_get, volume_remove |
| `volumes_mount_add.json` | mount_add |
| `secrets_crud.json` | secret_set, secret_list, secret_remove |
| `snapshots_crud.json` | snapshot_create, snapshot_list, snapshot_restore, snapshot_fork, snapshot_remove |
| `errors_not_found.json` | get→not_found, start→invalid_state |
| `errors_bad_request.json` | fs_read→bad_request, snapshot_create→unsupported |
| `timeout_step.json` | per-step timeout |
| `capability_gating.json` | unadvertised capability → unsupported |
| `security_context_rejected.json` | create with a security context → unsupported |

## Constraints

- The harness and protocol crates under `crates/` are **frozen** — do not modify
  them from this directory.
- Scenario ids must stay short enough that the default socket path
  (`$TMPDIR/nanosb-conf-<pid>-<n>-<id>.sock`) fits under the Unix `SUN_LEN`
  limit (~103 bytes).
- Response frame shapes must match `crates/protocol/src/verbs.rs` and
  `types.rs` exactly — never invent a frame shape.