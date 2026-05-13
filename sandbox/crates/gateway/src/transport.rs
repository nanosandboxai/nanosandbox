//! Platform-dispatching HTTP transport for gateway communication.
//!
//! On Windows, routes through HvSocket (AF_HYPERV) to bypass HCN NAT.
//! On Unix/macOS, uses TCP via gvproxy port mappings.

use crate::error::{Error, Result};

/// Transport configuration for reaching the gateway inside a VM.
pub struct Transport {
    /// TCP address (host:port) for gateway HTTP — Unix/macOS.
    gateway_addr: Option<String>,
    /// HCS VM ID for HvSocket connections — Windows only.
    #[cfg(target_os = "windows")]
    hcs_vm_id: Option<String>,
}

impl Transport {
    /// Create a new transport for Unix/macOS (TCP via gvproxy).
    #[cfg(not(target_os = "windows"))]
    pub fn new(gateway_addr: Option<String>) -> Self {
        Self { gateway_addr }
    }

    /// Create a new transport for Windows (HvSocket + TCP fallback).
    #[cfg(target_os = "windows")]
    pub fn new(gateway_addr: Option<String>, hcs_vm_id: Option<String>) -> Self {
        Self {
            gateway_addr,
            hcs_vm_id,
        }
    }

    /// Check if the transport has a valid gateway endpoint.
    pub fn is_available(&self) -> bool {
        self.gateway_addr.is_some()
    }

    /// Get the gateway TCP address.
    pub fn gateway_addr(&self) -> Option<&str> {
        self.gateway_addr.as_deref()
    }

    /// Get the HCS VM ID (Windows only).
    #[cfg(target_os = "windows")]
    pub fn hcs_vm_id(&self) -> Option<&str> {
        self.hcs_vm_id.as_deref()
    }

    /// Get the gateway TCP address or error.
    #[cfg(not(target_os = "windows"))]
    fn require_addr(&self) -> Result<&str> {
        self.gateway_addr
            .as_deref()
            .ok_or(Error::NotAvailable)
    }

    /// HTTP GET to the gateway.
    pub fn http_get(&self, path: &str) -> Result<(u16, String)> {
        let addr = self.gateway_addr.as_ref().ok_or(Error::NotAvailable)?;
        crate::http::http_get(addr, path).map_err(Error::HttpFailed)
    }

    /// HTTP POST to the gateway.
    pub fn http_post(&self, path: &str, json_body: &str) -> Result<(u16, String)> {
        let addr = self.gateway_addr.as_ref().ok_or(Error::NotAvailable)?;
        crate::http::http_post(addr, path, json_body).map_err(Error::HttpFailed)
    }

    /// HTTP DELETE to the gateway.
    pub fn http_delete(&self, path: &str) -> Result<(u16, String)> {
        let addr = self.gateway_addr.as_ref().ok_or(Error::NotAvailable)?;
        crate::http::http_delete(addr, path).map_err(Error::HttpFailed)
    }

    /// HTTP POST with SSE streaming to the gateway.
    pub fn http_post_sse<F>(&self, path: &str, json_body: &str, on_output: F) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        let addr = self.gateway_addr.as_ref().ok_or(Error::NotAvailable)?;
        crate::http::http_post_sse(addr, path, json_body, on_output)
            .map_err(Error::HttpFailed)
    }
}
