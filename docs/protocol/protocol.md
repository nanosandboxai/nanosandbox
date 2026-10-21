# Nanosandbox Control Protocol

Version `1.0.0`. This is the wire contract between the `nanosb-runtime` helper
and every language SDK. It is the frozen reference for Epic 1 (Multi-language
Sandbox SDK). The authoritative implementation lives in
`crates/protocol/src/`; this document mirrors it and must stay in sync.

## 1. Substrate: helper-based, no C FFI

The substrate is a single slim helper process, `nanosb-runtime`. Every SDK is a
**thin protocol client** that talks to the helper over a local Unix socket. There
is no C FFI and no native addon in any SDK (decision D2). The C ABI is retired.

All planning stays in the helper. Clients send intent and receive results; they
never reimplement mount planning, agent command construction, or secret
resolution. One transport carries everything: `exec_stream`, `attach`,
`logs --follow`, and `fs` streams all ride the same socket.

## 2. Transport

- **Socket:** a Unix stream socket. Three well-known socket files are defined in
  `crates/protocol/src/lib.rs`:
  - `control.sock` — per-sandbox control socket, relative to a sandbox's state
    directory.
  - `exec.sock` — per-sandbox exec bridge socket.
  - `helper.sock` — the global helper daemon socket, relative to
    `$HOME/.nanosandbox`.
- **Framing:** newline-delimited JSON (NDJSON). One compact JSON message per
  line, terminated by `\n`. See `crates/protocol/src/frame.rs`.
- **Encoding:** binary payloads are carried as standard base64 (RFC 4648, `=`
  padding). The single implementation is in `crates/protocol/src/base64.rs`, so
  every helper and SDK agrees byte-for-byte.
- **Blank lines** are skipped (keep-alive). A clean EOF returns no frame. A
  malformed line is answered with a `bad_request` error.

## 3. Handshake: hello / version / capability

Every connection opens with a `hello` request. The helper replies with the
negotiated protocol version and the capability list it implements. Clients must
check capabilities before calling an optional verb.

### `hello` request

```json
{
  "type": "hello",
  "client": "nanosb-sdk-python",
  "sdk_version": "0.1.0",
  "protocol_min": "1.0.0",
  "protocol_max": "1.0.0"
}
```

- `client` — client identity (informational).
- `sdk_version` — client version (informational).
- `protocol_min` / `protocol_max` — inclusive range of protocol versions the
  client understands. The helper negotiates a version in the intersection.

### `hello_ok` response

```json
{
  "type": "hello_ok",
  "protocol": "1.0.0",
  "helper_version": "0.3.0",
  "capabilities": ["lifecycle", "exec", "fs", "logs", "metrics", "volumes", "secrets", "snapshots", "networking", "handles", "ephemeral", "signal", "wait", "exec_pty"]
}
```

### Capabilities

Clients gate optional verbs on these. The full set (`Capability::all()`):

