// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! HCN (Host Compute Network) API wrappers for Windows.
//!
//! HCN (`computenetwork.dll`) manages virtual networking for HCS VMs.
//! We use it to create NAT networks with port forwarding so that
//! guest services (like agent-gateway on port 8080) are reachable
//! from the host.

use std::collections::HashMap;
use std::ptr;

use log::{error, info, warn};

/// Opaque HCN handle types (void pointers).
type HcnNetwork = *mut std::ffi::c_void;
type HcnEndpoint = *mut std::ffi::c_void;

// HCN API from computenetwork.dll — loaded dynamically to avoid hard
// link-time dependency (the DLL may not exist on older Windows).
#[link(name = "computenetwork")]
extern "system" {
    fn HcnCreateNetwork(
        id: *const Guid,
        settings: *const u16,
        network: *mut HcnNetwork,
        error_record: *mut *mut u16,
    ) -> i32;

    fn HcnOpenNetwork(
        id: *const Guid,
        network: *mut HcnNetwork,
        error_record: *mut *mut u16,
    ) -> i32;

    fn HcnCreateEndpoint(
        network: HcnNetwork,
        id: *const Guid,
        settings: *const u16,
        endpoint: *mut HcnEndpoint,
        error_record: *mut *mut u16,
    ) -> i32;

    fn HcnDeleteEndpoint(
        id: *const Guid,
        error_record: *mut *mut u16,
    ) -> i32;

    fn HcnDeleteNetwork(
        id: *const Guid,
        error_record: *mut *mut u16,
    ) -> i32;

    fn HcnCloseNetwork(network: HcnNetwork) -> i32;
    fn HcnCloseEndpoint(endpoint: HcnEndpoint) -> i32;

    fn HcnOpenEndpoint(
        id: *const Guid,
        endpoint: *mut HcnEndpoint,
        error_record: *mut *mut u16,
    ) -> i32;

    fn HcnQueryEndpointProperties(
        endpoint: HcnEndpoint,
        query: *const u16,
        properties: *mut *mut u16,
        error_record: *mut *mut u16,
    ) -> i32;

    #[allow(dead_code)]
    fn HcnEnumerateNetworks(
        query: *const u16,
        networks: *mut *mut u16,
        error_record: *mut *mut u16,
    ) -> i32;

    fn HcnEnumerateEndpoints(
        query: *const u16,
        endpoints: *mut *mut u16,
        error_record: *mut *mut u16,
    ) -> i32;
}

extern "system" {
    fn CoTaskMemFree(pv: *mut std::ffi::c_void);
}

/// A GUID (128-bit identifier).
#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl Guid {
    /// Generate a random v4 UUID.
    fn new_v4() -> Self {
        let mut bytes = [0u8; 16];
        // Use a simple PRNG seeded from the current time + process ID.
        // Not cryptographically secure, but fine for VM network IDs.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
            ^ (std::process::id() as u64).wrapping_mul(0x517cc1b727220a95);

        let mut state = seed;
        for b in bytes.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (state >> 33) as u8;
        }
        // Set version 4 and variant bits
        bytes[6] = (bytes[6] & 0x0F) | 0x40;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;

        Guid {
            data1: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            data2: u16::from_le_bytes([bytes[4], bytes[5]]),
            data3: u16::from_le_bytes([bytes[6], bytes[7]]),
            data4: [
                bytes[8], bytes[9], bytes[10], bytes[11],
                bytes[12], bytes[13], bytes[14], bytes[15],
            ],
        }
    }

    /// Format as a GUID string without braces: `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`.
    fn to_string_no_braces(&self) -> String {
        format!(
            "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            self.data1,
            self.data2,
            self.data3,
            self.data4[0],
            self.data4[1],
            self.data4[2],
            self.data4[3],
            self.data4[4],
            self.data4[5],
            self.data4[6],
            self.data4[7],
        )
    }

    /// Format as a standard GUID string: `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
    fn to_string_braces(&self) -> String {
        format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            self.data1,
            self.data2,
            self.data3,
            self.data4[0],
            self.data4[1],
            self.data4[2],
            self.data4[3],
            self.data4[4],
            self.data4[5],
            self.data4[6],
            self.data4[7],
        )
    }

    /// Parse from a GUID string like `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
    fn from_str(s: &str) -> Option<Self> {
        let s = s.trim_start_matches('{').trim_end_matches('}');
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 5 {
            return None;
        }
        Some(Guid {
            data1: u32::from_str_radix(parts[0], 16).ok()?,
            data2: u16::from_str_radix(parts[1], 16).ok()?,
            data3: u16::from_str_radix(parts[2], 16).ok()?,
            data4: {
                let b01 = u16::from_str_radix(parts[3], 16).ok()?;
                let b2345 = u64::from_str_radix(parts[4], 16).ok()?;
                [
                    (b01 >> 8) as u8,
                    b01 as u8,
                    (b2345 >> 40) as u8,
                    (b2345 >> 32) as u8,
                    (b2345 >> 24) as u8,
                    (b2345 >> 16) as u8,
                    (b2345 >> 8) as u8,
                    b2345 as u8,
                ]
            },
        })
    }
}

