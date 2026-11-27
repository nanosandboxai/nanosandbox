# nanosandbox — C#/.NET SDK

A **BCL-only** .NET 8 protocol client for the nanosandbox helper
(`nanosb-protocol` v1.0.0). It speaks NDJSON over a Unix stream socket using only
the .NET base class library (`System.Net.Sockets` + `System.Text.Json` framing,
with a small `JsonValue` model) — no third-party runtime packages. MSTest is used
for tests only.

The client surface mirrors the other SDKs and the driver contract in
[`sdk/conformance/SPEC.md`](../conformance/SPEC.md). It passes the shared
conformance suite (40/40).

## Requirements

- .NET SDK 8.0+ (targets `net8.0`).

## Install

```bash
dotnet add package Nanosandbox.Sdk
```

## Layout

```
sdk/dotnet/
├── dotnet.csproj                    # net8.0 test project (MSTest) — compiles the SDK + tests
├── README.md
├── nanosandbox/
│   ├── Json.cs                      # JsonValue: minimal JSON value type + parser/serializer
│   ├── Base64.cs                    # Base64 codec
│   ├── Conn.cs                      # NanosbError, Conn (Unix socket + NDJSON + hello handshake)
│   ├── Client.cs                    # SandboxClient (stateless, dial-per-call)
│   ├── Builders.cs                  # SandboxBuilder, VolumeBuilder
│   ├── Sandbox.cs                   # Sandbox (lifecycle/exec/fs/obs/volumes/secrets/snapshots) + AttachSession
│   └── ManagedRuntime.cs            # locate + spawn nanosb-runtime serve
├── tests/
│   ├── JsonTest.cs                  # unit tests (JSON round-trip)
│   └── E2ETest.cs                   # opt-in live microVM smoke
└── conformance/
    ├── conformance.csproj           # exe
    ├── Run.cs                       # conformance driver
    └── Driver.cs                    # step dispatch + assert evaluation
```

## Quickstart (complete program)

Requires .NET SDK 8.0+; the first run downloads the runtime bundle. In a console
project with the package installed:

```csharp
using System;
using System.Collections.Generic;

using var rt = ManagedRuntime.start();
var client = SandboxClient.connect(rt.Socket);
var box = client.createWith(new SandboxBuilder("library/alpine:latest").withName("demo"));

box.start();
Console.Write(box.exec("echo", new List<string> { "hello from a microVM" }).str("stdout"));
box.remove(true);
```

Expected output:

```text
hello from a microVM
```

## Usage

```csharp
using System;
using System.Collections.Generic;

// Connect to the helper socket (performs the hello handshake).
var client = SandboxClient.connect("/path/to/helper.sock");
client.supports("exec"); // true | false

// Create a sandbox with the fluent builder.
var box = client.createWith(new SandboxBuilder("library/alpine:latest")
    .withName("demo").withResources(2, 512).publishTcp(8080, 80).withLabel("team", "sdk"));

// Lifecycle.
box.start();
var state = box.state().str("state"); // "running"

// Buffered exec.
var outv = box.exec("echo", new List<String> { "hi" });
outv.intVal("exit_code"); // 0
outv.str("stdout");       // "hi\n"

// Streamed exec: opening `ok`, then guest frames until `exit`/`error`.
var code = box.execStream(JsonValue.makeObject(), ev => {
    if (ev.str("event") == "stdout") Console.Write(ev.str("data"));
});
// code == 0

// Filesystem.
box.fsWrite("/tmp/x", System.Text.Encoding.UTF8.GetBytes("hello"), false);
var text = box.fsReadToString("/etc/hostname");
var data = box.fsRead("/tmp/x");
box.fsExists("/tmp/x");
box.fsMkdir("/tmp/dir", true);
var stat = box.fsStat("/tmp/dir");
var inner = stat.has("stat") ? stat.get("stat") : stat; // is_dir at inner.boolVal("is_dir")

// Logs (buffered + follow) and metrics (+ stream).
var lines = box.logsWith(10, -1, null).arr("lines");
box.metrics().intVal("disk_used_bytes");
box.metricsStream(100, sample => Console.WriteLine(sample.num("cpu_percent")));

// Volumes, secrets, snapshots.
box.volumeCreate("data", "/host/volumes/data");
box.secretSet("API_KEY", "s3cr3t");
var snap = box.snapshotCreate(null); // dest is optional (null omits it)

// Typed errors.
try {
    client.get("missing");
} catch (NanosbError e) {
    e.errorCode;     // "not_found"
    e.isNotFound();  // true
}
```

Every call dials a fresh connection and re-negotiates the `hello` handshake
(stateless). Streaming verbs (`exec_stream`, `attach`, `logs` with `follow`,
`metrics_stream`, `fs_write_stream`) hold one connection for the stream: an
opening `ok` ack, then frames until the terminal frame (`guest exit/error` or
`event done`).

## Managing a real sandbox (`ManagedRuntime`)

`ManagedRuntime.start()` locates and starts the real `nanosb-runtime` helper so
you can boot and manage microVMs without launching it yourself. If the helper is
not installed, `start()` **auto-provisions** it: it runs the shared
`ensure-runtime.sh` provisioner, which downloads the per-platform runtime bundle
(nanosb-runtime + libkrunfw + gvproxy + exec-agent) from the release, verifies
it, and installs it under `$NANOSB_HOME`, pinned to the SDK's release version.
This makes `dotnet add package Nanosandbox.Sdk` the only step needed.

```csharp
using (var rt = ManagedRuntime.start()) {
    var client = SandboxClient.connect(rt.Socket);
var box = client.createWith(new SandboxBuilder("library/alpine:latest").withName("demo").withResources(1, 512));
    Console.WriteLine(box.exec("echo", new List<String> { "hello from a microVM" }).str("stdout"));
}
```

The helper is found via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime`
→ `PATH` → the repo `target/debug/nanosb-runtime`. When none exists, the
provisioner is resolved from `NANOSB_PROVISIONER`, then
`scripts/sdk/ensure-runtime.sh` relative to the package/repo, then an embedded
copy shipped with the SDK. Provisioning config is read from the environment:
`NANOSB_HOME` (install root), `NANOSB_VERSION` (version to download; defaults to
the SDK's own version, e.g. `0.3.0`), `NANOSB_RUNTIME_URL` (release base URL),
`NANOSB_RUNTIME_SHA256` (optional bundle checksum), and `NANOSB_OFFLINE=1`
(never download).

## Headless code agents

Run Claude, Codex, or another coding agent in the sandbox instead of Alpine:
see [Headless Code Agents](https://nanosandbox.ai/docs/sdk/agents) and the
shared example in [`../README.md`](../README.md).

## Testing

Unit tests (no VM required):

```bash
dotnet test sdk/dotnet
```

Conformance driver — all 40 scenarios in `sdk/conformance/scenarios/`:

```bash
dotnet run --project sdk/dotnet/conformance
# NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance dotnet run --project sdk/dotnet/conformance
```

Live microVM E2E (opt-in; needs the helper + libkrun/gvproxy + a cached image):

```bash
NANOSB_E2E=1 dotnet test sdk/dotnet --filter E2ETest
```

Expected conformance output: `40 passed, 0 failed`, exit code 0.