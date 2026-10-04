// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! Windows HCS VM and vCPU state management.
//!
//! Uses the Host Compute Service (computecore.dll) to create lightweight
//! Hyper-V VMs with LinuxKernelDirect boot. HCS handles LAPIC, IOAPIC,
//! IDT, PIT, and interrupt routing internally.

use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;
use std::result;
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};
use log::{error, info};
use utils::eventfd::EventFd;
use vm_memory::GuestMemoryMmap;

use hcs::{HcsVm, NetworkAdapterConfig, VmConfig};

use crate::vmm_config::machine_config::CpuFeaturesTemplate;

use super::super::FC_EXIT_CODE_OK;

/// Errors associated with the HCS hypervisor backend.
#[derive(Debug)]
pub enum Error {
    /// HCS operation failed.
    Hcs(hcs::Error),
    /// Guest memory error.
    GuestMemoryMmap(vm_memory::GuestMemoryError),
    /// Failed to clone EventFd.
    EventFd(io::Error),
    /// vCPU count is not initialized.
    VcpuCountNotInitialized,
    /// Cannot run the VCPUs.
    VcpuRun,
    /// Cannot spawn a new vCPU thread.
    VcpuSpawn(io::Error),
    /// Cannot cleanly initialize vcpu TLS.
    VcpuTlsInit,
    /// Vcpu not present in TLS.
    VcpuTlsNotPresent,
    /// Cannot configure the microvm.
    VmSetup(hcs::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Error::Hcs(e) => write!(f, "HCS error: {e}"),
            Error::GuestMemoryMmap(e) => write!(f, "Guest memory error: {e:?}"),
            Error::EventFd(e) => write!(f, "EventFd error: {e}"),
            Error::VcpuCountNotInitialized => write!(f, "vCPU count is not initialized"),
            Error::VcpuRun => write!(f, "Cannot run the VCPUs"),
            Error::VcpuSpawn(e) => write!(f, "Cannot spawn a new vCPU thread: {e}"),
            Error::VcpuTlsInit => write!(f, "Cannot clean init vcpu TLS"),
            Error::VcpuTlsNotPresent => write!(f, "Vcpu not present in TLS"),
            Error::VmSetup(e) => write!(f, "Cannot configure the microvm: {e}"),
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

// ─── VM ───────────────────────────────────────────────────────────────────────

/// A wrapper around an HCS compute system.
///
/// With HCS, memory is managed by the hypervisor internally — the `memory_init`
/// and `configure` methods are no-ops kept for API compatibility with builder.rs
/// (which will be updated in Task 4).
pub struct Vm {
    _placeholder: (),
}

impl Vm {
    /// Construct a new HCS VM placeholder.
    ///
    /// The actual HCS compute system is created in `Vcpu::run()` because HCS
    /// needs the full config (kernel path, cmdline) which isn't available at
    /// `Vm::new()` time.
    pub fn new(_nested_enabled: bool) -> Result<Self> {
        Ok(Vm { _placeholder: () })
    }

    /// Configure partition vCPU count.
    ///
    /// With HCS this is a no-op — CPU count is passed in the JSON config at
    /// VM creation time. Kept for API compatibility with builder.rs.
    pub fn configure(&mut self, _vcpu_count: u32) -> Result<()> {
        Ok(())
    }

    /// Map guest memory.
    ///
    /// With HCS, memory is managed by the hypervisor internally — this is a
    /// no-op. Kept for API compatibility with builder.rs.
    pub fn memory_init(&mut self, _guest_mem: &GuestMemoryMmap) -> Result<()> {
        Ok(())
    }
}

// ─── vCPU config ──────────────────────────────────────────────────────────────

/// vCPU configuration (matches Linux/macOS pattern).
#[derive(Debug, Eq, PartialEq)]
pub struct VcpuConfig {
    pub vcpu_count: u8,
    pub ht_enabled: bool,
    pub cpu_template: Option<CpuFeaturesTemplate>,
}

// ─── vCPU ─────────────────────────────────────────────────────────────────────

/// Events sent to the vCPU thread.
pub enum VcpuEvent {
    Resume,
    Pause,
}

/// Responses from the vCPU thread.
pub enum VcpuResponse {
    Resumed,
    Paused,
    Exited(u8),
}

/// Handle to a running vCPU thread.
pub struct VcpuHandle {
    event_sender: Sender<VcpuEvent>,
    response_receiver: Receiver<VcpuResponse>,
    #[allow(dead_code)]
    vcpu_thread: thread::JoinHandle<()>,
}

impl VcpuHandle {
    pub fn send_event(&self, event: VcpuEvent) -> result::Result<(), Error> {
        self.event_sender
            .send(event)
            .map_err(|_| Error::VcpuRun)
    }

    pub fn response_receiver(&self) -> &Receiver<VcpuResponse> {
        &self.response_receiver
    }
}

/// A virtual CPU backed by HCS.
///
/// With HCS, there is no manual vCPU run loop. The VM runs autonomously
/// after `HcsStartComputeSystem`. This "vCPU" thread just manages lifecycle.
pub struct Vcpu {
    id: u8,
    /// Kernel path on the host filesystem.
    kernel_path: PathBuf,
    /// Initrd path on the host filesystem.
    initrd_path: Option<PathBuf>,
    /// Kernel command line.
    cmdline: String,
    /// Memory size in MB.
    memory_mb: u32,
    /// CPU count.
    cpu_count: u32,
    /// Pre-created HCN networking (keeps endpoint alive for VM lifetime).
    hcn_networking: Option<hcs::hcn::HcnNetworking>,
    /// Channel to receive events from VMM.
    event_receiver: Receiver<VcpuEvent>,
    event_sender: Option<Sender<VcpuEvent>>,
    /// Channel to send responses to VMM.
    response_sender: Sender<VcpuResponse>,
    response_receiver: Option<Receiver<VcpuResponse>>,
    /// Exit event to signal VMM when the VM exits.
    exit_evt: EventFd,
}

impl Vcpu {
    /// Create a new HCS-based vCPU.
    pub fn new(
        id: u8,
        kernel_path: PathBuf,
        initrd_path: Option<PathBuf>,
        cmdline: String,
        memory_mb: u32,
        cpu_count: u32,
        hcn_networking: Option<hcs::hcn::HcnNetworking>,
        exit_evt: &EventFd,
    ) -> Result<Self> {
        let (event_sender, event_receiver) = unbounded();
        let (response_sender, response_receiver) = unbounded();

        Ok(Vcpu {
            id,
            kernel_path,
            initrd_path,
            cmdline,
            memory_mb,
            cpu_count,
            hcn_networking,
            event_receiver,
            event_sender: Some(event_sender),
            response_sender,
            response_receiver: Some(response_receiver),
            exit_evt: exit_evt.try_clone().map_err(Error::EventFd)?,
        })
    }

    /// Set the MMIO bus.
    ///
    /// With HCS the VMM does not need direct bus access from the vCPU thread
    /// (HCS handles device emulation internally), but the method is kept for
    /// API compatibility with `lib.rs`/builder.rs.
    pub fn set_mmio_bus(&mut self, _bus: devices::Bus) {}

    /// Register the kick signal handler.
    ///
    /// No-op on Windows — signal-based vCPU kick is a Linux KVM concept.
    pub fn register_kick_signal_handler() {}

    /// Main run method — creates the HCS VM, starts it, waits for exit.
    fn run(&mut self) {
        info!("vcpu {}: waiting for Resume event", self.id);

        // Wait for the initial Resume event before starting.
        loop {
            match self.event_receiver.recv() {
                Ok(VcpuEvent::Resume) => {
                    info!("vcpu {}: resumed, creating HCS VM", self.id);
                    let _ = self.response_sender.send(VcpuResponse::Resumed);
                    break;
                }
                Ok(VcpuEvent::Pause) => {
                    let _ = self.response_sender.send(VcpuResponse::Paused);
                }
                Err(_) => return,
            }
        }

        // Build HCS VM config using pre-created HCN networking.
        let network_adapter = self.hcn_networking.as_ref().map(|net| NetworkAdapterConfig {
            endpoint_id: net.endpoint_id_string(),
        });

        let config = VmConfig {
            kernel_path: self.kernel_path.clone(),
            initrd_path: self.initrd_path.clone(),
            cmdline: self.cmdline.clone(),
            memory_mb: self.memory_mb,
            cpu_count: self.cpu_count,
            network_adapter,
            enable_hvsocket: true,
        };

        let vm_id = format!("libkrun-{}-{}", std::process::id(), self.id);

        // Diagnostic: log kernel/disk/memory config for debugging boot failures.
        eprintln!("hcs: kernel={}", self.kernel_path.display());
        if let Some(ref initrd) = self.initrd_path {
            eprintln!("hcs: initrd={}", initrd.display());
        }
        eprintln!("hcs: memory={}MB, cpus={}", self.memory_mb, self.cpu_count);
        eprintln!("hcs: cmdline={}", self.cmdline);
        // Create the HCS VM.
        eprintln!("hcs: creating HCS VM '{}'...", vm_id);
        let vm = match HcsVm::create(&vm_id, &config) {
            Ok(vm) => {
                eprintln!("hcs: HCS VM created OK");
                vm
            }
            Err(e) => {
                eprintln!("hcs: ERROR: HcsCreateComputeSystem failed: {e}");
                error!("vcpu {}: HcsCreateComputeSystem failed: {e}", self.id);
                let _ = self.exit_evt.write(1);
                let _ = self.response_sender.send(VcpuResponse::Exited(1));
                return;
            }
        };

        // Connect to the console pipe to capture guest output.
        // HCS creates a named pipe server at \\.\pipe\libkrun-console for COM0.
        let _console_thread = {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;

            let pipe_name_string = format!(r"\\.\pipe\libkrun-console-{}", std::process::id());
            const GENERIC_READ: u32 = 0x80000000;
            const GENERIC_WRITE: u32 = 0x40000000;
            const OPEN_EXISTING: u32 = 3;
            const INVALID_HANDLE: usize = usize::MAX;

            extern "system" {
                fn CreateFileW(
                    name: *const u16, access: u32, share: u32, security: usize,
                    creation: u32, flags: u32, template: usize,
                ) -> usize;
                fn ReadFile(
                    file: usize, buffer: *mut u8, to_read: u32, read: *mut u32,
                    overlapped: usize,
                ) -> i32;
                fn CloseHandle(handle: usize) -> i32;
            }

            let pipe_name: Vec<u16> = OsStr::new(&pipe_name_string)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let h = unsafe {
                CreateFileW(
                    pipe_name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0, 0, OPEN_EXISTING, 0, 0,
                )
            };

            eprintln!("hcs: connecting to console pipe: {}", pipe_name_string);
            if h != INVALID_HANDLE {
                eprintln!("hcs: console pipe connected OK");
                Some(thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let mut line_buf = Vec::with_capacity(256);
                    // Write to stderr so the runtime's subprocess reader can
                    // capture guest output (stdout is piped but not read).
                    let stderr = std::io::stderr();
                    loop {
                        let mut bytes_read: u32 = 0;
                        let ok = unsafe {
                            ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &mut bytes_read, 0)
                        };
                        if ok == 0 || bytes_read == 0 {
                            break;
                        }
                        let data = &buf[..bytes_read as usize];
                        use std::io::Write;
                        for &b in data {
                            if b == b'\n' {
                                if line_buf.last() == Some(&b'\r') {
                                    line_buf.pop();
                                }
                                if !line_buf.is_empty() {
                                    let mut out = stderr.lock();
                                    let _ = out.write_all(b"[guest] ");
                                    let _ = out.write_all(&line_buf);
                                    let _ = out.write_all(b"\n");
                                    let _ = out.flush();
                                }
                                line_buf.clear();
                            } else {
                                line_buf.push(b);
                            }
                        }
                    }
                    if !line_buf.is_empty() {
                        use std::io::Write;
                        let mut out = stderr.lock();
                        let _ = out.write_all(b"[guest] ");
                        let _ = out.write_all(&line_buf);
                        let _ = out.write_all(b"\n");
                    }
                    unsafe { CloseHandle(h); }
                }))
            } else {
                eprintln!("hcs: WARNING: could not connect to console pipe (no guest output will be captured)");
                info!("vcpu: could not connect to console pipe (VM may not have COM port)");
                None
            }
        };

