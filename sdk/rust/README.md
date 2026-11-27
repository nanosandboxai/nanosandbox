# nanosandbox-sdk (Rust SDK)

The Rust SDK for the [nanosandbox](https://github.com/nanosandboxai/nanosandbox)
protocol helper. It re-exports the reference `nanosb-client` crate under the
stable `nanosandbox-sdk` name, mirroring the other language SDKs.

## Install

Requires Rust 1.94 or newer (the release toolchain) and macOS Apple Silicon or
Linux x86_64/aarch64.

```bash
cargo add nanosandbox-sdk
```

## Quickstart (complete program)

The first run downloads the per-platform runtime bundle. Save this as
`src/main.rs` in a new crate and run `cargo run`:

```rust
use nanosandbox_sdk::{ManagedRuntime, Result, SandboxBuilder, SandboxClient};

fn main() -> Result<()> {
    let rt = ManagedRuntime::start().unwrap();
    let client = SandboxClient::connect(rt.socket())?;
    let sandbox = client
        .create_with(
            SandboxBuilder::new("library/alpine:latest")
                .name("demo")
                .cpus(1)
                .memory_mb(512),
        )?;

    sandbox.start()?;
    let out = sandbox.exec("echo", &["hello from a microVM"])?;
    print!("{}", out.stdout);

    sandbox.remove(true)?;
    drop(rt);
    Ok(())
}
```

Expected output:

```text
hello from a microVM
```

## Usage

The snippet below is a fragment; see the Quickstart for a complete program.

```rust
use nanosandbox_sdk::{SandboxBuilder, SandboxClient};

let client = SandboxClient::local()?;
let sandbox = client
    .create_with(SandboxBuilder::new("alpine:latest").name("demo"))?;
let out = sandbox.exec("echo", &["hi"])?;
assert_eq!(out.stdout, "hi\n");
```

## Managing a real sandbox (`ManagedRuntime`)

`ManagedRuntime::start()` locates and starts the real `nanosb-runtime` helper on
a private socket so you can boot and manage microVMs without launching it
yourself. Dropping the value kills the helper and removes its temp directory:

```rust
use nanosandbox_sdk::{ManagedRuntime, SandboxBuilder, SandboxClient};

let rt = ManagedRuntime::start()?;
let client = SandboxClient::connect(rt.socket())?;
let sandbox = client
    .create_with(SandboxBuilder::new("library/alpine:latest").name("demo"))?;
let out = sandbox.exec("echo", &["hello from a microVM"])?;
drop(rt);
```

The helper is found via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime`
→ `PATH` → the repo `target/debug/nanosb-runtime`.

If the helper is **not** found, `ManagedRuntime` auto-provisions it: it runs the
shared `ensure-runtime.sh` provisioner (located via `NANOSB_PROVISIONER` → the
repo `scripts/sdk/ensure-runtime.sh`, or an embedded copy shipped with this
crate), which downloads, verifies, and installs the per-platform runtime bundle
under `$NANOSB_HOME`, pinned to the SDK's release version. Config comes from the
environment: `NANOSB_HOME`, `NANOSB_VERSION` (required to download),
`NANOSB_RUNTIME_URL`, `NANOSB_RUNTIME_SHA256`, `NANOSB_OFFLINE=1` to never
download, and `NANOSB_PROVISIONER`.

## Building

```bash
cargo build -p nanosandbox-sdk
```

## Headless code agents

Run Claude, Codex, or another coding agent in the sandbox instead of Alpine:
see [Headless Code Agents](https://nanosandbox.ai/docs/sdk/agents) and the
shared example in [`../README.md`](../README.md).

## Testing

The SDK is covered by the `nanosb-client` unit suite (in-process fake helper,
no VM required) plus unit tests for the `ManagedRuntime` resolution logic:

```bash
cargo test -p nanosb-client
cargo test -p nanosandbox-sdk
```