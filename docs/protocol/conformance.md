# Protocol Conformance

The conformance suite proves that every SDK speaks the protocol in
[`protocol.md`](protocol.md) byte-for-byte. It is a language-agnostic, CI-safe
harness: `nanosb-conformance` replays a scripted NDJSON exchange over a Unix
stream socket, so every SDK driver is tested against a real protocol target
without a VM or network.

The authoritative contract lives in
[`sdk/conformance/SPEC.md`](../../sdk/conformance/SPEC.md). This page
summarises it and links to the details; it does not duplicate the spec verbatim.

## The harness

`nanosb-conformance serve --scenario <file>` reads a scenario JSON, listens on a
Unix socket, and prints a `READY <socket-path>` line to stdout. A **driver** (one
per SDK) launches the harness, connects its SDK client to that socket, drives
the scripted operations, and evaluates the scenario's `assert` block.

The harness auto-answers `hello`, matches incoming requests against scripted
steps in order, enforces capability gating, applies per-step timeouts, and
flushes streaming response arrays up to a terminal frame. See SPEC.md §3 for the
full server semantics.

## Scenario schema

Each scenario is one JSON file in `sdk/conformance/scenarios/`:

```json
{
  "id": "<snake_case>",
  "description": "<one line>",
  "capabilities": ["lifecycle", "exec", "fs", "logs", "metrics", "volumes", "secrets", "snapshots", "networking", "handles", "ephemeral", "docker"],
  "steps": [
    {
      "expect": { "<partial request object>" },
      "respond": { "<single frame>" } | [ { "<frame 1>" }, { "<frame 2>" } ],
      "delay_ms": 5,
      "timeout_ms": 2000
    }
  ],
  "assert": { "<SDK-level expected outcomes; free-form, interpreted by the driver>" }
}
```

`docker` is protocol-declared but no frozen scenario advertises it, so the 35-scenario corpus is unchanged.

- `expect` is a **partial** request match: every field present must appear in the
  incoming request with an equal value. Omit to accept any request.
- `respond` is one frame, or an array flushed back-to-back (a streaming step).
  An array stops early at a terminal frame.
- `assert` is free-form; the driver interprets the keys it understands and fails
  the run on any violation. Unknown keys are ignored.

The schema is **frozen** and matches `crates/conformance/src/scenario.rs`
exactly. See SPEC.md §2 for the full field reference.

## Driver contract

A driver is a small program, one per SDK, that:

1. **Launches** `nanosb-conformance serve --scenario <file>` and reads the
   `READY <path>` line from stdout.
2. **Connects** its SDK client to that path (the client performs the `hello`
   handshake; the driver records the advertised capabilities).
3. **Drives each step**: reads `expect.type`, invokes the SDK operation mapped
   from it (passing the other `expect` fields as arguments), and consumes the
   `respond` frames. Streaming steps use the SDK's streaming API.
4. **Handles errors**: does not abort on `error` frames; records them and lets
   `assert` decide.
5. **Evaluates `assert`** and fails the run on any violation.

If the SDK does not wrap a verb (e.g. `attach_input`, `snapshot_list`), the
driver sends the raw request frame on a fresh connection. See SPEC.md §4 for the
full contract, including timeout and capability-gating scenarios.

## `expect.type` → SDK operation mapping

The driver maps each protocol verb to the SDK operation that produces it. The
full table is in SPEC.md §5; the canonical mapping (reference:
`nanosb-client` / `nanosandbox-python`) is:

| `expect.type` | Capability | SDK operation (sandbox handle unless noted) |
|---|---|---|
| `create` | lifecycle | `client.create(spec)` / `create_with(builder)` |
| `get` | lifecycle | `client.get(name)` |
| `list` | lifecycle | `client.list(all)` |
| `start` | lifecycle | `sandbox.start()` |
| `stop` | lifecycle | `sandbox.stop(force)` |
| `kill` | lifecycle | `sandbox.kill()` |
| `remove` | lifecycle | `sandbox.remove(force)` |
| `drain` | lifecycle | `sandbox.drain()` |
| `touch` | lifecycle | `sandbox.touch()` |
| `pause` | lifecycle/snapshots | `sandbox.pause()` |
| `resume` | lifecycle/snapshots | `sandbox.resume()` |
| `modify` | lifecycle | `sandbox.modify(spec)` |
| `status` | lifecycle | `sandbox.state()` |
| `ping` | lifecycle | `client.ping()` |
| `exec` | exec | `sandbox.exec(command, args)` / `exec_with(req)` |
| `exec_stream` | exec | `sandbox.exec_stream(req, on_event)` |
| `shell` | exec | `sandbox.shell(script)` |
| `attach` | exec | `sandbox.attach(on_event)` / `attach_session()` |
| `attach_input` | exec | `attach_session.send_input(bytes)` (raw if unwrapped) |
| `attach_resize` | exec | `attach_session.resize(cols, rows)` (raw if unwrapped) |
| `fs_read` | fs | `sandbox.fs_read(path)` |
| `fs_read_to_string` | fs | `sandbox.fs_read_to_string(path)` |
| `fs_write` | fs | `sandbox.fs_write(path, data)` |
| `fs_write_stream` | fs | `sandbox.fs_write_stream(path, data, chunk_size)` |
| `fs_data` | fs | body frame of a streamed write (raw) |
| `fs_list` | fs | `sandbox.fs_list(path)` |
| `fs_stat` | fs | `sandbox.fs_stat(path)` |
| `fs_exists` | fs | `sandbox.fs_exists(path)` |
| `fs_mkdir` | fs | `sandbox.fs_mkdir(path, recursive)` |
| `fs_remove` | fs | `sandbox.fs_remove(path)` |
| `fs_remove_dir` | fs | `sandbox.fs_remove_dir(path, recursive)` |
| `fs_copy` | fs | `sandbox.fs_copy(from, to)` |
| `fs_rename` | fs | `sandbox.fs_rename(from, to)` |
| `fs_copy_from_host` | fs | `sandbox.fs_copy_from_host(host_path, guest_path)` |
| `fs_copy_to_host` | fs | `sandbox.fs_copy_to_host(guest_path, host_path)` |
| `logs` | logs | `sandbox.logs(tail)` / `logs_with(tail, cursor, source)` |
| `metrics` | metrics | `sandbox.metrics()` |
| `metrics_stream` | metrics | `sandbox.metrics_stream(interval_ms, on_event)` |
| `volume_create` | volumes | `sandbox.volume_create(volume_spec)` |
| `volume_list` | volumes | `sandbox.volume_list()` |
| `volume_get` | volumes | `sandbox.volume_get(volume)` |
| `volume_remove` | volumes | `sandbox.volume_remove(volume)` |
| `mount_add` | volumes | `sandbox.mount_add(mount_spec)` |
| `secret_set` | secrets | `sandbox.secret_set(key, value)` |
| `secret_list` | secrets | `sandbox.secret_list()` |
| `secret_remove` | secrets | `sandbox.secret_remove(key)` |
| `snapshot_create` | snapshots | `sandbox.snapshot_create(dest)` |
| `snapshot_list` | snapshots | `client.snapshots()` |
| `snapshot_restore` | snapshots | `sandbox.snapshot_restore(snapshot, name)` |
| `snapshot_fork` | snapshots | `sandbox.snapshot_fork(snapshot, name)` |
| `snapshot_remove` | snapshots | `sandbox.snapshot_remove(snapshot)` |

## Streaming semantics for drivers

Streaming verbs (`exec_stream`, `attach`, `logs` with `follow: true`,
`metrics_stream`, `fs_write_stream`) follow one contract:

1. The driver sends the request.
2. The server answers an opening ack `{"type":"ok","version":"1.0.0"}` (or an
   `error`).
3. The server flushes the remaining frames back-to-back, stopping at the
   terminal frame (`guest exit/error` or `event done`).

The SDK's streaming API must surface the ack, deliver the intermediate frames to
the callback, and return/complete on the terminal frame. The driver must not send
another request on the same connection until the stream has terminated.

`fs_write_stream` is the inverse: the driver sends the opening request, reads the
ack, then sends one `fs_data` frame per chunk (each answered with an `ok` ack),
the last with `eof: true`.

## `assert` conventions

The driver interprets these keys (all optional; unknown keys ignored): exit
codes and stdout/stderr expectations, per-operation `*_acked` flags, `get`/`list`
field expectations, fs results (`entries_contain`, `stat_is_dir`, `exists`,
`read_bytes_base64`, `content_equals`, `chunks_acked`), log expectations
(`lines_contain`, `next_cursor`, `all_source`), metrics (`cpu_percent`,
`stream_samples`, `has_fields`), volume/secret/snapshot expectations, attach
event sequences, error codes (`error_code`, `second_error_code`,
`error_message_contains`), and the special `expects_timeout` / `expects_unsupported`
modes. See SPEC.md §7 for the complete reference.

## Coverage

The corpus covers lifecycle, exec, exec_stream, shell, attach, fs, logs, metrics,
volumes, secrets, snapshots, errors, timeouts, and capability gating. The full
coverage matrix (scenario file → verbs exercised) is in
[`sdk/conformance/README.md`](../../sdk/conformance/README.md).