        // Start the VM.
        eprintln!("hcs: starting HCS VM...");
        if let Err(e) = vm.start() {
            eprintln!("hcs: ERROR: HcsStartComputeSystem failed: {e}");
            error!("vcpu {}: HcsStartComputeSystem failed: {e}", self.id);
            let _ = self.exit_evt.write(1);
            let _ = self.response_sender.send(VcpuResponse::Exited(1));
            return;
        }

        eprintln!("hcs: HCS VM started OK, waiting for exit...");
        info!("vcpu {}: HCS VM started, waiting for exit", self.id);

        // Query the RuntimeId GUID assigned by Hyper-V — this is what AF_HYPERV needs.
        match vm.runtime_id() {
            Ok(runtime_id) => {
                eprintln!("hcs: RuntimeId = {}", runtime_id);
                println!("NANOSB_HCS_VM_ID={}", runtime_id);
            }
            Err(e) => {
                eprintln!("hcs: WARNING: failed to get RuntimeId: {e}, HvSocket will not work");
                // Still output the identity string as fallback
                println!("NANOSB_HCS_VM_ID={}", vm_id);
            }
        }

        // Output guest IP so the parent process can connect via TCP.
        if let Some(ref hcn) = self.hcn_networking {
            println!("NANOSB_GUEST_IP={}", hcn.guest_ip());
            println!("NANOSB_ENDPOINT_ID={}", hcn.endpoint_id_string());
        }

