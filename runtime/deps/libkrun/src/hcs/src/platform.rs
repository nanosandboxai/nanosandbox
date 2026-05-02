// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! HCS platform implementation for Windows.

use std::fmt;
use std::path::PathBuf;
use std::ptr;

use log::info;
use serde_json::json;
use windows_sys::Win32::Foundation::S_OK;
use windows_sys::Win32::System::HostComputeSystem::{
    HcsCloseComputeSystem, HcsCloseOperation, HcsCreateComputeSystem, HcsCreateOperation,
    HcsGetComputeSystemProperties, HcsStartComputeSystem, HcsTerminateComputeSystem,
    HcsWaitForComputeSystemExit, HcsWaitForOperationResult, HCS_OPERATION, HCS_SYSTEM,
};

/// Errors from HCS operations.
#[derive(Debug)]
pub enum Error {
    /// HCS API call failed with HRESULT.
    Hresult(&'static str, i32),
    /// JSON serialization failed.
    Json(serde_json::Error),
    /// The VM handle is invalid (null).
    InvalidHandle,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Hresult(func, hr) => write!(f, "{func} failed: HRESULT 0x{hr:08X}"),
            Error::Json(e) => write!(f, "JSON error: {e}"),
            Error::InvalidHandle => write!(f, "Invalid HCS handle"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Configuration for a Plan9 filesystem share.
pub struct Plan9Share {
    pub name: String,
    pub host_path: PathBuf,
    pub access_name: String,
    pub port: u32,
}

/// Network adapter configuration for an HCS VM.
pub struct NetworkAdapterConfig {
    /// HCN endpoint ID (GUID string like `{XXXXXXXX-...}`).
    pub endpoint_id: String,
}

/// SCSI virtual disk configuration.
pub struct ScsiDisk {
    /// Path to the virtual disk file on the host.
    pub path: PathBuf,
    /// Read-only attachment.
    pub read_only: bool,
}

/// Configuration for an HCS virtual machine.
pub struct VmConfig {
    pub kernel_path: PathBuf,
    pub initrd_path: Option<PathBuf>,
    pub cmdline: String,
    pub memory_mb: u32,
    pub cpu_count: u32,
    pub plan9_shares: Vec<Plan9Share>,
    /// Optional network adapter (HCN endpoint).
    pub network_adapter: Option<NetworkAdapterConfig>,
    /// SCSI virtual disks (appear as /dev/sda, /dev/sdb, etc. in guest).
    pub scsi_disks: Vec<ScsiDisk>,
    /// Enable HvSocket for direct host↔guest communication (bypasses HCN NAT).
    pub enable_hvsocket: bool,
}

impl VmConfig {
    /// Serialize to HCS JSON schema v2.3.
    fn to_json(&self) -> std::result::Result<String, serde_json::Error> {
        let kernel_path = self.kernel_path.to_string_lossy().to_string();
        let initrd_path = self
            .initrd_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let shares: Vec<serde_json::Value> = self
            .plan9_shares
            .iter()
            .map(|s| {
                json!({
                    "Name": s.name,
                    "Path": s.host_path.to_string_lossy().to_string(),
                    "AccessName": s.access_name,
                    "Port": s.port
                })
            })
            .collect();

        let pipe_name = format!("\\\\.\\pipe\\libkrun-console-{}", std::process::id());

        let mut devices = json!({
            "ComPorts": {
                "0": {
                    "NamedPipe": pipe_name
                }
            },
            "Plan9": {
                "Shares": shares
            }
        });

        // Add network adapter if configured
        if let Some(ref net) = self.network_adapter {
            devices["NetworkAdapters"] = json!({
                "eth0": {
                    "EndpointId": net.endpoint_id
                }
            });
        }

        // Enable HvSocket for direct host↔guest communication.
        // This allows the host to connect to AF_VSOCK listeners in the guest,
        // bypassing HCN NAT which has a ~60s TCP convergence delay.
        if self.enable_hvsocket {
            // ServiceTable entries are required for HCS to actually route
            // AF_HYPERV connections to the guest's AF_VSOCK listeners.
            // Port 50001 (gateway)        = GUID 0000C351-FACB-11E6-BD58-64006A7986D3
            // Port 50022 (SSH)            = GUID 0000C366-FACB-11E6-BD58-64006A7986D3
            // Port 50090 (inbound fwd)    = GUID 0000C3AA-FACB-11E6-BD58-64006A7986D3
            devices["HvSocket"] = json!({
                "HvSocketConfig": {
                    "DefaultBindSecurityDescriptor": "D:P(A;;FA;;;WD)",
                    "DefaultConnectSecurityDescriptor": "D:P(A;;FA;;;WD)",
                    "ServiceTable": {
                        "0000C351-FACB-11E6-BD58-64006A7986D3": {
                            "BindSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "ConnectSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "AllowWildcardBinds": true
                        },
                        "0000C366-FACB-11E6-BD58-64006A7986D3": {
                            "BindSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "ConnectSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "AllowWildcardBinds": true
                        },
                        "0000C3AA-FACB-11E6-BD58-64006A7986D3": {
                            "BindSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "ConnectSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "AllowWildcardBinds": true
                        },
                        "0000C3A5-FACB-11E6-BD58-64006A7986D3": {
                            "BindSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "ConnectSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "AllowWildcardBinds": true
                        },
                        "0000C3B0-FACB-11E6-BD58-64006A7986D3": {
                            "BindSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "ConnectSecurityDescriptor": "D:P(A;;FA;;;WD)",
                            "AllowWildcardBinds": true
                        }
                    }
                }
            });
        }

        // Add SCSI disks if configured
        if !self.scsi_disks.is_empty() {
            let mut attachments = serde_json::Map::new();
            for (i, disk) in self.scsi_disks.iter().enumerate() {
                attachments.insert(i.to_string(), json!({
                    "Type": "VirtualDisk",
                    "Path": disk.path.to_string_lossy().to_string(),
                    "ReadOnly": disk.read_only
                }));
            }
            devices["Scsi"] = json!({
                "primary": {
                    "Attachments": attachments
                }
            });
        }

        let config = json!({
            "Owner": "libkrun",
            "SchemaVersion": { "Major": 2, "Minor": 3 },
            "ShouldTerminateOnLastHandleClosed": true,
            "VirtualMachine": {
                "StopOnReset": true,
                "Chipset": {
                    "UseUtc": true,
                    "LinuxKernelDirect": {
                        "KernelFilePath": kernel_path,
                        "InitRdPath": initrd_path,
                        "KernelCmdLine": self.cmdline
                    }
                },
                "ComputeTopology": {
                    "Memory": {
                        "SizeInMB": self.memory_mb,
                        "AllowOvercommit": true,
                        "EnableHotHint": true,
                        "EnableColdHint": true,
                        "EnableDeferredCommit": true
                    },
                    "Processor": {
                        "Count": self.cpu_count
                    }
                },
                "Devices": devices
            }
        });

        serde_json::to_string(&config)
    }
}

/// Helper: create an HCS operation handle or return `InvalidHandle`.
fn make_operation() -> Result<HCS_OPERATION> {
    let op = unsafe { HcsCreateOperation(ptr::null(), None) };
    if op.is_null() {
        Err(Error::InvalidHandle)
    } else {
        Ok(op)
    }
}

/// Wait for an HCS operation to complete and log its result.
fn wait_operation(op: HCS_OPERATION, name: &'static str) -> Result<()> {
    let mut result_doc: *mut u16 = ptr::null_mut();
    let hr = unsafe { HcsWaitForOperationResult(op, 10_000, &mut result_doc) };
    if !result_doc.is_null() {
        let len = unsafe {
            let mut l = 0usize;
            while *result_doc.add(l) != 0 { l += 1; }
            l
        };
        let doc = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(result_doc, len) });
        eprintln!("hcs: {} result: {}", name, doc);
        info!("hcs: {} result: {}", name, doc);
    }
    if hr != S_OK {
        eprintln!("hcs: {} failed: HRESULT 0x{:08X}", name, hr);
        return Err(Error::Hresult(name, hr));
    }
    Ok(())
}

/// A running HCS compute system (lightweight Hyper-V VM).
pub struct HcsVm {
    handle: HCS_SYSTEM,
    id: String,
}

// HCS_SYSTEM is a raw pointer; we manage its lifetime explicitly.
unsafe impl Send for HcsVm {}

impl HcsVm {
    /// Create a VM from a raw JSON config string.
    pub fn create_from_json(id: &str, json_str: &str) -> Result<Self> {
        info!("hcs: creating VM '{}' with raw JSON", id);
        Self::create_impl(id, json_str)
    }

