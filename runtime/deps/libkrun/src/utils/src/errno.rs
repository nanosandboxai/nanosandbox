// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0
//
// Cross-platform errno module for Windows.
// On Unix, vmm_sys_util::errno is used instead (re-exported from lib.rs).

use std::fmt;
use std::io;

/// Wrapper over a platform error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(i32);

impl Error {
    /// Creates a new Error from the given error code.
    pub fn new(code: i32) -> Error {
        Error(code)
    }

    /// Returns the error code.
    pub fn errno(&self) -> i32 {
        self.0
    }

    /// Returns the last OS error.
    #[cfg(target_os = "windows")]
    pub fn last() -> Error {
        Error(io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        io::Error::from_raw_os_error(self.0).fmt(f)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error(e.raw_os_error().unwrap_or(0))
    }
}

/// A specialized Result type for errno operations.
pub type Result<T> = std::result::Result<T, Error>;
