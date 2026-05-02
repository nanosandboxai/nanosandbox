//! gvproxy stub for platforms without Unix socket support (Windows).
//!
//! Provides the same types as gvproxy.rs but all operations return errors
//! or "not available." libkrun.rs already handles the no-gvproxy case by
//! falling back to TSI networking.

use std::path::{Path, PathBuf};

pub const GUEST_IP: &str = "192.168.127.2";

pub struct GvproxyInstance {
    _socket_path: PathBuf,
}

impl GvproxyInstance {
    pub fn socket_path(&self) -> &Path {
        &self._socket_path
    }

    pub fn expose_port(&self, _host_port: u16, _guest_port: u16) -> Result<(), String> {
        Err("gvproxy not supported on Windows".into())
    }

    pub fn stop(&mut self) {}
}

pub struct GvproxyManager;

impl GvproxyManager {
    pub fn is_available() -> bool {
        false
    }

    pub fn start(_sandbox_id: &str) -> Result<GvproxyInstance, String> {
        Err("gvproxy not supported on Windows".into())
    }
}
