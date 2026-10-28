# Conformance Scenario Spec

This document is the contract between the `nanosb-conformance` harness and the
per-SDK **drivers** that exercise it. The scenario schema is **frozen** — it
matches `crates/conformance/src/scenario.rs` exactly. Do not change the schema
without changing the harness.

## 1. Purpose

A conformance scenario is a scripted NDJSON exchange. The harness
(`nanosb-conformance serve`) replays it over a Unix stream socket so that every
SDK can be tested against a real, byte-level protocol target without a VM or
network. The **driver** is a small program, one per SDK, that:

1. launches the harness for a scenario,
2. connects its SDK client to the harness socket,
3. drives the scripted operations through the SDK,
4. evaluates the scenario's `assert` block.

The corpus lives in `scenarios/`; each file is one scenario.

## 2. Scenario schema

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

### Top-level fields

| Field | Type | Meaning |
|---|---|---|
| `id` | string | Unique snake_case identifier. Also used to derive the default socket path, so keep it short enough that `$TMPDIR/nanosb-conf-<pid>-<n>-<id>.sock` stays under the Unix `SUN_LEN` limit (~103 bytes). |
| `description` | string | One-line human summary. |
| `capabilities` | string[] | Capabilities the server advertises in the `hello` handshake. |
| `steps` | Step[] | Scripted request/response exchanges, consumed **in order**. |
| `assert` | object | Free-form SDK-level expectations, interpreted by the driver (see §7). |

### Step fields

| Field | Type | Meaning |
|---|---|---|
| `expect` | object (optional) | Partial request match. Every field present must appear in the incoming request with an equal value; nested objects recurse, arrays match element-wise with equal length. Omit to accept any request. |
| `respond` | object \| object[] | One frame, or an array of frames flushed back-to-back without waiting for another request line. An array stops early at a terminal frame. |
| `delay_ms` | number (optional) | Delay before sending the response, in milliseconds. |
| `timeout_ms` | number (optional) | Max time to wait for the matching request. When it elapses the server answers `{"type":"error","code":"timeout","message":"step wait budget elapsed"}` and advances past the step. |

## 3. Server semantics

These are the observable behaviours of `nanosb-conformance serve` (see
`crates/conformance/src/server.rs` and `matcher.rs`):

- **`hello` is auto-answered.** The server replies to every `hello` frame with
  `{"type":"hello_ok","protocol":"1.0.0","helper_version":"...","capabilities":[...]}`
  using the scenario's `capabilities`. `hello` is never a step.
- **Steps are matched in order.** The server keeps a cursor into `steps`. Each
  incoming request is matched against the first step at or after the cursor
  whose `expect` matches (a step with no `expect` matches anything). A match
  advances the cursor past that step.
- **Capability gating.** A request verb whose capability is not advertised is
  answered `{"type":"error","code":"unsupported","message":"verb \`<ty>\` is not advertised"}`
  **before** step matching. The verb→capability table is in `matcher.rs`.
- **Per-step timeout.** A step with `timeout_ms` whose expected request never
  arrives is answered with a `timeout` error and the cursor advances.
- **Unmatched request.** A request that matches no step is answered
  `{"type":"error","code":"internal","message":"no scripted response"}`.
- **Streaming flush.** When `respond` is an array, all frames are written
  back-to-back. The flush stops early at a **terminal frame**:
  - `{"type":"guest","event":"exit"|"error",...}`
  - `{"type":"event","event":"done"}`
- **Malformed frame.** A request line that is not valid JSON is answered
  `{"type":"error","code":"bad_request","message":"malformed frame"}`.

### Response frame shapes

Every `respond` frame must be a valid `Response` from
`crates/protocol/src/verbs.rs` / `types.rs` (serde-tagged by `"type"`). The
shapes used by the corpus:

| Frame | Shape |
|---|---|
| ack | `{"type":"ok","version":"1.0.0"}` |
| error | `{"type":"error","code":"<code>","message":"..."}` — codes: `bad_request`, `incompatible_version`, `not_found`, `invalid_state`, `unsupported`, `unauthorized`, `timeout`, `transport`, `io`, `internal` |
| exec result | `{"type":"exec_result","exit_code":0,"stdout":"...","stderr":"...","duration_ms":1}` |
| fs data | `{"type":"fs_data","data_base64":"...","eof":true}` |
| fs content | `{"type":"fs_content","content":"..."}` |
| fs entries | `{"type":"fs_entries","entries":[{"name":"...","path":"...","is_dir":false,"size":0}]}` |
| fs stat | `{"type":"fs_stat","stat":{"path":"...","is_dir":true,"size":0}}` |
| fs exists | `{"type":"fs_exists","exists":true}` |
| logs | `{"type":"logs","lines":[{"cursor":0,"timestamp_ms":1000,"source":"stdout","data":"..."}],"next_cursor":1}` |
| metrics | `{"type":"metrics","cpu_percent":1.5,"memory_used_bytes":100,"memory_total_bytes":200,"disk_used_bytes":300,"disk_total_bytes":400,"net_rx_bytes":5,"net_tx_bytes":6,"timestamp_ms":1000}` |
| volumes | `{"type":"volumes","volumes":[{"name":"data","dir":"/host/data","used_bytes":0}]}` |
| volume | `{"type":"volume","name":"data","dir":"/host/data","used_bytes":0}` |
| secrets | `{"type":"secrets","secrets":[{"key":"K","value":null}]}` |
| sandbox | `{"type":"sandbox","name":"box","state":"running","image":"alpine:latest","pid":4242,"labels":{}}` |
| list | `{"type":"list","sandboxes":[{...}],"next_cursor":null}` |
| status | `{"type":"status","state":"running","pid":4242}` |
| snapshot | `{"type":"snapshot","id":"snap-1","sandbox":"box","image":"alpine:latest","created_at_ms":1000,"includes_memory":false}` |
| snapshots | `{"type":"snapshots","snapshots":[{...}]}` |
| guest event | `{"type":"guest","event":"stdout"|"stderr","data":"..."}` · `{"type":"guest","event":"exit","code":0}` · `{"type":"guest","event":"error","message":"..."}` |
| stream event | `{"type":"event","event":"log","cursor":0,"timestamp_ms":1000,"source":"stdout","data":"..."}` · `{"type":"event","event":"metrics",...}` · `{"type":"event","event":"done"}` |

