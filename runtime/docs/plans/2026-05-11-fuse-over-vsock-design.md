# FUSE-over-vsock for Windows HCS Filesystem Sharing — Design

Issue: [runtime#134](https://github.com/nanosandboxai/runtime/issues/134)

## Problem

On Windows, the runtime uses HCS (Host Compute Service) to create lightweight Hyper-V VMs. Unlike Linux/macOS where the VMM has full control over virtio devices, HCS is a black-box API that does not allow injecting custom virtio devices. The previous solution used HCS's built-in Plan9 (9P) file sharing to expose host directories to the guest VM.

Plan9 had critical limitations:

- **Broken symlinks**: NTFS doesn't preserve Unix symlinks, so OCI container images extracted on Windows lost `/bin -> usr/bin` and similar usr-merge symlinks
- **Wrong permissions**: 9P from Windows sets 0777 on all files (NTFS doesn't track Unix permissions), breaking sshd which requires 0600 on host keys
- **Performance overhead**: Plan9 involves HCS's built-in 9P server as an intermediary, adding latency
- **Protocol mismatch**: Linux/macOS use virtio-fs (FUSE protocol), while Windows used a completely different 9P protocol path, doubling the maintenance surface
- **Extensive workarounds**: `plan9_mount` needed `fix_usr_merge_symlinks()`, `fix_lib64()`, tmpfs overlays on `/etc/ssh` and `/root/.ssh`, and busybox as a fallback shell — all to compensate for NTFS/9P artifacts

## Chosen Approach: FUSE Protocol over AF_VSOCK

Tunnel the FUSE protocol over AF_VSOCK (HvSocket on Windows), reusing the existing `Server<PassthroughFs>` and `windows/passthrough.rs` code that was already built for virtio-fs but unused on Windows because HCS doesn't support custom virtio devices.

```
Host (Windows)                              Guest (Linux VM)
─────────────────                          ──────────────────
socket_worker.rs                           fuse_mount.rs
  └─ Server<PassthroughFs>                   └─ /dev/fuse bridge
     (existing FUSE code)    ←─ HvSocket ─→     (reads FUSE from kernel,
     windows/passthrough.rs     (AF_VSOCK)       relays to host over vsock)
```

### Why This Approach

- **Reuses existing code**: `Server<PassthroughFs>` + `windows/passthrough.rs` (~2700 lines) were already written and handle all FUSE operations via Windows API calls
- **No external dependencies**: No WinFSP, no additional drivers — everything is self-contained
- **Correct semantics**: The FUSE passthrough translates file operations properly, preserving symlinks, permissions, and other Unix attributes
- **Unified protocol**: Windows now uses the same FUSE protocol as Linux/macOS
- **HCS compatible**: AF_VSOCK/HvSocket is fully supported by HCS — the transport is proven by `vsock_proxy` which already uses the same pattern

### Alternatives Considered

- **WinFSP**: Designed for the opposite direction (making Windows apps see custom filesystems, not sharing files to Linux VMs). Also requires a separate GPLv3 installer.
- **Native HCS virtio-fs**: HCS does not expose custom virtio devices to third parties. Microsoft uses it internally for WSL2 but the API is not public.
- **Keep Plan9 with workarounds**: Maintenance burden, fundamentally broken symlinks/permissions.

## Architecture

### FUSE Protocol Self-Framing

The FUSE protocol is naturally self-framing, making it straightforward to tunnel over a byte stream:

```
Request:  [InHeader (40 bytes)][opcode-specific body][variable data]
          InHeader.len = total request size

Response: [OutHeader (16 bytes)][opcode-specific body][variable data]
          OutHeader.len = total response size
```

### Host Side: socket_worker.rs

A new module in `devices/src/virtio/fs/` that serves FUSE messages over any bidirectional byte stream:

```rust
pub fn serve_fuse_on_stream<S: Read + Write>(
    name: &str,
    root_dir: &str,
    stream: S,
    stop: &Arc<AtomicBool>,
) {
    let server = Server::new(PassthroughFs::new(config));
    loop {
        // 1. Read InHeader (40 bytes) from stream
        // 2. Read remaining body (InHeader.len - 40 bytes)
        // 3. Create Reader::from_buffer(request)
        // 4. Create Writer::from_buffer(response_buf)
        // 5. server.handle_message(reader, writer, ...)
        // 6. Write response (writer.bytes_written()) to stream
    }
}
```

Key design decisions:

- **Buffer-backed Reader/Writer**: New `Reader::from_buffer()` and `Writer::from_buffer()` constructors added to `descriptor_utils.rs`. These create `VolatileSlice`-backed buffers from raw byte slices, allowing the existing `Server::handle_message()` to work unmodified.
- **No DAX support**: `VirtioShmRegion` is `None` — no shared memory over vsock. The guest falls back to regular `READ`/`WRITE` ops which is fine for the socket transport.
- **Generic stream**: The function accepts any `Read + Write` type, making it testable with TCP sockets or in-memory buffers.

### Guest Side: fuse_mount.rs

A static Linux binary (cross-compiled for `x86_64-unknown-linux-musl`) that runs as PID 1 inside the guest VM. It replaces `plan9_mount` and:

1. Mounts basic filesystems (`/dev`, `/proc`, `/sys`)
2. Connects to the host FUSE server via `AF_VSOCK` port 50000
3. Opens `/dev/fuse` and mounts a FUSE filesystem at `/mnt`
4. Forks a bridge process that relays FUSE messages between `/dev/fuse` and vsock
5. Sets up the rootfs (SSH keys, essential mounts, workarounds)
6. Chroots into `/mnt` and execs the user command

The FUSE bridge loop:

```
loop {
    request = read(/dev/fuse)    // Kernel writes complete FUSE messages
    write_all(vsock, request)    // Send to host
    response = read(vsock)       // Read OutHeader, then body
    write_all(/dev/fuse, response) // Return to kernel
}
```

### Transport: HvSocket / AF_VSOCK

HCS maps AF_HYPERV (Windows) to AF_VSOCK (Linux guest). Port numbers are converted to GUIDs:

```
Port 50000 → GUID {0000C350-FACB-11E6-BD58-64006A7986D3}
Port 50002 → GUID {0000C352-FACB-11E6-BD58-64006A7986D3}
```

These are registered in the HCS VM config's `HvSocket.ServiceTable` to allow the guest to connect to the host.

### In-Guest Layer Extraction

For large OCI images, the host writes a `.nanosb-layers` manifest instead of extracting layers on NTFS. The guest opens a second FUSE connection on port 50002 to access the blobs directory and extracts layers into a tmpfs at `/mnt` using `busybox tar`.

### Kernel Cmdline Flags

| Flag | Value | Purpose |
|------|-------|---------|
| `nanosb.fuse_rootfs` | `1` | Signals FUSE rootfs mode to guest init scripts |
| `nanosb.extract_layers` | `1` | Enables in-guest OCI layer extraction |
| `nanosb.ssh_key` | `<base64>` | SSH public key for injection |
| `nanosb.exec` | `<base64>` | Base64-encoded exec config |
| `rdinit` | `/init.krun` | Points kernel to initramfs init script |

## Components Modified

### New Files

| File | Lines | Purpose |
|------|-------|---------|
| `devices/src/virtio/fs/socket_worker.rs` | 230 | Host-side FUSE server over byte stream |
| `hcs/src/fuse_mount.rs` | 815 | Guest-side `/dev/fuse` ↔ vsock bridge binary |

### Modified Files

| File | Change |
|------|--------|
| `devices/src/virtio/descriptor_utils.rs` | Added `Reader::from_buffer()` / `Writer::from_buffer()` |
| `devices/src/virtio/fs/mod.rs` | Made `server` and `socket_worker` modules public |
| `vmm/src/builder.rs` | Removed Plan9 shares, uses `nanosb.fuse_rootfs=1` cmdline |
| `vmm/src/windows/vstate.rs` | Removed `plan9_shares` from `Vcpu` |
| `hcs/src/platform.rs` | Removed `Plan9Share`, removed Plan9 JSON serialization, added FUSE HvSocket ports |
| `hcs/src/stub.rs` | Removed `Plan9Share` from non-Windows stubs |
| `hcs/src/initrd.rs` | Replaced 9P init scripts with FUSE init script, inject `fuse_mount` |
| `hcs/Cargo.toml` | Removed `plan9_mount` binary, added `fuse_mount` |
| `.github/workflows/build-release.yml` | Build `fuse_mount` instead of `plan9_mount` |

### Deleted Files

| File | Lines | Reason |
|------|-------|--------|
| `hcs/src/plan9_mount.rs` | 586 | Replaced by `fuse_mount.rs` |

### Related Repos

| Repo | File | Change |
|------|------|--------|
| `cli` | `scripts/uninstall.ps1` | Replaced `plan9_mount` with `fuse_mount` in dependency cleanup |

## Prerequisites

### WSL2 Kernel FUSE Support

The guest runs on a WSL2 kernel (provided by Windows). It must have `CONFIG_FUSE_FS=y` for `/dev/fuse` to be available. WSL2 kernels typically include FUSE support. This must be verified on target systems before deployment.

If FUSE is not available, an alternative approach would be to use a userspace FUSE library (e.g., statically-linked `libfuse3`) that implements the FUSE protocol in userspace without `/dev/fuse`.

## Performance Considerations

- **Message overhead**: Each FUSE operation requires a vsock round-trip (request + response). For metadata-heavy workloads this adds latency vs in-kernel 9P.
- **Data throughput**: FUSE supports up to 1MB reads/writes per message. Vsock throughput on Hyper-V is typically >1 Gbps, so large file I/O should not be a bottleneck.
- **No zero-copy**: The socket transport copies data through userspace buffers (no DAX, no shared memory). This is the same behavior as the existing `windows/passthrough.rs` which already uses buffered I/O.
- **Future optimization**: Multi-threaded request handling could be added to `socket_worker.rs` for parallel FUSE operations.

## Testing Plan

1. **Unit tests**: Verify `Reader::from_buffer()` / `Writer::from_buffer()` with FUSE protocol messages
2. **Integration test**: `test_alpine_echo_fuse_rootfs` and `test_alpine_ls_fuse_rootfs` (renamed from plan9 tests)
3. **End-to-end on Windows**: Build full stack, create HCS VM, verify FUSE mount works
4. **Layer extraction**: Test in-guest OCI layer extraction via second FUSE connection
5. **Performance benchmark**: Compare file I/O throughput and latency vs previous Plan9 approach