    /// Create a VM. Calls `HcsCreateComputeSystem`.
    pub fn create(id: &str, config: &VmConfig) -> Result<Self> {
        let json_str = config.to_json().map_err(Error::Json)?;
        info!("hcs: creating VM '{}' with config: {}", id, json_str);
        Self::create_impl(id, &json_str)
    }

    fn create_impl(id: &str, json_str: &str) -> Result<Self> {

        let id_wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        let json_wide: Vec<u16> = json_str
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let operation = make_operation()?;
        let mut handle: HCS_SYSTEM = ptr::null_mut();

        let hr = unsafe {
            HcsCreateComputeSystem(
                id_wide.as_ptr(),
                json_wide.as_ptr(),
                operation,
                ptr::null(),
                &mut handle,
            )
        };

        if hr != S_OK {
            unsafe { HcsCloseOperation(operation) };
            return Err(Error::Hresult("HcsCreateComputeSystem", hr));
        }
        if handle.is_null() {
            unsafe { HcsCloseOperation(operation) };
            return Err(Error::InvalidHandle);
        }

        // Wait for create operation to complete
        wait_operation(operation, "HcsCreateComputeSystem")?;
        unsafe { HcsCloseOperation(operation) };

        info!("HCS VM '{}' created (handle={:p})", id, handle);
        Ok(HcsVm {
            handle,
            id: id.to_string(),
        })
    }