## 4. Driver contract

A driver is a program that runs one scenario against one SDK. The contract:

### (a) Launch the harness

```
nanosb-conformance serve --scenario <file.json> [--socket <path>] [--capabilities a,b,c]
```

- `--scenario` (required): the scenario JSON file.
- `--socket` (optional): override the socket path. Defaults to a fresh temp
  path derived from the scenario id.
- `--capabilities` (optional): comma-separated capability override for the
  `hello` handshake (overrides the scenario's `capabilities`).

### (b) Read the `READY` line

The harness prints exactly one line to **stdout** once the server is listening:

```
READY /var/folders/.../nanosb-conf-123-0-lifecycle_create_get.sock
```

The driver must read this line and parse the socket path (everything after
`READY `). If the process exits before printing `READY`, the scenario failed to
parse — fail the run with the stderr output.

### (c) Connect the SDK client

Dial the SDK client against the socket path. The client performs the `hello`
handshake; the driver should record the advertised `capabilities` (needed for
capability-gating scenarios).

### (d) Drive each step

For each step, in order:

1. Read `expect.type` — the protocol verb the driver must invoke.
2. Invoke the SDK operation mapped from `expect.type` (see §5), passing the
   other `expect` fields as arguments. The SDK adds its own fields (e.g. `name`
   for sandbox-scoped verbs, `args: []` defaults); the server's partial match
   tolerates extras.
3. Consume the response frames. For a single-frame `respond`, one response is
   expected. For an array `respond` (a streaming step), the SDK's streaming API
   consumes the frames until the terminal frame.
4. If the SDK raises a typed error for an `error` frame, record it (do not
   abort) — the `assert` block decides whether an error was expected.

**Raw frames.** If the SDK does not wrap a verb (e.g. `attach_input`,
`snapshot_list` in some SDKs), the driver sends the raw request frame on a
fresh connection and reads the raw response. The request must contain the
`expect` fields verbatim (plus `name` where the verb is sandbox-scoped).

**Timeout steps.** When `assert.expects_timeout` is true, the driver must
**not** send the request for the timeout step. It waits for the server's
`timeout` error frame and verifies the code. After the timeout the cursor is
past the end of the steps; the driver stops driving.

**Capability-gating scenarios.** When `assert.expects_unsupported` is true, the
driver first verifies the handshake did **not** advertise the omitted
capability (`supports(...) == false`), then invokes the verb and expects an
`unsupported` error.

### (e) Evaluate `assert`

The `assert` object is free-form; the driver interprets the keys it understands
(§7) and fails the run if any is violated. Unknown keys are ignored (forward
compatibility).

## 5. `expect.type` → SDK operation mapping

The driver maps each protocol verb to the SDK operation that produces it. The
table below lists the verb, the sandbox-scoped `name` behaviour, and the
canonical SDK operation name (reference: `nanosb-client` / `nanosandbox-python`).

| `expect.type` | Capability | SDK operation (sandbox handle unless noted) |
|---|---|---|
| `create` | lifecycle | `client.create(spec)` / `create_with(builder)` |
| `get` | lifecycle | `client.get(name)` |
| `list` | lifecycle | `client.list(all)` |
| `start` | lifecycle | `sandbox.start()` |
| `stop` | lifecycle | `sandbox.stop(force)` |
| `kill` | lifecycle/signal | `sandbox.kill()` / `sandbox.kill_with(signal)` |
| `wait` | wait | `sandbox.wait(timeout)` |
| `remove` | lifecycle | `sandbox.remove(force)` |
| `drain` | lifecycle | `sandbox.drain()` |
| `touch` | lifecycle | `sandbox.touch()` |
| `pause` | lifecycle/snapshots | `sandbox.pause()` |
| `resume` | lifecycle/snapshots | `sandbox.resume()` |
| `modify` | lifecycle | `sandbox.modify(spec)` |
| `status` | lifecycle | `sandbox.state()` |
| `ping` | lifecycle | `client.ping()` |
| `exec` | exec | `sandbox.exec(command, args)` / `exec_with(req)` |
| `exec_stream` | exec | `sandbox.exec_stream(req, on_event)` / `sandbox.exec_pty(req)` when `tty` |
| `exec_input` | exec | tty `exec_stream` live stdin (raw if unwrapped) |
| `exec_resize` | exec | tty `exec_stream` PTY resize (raw if unwrapped) |
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

## 6. Streaming semantics for drivers

Streaming verbs (`exec_stream`, `attach`, `logs` with `follow: true`,
`metrics_stream`, `fs_write_stream`) follow one contract:

1. The driver sends the request.
2. The server answers an opening ack `{"type":"ok","version":"1.0.0"}` (or an
   `error`).
3. The server flushes the remaining frames back-to-back, stopping at the
   terminal frame (`guest exit/error` or `event done`).

The SDK's streaming API must surface the ack, deliver the intermediate frames
to the callback, and return/complete on the terminal frame. The driver must not
send another request on the same connection until the stream has terminated.

`fs_write_stream` is the inverse: the driver sends the opening request, reads
the ack, then sends one `fs_data` frame per chunk (each answered with an `ok`
ack), the last with `eof: true`.

## 7. `assert` conventions

The driver interprets these keys (all optional; unknown keys ignored):

| Key | Meaning |
|---|---|
| `exit_code` | Expected exit code of the last exec/exec_stream. |
| `stdout_equals` / `stderr_equals` | Exact expected stdout/stderr. |
| `stdout_contains` / `stderr_contains` | Substrings that must appear in stdout/stderr. |
| `create_acked`, `start_acked`, `stop_acked`, `kill_acked`, `remove_acked`, `drain_acked`, `touch_acked`, `pause_acked`, `resume_acked`, `modify_acked`, `write_acked`, `mkdir_acked`, `remove_dir_acked`, `copy_acked`, `rename_acked`, `copy_from_host_acked`, `copy_to_host_acked`, `mount_add_acked`, `set_acked`, `restore_acked`, `fork_acked`, `input_acked`, `resize_acked`, `write_stream_acked` | The corresponding operation completed without error. |
| `get_name`, `get_state`, `get_image` | Expected fields of the `get` result. |
| `list_names`, `list_states` | Expected names/states in the `list` result. |
| `entries_contain` | Names that must appear in the `fs_list` result. |
| `stat_is_dir` | Expected `is_dir` of the `fs_stat` result. |
| `exists` | Expected `fs_exists` result. |
| `read_bytes_base64` | Expected base64 of the `fs_read` result. |
| `content_equals` | Expected `fs_read_to_string` content. |
| `chunks_acked` | Number of `fs_data` chunks acked in a streamed write. |
| `lines_contain`, `resume_lines_contain`, `follow_lines_contain` | Substrings that must appear in the log lines. |
| `next_cursor` | Expected `next_cursor` of the first `logs` response. |
| `all_source` | Every returned log line must have this `source`. |
| `cpu_percent`, `memory_used_bytes` | Expected metrics values. |
| `has_fields` | Metrics fields that must be present. |
| `stream_samples` | Number of metrics samples received before `done`. |
| `first_cpu_percent` | `cpu_percent` of the first metrics sample. |
| `volume_created`, `volumes_contain`, `volume_get_name` | Expected volume names. |
| `secrets_contain` | Expected secret keys. |
| `secret_values_redacted` | Listed secret values must be `null`/empty. |
| `snapshot_created`, `snapshots_contain` | Expected snapshot ids. |
| `attach_events`, `attach_exit_code` | Expected guest event sequence and exit code of an attach. |
| `error_code`, `second_error_code` | Expected error code(s) of the first/second erroring step. |
| `wait_exit_code`, `wait_state` | Expected terminal exit code / state of the `wait` result. |
| `error_message_contains` | Substring that must appear in the error message. |
| `expects_timeout`, `timeout_code` | The driver must not send the timeout step's request and must observe a `timeout` error. |
| `expects_unsupported`, `unsupported_verb` | The handshake must omit the capability and the verb must fail with `unsupported`. |
| `note` | Human guidance for the driver author; not asserted. |

## 8. Writing a driver

1. **Pick a runner.** Any language works — the driver only needs to spawn a
   subprocess, read a line, and speak NDJSON over a Unix socket (or use the
   SDK's client).
2. **Implement the mapping.** Translate each `expect.type` in §5 to your SDK's
   operation. Where the SDK lacks a wrapper, send the raw frame.
3. **Handle streaming.** Use the SDK's streaming API for array `respond` steps;
   verify the terminal frame ends the stream.
4. **Handle errors.** Do not abort on `error` frames; record them and let
   `assert` decide.
5. **Evaluate `assert`.** Implement the keys in §7 that your SDK can observe.
6. **Run every scenario** in `scenarios/`; a scenario passes when the driver
   completes without an assertion failure and the harness printed `READY`.

See `README.md` for the run loop and the coverage matrix.