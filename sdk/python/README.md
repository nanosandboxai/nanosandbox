# nanosandbox — Python SDK

A **stdlib-only** Python protocol client for the nanosandbox helper
(`nanosb-protocol` v1.0.0). It speaks NDJSON over a Unix stream socket using only
the standard library (`socket`, `json`, `base64`) — no third-party dependencies.

The client surface mirrors the Rust reference client
(`crates/client/src/client.rs`) and the driver contract in
[`sdk/conformance/SPEC.md`](../conformance/SPEC.md). It passes the shared
conformance suite (35/35).

## Requirements

- Python 3.9+

## Install

```bash
pip install nanosandbox-sdk
```

For local development, install from the repo:

```bash
pip install ./sdk/python          # or: pip install -e ./sdk/python
```

## Usage

```python
from nanosandbox import SandboxClient, SandboxBuilder

# Connect to the helper socket (performs the hello handshake).
client = SandboxClient.connect("/path/to/helper.sock")
client.supports("exec")  # True | False

# Create a sandbox with the fluent builder.
spec = (
    SandboxBuilder("library/alpine:latest")
    .with_name("demo")
    .with_resources(2, 512)
    .publish_tcp(8080, 80)
    .with_label("team", "sdk")
    .build()
)
box = client.create(spec)

# Lifecycle.
box.start()
box.state()               # "running"

# Buffered exec.
out = box.exec("echo", ["hi"])          # {"exit_code": 0, "stdout": "hi\n", ...}

# Streamed exec: opening `ok`, then guest frames until `exit`/`error`.
code = box.exec_stream({"command": "echo", "args": ["hi"]},
                       lambda ev: print(ev.get("data"), end="") if ev["event"] == "stdout" else None)

# Filesystem.
box.fs_write("/tmp/x", b"hello")
box.fs_read_to_string("/tmp/x")          # "hello"
box.fs_read("/tmp/x")                    # b"hello"
box.fs_exists("/tmp/x")                  # True
box.fs_mkdir("/tmp/dir", recursive=True)
box.fs_stat("/tmp/dir")["stat"]["is_dir"]  # True

# Logs (buffered + follow) and metrics (+ stream).
lines = box.logs_with(tail=10, cursor=0, source="stdout")   # list of log lines
box.metrics()["disk_used_bytes"]
box.metrics_stream(100, lambda sample: print(sample["cpu_percent"]))

# Volumes, secrets, snapshots.
box.volume_create("data", "/host/volumes/data")
box.secret_set("API_KEY", "s3cr3t")
snap = box.snapshot_create()

# Typed errors.
from nanosandbox import NanosbError
try:
    client.get("missing")
except NanosbError as e:
    e.code            # "not_found"
    e.is_not_found()  # True
```

Every call dials a fresh connection and re-negotiates the `hello` handshake
(stateless). Streaming verbs (`exec_stream`, `attach`, `logs` with `follow`,
`metrics_stream`, `fs_write_stream`) hold one connection for the stream: an
opening `ok` ack, then frames until the terminal frame (`guest exit/error` or
`event done`).

## Managing a real sandbox (`ManagedRuntime`)

`ManagedRuntime` locates and starts the real `nanosb-runtime` helper so you can
boot and manage microVMs without launching it yourself:

```python
from nanosandbox import ManagedRuntime, SandboxClient

with ManagedRuntime() as rt:
    client = SandboxClient.connect(str(rt.socket))
    box = client.create({"image": "library/alpine:latest", "name": "demo"})
    print(box.exec("echo", ["hello from a microVM"])["stdout"])
```

The helper is found via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime`
→ `PATH` → the repo `target/debug/nanosb-runtime`.

If the helper is missing, `ManagedRuntime` auto-provisions it: it runs the shared
`ensure-runtime.sh` provisioner (located via `NANOSB_PROVISIONER` → the repo
`scripts/sdk/ensure-runtime.sh`, or an embedded copy shipped with the package),
which downloads, verifies, and installs the runtime into `$NANOSB_HOME`, pinned
to the SDK's release version. So `pip install nanosandbox-sdk` is the only step
needed. Config comes from the environment: `NANOSB_HOME`, `NANOSB_VERSION`
(required to download), `NANOSB_RUNTIME_URL`, `NANOSB_RUNTIME_SHA256`,
`NANOSB_OFFLINE=1` to never download, and `NANOSB_PROVISIONER`.

## Testing

Unit tests (in-process fake helper, no VM):

```bash
python3 -m unittest discover -s sdk/python/tests
```

Conformance driver — all 35 scenarios in `sdk/conformance/scenarios/`:

```bash
python3 sdk/python/conformance/run.py
# NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance python3 sdk/python/conformance/run.py
```

Live microVM E2E (opt-in; needs the helper + libkrun/gvproxy + a cached image):

```bash
NANOSB_E2E=1 python3 sdk/python/tests/test_e2e.py
```

## Layout

```
sdk/python/
├── pyproject.toml              # package metadata
├── nanosandbox/
│   ├── client.py               # NanosbError, Conn, Sandbox, SandboxBuilder, SandboxClient
│   └── runtime.py              # ManagedRuntime
├── tests/
│   ├── test_client.py          # unit tests + fake NDJSON helper
│   └── test_e2e.py             # opt-in live microVM smoke
└── conformance/
    └── run.py                  # conformance driver
```