    /// Start the VM. Calls `HcsStartComputeSystem`.
    pub fn start(&self) -> Result<()> {
        let operation = make_operation()?;

        let hr = unsafe { HcsStartComputeSystem(self.handle, operation, ptr::null()) };

        if hr != S_OK {
            unsafe { HcsCloseOperation(operation) };
            return Err(Error::Hresult("HcsStartComputeSystem", hr));
        }

        // Wait for start operation to complete
        wait_operation(operation, "HcsStartComputeSystem")?;
        unsafe { HcsCloseOperation(operation) };

        info!("HCS VM '{}' started", self.id);
        Ok(())
    }

    /// Query the RuntimeId GUID assigned by Hyper-V to this compute system.
    ///
    /// The RuntimeId is the VM partition GUID that AF_HYPERV sockets need
    /// for the `vm_id` field in `sockaddr_hv`. This is different from the
    /// identity string passed to `HcsCreateComputeSystem`.
    pub fn runtime_id(&self) -> Result<String> {
        let operation = make_operation()?;

        let hr = unsafe {
            HcsGetComputeSystemProperties(self.handle, operation, ptr::null())
        };

        if hr != S_OK {
            unsafe { HcsCloseOperation(operation) };
            return Err(Error::Hresult("HcsGetComputeSystemProperties", hr));
        }

        let mut result_doc: *mut u16 = ptr::null_mut();
        let hr = unsafe { HcsWaitForOperationResult(operation, 5_000, &mut result_doc) };
        if hr != S_OK {
            unsafe { HcsCloseOperation(operation) };
            return Err(Error::Hresult("HcsGetComputeSystemProperties (wait)", hr));
        }

        let json_str = if !result_doc.is_null() {
            let len = unsafe {
                let mut l = 0usize;
                while *result_doc.add(l) != 0 { l += 1; }
                l
            };
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(result_doc, len) })
        } else {
            unsafe { HcsCloseOperation(operation) };
            return Err(Error::Hresult("HcsGetComputeSystemProperties (no result)", 0));
        };

        unsafe { HcsCloseOperation(operation) };

        // Parse RuntimeId from the JSON response
        if let Ok(props) = serde_json::from_str::<serde_json::Value>(&json_str) {
            if let Some(runtime_id) = props.get("RuntimeId").and_then(|v| v.as_str()) {
                info!("HCS VM '{}' RuntimeId: {}", self.id, runtime_id);
                return Ok(runtime_id.to_string());
            }
        }

        eprintln!("hcs: RuntimeId not found in properties: {}", &json_str[..json_str.len().min(500)]);
        Err(Error::Hresult("HcsGetComputeSystemProperties (no RuntimeId)", 0))
    }

    /// Wait for the VM to exit. Calls `HcsWaitForComputeSystemExit`.
    ///
    /// Note: this function takes a result-document pointer rather than an
    /// operation handle — it blocks synchronously until the VM exits.
    pub fn wait(&self) -> Result<()> {
        let mut result_doc: *mut u16 = ptr::null_mut();
        let hr = unsafe {
            HcsWaitForComputeSystemExit(self.handle, u32::MAX, &mut result_doc)
        };

        // Log result document if available — this contains the VM exit reason.
        if !result_doc.is_null() {
            let len = unsafe {
                let mut l = 0usize;
                while *result_doc.add(l) != 0 { l += 1; }
                l
            };
            let doc = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(result_doc, len) });
            eprintln!("hcs: VM exit result: {}", doc);
            info!("hcs: wait result: {}", doc);
        } else {
            eprintln!("hcs: VM exited (no result document)");
        }

        if hr != S_OK {
            return Err(Error::Hresult("HcsWaitForComputeSystemExit", hr));
        }

        info!("HCS VM '{}' exited", self.id);
        Ok(())
    }

    /// Terminate the VM. Calls `HcsTerminateComputeSystem`.
    pub fn terminate(&self) -> Result<()> {
        let operation = make_operation()?;

        let hr =
            unsafe { HcsTerminateComputeSystem(self.handle, operation, ptr::null()) };

        unsafe { HcsCloseOperation(operation) };

        if hr != S_OK {
            return Err(Error::Hresult("HcsTerminateComputeSystem", hr));
        }

        info!("HCS VM '{}' terminated", self.id);
        Ok(())
    }
}

impl Drop for HcsVm {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { HcsCloseComputeSystem(self.handle) };
            self.handle = ptr::null_mut();
        }
    }
}