| Capability | Verbs it gates |
|---|---|
| `lifecycle` | create/start/stop/kill/remove/get/list/modify/drain/touch/ping |
| `exec` | exec / exec_stream / shell / attach |
| `fs` | fs/* verbs |
| `logs` | logs (+ follow + cursor) |
| `metrics` | metrics / metrics_stream |
| `volumes` | volumes/* verbs |
| `secrets` | secrets/* verbs |
| `snapshots` | snapshot/* + pause/resume |
| `networking` | publish TCP/UDP ports + policy passthrough |
| `handles` | labels + handles |
| `ephemeral` | ephemeral sandboxes |
| `signal` | `kill` with an explicit signal (`SIGTERM`/`SIGKILL`) |
| `wait` | blocking `wait` verb |
| `exec_pty` | tty `exec_stream` + live stdin/`exec_resize` frames |

Version comparison is numeric segment-by-segment (`cmp_version`); a client's
range intersects a server version via `version_in_range`.

## 4. Verbs

`Request` is client → helper; `Response` is helper → client. Both are
serde-tagged by `"type"` with `snake_case` variant names. One request yields one
immediate `Response`; verbs that stream then emit additional `Guest` / `Event`
frames terminated by an exit/done (see §6).

Sandbox-scoped verbs carry an optional `name`. On a per-sandbox socket the name
may be omitted (the socket is already bound to one sandbox); on the global
helper socket it is required.

### 4.1 Request verbs

The complete inventory, matching `crates/protocol/src/verbs.rs` 1:1.

| `type` | Payload | Capability |
|---|---|---|
| `hello` | `HelloRequest` | — |
| `ping` | — | lifecycle |
| `status` | `{ name? }` | lifecycle |
| `create` | `CreateRequest { spec: SandboxSpec }` | lifecycle |
| `start` | `{ name? }` | lifecycle |
| `stop` | `{ name?, force }` | lifecycle |
| `kill` | `{ name?, signal? }` | lifecycle (signal ⇒ `signal`) |
| `wait` | `{ name?, timeout_secs? }` | wait |
| `remove` | `{ name?, force }` | lifecycle |
| `get` | `{ name }` | lifecycle |
| `list` | `{ all, limit?, cursor?, label? }` | lifecycle |
| `modify` | `{ name?, spec: ModifySpec }` | lifecycle |
| `drain` | `{ name? }` | lifecycle |
| `touch` | `{ name? }` | lifecycle |
| `pause` | `{ name? }` | lifecycle/snapshots |
| `resume` | `{ name? }` | lifecycle/snapshots |
| `exec` | `ExecRequest` | exec |
| `exec_stream` | `ExecRequest` | exec |
| `exec_input` | `{ data }` — live stdin for a tty `exec_stream` | exec |
| `exec_resize` | `{ cols, rows }` — PTY resize for a tty `exec_stream` | exec |
| `shell` | `ShellRequest` | exec |
| `attach` | `{ name? }` | exec |
| `attach_input` | `{ data }` | exec |
| `attach_resize` | `{ cols, rows }` | exec |
| `fs_read` | `FsPathSpec` | fs |
| `fs_read_to_string` | `FsPathSpec` | fs |
| `fs_write` | `FsWriteSpec` | fs |
| `fs_write_stream` | `FsWriteStreamSpec` | fs |
| `fs_data` | `{ data_base64, eof }` | fs |
| `fs_list` | `FsPathSpec` | fs |
| `fs_stat` | `FsPathSpec` | fs |
| `fs_exists` | `FsPathSpec` | fs |
| `fs_mkdir` | `FsMkdirSpec` | fs |
| `fs_remove` | `FsPathSpec` | fs |
| `fs_remove_dir` | `FsRemoveDirSpec` | fs |
| `fs_copy` | `FsCopySpec` | fs |
| `fs_rename` | `FsCopySpec` | fs |
| `fs_copy_from_host` | `FsTransferSpec` | fs |
| `fs_copy_to_host` | `FsTransferSpec` | fs |
| `logs` | `LogsSpec` | logs |
| `metrics` | `{ name? }` | metrics |
| `metrics_stream` | `{ name?, interval_ms? }` | metrics |
| `volume_create` | `VolumeCreateSpec` | volumes |
| `volume_list` | `{ name? }` | volumes |
| `volume_get` | `VolumeRef` | volumes |
| `volume_remove` | `VolumeRef` | volumes |
| `mount_add` | `MountAddSpec` | volumes |
| `secret_set` | `SecretSetSpec` | secrets |
| `secret_list` | `{ name? }` | secrets |
| `secret_remove` | `SecretRef` | secrets |
| `snapshot_create` | `SnapshotCreateSpec` | snapshots |
| `snapshot_list` | — | snapshots |
| `snapshot_restore` | `SnapshotRestoreSpec` | snapshots |
| `snapshot_fork` | `SnapshotRestoreSpec` | snapshots |
| `snapshot_remove` | `{ snapshot }` | snapshots |

### 4.2 Response variants

The complete inventory, matching `crates/protocol/src/verbs.rs` 1:1.

| `type` | Payload |
|---|---|
| `hello_ok` | `HelloResponse` |
| `pong` | `{ version }` |
| `ok` | `{ version }` |
| `error` | `ProtocolError { code, message }` |
| `status` | `{ state, pid?, exit_code?, started_at?, config_hash? }` |
| `wait` | `{ exit_code, exited_at_ms, state }` |
| `sandbox` | `SandboxInfo` |
| `list` | `SandboxList` |
| `exec_result` | `ExecResult` |
| `fs_data` | `{ data_base64, eof }` |
| `fs_content` | `{ content }` |
| `fs_entries` | `{ entries: [FsEntry] }` |
| `fs_stat` | `FsStat` |
| `fs_exists` | `{ exists }` |
| `metrics` | `Metrics` |
| `logs` | `{ lines: [LogLine], next_cursor? }` |
| `volumes` | `{ volumes: [VolumeInfo] }` |
| `volume` | `VolumeInfo` |
| `secrets` | `{ secrets: [SecretEntry] }` |
| `snapshot` | `SnapshotInfo` |
| `snapshots` | `{ snapshots: [SnapshotInfo] }` |
| `guest` | `GuestEvent` |
| `event` | `StreamEvent` |

### 4.3 Request payload structs

These are the body types referenced above.

**`ExecRequest`** — `{ name?, command, args, cwd?, env, user?, shell,
timeout_secs?, stdin?, rlimits, tty?, cols?, rows? }`. `shell: true` runs the
command through a shell. `tty: true` allocates a PTY (initial size `cols`×
`rows`) and makes `exec_stream` a bidirectional session (see §6).

**`ShellRequest`** — `{ name?, script, cwd?, env, user?, timeout_secs? }`.

**`FsPathSpec`** — `{ name?, path, offset?, length? }`.

**`FsWriteSpec`** — `{ name?, path, data_base64, append, mode? }`.

**`FsWriteStreamSpec`** — `{ name?, path, append, mode? }`. The opening frame of
a streamed write; `fs_data` frames follow.

**`FsMkdirSpec`** — `{ name?, path, recursive, mode? }`.

**`FsRemoveDirSpec`** — `{ name?, path, recursive }`.

**`FsCopySpec`** — `{ name?, from, to }` (used by both `fs_copy` and
`fs_rename`).

**`FsTransferSpec`** — `{ name?, host_path, guest_path }` (used by
`fs_copy_from_host` / `fs_copy_to_host`).

**`LogsSpec`** — `{ name?, follow, tail?, cursor?, source? }`.

**`VolumeCreateSpec`** — `{ name?, volume: VolumeSpec }`.

**`VolumeRef`** — `{ name?, volume }`.

**`MountAddSpec`** — `{ name?, mount: MountSpec }`.

**`SecretSetSpec`** — `{ name?, key, value }`.

**`SecretRef`** — `{ name?, key }`.

**`SnapshotCreateSpec`** — `{ name?, dest?, include_memory }`.

**`SnapshotRestoreSpec`** — `{ snapshot, name }` (used by both
`snapshot_restore` and `snapshot_fork`).

## 5. Shared message types

Defined in `crates/protocol/src/types.rs`. These are language-neutral and do not
depend on the runtime crate.

- **`SandboxState`** — `starting` | `running` | `stopped` | `error`.
- **`SandboxInfo`** — `{ name, state, image, pid?, exit_code?, started_at?,
  config_hash?, detached, ephemeral, labels }`.
- **`ExecResult`** — `{ exit_code, stdout, stderr, duration_ms }`.
- **`Stream`** — `stdout` | `stderr`.
- **`FsEntry`** — `{ name, path, is_dir, is_symlink, size, mode? }`.
- **`FsStat`** — `{ path, is_dir, is_symlink, size, mode?, modified_ms? }`.
- **`Metrics`** — `{ cpu_percent, memory_used_bytes, memory_total_bytes,
  disk_used_bytes, disk_total_bytes, net_rx_bytes, net_tx_bytes, timestamp_ms }`.
- **`LogLine`** — `{ cursor, timestamp_ms, source, data }`. `cursor` is
  monotonic; the client passes it back to resume after this line.
- **`LogSource`** — `stdout` | `stderr` | `pty` | `system`.
- **`MountSpec`** — `{ host_path, guest_path, readonly, mount_type, file, tmpfs,
  disk_image?, options, uid?, gid? }`.
- **`MountType`** — `bind` | `virtiofs` | `tmpfs`.
- **`PortPublish`** — `{ host_port, container_port, protocol, bind_address? }`.
  `protocol` defaults to `"tcp"`.
- **`NetworkSpec`** — `{ enabled, mode?, scope?, publish, dns, dns_zones,
  policy? }`. `enabled` defaults to `true` (sandbox egress on). This is the
  Epic 1 **config surface**; enforcement is Epic 3.
- **`Rlimit`** — `{ resource, soft?, hard? }`.
- **`PullPolicy`** — `always` | `missing` | `never`.
- **`VolumeSpec`** — `{ name, dir?, disk_image?, fstype?, quota_bytes? }`.
- **`VolumeInfo`** — `{ name, dir?, disk_image?, fstype?, used_bytes }`.
- **`SecretEntry`** — `{ key, value? }`. Values are never returned in responses
  and never persisted.
- **`SnapshotInfo`** — `{ id, sandbox, image?, path?, hash?, created_at_ms,
  includes_memory }`.
- **`ModifySpec`** — `{ idle_timeout_secs?, max_duration_secs?, labels }`.
- **`SandboxSpec`** — the full creation spec: `{ name?, image, cpus?,
  memory_mb?, env, mounts, network, workdir?, timeout_secs?, user?, home?,
  command?, command_args, rlimits, labels, ephemeral, pull_policy, scripts,
  detached, replace, hostname?, secret_env }`.
- **`SandboxList`** — `{ sandboxes: [SandboxInfo], next_cursor? }`.
- **`GuestEvent`** — `{ event: started | stdout | stderr | exit | error }`:
  `started { pid }`, `stdout { data }`, `stderr { data }`, `exit { code }`,
  `error { message }`.

## 6. Streaming semantics

Streaming verbs receive an opening ack, then frames until a terminal frame.

The streaming verbs are: `exec_stream`, `attach`, `metrics_stream`, `logs` with
`follow: true`, and `fs_write_stream` (+ its `fs_data` body frames).

The contract:

1. The client sends the request.
2. The helper answers an opening ack `{"type":"ok","version":"1.0.0"}` (or an
   `error`).
3. The helper then emits frames until a **terminal frame**:
   - `{"type":"guest","event":"exit"|"error",...}` for exec/attach streams.
   - `{"type":"event","event":"done"}` for logs-follow / metrics_stream.
4. The client must not send another request on the same connection until the
   stream has terminated.

**Interactive carve-out.** A tty `exec_stream` (`ExecRequest.tty: true`) is
bidirectional: after the opening ack the client may interleave `exec_input`
(live stdin) and `exec_resize` (PTY size) request frames, each answered with an
`ok` ack, until the terminal `guest exit`/`error` frame. This is the same
client→server-frame pattern already used by `fs_write_stream`. These frame
types are only valid during a tty `exec_stream`; sent standalone they answer
`invalid_state`.

### Exit-code contract

A sandbox is `starting → running → (stopped | error)`. A **terminal** state
(`stopped`/`error`) always carries an `exit_code` and an exit timestamp; the
`wait` response and the persisted `state.json` report both. A supervisor or VM
that dies without a clean exit is reconciled to a deterministic terminal code
(`128 + signal`, e.g. `137` for `SIGKILL`; `255` when unknowable) — a sandbox is
never left stuck in `running`.

`fs_write_stream` is the inverse: the client sends the opening request, reads
the ack, then sends one `fs_data` frame per chunk (each answered with an `ok`
ack), the last with `eof: true`.

### `StreamEvent`

A generic stream event, tagged by `event`:

| `event` | Payload |
|---|---|
| `log` | `LogLine` |
| `metrics` | `Metrics` |
| `done` | — (the stream ended cleanly) |

## 7. Error codes

Defined in `crates/protocol/src/error.rs`. `ProtocolError` is
`{ code, message }`; `code` is a stable, language-neutral string SDKs may branch
on.

| Code | Meaning |
|---|---|
| `bad_request` | The request could not be parsed / is malformed. |
| `incompatible_version` | The protocol version is incompatible. |
| `not_found` | The named sandbox does not exist. |
| `invalid_state` | The sandbox exists but is not in a state to serve the request. |
| `unsupported` | The requested verb is not supported by this helper build. |
| `unauthorized` | Authentication / authorisation failure. |
| `timeout` | A timeout elapsed. |
| `transport` | The underlying transport (socket) failed. |
| `io` | An I/O or filesystem error occurred. |
| `internal` | Anything else; see `message`. |

## 8. Guest filesystem frames

Defined in `crates/protocol/src/guest_fs.rs`. These ride the same
length-prefixed JSON transport as exec, so `fs` works even with networking
disabled. Both the in-guest exec-agent and the host (`runtime::exec`) use this
single definition.

### `FsRequest` (tagged by `type`)

| `type` | Payload |
|---|---|
| `fs_read` | `{ path, offset?, length? }` |
| `fs_write` | `{ path, data_base64, append, mode? }` |
| `fs_write_stream_open` | `{ path, append, mode? }` — opens a streamed write; the agent keeps the handle open across the following `fs_write_chunk` frames until `fs_write_close`. |
| `fs_write_chunk` | `{ data_base64 }` — one chunk of a streamed write. |
| `fs_write_close` | — flush and close the open streamed write. |
| `fs_list` | `{ path }` |
| `fs_stat` | `{ path }` |
| `fs_exists` | `{ path }` |
| `fs_mkdir` | `{ path, recursive, mode? }` |
| `fs_remove` | `{ path }` |
| `fs_remove_dir` | `{ path, recursive }` |
| `fs_copy` | `{ from, to }` |
| `fs_rename` | `{ from, to }` |

### `FsResponse` (tagged by `type`)

| `type` | Payload |
|---|---|
| `fs_data` | `{ data_base64 }` |
| `fs_entries` | `{ entries: [FsEntry] }` |
| `fs_stat` | `{ stat: FsStat }` |
| `fs_exists` | `{ exists }` |
| `ok` | — |
| `error` | `{ message }` |

## 9. Deliberate divergences

- **SSH is removed.** There is no `ssh` verb. Use `exec` / `attach` instead.
  SSH was a network-reachable surface that was deliberately dropped; `attach`
  over the console/vsock channel is arguably more secure because it exposes no
  network listener.
- **Networking enforcement is Epic 3.** This protocol carries only the
  networking **config surface** (`NetworkSpec`: published ports, DNS, policy
  passthrough). The enforcement engine (policy, allowlists, DNS/TLS
  interception, rate limits, interface overrides, host CA trust, host access,
  NAT64) is Epic 3 — Runtime Networking & Isolation.