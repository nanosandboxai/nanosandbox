# nanosandbox-sdk (Rust SDK)

The Rust SDK for the [nanosandbox](https://github.com/nanosandboxai/nanosandbox)
protocol helper. It re-exports the reference `nanosb-client` crate under the
stable `nanosandbox-sdk` name, mirroring the other language SDKs.

## Usage

```rust
use nanosandbox_sdk::{SandboxBuilder, SandboxClient};

let client = SandboxClient::local()?;
let sandbox = client
    .create_with(SandboxBuilder::new("alpine:latest").name("demo"))?;
let out = sandbox.exec("echo", &["hi"])?;
assert_eq!(out.stdout, "hi\n");
```

## Building

```bash
cargo build -p nanosandbox-sdk
```

## Testing

The SDK is covered by the `nanosb-client` unit suite (in-process fake helper,
no VM required):

```bash
cargo test -p nanosb-client
```