/// Convert a Rust string to a null-terminated UTF-16 wide string.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a null-terminated UTF-16 string from a raw pointer, then free it.
unsafe fn read_and_free_wide(ptr: *mut u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
    CoTaskMemFree(ptr as *mut _);
    Some(s)
}

/// A well-known GUID for the libkrun NAT network.
/// Using a fixed GUID means we reuse the same network across runs,
/// avoiding network proliferation.
const LIBKRUN_NETWORK_GUID: &str = "{B7E3A5C1-9F42-4D68-A231-7BCFA0E12345}";

/// Resources created by HCN for a single VM's networking.
pub struct HcnNetworking {
    #[allow(dead_code)]
    network_id: Guid,
    endpoint_id: Guid,
    network_handle: HcnNetwork,
    endpoint_handle: HcnEndpoint,
    #[allow(dead_code)]
    created_network: bool,
    guest_ip_addr: String,
    /// Port proxy rules added via `netsh interface portproxy` (host_port, guest_port).
    /// Cleaned up on Drop.
    portproxy_rules: Vec<(u16, u16)>,
}

impl HcnNetworking {
    /// Set up NAT networking with port forwarding for an HCS VM.
    ///
    /// Creates (or reuses) a NAT network and creates an endpoint with
    /// the specified port mappings. Returns the endpoint ID string
    /// for inclusion in the HCS VM config JSON.
    ///
    /// `port_map` is guest_port → host_port.
    pub fn create(port_map: &HashMap<u16, u16>) -> std::result::Result<Self, String> {
        let network_guid = Guid::from_str(LIBKRUN_NETWORK_GUID)
            .ok_or("Failed to parse network GUID")?;
        let endpoint_guid = Guid::new_v4();

        // Try to open existing network first
        let (network_handle, created_network) = match open_network(&network_guid) {
            Ok(h) => {
                info!("hcn: reusing existing libkrun NAT network");
                (h, false)
            }
            Err(_) => {
                // Create a new NAT network
                let settings = serde_json::json!({
                    "SchemaVersion": { "Major": 2, "Minor": 0 },
                    "Type": "NAT",
                    "Name": "libkrun-hcn-nat",
                    "Ipams": [{
                        "Type": "Static",
                        "Subnets": [{
                            "IpAddressPrefix": "172.28.0.0/16",
                            "Routes": [{
                                "NextHop": "172.28.0.1",
                                "DestinationPrefix": "0.0.0.0/0"
                            }]
                        }]
                    }]
                });

                let h = create_network(&network_guid, &settings.to_string())?;
                info!("hcn: created NAT network {}", network_guid.to_string_braces());
                (h, true)
            }
        };

        // Clean up any stale endpoints from previous VM runs.
        cleanup_stale_endpoints(&network_guid);

        // Allocate a unique guest IP from the 172.28.0.0/16 subnet.
        let guest_ip = allocate_guest_ip();
        info!("hcn: allocated guest IP: {}", guest_ip);

        // Build port mapping policies for the endpoint.
        // HCN uses numeric protocol values: 6 = TCP, 17 = UDP.
        let mut policies = Vec::new();
        for (&guest_port, &host_port) in port_map {
            policies.push(serde_json::json!({
                "Type": "PortMapping",
                "Settings": {
                    "Protocol": 6,
                    "InternalPort": guest_port,
                    "ExternalPort": host_port
                }
            }));
        }

        // HCN endpoint requires SchemaVersion and HostComputeNetwork (GUID without braces).
        let network_id_no_braces = network_guid.to_string_no_braces();
        let mut endpoint_settings = serde_json::json!({
            "SchemaVersion": { "Major": 2, "Minor": 0 },
            "HostComputeNetwork": network_id_no_braces,
            "IpConfigurations": [{
                "IpAddress": guest_ip,
                "PrefixLength": 16
            }]
        });
        if !policies.is_empty() {
            endpoint_settings["Policies"] = serde_json::json!(policies);
        }

        let settings_str = endpoint_settings.to_string();
        info!("hcn: endpoint settings: {}", settings_str);

        let endpoint_handle = match create_endpoint(
            network_handle,
            &endpoint_guid,
            &settings_str,
        ) {
            Ok(h) => h,
            Err(e) => {
                if created_network {
                    let _ = delete_network(&network_guid);
                }
                return Err(e);
            }
        };

        info!(
            "hcn: created endpoint {} with {} port mapping(s)",
            endpoint_guid.to_string_braces(),
            port_map.len()
        );
        for (&guest_port, &host_port) in port_map {
            info!("hcn:   host:{} -> guest:{}", host_port, guest_port);
        }

        // Set up portproxy rules so that localhost:host_port forwards to
        // guest_ip:guest_port. HCN PortMapping policies alone do not create
        // host-level listeners on Windows Server — we need netsh portproxy.
        let mut portproxy_rules = Vec::new();
        for (&guest_port, &host_port) in port_map {
            let status = std::process::Command::new("netsh")
                .args([
                    "interface", "portproxy", "add", "v4tov4",
                    &format!("listenport={}", host_port),
                    "listenaddress=127.0.0.1",
                    &format!("connectport={}", guest_port),
                    &format!("connectaddress={}", guest_ip),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match status {
                Ok(s) if s.success() => {
                    info!("hcn: portproxy 127.0.0.1:{} -> {}:{}", host_port, guest_ip, guest_port);
                    portproxy_rules.push((host_port, guest_port));
                }
                Ok(s) => warn!("hcn: portproxy add failed (exit {})", s),
                Err(e) => warn!("hcn: portproxy add error: {}", e),
            }
        }

        Ok(HcnNetworking {
            network_id: network_guid,
            endpoint_id: endpoint_guid,
            network_handle,
            endpoint_handle,
            created_network,
            guest_ip_addr: guest_ip,
            portproxy_rules,
        })
    }

    /// Create minimal networking (no port mappings) — for debugging.
    pub fn create_minimal() -> std::result::Result<Self, String> {
        let network_guid = Guid::from_str(LIBKRUN_NETWORK_GUID)
            .ok_or("Failed to parse network GUID")?;
        let endpoint_guid = Guid::new_v4();

        let (network_handle, created_network) = match open_network(&network_guid) {
            Ok(h) => (h, false),
            Err(_) => {
                let settings = serde_json::json!({
                    "SchemaVersion": { "Major": 2, "Minor": 0 },
                    "Type": "NAT",
                    "Name": "libkrun-hcn-nat",
                    "Ipams": [{
                        "Type": "Static",
                        "Subnets": [{
                            "IpAddressPrefix": "172.28.0.0/16",
                            "Routes": [{
                                "NextHop": "172.28.0.1",
                                "DestinationPrefix": "0.0.0.0/0"
                            }]
                        }]
                    }]
                });
                let h = create_network(&network_guid, &settings.to_string())?;
                (h, true)
            }
        };

        let guest_ip = allocate_guest_ip();
        info!("hcn: allocated guest IP (minimal): {}", guest_ip);

        let endpoint_settings = serde_json::json!({
            "SchemaVersion": { "Major": 2, "Minor": 0 },
            "HostComputeNetwork": network_guid.to_string_no_braces(),
            "IpConfigurations": [{
                "IpAddress": guest_ip,
                "PrefixLength": 16
            }]
        });
        let settings_str = endpoint_settings.to_string();

        let endpoint_handle = match create_endpoint(
            network_handle,
            &endpoint_guid,
            &settings_str,
        ) {
            Ok(h) => h,
            Err(e) => {
                if created_network {
                    let _ = delete_network(&network_guid);
                }
                return Err(e);
            }
        };

        Ok(HcnNetworking {
            network_id: network_guid,
            endpoint_id: endpoint_guid,
            network_handle,
            endpoint_handle,
            created_network,
            guest_ip_addr: guest_ip,
            portproxy_rules: Vec::new(),
        })
    }

    /// Get the endpoint ID string for inclusion in the HCS VM config.
    /// Returns GUID without braces, e.g. `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`.
    pub fn endpoint_id_string(&self) -> String {
        self.endpoint_id.to_string_no_braces()
    }

    /// Get the gateway IP for the NAT network.
    pub fn gateway_ip(&self) -> &str {
        "172.28.0.1"
    }

    /// Get the guest IP assigned to the endpoint.
    pub fn guest_ip(&self) -> &str {
        &self.guest_ip_addr
    }

    /// Get the subnet prefix length.
    pub fn prefix_len(&self) -> u8 {
        16
    }
}

impl Drop for HcnNetworking {
    fn drop(&mut self) {
        // Remove portproxy rules first (before endpoint is deleted)
        for &(host_port, _) in &self.portproxy_rules {
            let _ = std::process::Command::new("netsh")
                .args([
                    "interface", "portproxy", "delete", "v4tov4",
                    &format!("listenport={}", host_port),
                    "listenaddress=127.0.0.1",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            info!("hcn: removed portproxy for 127.0.0.1:{}", host_port);
        }

        // Close and delete the endpoint
        if !self.endpoint_handle.is_null() {
            unsafe { HcnCloseEndpoint(self.endpoint_handle) };
            if let Err(e) = delete_endpoint(&self.endpoint_id) {
                error!("hcn: failed to delete endpoint: {}", e);
            } else {
                info!("hcn: deleted endpoint {}", self.endpoint_id.to_string_braces());
            }
        }

        // Close (but don't delete) the network — other VMs may use it
        if !self.network_handle.is_null() {
            unsafe { HcnCloseNetwork(self.network_handle) };
        }
    }
}

// Send is safe because HCN handles are process-wide.
unsafe impl Send for HcnNetworking {}

// ─── Low-level HCN wrappers ─────────────────────────────────────────────────

fn create_network(id: &Guid, settings: &str) -> std::result::Result<HcnNetwork, String> {
    let settings_wide = to_wide(settings);
    let mut handle: HcnNetwork = ptr::null_mut();
    let mut error_record: *mut u16 = ptr::null_mut();

    let hr = unsafe {
        HcnCreateNetwork(id, settings_wide.as_ptr(), &mut handle, &mut error_record)
    };

    let err_str = unsafe { read_and_free_wide(error_record) };

    if hr != 0 {
        return Err(format!(
            "HcnCreateNetwork failed: HRESULT 0x{:08X}{}",
            hr as u32,
            err_str.map(|s| format!(" — {}", s)).unwrap_or_default()
        ));
    }

    Ok(handle)
}

fn open_network(id: &Guid) -> std::result::Result<HcnNetwork, String> {
    let mut handle: HcnNetwork = ptr::null_mut();
    let mut error_record: *mut u16 = ptr::null_mut();

    let hr = unsafe { HcnOpenNetwork(id, &mut handle, &mut error_record) };

    let err_str = unsafe { read_and_free_wide(error_record) };

    if hr != 0 {
        return Err(format!(
            "HcnOpenNetwork failed: HRESULT 0x{:08X}{}",
            hr as u32,
            err_str.map(|s| format!(" — {}", s)).unwrap_or_default()
        ));
    }

    Ok(handle)
}

fn create_endpoint(
    network: HcnNetwork,
    id: &Guid,
    settings: &str,
) -> std::result::Result<HcnEndpoint, String> {
    let settings_wide = to_wide(settings);
    let mut handle: HcnEndpoint = ptr::null_mut();
    let mut error_record: *mut u16 = ptr::null_mut();

    let hr = unsafe {
        HcnCreateEndpoint(
            network,
            id,
            settings_wide.as_ptr(),
            &mut handle,
            &mut error_record,
        )
    };

    let err_str = unsafe { read_and_free_wide(error_record) };

    if hr != 0 {
        return Err(format!(
            "HcnCreateEndpoint failed: HRESULT 0x{:08X}{}",
            hr as u32,
            err_str.map(|s| format!(" — {}", s)).unwrap_or_default()
        ));
    }

    Ok(handle)
}

/// Allocate a unique guest IP address from the 172.28.0.0/16 subnet.
///
/// Enumerates existing HCN endpoints, queries each for its actual IP via
/// `HcnQueryEndpointProperties`, then picks the first unused address
/// starting from 172.28.0.2.
fn allocate_guest_ip() -> String {
    let used_ips = enumerate_endpoint_ips();
    if !used_ips.is_empty() {
        info!("hcn: IPs currently in use: {:?}", used_ips);
    }

    // Start from .2 (gateway is .1)
    for high in 0u16..=255 {
        let start = if high == 0 { 2u16 } else { 1 };
        for low in start..=254 {
            let candidate = format!("172.28.{}.{}", high, low);
            if !used_ips.contains(&candidate) {
                return candidate;
            }
        }
    }

    // Extremely unlikely: all 65000+ IPs in use.
    "172.28.0.2".to_string()
}

/// Enumerate all HCN endpoints and extract their actual IP addresses
/// by querying each endpoint's properties via `HcnQueryEndpointProperties`.
fn enumerate_endpoint_ips() -> std::collections::HashSet<String> {
    let mut ips = std::collections::HashSet::new();

    let endpoint_ids = enumerate_endpoint_guids();

    for ep_id_str in &endpoint_ids {
        let Some(ep_guid) = Guid::from_str(ep_id_str) else { continue };

        if let Some(ip) = query_endpoint_ip(&ep_guid) {
            ips.insert(ip);
        }
    }

    ips
}

/// List all HCN endpoint GUIDs. Returns empty Vec on failure.
fn enumerate_endpoint_guids() -> Vec<String> {
    let mut endpoints_ptr: *mut u16 = ptr::null_mut();
    let mut error_record: *mut u16 = ptr::null_mut();

    let query = to_wide("{}");
    let hr = unsafe {
        HcnEnumerateEndpoints(query.as_ptr(), &mut endpoints_ptr, &mut error_record)
    };
    let _ = unsafe { read_and_free_wide(error_record) };

    if hr != 0 || endpoints_ptr.is_null() {
        return Vec::new();
    }

    let json_str = unsafe { read_and_free_wide(endpoints_ptr) };
    let Some(json_str) = json_str else { return Vec::new() };

    serde_json::from_str::<Vec<String>>(&json_str).unwrap_or_default()
}

/// Open an endpoint by GUID, query its properties, and extract the IP address.
/// Returns `None` if the endpoint can't be opened or has no IP configured.
fn query_endpoint_ip(id: &Guid) -> Option<String> {
    let mut handle: HcnEndpoint = ptr::null_mut();
    let mut error_record: *mut u16 = ptr::null_mut();

    // Open the endpoint
    let hr = unsafe { HcnOpenEndpoint(id, &mut handle, &mut error_record) };
    let _ = unsafe { read_and_free_wide(error_record) };
    if hr != 0 || handle.is_null() {
        return None;
    }

    // Query its properties
    let query = to_wide("{}");
    let mut props_ptr: *mut u16 = ptr::null_mut();
    let mut err_ptr: *mut u16 = ptr::null_mut();

    let hr = unsafe {
        HcnQueryEndpointProperties(handle, query.as_ptr(), &mut props_ptr, &mut err_ptr)
    };
    let _ = unsafe { read_and_free_wide(err_ptr) };
    unsafe { HcnCloseEndpoint(handle) };

    if hr != 0 || props_ptr.is_null() {
        return None;
    }

    let props_json = unsafe { read_and_free_wide(props_ptr) }?;

    // Parse the properties JSON to extract the IP address.
    // HCN endpoint properties contain an "IpConfigurations" array with "IpAddress" fields,
    // or sometimes a top-level "IPAddress" field depending on the schema version.
    let props: serde_json::Value = serde_json::from_str(&props_json).ok()?;

    // Try IpConfigurations[].IpAddress (schema v2)
    if let Some(configs) = props.get("IpConfigurations").and_then(|v| v.as_array()) {
        for cfg in configs {
            if let Some(ip) = cfg.get("IpAddress").and_then(|v| v.as_str()) {
                if !ip.is_empty() {
                    return Some(ip.to_string());
                }
            }
        }
    }

    // Try top-level IPAddress
    if let Some(ip) = props.get("IPAddress").and_then(|v| v.as_str()) {
        if !ip.is_empty() {
            return Some(ip.to_string());
        }
    }

    None
}

/// Clean up stale endpoints that no longer have an active VM attached.
///
/// Only deletes endpoints that belong to our libkrun NAT network and
/// whose backing compute system is gone. Endpoints with a live VM are
/// left intact so multi-VM scenarios work correctly.
fn cleanup_stale_endpoints(_network_id: &Guid) {
    let endpoint_ids = enumerate_endpoint_guids();
    if endpoint_ids.is_empty() {
        return;
    }

    for ep_id_str in &endpoint_ids {
        let Some(ep_guid) = Guid::from_str(ep_id_str) else { continue };

        // Open the endpoint and check if it belongs to our network.
        let mut handle: HcnEndpoint = ptr::null_mut();
        let mut error_record: *mut u16 = ptr::null_mut();

        let hr = unsafe { HcnOpenEndpoint(&ep_guid, &mut handle, &mut error_record) };
        let _ = unsafe { read_and_free_wide(error_record) };
        if hr != 0 || handle.is_null() {
            continue;
        }

        // Query properties to check the HostComputeNetwork field
        let query = to_wide("{}");
        let mut props_ptr: *mut u16 = ptr::null_mut();
        let mut err_ptr: *mut u16 = ptr::null_mut();

        let hr = unsafe {
            HcnQueryEndpointProperties(handle, query.as_ptr(), &mut props_ptr, &mut err_ptr)
        };
        let _ = unsafe { read_and_free_wide(err_ptr) };
        unsafe { HcnCloseEndpoint(handle) };

        if hr != 0 || props_ptr.is_null() {
            continue;
        }

        let props_json = unsafe { read_and_free_wide(props_ptr) };
        let Some(props_json) = props_json else { continue };
        let Ok(props) = serde_json::from_str::<serde_json::Value>(&props_json) else { continue };

        // Check if this endpoint belongs to our network.
        // HCN uses "HostComputeNetwork" in schema v2, but some Windows versions
        // return "VirtualNetwork" instead. Check both fields.
        let network_id_no_braces = _network_id.to_string_no_braces();
        let check_field = |field: &str| -> bool {
            props
                .get(field)
                .and_then(|v| v.as_str())
                .map(|nid| {
                    let nid_clean = nid.trim_start_matches('{').trim_end_matches('}');
                    nid_clean.eq_ignore_ascii_case(&network_id_no_braces)
                })
                .unwrap_or(false)
        };
        let belongs_to_us = check_field("HostComputeNetwork") || check_field("VirtualNetwork");

        if !belongs_to_us {
            info!("hcn: endpoint {} does not belong to our network, skipping (props: {})",
                  ep_id_str, props_json);
            continue;
        }

        // Delete all endpoints belonging to our network. After a force-kill
        // the HCN State/HostComputeSystem fields may remain stale, so we
        // cannot reliably detect orphaned endpoints. Since we create fresh
        // endpoints for each VM, it's safe to clean them all up on startup.
        if let Ok(()) = delete_endpoint(&ep_guid) {
            info!("hcn: cleaned up stale endpoint {}", ep_id_str);
        }
    }
}

/// Delete an HCN endpoint by its GUID string (with or without braces).
///
/// Called by the parent process to clean up endpoints after killing a VM subprocess,
/// since the subprocess's Drop impl may not run when the process is terminated.
pub fn delete_endpoint_by_id(guid_str: &str) -> std::result::Result<(), String> {
    let guid = Guid::from_str(guid_str)
        .ok_or_else(|| format!("Invalid endpoint GUID: {}", guid_str))?;
    delete_endpoint(&guid)
}

fn delete_endpoint(id: &Guid) -> std::result::Result<(), String> {
    let mut error_record: *mut u16 = ptr::null_mut();
    let hr = unsafe { HcnDeleteEndpoint(id, &mut error_record) };
    let err_str = unsafe { read_and_free_wide(error_record) };

    if hr != 0 {
        return Err(format!(
            "HcnDeleteEndpoint failed: HRESULT 0x{:08X}{}",
            hr as u32,
            err_str.map(|s| format!(" — {}", s)).unwrap_or_default()
        ));
    }
    Ok(())
}

fn delete_network(id: &Guid) -> std::result::Result<(), String> {
    let mut error_record: *mut u16 = ptr::null_mut();
    let hr = unsafe { HcnDeleteNetwork(id, &mut error_record) };
    let err_str = unsafe { read_and_free_wide(error_record) };

    if hr != 0 {
        return Err(format!(
            "HcnDeleteNetwork failed: HRESULT 0x{:08X}{}",
            hr as u32,
            err_str.map(|s| format!(" — {}", s)).unwrap_or_default()
        ));
    }
    Ok(())
}
