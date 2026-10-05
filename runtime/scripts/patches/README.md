# libkrun init patch series — next-mode (zero-image-customization)

This directory contains patches against the pinned upstream libkrun v1.19.5
checkout at `~/.cache/nanosandbox/libkrun` (SHA `fb988873026120e0e81b31295aaa4a05d27921f2`).

## Purpose

The patches add two capabilities to libkrun's built-in init blob (`/init.krun`)
for the "next" runtime mode, where vanilla OCI images boot with zero nanosb
customization:

1. **Extra virtiofs mounts** — mount additional virtiofs tags at arbitrary
   guest paths (RW or RO), driven by the `KRUN_EXTRA_MOUNTS` env var.
2. **Static network bring-up** — configure `eth0` with a static IP, gateway,
   and DNS (resolv.conf) without any in-image tools, driven by the
   `KRUN_NET_CONFIG` env var.

Both features are gated behind `KRUN_NEXT_MODE=1`. When that env var is absent
(legacy mode), the init behavior is completely unchanged.

## Patch order

Apply in order:

| # | File | What it adds |
|---|------|-------------|
| 1 | `0001-init-add-extra-virtiofs-mounts-for-next-mode.patch` | `mount_extra_virtiofs()` — parses `KRUN_EXTRA_MOUNTS` JSON array, creates target dirs, mounts with `-o tag=<tag>` via virtiofs, respects readonly |
| 2 | `0002-init-add-static-network-bring-up-for-next-mode.patch` | `configure_static_network()` — parses `KRUN_NET_CONFIG` JSON, sets IP/netmask/gateway via `ioctl(SIOCSIFADDR)` + `SIOCSIFNETMASK` + `SIOCADDRT`, writes `/etc/resolv.conf` |
| 3 | `0003-combined-next-mode-init-changes.patch` | Combined patch (1+2) for convenience |

## Integration with `runtime/scripts/build-libkrun.sh`

The build script now applies the combined patch (`0003-combined-next-mode-init-changes.patch`)
automatically after the checkout/SHA verification step and before the cargo build.
The application is idempotent: if the patch is already applied, it skips cleanly.

To apply manually (e.g., for testing):

```bash
cd ~/.cache/nanosandbox/libkrun
git checkout v1.19.5
git am runtime/scripts/patches/0003-combined-next-mode-init-changes.patch
```

To build with the patches to a custom output directory:

```bash
LIBKRUN_OUTPUT_DIR=/tmp/nanosb-libkrun-patched ./runtime/scripts/build-libkrun.sh
```

## Env var reference (next mode only)

Set these in the VM's kernel cmdline (via `krun_set_exec` → `KRUN_INIT` env):

| Env var | Format | Example |
|---------|--------|---------|
| `KRUN_NEXT_MODE` | `"1"` to enable | `KRUN_NEXT_MODE=1` |
| `KRUN_EXTRA_MOUNTS` | JSON array of `{tag, target, readonly}` | `'[{"tag":"config","target":"/mnt/config","readonly":true},{"tag":"state","target":"/mnt/state","readonly":false}]'` |
| `KRUN_NET_CONFIG` | JSON object with `ip`, `gateway`, `dns` | `'{"ip":"192.168.127.2/24","gateway":"192.168.127.1","dns":["8.8.8.8","1.1.1.1"]}'` |

## Design decisions

- **Env vars over config file**: The existing init already reads `KRUN_CONFIG`
  (JSON file) for OCI-style config. Adding new env vars keeps the next-mode
  config alongside the existing `KRUN_INIT`, `KRUN_DHCP`, etc. pattern and
  avoids writing a config file into the rootfs.
- **Gated behind `KRUN_NEXT_MODE=1`**: Ensures zero regression for legacy
  gateway-based images. The new code paths are only reached when explicitly
  opted in.
- **Static IP over DHCP**: The init already has `do_dhcp()` for DHCP. For
  next mode we use static IP because gvproxy assigns a fixed lease
  (192.168.127.2) and we don't need a DHCP exchange. This also works when
  gvproxy is not running (TSI fallback).
- **virtiofs mount via `mount -t virtiofs`**: Requires the kernel to have
  virtiofs support (CONFIG_FUSE_VIRTIO_FS). libkrunfw kernels include this.
  The tag is passed via `-o tag=<tag>`.

## What cannot be done via patch

1. **Console TTY resize propagation**: The init's `setup_redirects()` already
   handles virtio-ports for krun-stdin/stdout/stderr. TTY resize (SIGWINCH)
   is handled by the host-side `krun_add_console_port_tty` — the init does
   not need changes for this.
2. **Entrypoint resolution from OCI image config**: The init already reads
   `/.krun_config.json` (written by the host from the OCI image config) for
   `Entrypoint`, `Cmd`, `Env`, `WorkingDir`. No patch needed.
3. **Exit code propagation**: Already handled by `set_exit_code()` via the
   virtiofs ioctl. No patch needed.
4. **DHCP**: Already implemented via `KRUN_DHCP=1` + `do_dhcp()`. No patch
   needed for DHCP, but we add static IP as an alternative.
