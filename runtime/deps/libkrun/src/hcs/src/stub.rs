// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! Stub types for non-Windows platforms.

use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    NotSupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "HCS is not supported on this platform")
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct HcsVm;

impl HcsVm {
    pub fn create(_id: &str, _config: &VmConfig) -> Result<Self> {
        Err(Error::NotSupported)
    }
    pub fn start(&self) -> Result<()> {
        Err(Error::NotSupported)
    }
    pub fn wait(&self) -> Result<()> {
        Err(Error::NotSupported)
    }
    pub fn terminate(&self) -> Result<()> {
        Err(Error::NotSupported)
    }
}

pub struct VmConfig {
    pub kernel_path: PathBuf,
    pub initrd_path: Option<PathBuf>,
    pub cmdline: String,
    pub memory_mb: u32,
    pub cpu_count: u32,
    pub network_adapter: Option<NetworkAdapterConfig>,
}

pub struct NetworkAdapterConfig {
    pub endpoint_id: String,
}

