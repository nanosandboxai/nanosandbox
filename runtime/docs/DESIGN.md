# Nanosandbox Design Document

## Executive Summary

Nanosandbox is a Rust SDK for managing VM-isolated sandboxes using libkrun via direct FFI. It provides a simple, async API for creating and managing secure execution environments with full OCI image support.

## Goals

1. **Security**: VM-level isolation for untrusted code execution
2. **Compatibility**: Support any OCI image from any registry
3. **Performance**: Sub-second boot times, minimal overhead
4. **Simplicity**: Easy-to-use Rust SDK with async/await
5. **Self-Hosted**: No external service dependencies

## Non-Goals

1. Kubernetes integration (use DD-Code's Kubernetes runtime instead)
2. Windows support (libkrun doesn't support Windows)
3. Nested virtualization support

## Technical Design

### 1. Core Components

#### 1.1 Sandbox Struct

```rust
pub struct Sandbox {
    id: String,
    config: SandboxConfig,
    runtime: Runtime,
    status: SandboxStatus,
}

impl Sandbox {
    /// Create a new sandbox from an OCI image
    pub async fn create(config: SandboxConfig) -> Result<Self>;
    
    /// Start the sandbox
    pub async fn start(&mut self) -> Result<()>;
    
    /// Execute a command
    pub async fn exec(&self, cmd: &str, args: &[&str]) -> Result<ExecResult>;
    
    /// Execute with streaming output
    pub async fn exec_stream<F>(&self, cmd: &str, on_output: F) -> Result<i32>
    where
        F: FnMut(OutputChunk) + Send;
    
    /// Stop the sandbox
    pub async fn stop(&mut self) -> Result<()>;
    
    /// Destroy the sandbox
    pub async fn destroy(self) -> Result<()>;
}
```

#### 1.2 SandboxConfig

```rust
pub struct SandboxConfig {
    /// Unique name for the sandbox
    pub name: String,
    
    /// OCI image reference (e.g., "ghcr.io/devdone-labs/agents-registry/claude:latest")
    pub image: String,
    
    /// CPU cores to allocate
    pub cpus: u32,
    
    /// Memory in MB
    pub memory_mb: u32,
    
    /// Mount points (host:container)
    pub mounts: Vec<Mount>,
    
    /// Environment variables
    pub env: HashMap<String, String>,
    
    /// Network configuration
    pub network: NetworkConfig,
    
    /// Working directory inside sandbox
    pub workdir: String,
}
```

#### 1.3 ImageManager

```rust
pub struct ImageManager {
    /// Cache directory for layers
    cache_dir: PathBuf,
    /// Registry clients
    registries: HashMap<String, RegistryClient>,
}

impl ImageManager {
    /// Pull an image from a registry
    pub async fn pull(&self, image: &str) -> Result<ImageRef>;
    
    /// Check if image exists locally
    pub async fn exists(&self, image: &str) -> bool;
    
    /// List cached images
    pub async fn list(&self) -> Vec<ImageInfo>;
    
    /// Remove a cached image
    pub async fn remove(&self, image: &str) -> Result<()>;
    
    /// Get image manifest
    pub async fn inspect(&self, image: &str) -> Result<ImageManifest>;
}
```

### 2. Runtime Integration

#### 2.1 libkrun FFI

Nanosandbox calls libkrun's C API directly via FFI for VM management:

```
nanosandbox-sdk
      │
      ▼
   libkrun FFI (direct C API calls)
      │
      ▼
   libkrun (VMM + virtio)
      │
      ▼
   KVM/HVF (hypervisor)
```

#### 2.2 OCI Bundle Creation

For each sandbox, we create an OCI bundle:

```
/tmp/nanosandbox/{sandbox-id}/
├── config.json          # OCI runtime spec
├── rootfs/             # Extracted image layers
│   ├── bin/
│   ├── lib/
│   ├── usr/
│   └── ...
└── state.json          # Runtime state
```

### 3. Image Management

#### 3.1 Registry Protocol

Support OCI Distribution Spec for pulling images:

1. Authenticate with registry (if required)
2. Fetch manifest by tag/digest
3. Download layer blobs
4. Cache layers by digest (content-addressable)
5. Extract layers to create rootfs

#### 3.2 Layer Caching

```
~/.nanosandbox/
├── cache/
│   └── blobs/
│       ├── sha256:abc123...  # Layer blob
│       └── sha256:def456...
├── images/
│   └── ghcr.io/
│       └── devdone-labs/
│           └── dd-agents/
│               └── latest -> sha256:...
└── sandboxes/
    └── {sandbox-id}/
        └── rootfs/
```

### 4. Networking

#### 4.1 TSI (Transparent Socket Impersonation)

Default networking mode using libkrun's TSI:

- No virtual network interface needed
- Outbound connections work transparently
- Inbound connections via port forwarding

#### 4.2 Network Isolation

Optional modes:
- `none` - No network access
- `tsi` - TSI networking (default)
- `bridge` - Virtual bridge network (requires passt/gvproxy)

### 5. Security Considerations

#### 5.1 Threat Model

- Sandboxed code is untrusted
- Host filesystem is protected via VM isolation
- Network access controlled by configuration
- Resource limits enforced by hypervisor

#### 5.2 Isolation Guarantees

| Resource | Protection |
|----------|------------|
| Memory | Separate VM address space |
| CPU | Hardware scheduling |
| Filesystem | virtio-fs with path restrictions |
| Network | TSI or isolated virtual network |
| Devices | No device passthrough by default |

### 6. Platform Support

#### 6.1 Linux

- Requires KVM support (`/dev/kvm`)
- Tested on: Ubuntu 22.04+, Fedora 38+, Debian 12+
- Architectures: x86_64, aarch64

#### 6.2 macOS

- Requires Apple Silicon (M1/M2/M3/M4)
- Uses Hypervisor.framework (HVF)
- macOS 14.0+ required

### 7. API Examples

#### 7.1 Basic Usage

```rust
use nanosandbox::{Sandbox, SandboxConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // Create sandbox
    let config = SandboxConfig::builder()
        .name("my-sandbox")
        .image("ghcr.io/devdone-labs/agents-registry/claude:latest")
        .cpus(2)
        .memory_mb(4096)
        .build();
    
    let mut sandbox = Sandbox::create(config).await?;
    sandbox.start().await?;
    
    // Execute command
    let result = sandbox.exec("python", &["-c", "print('Hello!')"]).await?;
    println!("Output: {}", result.stdout);
    
    // Cleanup
    sandbox.destroy().await?;
    Ok(())
}
```

#### 7.2 Streaming Output

```rust
use nanosandbox::{Sandbox, OutputChunk};

let sandbox = Sandbox::create(config).await?;
sandbox.start().await?;

sandbox.exec_stream("long-running-command", |chunk| {
    match chunk.stream {
        Stream::Stdout => print!("{}", chunk.data),
        Stream::Stderr => eprint!("{}", chunk.data),
    }
}).await?;
```

#### 7.3 With Mounts

```rust
let config = SandboxConfig::builder()
    .name("dev-sandbox")
    .image("node:20")
    .mount("/home/user/project", "/workspace")
    .workdir("/workspace")
    .build();

let sandbox = Sandbox::create(config).await?;
sandbox.exec("npm", &["install"]).await?;
sandbox.exec("npm", &["run", "build"]).await?;
```

## Implementation Milestones

### M1: Foundation
- [ ] Project structure and build system
- [ ] Basic OCI image pulling (single registry)
- [ ] Layer extraction and caching
- [ ] libkrun FFI integration

### M2: Sandbox Management
- [ ] Sandbox create/start/stop/destroy
- [ ] Command execution
- [ ] Output capture and streaming
- [ ] Resource limits (CPU, memory)

### M3: Advanced Features
- [ ] Multiple registry support
- [ ] Image authentication
- [ ] virtio-fs mounts
- [ ] TSI networking

### M4: Production Ready
- [ ] Comprehensive testing
- [ ] Performance optimization
- [ ] Documentation
- [ ] CLI tool (nanosandbox-cli)

### M5: Extended Features
- [ ] nanosandbox-server (HTTP API)
- [ ] GPU passthrough
- [ ] Checkpoint/restore

## Dependencies

### Required

- `libkrun` - VM management (called via FFI)
- `libkrunfw` - Guest firmware

### Rust Crates

- `tokio` - Async runtime
- `oci-distribution` - OCI registry client
- `tar` - Layer extraction
- `serde` - Configuration serialization
- `thiserror` - Error handling

## Open Questions

1. **Snapshot support**: Priority for checkpoint/restore?
   - Useful for fast startup
   - Complex to implement correctly

3. **Multi-arch images**: Automatic platform selection?
   - Need to handle manifest lists
   - Platform detection on host

## References

- [OCI Runtime Spec](https://github.com/opencontainers/runtime-spec)
- [OCI Distribution Spec](https://github.com/opencontainers/distribution-spec)
- [libkrun Documentation](https://github.com/containers/libkrun)