        // Wait for the VM to exit.
        let wait_start = std::time::Instant::now();
        match vm.wait() {
            Ok(()) => {
                let elapsed = wait_start.elapsed();
                eprintln!("hcs: HCS VM exited normally after {:.1}s", elapsed.as_secs_f64());
                if elapsed.as_secs() < 5 {
                    eprintln!("hcs: WARNING: VM exited very quickly ({:.1}s) — kernel may have crashed before console init", elapsed.as_secs_f64());
                    eprintln!("hcs: HINT: check kernel path exists and is a valid bzImage, and that Hyper-V is enabled");
                }
                info!("vcpu {}: HCS VM exited normally", self.id);
            }
            Err(e) => {
                eprintln!("hcs: ERROR: HCS VM wait error: {e}");
                error!("vcpu {}: HCS VM wait error: {e}", self.id);
            }
        }

        // Signal exit to the VMM.
        let _ = self.exit_evt.write(1);
        let _ = self
            .response_sender
            .send(VcpuResponse::Exited(FC_EXIT_CODE_OK));
        info!("vCPU {}: exited", self.id);
    }

    /// Start this vCPU in its own thread.
    pub fn start_threaded(mut self) -> Result<VcpuHandle> {
        let event_sender = self.event_sender.take().unwrap();
        let response_receiver = self.response_receiver.take().unwrap();

        let vcpu_thread = thread::Builder::new()
            .name(format!("vcpu-{}", self.id))
            .spawn(move || {
                self.run();
            })
            .map_err(Error::VcpuSpawn)?;

        Ok(VcpuHandle {
            event_sender,
            response_receiver,
            vcpu_thread,
        })
    }
}

// ─── Boot helpers (stubs — kept for builder.rs compatibility until Task 4) ────

/// Write identity-mapped page tables to guest memory.
///
/// This is a no-op stub. With HCS/LinuxKernelDirect boot, the hypervisor
/// sets up page tables internally. This function is called from builder.rs
/// and will be removed in Task 4 when builder.rs is updated for HCS.
pub fn setup_boot_page_tables(_guest_mem: &GuestMemoryMmap) -> Result<()> {
    Ok(())
}

/// Write a minimal GDT to guest memory.
///
/// This is a no-op stub. With HCS/LinuxKernelDirect boot, the hypervisor
/// sets up the GDT internally. This function is called from builder.rs
/// and will be removed in Task 4 when builder.rs is updated for HCS.
pub fn setup_boot_gdt(_guest_mem: &GuestMemoryMmap) -> Result<()> {
    Ok(())
}
