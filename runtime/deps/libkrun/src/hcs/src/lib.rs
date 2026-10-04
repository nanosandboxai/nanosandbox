// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! Rust wrappers for the Windows Host Compute Service (HCS) API.
//!
//! HCS (`computecore.dll`) creates lightweight Hyper-V VMs — the same API
//! that WSL2 uses. Unlike WHPX, HCS manages LAPIC, IOAPIC, IDT, PIT,
//! and interrupt routing internally.

#[cfg(target_os = "windows")]
mod platform;

#[cfg(target_os = "windows")]
pub use platform::*;

#[cfg(target_os = "windows")]
pub mod hcn;

pub mod initrd;

#[cfg(not(target_os = "windows"))]
mod stub;

#[cfg(not(target_os = "windows"))]
pub use stub::*;
