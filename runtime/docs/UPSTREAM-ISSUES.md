# Upstream Issues and Patches

This document tracks issues we've identified and patches we've applied to upstream
dependencies (libkrun, libkrunfw) in our submodules.

## Applied Patches

### 1. DoS Panic on Malformed Network Packets (libkrun #577)

**File:** `libkrun/src/devices/src/virtio/net/unixstream.rs`

**Problem:** The `read_frame()` function reads a 4-byte frame length header from the
network proxy as a raw `u32`, then uses it to slice a buffer without bounds checking:
```rust
let frame_length = self.expecting_frame_length as usize;
self.read_loop(&mut buf[..frame_length], false)?; // panics if frame_length > buf.len()
```
A malicious or buggy network proxy can send a frame_length exceeding MAX_BUFFER_SIZE (65562),
causing an index-out-of-bounds panic that crashes the entire VM.

**Fix:** Added bounds validation before the slice operation. Returns an error instead of
panicking. Also replaced `assert!`/`panic!` calls in `write_frame()` and `try_finish_write()`
with proper error returns using `saturating_sub` for arithmetic safety.

### 2. Hardcoded target/release Paths in Makefile (libkrun #447)

**File:** `libkrun/Makefile`

**Problem:** All build output paths are hardcoded as `target/release/` and `target/debug/`,
ignoring the standard `CARGO_TARGET_DIR` environment variable. This prevents:
- Custom build output directories
- Shared build caches across projects
- CI systems that redirect build artifacts

**Fix:** Introduced a `TARGET_DIR` Make variable that respects `CARGO_TARGET_DIR` when set,
defaulting to `target` (standard Cargo behavior). All hardcoded `target/release` and
`target/debug` references replaced with `$(TARGET_DIR)/release` and `$(TARGET_DIR)/debug`.

### 3. TX Busy-Loop on Linux (libkrun #602)

**File:** `libkrun/src/devices/src/virtio/net/worker.rs`

**Problem:** The `process_tx_loop()` function can busy-spin on Linux when the backend
cannot accept writes (`NothingWritten`). On macOS, a timer-based retry mechanism
(`write_retry_delay_us`) prevents this, but Linux has no equivalent — it relies solely
on epoll `OUT` events which may not fire for all backend types (e.g., unixgram with
`ENOBUFS`).

**Fix:** Added `std::thread::yield_now()` on Linux when breaking out of the TX loop due
to a deferred frame. This prevents CPU spinning while waiting for the backend socket to
drain, without adding the complexity of a timerfd-based solution.

## Known Issues (Not Yet Fixed)

### 4. Double-Init Bug: init Script Executed Twice

**Affects:** All libkrun versions with the built-in init process

**Problem:** When libkrun boots a VM, its internal `/init.krun` (PID 1) executes the
configured init script twice — once directly and once via a forked copy. This causes
duplicate process execution inside the guest.

**Workaround in nanosandbox:** We inject a dedup lock at the top of `nanosb-init.sh`:
```sh
if ! mkdir /tmp/.nanosb-init-lock 2>/dev/null; then
    while true; do sleep 3600; done
fi
```
The second instance fails the `mkdir` (already exists) and sleeps forever instead of
running the init sequence again. It cannot `exit` because PID 1 may depend on its
direct child staying alive.

**Reproduction steps:**
1. Create a minimal rootfs with a script that logs its PID and timestamps
2. Configure libkrun to run that script as the exec command
3. Observe two instances of the script running in the guest
4. The two instances have different PIDs but both are children of PID 1

**Root cause:** In `libkrun/init/init.c`, the init process forks and both parent and
child end up executing the configured command. The intended behavior is for one to be
PID 1 (reaper) and the other to exec the user command, but the current implementation
causes both paths to reach the exec.

**Status:** Should be reported upstream to containers/libkrun.

## Submodule Versions

| Component | Upstream | Commit | Notes |
|-----------|----------|--------|-------|
| libkrun | containers/libkrun | a9e04fd | v1.17.3 + our patches |
| libkrunfw | containers/libkrunfw | 430e31b | v5.3.0, kernel 6.12.76 |
