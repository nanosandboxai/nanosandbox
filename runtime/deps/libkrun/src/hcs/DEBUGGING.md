# hcs crate — local debugging & examples

The original libkrun-win repo shipped example binaries exercising HCS, HCN,
plan9, SCSI, ext4, VHDX, etc. They are NOT part of the workspace build.

## Prerequisites

- Windows 10/11 or Windows Server 2022+ with Hyper-V / HCS enabled
- Rust stable toolchain
- `krunfw.dll` built (see `../libkrunfw-win/`)

## Running an example locally

1. Restore the example source from libkrun-win history:
   ```bash
   git -C /path/to/libkrun-win show c83b035:src/hcs/examples/<name>.rs > examples/<name>.rs
   ```
2. Temporarily add to `Cargo.toml`:
   ```toml
   [dev-dependencies]
   russh = "0.57"
   tokio = { version = "1", features = ["full"] }

   [[example]]
   name = "<name>"
   ```
3. Build and run:
   ```bash
   cargo run --example <name>
   ```

## Available examples (from libkrun-win)

- `hcs_test.rs` — HCS VM lifecycle smoke test
- `make_initrd.rs` — build minimal initrd image
- `net_vm_test.rs` — VM with virtio-net
- `plan9_test.rs` — plan9 filesystem mount
- `rootfs_test.rs` — boot with prebuilt rootfs
- `scsi_test.rs` — virtio-scsi device
- `tui_test.rs` — interactive serial TUI
- `vhdx_test.rs` — VHDX disk creation
- `bidir_test.rs`, `exec_config_test.rs`, `hcn_test.rs`, `ext4_debug.rs` — misc
