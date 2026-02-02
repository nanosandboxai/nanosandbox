# Windows Container Runtime Implementation

## Overview

This document describes the Windows Container runtime implementation for Nanosandbox, providing the same interface as Linux (KVM/libkrun) and macOS (HVF/krunvm).

## Architecture

Nanosandbox uses **one runtime per platform**:

| Platform | Runtime | Hypervisor | Isolation |
|----------|---------|------------|-----------|
| Linux | crun/krun | KVM | VM-based via libkrun |
| macOS | krunvm | HVF (Hypervisor.framework) | VM-based |
| **Windows** | **Windows Containers (HCS)** | **Hyper-V / Process** | **Container-based** |

## Windows Container Runtime

### Technology Stack

Windows Containers use the Host Compute Service (HCS), which provides:

- **Process Isolation**: Containers share the host kernel (faster, less isolation)
- **Hyper-V Isolation**: Each container runs in a lightweight VM (stronger isolation)

### Prerequisites

Before using Nanosandbox on Windows, ensure the following:

#### 1. Enable Windows Containers Feature

```powershell
# Run as Administrator
Enable-WindowsOptionalFeature -Online -FeatureName Containers -All

# Restart if required
Restart-Computer
```

#### 2. Enable Hyper-V (Optional, for Hyper-V Isolation)

```powershell
# Run as Administrator
Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All

# Restart if required
Restart-Computer
```

#### 3. Start HCS Service

```powershell
# Start the Host Compute Service
Start-Service vmcompute

# Verify it's running
Get-Service vmcompute
```

#### 4. Install Container Tools

Install Docker Desktop for Windows, or manually install `runhcs.exe`:

```powershell
# Option 1: Docker Desktop (recommended)
# Download from https://www.docker.com/products/docker-desktop

# Option 2: Install containerd + runhcs manually
# See https://github.com/microsoft/hcsshim
```

### Validation

Nanosandbox validates all prerequisites before runtime initialization:

```rust
use nanosandbox::runtime::validate_runtime_prerequisites;

#[tokio::main]
async fn main() {
    match validate_runtime_prerequisites().await {
        Ok(()) => println!("All prerequisites met!"),
        Err(e) => {
            eprintln!("Prerequisites not met:");
            eprintln!("{}", e);
            // Error message includes installation instructions
        }
    }
}
```

### Isolation Modes

#### Process Isolation (Default)

- Faster startup and execution
- Lower resource overhead
- Containers share the host kernel
- Requires matching OS version between host and container

```rust
use nanosandbox::runtime::WindowsContainerRuntime;

let runtime = WindowsContainerRuntime::new().await?;
// Uses process isolation by default
```

#### Hyper-V Isolation

- Stronger security isolation
- Each container runs in a lightweight VM
- Supports different OS versions
- Requires Hyper-V feature enabled

```rust
use nanosandbox::runtime::{WindowsContainerRuntime, WindowsIsolation};

let runtime = WindowsContainerRuntime::with_isolation(WindowsIsolation::HyperV).await?;
```

## Windows Container Images

Windows containers require Windows-specific images:

```bash
# Windows Server Core
mcr.microsoft.com/windows/servercore:ltsc2022

# Windows Nano Server (smaller)
mcr.microsoft.com/windows/nanoserver:ltsc2022

# .NET Runtime
mcr.microsoft.com/dotnet/runtime:8.0-nanoserver-ltsc2022
```

### Image Compatibility

| Host OS | Process Isolation | Hyper-V Isolation |
|---------|-------------------|-------------------|
| Windows Server 2022 | LTSC2022 images only | Any Windows image |
| Windows 11 | Matching version only | Any Windows image |
| Windows 10 | Matching version only | Any Windows image |

## OCI Configuration

Windows containers use a different OCI runtime specification:

```json
{
  "ociVersion": "1.0.2",
  "process": {
    "terminal": false,
    "user": {
      "username": "ContainerUser"
    },
    "args": ["cmd.exe", "/c", "echo Hello"],
    "env": ["PATH=C:\\Windows\\system32;..."],
    "cwd": "C:\\app"
  },
  "root": {
    "path": "rootfs"
  },
  "windows": {
    "layerFolders": ["C:\\layers\\base"],
    "resources": {
      "memory": { "limit": 1073741824 },
      "cpu": { "count": 2 }
    },
    "hyperv": {}  // Present only for Hyper-V isolation
  }
}
```

## Error Handling

The runtime provides specific error types for Windows:

| Error | Cause | Fix |
|-------|-------|-----|
| `WindowsContainersNotEnabled` | Containers feature not enabled | `Enable-WindowsOptionalFeature -Online -FeatureName Containers -All` |
| `HyperVNotEnabled` | Hyper-V not enabled (for Hyper-V isolation) | `Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All` |
| `HcsNotRunning` | HCS service not running | `Start-Service vmcompute` |
| `RuntimeBinaryNotFound` | runhcs.exe not found | Install Docker Desktop or containerd |

## Usage Example

```rust
use nanosandbox::{Sandbox, SandboxConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create sandbox configuration
    let config = SandboxConfig::builder()
        .name("my-windows-sandbox")
        .image("mcr.microsoft.com/windows/nanoserver:ltsc2022")
        .cpus(2)
        .memory_mb(2048)
        .workdir("C:\\app")
        .build();

    // Create and start sandbox
    let mut sandbox = Sandbox::create(config).await?;
    sandbox.start().await?;

    // Execute a command
    let result = sandbox.exec("cmd.exe", &["/c", "echo Hello from Windows!"]).await?;
    println!("Output: {}", result.stdout);

    // Clean up
    sandbox.destroy().await?;
    Ok(())
}
```

## CLI Usage

```powershell
# Pull a Windows image
nanosb.exe pull mcr.microsoft.com/windows/nanoserver:ltsc2022

# Run a command in a new sandbox
nanosb.exe run mcr.microsoft.com/windows/nanoserver:ltsc2022 cmd.exe /c "echo Hello"

# List cached images
nanosb.exe images

# List running sandboxes
nanosb.exe ps
```

## Testing

Run the Windows E2E tests:

```powershell
# Run all Windows tests
cargo test --test windows_e2e_test --features cli

# Run with output
cargo test --test windows_e2e_test --features cli -- --nocapture

# Run full integration test (requires containers feature)
cargo test --test windows_e2e_test test_full_integration --features cli -- --ignored
```

## Comparison with Linux/macOS

| Feature | Linux | macOS | Windows |
|---------|-------|-------|---------|
| Runtime | crun/krun | krunvm | Windows Containers |
| Hypervisor | KVM | HVF | Hyper-V / Process |
| Image format | Linux OCI | Linux OCI | Windows OCI |
| Boot time | ~100-500ms | ~100-500ms | ~1-5s |
| Isolation | VM | VM | Container/VM |

## References

- [Windows Containers Documentation](https://docs.microsoft.com/en-us/virtualization/windowscontainers/)
- [HCS (Host Compute Service)](https://docs.microsoft.com/en-us/virtualization/api/)
- [runhcs](https://github.com/microsoft/hcsshim/tree/main/cmd/runhcs)
- [OCI Runtime Spec - Windows](https://github.com/opencontainers/runtime-spec/blob/main/config-windows.md)
