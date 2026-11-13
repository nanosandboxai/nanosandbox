//! nanosb-host — the thin host layer.
//!
//! Protocol-facing supervisor client and state types that a host-side consumer
//! (e.g. the containerd shim) needs, with **no TUI dependencies and no VM engine
//! (libkrun FFI)**. See `docs/runtime-capability-contract.md`.

pub mod capacity;
pub mod deploy;
pub mod spec;
pub mod supervisor;
