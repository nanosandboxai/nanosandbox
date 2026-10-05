//! Per-sandbox supervisor process.
//!
//! The supervisor is a detached (`setsid`) process that owns the VM lifecycle,
//! console I/O, logs, gvproxy, and the control socket for a single sandbox.
//! It is spawned by the CLI as a hidden `__supervise` subcommand.
//!
//! # Architecture
//!
//! ```text
//! CLI/TUI  ──spawns──▶  supervisor (setsid, detached)
//!                           │
//!                           ├── VM subprocess (internal-boot-vm)
//!                           ├── gvproxy sidecar
//!                           ├── console.log (append, 0600, rotated)
//!                           ├── control.sock (0600, same-UID)
//!                           └── state.json (registry entry)
//! ```
//!
//! # Protocol (control socket)
//!
//! Newline-delimited JSON over Unix stream socket.
//! See the plan §5 for verb definitions.

pub mod client;

use runtime::config::{ConsoleSpec, ExtraMount};
use runtime::runtime::{BootVmRequest, GvproxyManager};
use runtime::SandboxConfig;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Base directory for sandbox state: ~/.nanosandbox/sandboxes/{name}/
pub const SANDBOXES_DIR: &str = ".nanosandbox/sandboxes";

/// Maximum log file size before rotation (50 MB).
const LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Number of rotated log files to keep.
const LOG_ROTATION_COUNT: u32 = 3;

/// In-memory scrollback ring buffer capacity (in bytes).
const SCROLLBACK_CAPACITY: usize = 256 * 1024; // 256 KB

/// Default sandbox timeout (seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 3600;

/// Control socket protocol version.
const PROTOCOL_VERSION: &str = "0.1.0";

// ---------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------

/// Sandbox state as seen by the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Starting,
    Running,
    Stopped,
    Error,
}

/// Supervisor state persisted to `state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorState {
    pub sandbox_name: String,
    pub state: SandboxState,
    pub pid: Option<i32>,
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub config_hash: String,
}

// ---------------------------------------------------------------------------
// Control socket protocol types
// ---------------------------------------------------------------------------

/// Request from a client to the supervisor.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    #[serde(rename_all = "snake_case")]
    Status {},
    #[serde(rename_all = "snake_case")]
    Stop {
        force: Option<bool>,
    },
    #[serde(rename_all = "snake_case")]
    Attach {},
    #[serde(rename_all = "snake_case")]
    LogsFollow {
        tail: Option<usize>,
    },
    #[serde(rename_all = "snake_case")]
    Ping {},
}

/// Response from the supervisor to a client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlResponse {
    #[serde(rename_all = "snake_case")]
    Status {
        state: SandboxState,
        pid: Option<i32>,
        exit_code: Option<i32>,
        started_at: String,
        config_hash: String,
    },
    #[serde(rename_all = "snake_case")]
    Ok {
        version: String,
    },
    #[serde(rename_all = "snake_case")]
    Error {
        message: String,
    },
}

/// Frame sent to an attach client (output from VM).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachFrame {
    #[serde(rename_all = "snake_case")]
    Output {
        data: String,
    },
    #[serde(rename_all = "snake_case")]
    Exit {
        code: i32,
    },
}

/// Frame received from an attach client (input to VM).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachInput {
    #[serde(rename_all = "snake_case")]
    Input {
        data: String,
    },
    #[serde(rename_all = "snake_case")]
    Resize {
        cols: u16,
        rows: u16,
    },
}

// ---------------------------------------------------------------------------
// Ring buffer for scrollback
// ---------------------------------------------------------------------------

/// A simple byte ring buffer for in-memory scrollback.
pub struct ScrollbackRing {
    buffer: Vec<u8>,
    capacity: usize,
    start: usize,
    len: usize,
}

impl ScrollbackRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0u8; capacity],
            capacity,
            start: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        for &byte in data {
            if self.len < self.capacity {
                let idx = (self.start + self.len) % self.capacity;
                self.buffer[idx] = byte;
                self.len += 1;
            } else {
                self.buffer[self.start] = byte;
                self.start = (self.start + 1) % self.capacity;
            }
        }
    }

    /// Read the entire ring buffer into a Vec.
    pub fn read_all(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len);
        for i in 0..self.len {
            let idx = (self.start + i) % self.capacity;
            out.push(self.buffer[idx]);
        }
        out
    }

    /// Read the last `n` bytes.
    pub fn read_tail(&self, n: usize) -> Vec<u8> {
        let n = n.min(self.len);
        let mut out = Vec::with_capacity(n);
        let offset = self.len - n;
        for i in 0..n {
            let idx = (self.start + offset + i) % self.capacity;
            out.push(self.buffer[idx]);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Log rotation
// ---------------------------------------------------------------------------

/// Rotate log files: console.log -> console.log.1 -> console.log.2 -> console.log.3
fn rotate_logs(log_dir: &Path) {
    // Remove the oldest
    let oldest = log_dir.join(format!("console.log.{}", LOG_ROTATION_COUNT));
    let _ = std::fs::remove_file(&oldest);

    // Shift each file up
    for i in (1..=LOG_ROTATION_COUNT).rev() {
        let src = log_dir.join(format!("console.log.{}", i - 1));
        let dst = log_dir.join(format!("console.log.{}", i));
        if src.exists() {
            let _ = std::fs::rename(&src, &dst);
        }
    }

    // Rename current log
    let current = log_dir.join("console.log");
    if current.exists() {
        let rotated = log_dir.join("console.log.1");
        let _ = std::fs::rename(&current, &rotated);
    }
}

// ---------------------------------------------------------------------------
// Supervisor entry point
// ---------------------------------------------------------------------------

/// Arguments for the `__supervise` subcommand.
#[derive(Debug, Clone)]
pub struct SuperviseArgs {
    pub sandbox_name: String,
    pub config_json: String,
    pub console_stdin_fd: i32,
    pub console_stdout_fd: i32,
    pub console_stderr_fd: i32,
    pub console_tty: bool,
    pub extra_mounts_json: String,
    pub timeout_secs: u64,
}

/// Run the supervisor process. This function:
/// 1. Detaches via setsid
/// 2. Creates sandbox state directory
/// 3. Starts gvproxy
/// 4. Spawns the VM with console fds
/// 5. Writes console output to log file + ring buffer
/// 6. Listens on control socket
/// 7. Enforces timeout
/// 8. Cleans up on exit
pub fn run_supervisor(args: SuperviseArgs) -> ! {
    // Detach from parent process group
    unsafe {
        libc::setsid();
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let sandbox_dir = PathBuf::from(&home)
        .join(SANDBOXES_DIR)
        .join(&args.sandbox_name);

    // Create sandbox directory structure
    let logs_dir = sandbox_dir.join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create sandbox dirs: {}", e);
        std::process::exit(1);
    });

    // Set 0700 on sandbox dir
    let _ = std::fs::set_permissions(&sandbox_dir, std::fs::Permissions::from_mode(0o700));

    // Parse config
    let config: SandboxConfig = match serde_json::from_str(&args.config_json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to parse sandbox config: {}", e);
            std::process::exit(1);
        }
    };

    // Parse extra mounts
    let extra_mounts: Vec<ExtraMount> =
        serde_json::from_str(&args.extra_mounts_json).unwrap_or_default();

    // Build console spec
    let console = Some(ConsoleSpec {
        stdin_fd: args.console_stdin_fd,
        stdout_fd: args.console_stdout_fd,
        stderr_fd: args.console_stderr_fd,
        tty: args.console_tty,
    });

    // Compute config hash (simple SHA256 of the serialized config)
    let config_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(args.config_json.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    };

    let started_at = chrono::Utc::now().to_rfc3339();

    // Write initial state
    let state = SupervisorState {
        sandbox_name: args.sandbox_name.clone(),
        state: SandboxState::Starting,
        pid: None,
        exit_code: None,
        started_at: started_at.clone(),
        config_hash: config_hash.clone(),
    };
    write_state(&sandbox_dir, &state);

    // Start gvproxy
    let gvproxy = match GvproxyManager::start(&args.sandbox_name) {
        Ok(instance) => {
            info!("gvproxy started for sandbox '{}'", args.sandbox_name);
            Some(instance)
        }
        Err(e) => {
            warn!("Failed to start gvproxy: {} (TSI fallback)", e);
            None
        }
    };

    // Build the VM boot request
    let request = BootVmRequest {
        sandbox_id: args.sandbox_name.clone(),
        rootfs_path: config.image.clone(), // Will be resolved by the runtime
        cpus: config.cpus,
        memory_mb: config.memory_mb,
        command: config.command.clone().unwrap_or_else(|| "/bin/sleep".to_string()),
        command_args: if config.command.is_some() {
            config.command_args.clone()
        } else {
            vec!["infinity".to_string()]
        },
        network_scope: config.network.scope,
        port_mappings: config
            .network
            .port_mappings
            .iter()
            .map(|p| (p.host_port, p.container_port, p.protocol.clone()))
            .collect(),
        mounts: Vec::new(), // Will be populated by the runtime
        dns: config.network.dns.clone(),
        gvproxy_socket: gvproxy.as_ref().map(|g| g.socket_path().to_string_lossy().to_string()),
        run_as_root: config.run_as_root,
        runtime_mode: "next".to_string(),
        console,
        extra_mounts,
    };

    let config_json = serde_json::to_string(&request).unwrap_or_default();

    // Spawn the VM subprocess
    let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nanosb"));
    let mut cmd = std::process::Command::new(&exe_path);
    cmd.arg("internal-boot-vm")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to spawn VM subprocess: {}", e);
            let state = SupervisorState {
                sandbox_name: args.sandbox_name,
                state: SandboxState::Error,
                pid: None,
                exit_code: Some(-1),
                started_at,
                config_hash,
            };
            write_state(&sandbox_dir, &state);
            std::process::exit(1);
        }
    };

    // Write config to child's stdin
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(config_json.as_bytes());
    }

    let vm_pid = child.id() as i32;

    // Update state with PID
    let state = SupervisorState {
        sandbox_name: args.sandbox_name.clone(),
        state: SandboxState::Running,
        pid: Some(vm_pid),
        exit_code: None,
        started_at: started_at.clone(),
        config_hash: config_hash.clone(),
    };
    write_state(&sandbox_dir, &state);

    // Set up console log file
    let log_path = logs_dir.join("console.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .write(true)
        .open(&log_path)
        .unwrap_or_else(|e| {
            error!("Failed to open console log: {}", e);
            std::process::exit(1);
        });
    let _ = std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600));

    // Shared state between threads
    let scrollback = Arc::new(Mutex::new(ScrollbackRing::new(SCROLLBACK_CAPACITY)));
    let running = Arc::new(AtomicBool::new(true));
    let log_file = Arc::new(Mutex::new(log_file));
    let current_log_size = Arc::new(Mutex::new(0u64));

    // Start control socket listener
    let control_sock_path = sandbox_dir.join("control.sock");
    let _ = std::fs::remove_file(&control_sock_path);
    let listener = match UnixListener::bind(&control_sock_path) {
        Ok(l) => {
            let _ = std::fs::set_permissions(
                &control_sock_path,
                std::fs::Permissions::from_mode(0o600),
            );
            Some(l)
        }
        Err(e) => {
            error!(
                "Failed to bind control socket: {} — continuing without control socket",
                e
            );
            None
        }
    };

    let mut vm_threads = Vec::new();
    if let Some(out) = child.stdout.take() {
        vm_threads.push(spawn_output_reader(
            out,
            running.clone(),
            scrollback.clone(),
            log_file.clone(),
            current_log_size.clone(),
            logs_dir.clone(),
        ));
    }
    if let Some(err) = child.stderr.take() {
        vm_threads.push(spawn_output_reader(
            err,
            running.clone(),
            scrollback.clone(),
            log_file.clone(),
            current_log_size.clone(),
            logs_dir.clone(),
        ));
    }

    let _accept_thread = listener.map(|listener| {
        let running = running.clone();
        let scrollback = scrollback.clone();
        let sandbox_dir = sandbox_dir.clone();
        let sandbox_name = args.sandbox_name.clone();
        let started_at = started_at.clone();
        let config_hash = config_hash.clone();
        std::thread::spawn(move || {
            accept_connections(
                &listener,
                running,
                &scrollback,
                &sandbox_dir,
                &sandbox_name,
                &started_at,
                &config_hash,
            );
        })
    });

    // Wait for VM to exit or timeout
    wait_for_vm(
        &mut child,
        &running,
        args.timeout_secs,
        &args.sandbox_name,
        &sandbox_dir,
    );

    // Cleanup
    running.store(false, Ordering::SeqCst);
    for t in vm_threads {
        let _ = t.join();
    }

    // Stop gvproxy
    if let Some(mut g) = gvproxy {
        g.stop();
    }

    // Clean up control socket
    let _ = std::fs::remove_file(&control_sock_path);

    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn write_state(sandbox_dir: &Path, state: &SupervisorState) {
    let state_path = sandbox_dir.join("state.json");
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&state_path, &json);
        let _ = std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600));
    }
}

fn spawn_output_reader<R: Read + Send + 'static>(
    mut reader: R,
    running: Arc<AtomicBool>,
    scrollback: Arc<Mutex<ScrollbackRing>>,
    log_file: Arc<Mutex<std::fs::File>>,
    current_log_size: Arc<Mutex<u64>>,
    logs_dir: PathBuf,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            if !running.load(Ordering::SeqCst) {
                // Drain remaining data before exit
                let _ = reader.read(&mut buf);
                break;
            }

            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let data = &buf[..n];

                    // Write to scrollback
                    if let Ok(mut sb) = scrollback.lock() {
                        sb.push(data);
                    }

                    // Write to log file with rotation
                    if let Ok(lf) = log_file.lock() {
                        if let Ok(mut size) = current_log_size.lock() {
                            *size += n as u64;
                            if *size >= LOG_MAX_BYTES {
                                drop(lf);
                                drop(size);
                                // Rotate logs
                                rotate_logs(&logs_dir);
                                // Reopen log file
                                let log_path = logs_dir.join("console.log");
                                if let Ok(new_file) = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .write(true)
                                    .open(&log_path)
                                {
                                    if let Ok(mut lf2) = log_file.lock() {
                                        *lf2 = new_file;
                                    }
                                }
                                if let Ok(mut size) = current_log_size.lock() {
                                    *size = 0;
                                }
                            } else {
                                drop(size);
                                drop(lf);
                                // Re-acquire and write
                                if let Ok(mut lf2) = log_file.lock() {
                                    let _ = lf2.write_all(data);
                                    let _ = lf2.flush();
                                }
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

fn wait_for_vm(
    child: &mut std::process::Child,
    running: &AtomicBool,
    timeout_secs: u64,
    sandbox_name: &str,
    sandbox_dir: &Path,
) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let poll_interval = Duration::from_millis(100);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let exit_code = status.code().unwrap_or(-1);
                info!("VM exited with code {} for sandbox '{}'", exit_code, sandbox_name);
                running.store(false, Ordering::SeqCst);

                let state = SupervisorState {
                    sandbox_name: sandbox_name.to_string(),
                    state: SandboxState::Stopped,
                    pid: None,
                    exit_code: Some(exit_code),
                    started_at: String::new(),
                    config_hash: String::new(),
                };
                write_state(sandbox_dir, &state);
                return;
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    warn!("Sandbox '{}' timed out after {}s, killing VM", sandbox_name, timeout_secs);
                    let pid = child.id() as i32;
                    unsafe {
                        libc::kill(-pid, libc::SIGTERM);
                    }
                    std::thread::sleep(Duration::from_secs(5));
                    // Force kill
                    let _ = child.kill();
                    let _ = child.wait();
                    running.store(false, Ordering::SeqCst);

                    let state = SupervisorState {
                        sandbox_name: sandbox_name.to_string(),
                        state: SandboxState::Stopped,
                        pid: None,
                        exit_code: Some(-1),
                        started_at: String::new(),
                        config_hash: String::new(),
                    };
                    write_state(sandbox_dir, &state);
                    return;
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                error!("Error waiting for VM: {}", e);
                running.store(false, Ordering::SeqCst);
                return;
            }
        }
    }
}

fn accept_connections(
    listener: &UnixListener,
    running: Arc<AtomicBool>,
    scrollback: &Arc<Mutex<ScrollbackRing>>,
    sandbox_dir: &Path,
    sandbox_name: &str,
    started_at: &str,
    config_hash: &str,
) {
    listener.set_nonblocking(true).ok();

    let started_at = started_at.to_string();
    let config_hash = config_hash.to_string();
    let sandbox_name = sandbox_name.to_string();
    let sandbox_dir = sandbox_dir.to_path_buf();

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        match listener.accept() {
            Ok((stream, addr)) => {
                debug!("Control connection from {:?}", addr);
                let scrollback = scrollback.clone();
                let running = running.clone();
                let sandbox_dir = sandbox_dir.clone();
                let sandbox_name = sandbox_name.clone();
                let started_at = started_at.clone();
                let config_hash = config_hash.clone();

                std::thread::spawn(move || {
                    handle_connection(
                        stream,
                        &running,
                        &scrollback,
                        &sandbox_dir,
                        &sandbox_name,
                        &started_at,
                        &config_hash,
                    );
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                if running.load(Ordering::SeqCst) {
                    debug!("Accept error: {}", e);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_connection(
    mut stream: UnixStream,
    running: &AtomicBool,
    scrollback: &Arc<Mutex<ScrollbackRing>>,
    sandbox_dir: &Path,
    _sandbox_name: &str,
    _started_at: &str,
    _config_hash: &str,
) {
    // Check peer credentials (same UID only)
    #[cfg(target_os = "macos")]
    {
        // macOS: peer_cred is not available on UnixStream directly
        // We rely on socket permissions (0600) for access control
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketExt;
        if let Ok(cred) = stream.peer_cred() {
            let our_uid = unsafe { libc::getuid() };
            if cred.uid() != our_uid {
                let _ = stream.write_all(
                    serde_json::to_string(&ControlResponse::Error {
                        message: "Permission denied: peer UID mismatch".to_string(),
                    })
                    .unwrap_or_default()
                    .as_bytes(),
                );
                let _ = stream.write_all(b"\n");
                return;
            }
        }
    }

    let read_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            debug!("Failed to clone control stream: {}", e);
            return;
        }
    };
    let mut reader = BufReader::new(read_stream);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let request: ControlRequest = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        let resp = serde_json::to_string(&ControlResponse::Error {
                            message: format!("Invalid request: {}", e),
                        })
                        .unwrap_or_default();
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(b"\n");
                        continue;
                    }
                };

                match request {
                    ControlRequest::Ping {} => {
                        let resp = serde_json::to_string(&ControlResponse::Ok {
                            version: PROTOCOL_VERSION.to_string(),
                        })
                        .unwrap_or_default();
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                    ControlRequest::Status {} => {
                        let state = read_current_state(sandbox_dir);
                        let resp = serde_json::to_string(&ControlResponse::Status {
                            state: state.state,
                            pid: state.pid,
                            exit_code: state.exit_code,
                            started_at: state.started_at,
                            config_hash: state.config_hash,
                        })
                        .unwrap_or_default();
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                    ControlRequest::Stop { force } => {
                        // Signal the VM to stop
                        let pid = read_current_state(sandbox_dir).pid;
                        if let Some(pid) = pid {
                            if force.unwrap_or(false) {
                                unsafe { libc::kill(-pid, libc::SIGKILL) };
                            } else {
                                unsafe { libc::kill(-pid, libc::SIGTERM) };
                            }
                        }
                        let resp = serde_json::to_string(&ControlResponse::Ok {
                            version: PROTOCOL_VERSION.to_string(),
                        })
                        .unwrap_or_default();
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                    ControlRequest::Attach {} => {
                        // Send scrollback replay
                        if let Ok(sb) = scrollback.lock() {
                            let data = sb.read_all();
                            if !data.is_empty() {
                                let frame = serde_json::to_string(&AttachFrame::Output {
                                    data: String::from_utf8_lossy(&data).to_string(),
                                })
                                .unwrap_or_default();
                                let _ = stream.write_all(frame.as_bytes());
                                let _ = stream.write_all(b"\n");
                            }
                        }

                        // In a real implementation, we'd set up a channel to
                        // forward live output and accept input/resize frames.
                        // For now, send a placeholder and close.
                        let frame = serde_json::to_string(&AttachFrame::Exit { code: 0 })
                            .unwrap_or_default();
                        let _ = stream.write_all(frame.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                    ControlRequest::LogsFollow { tail } => {
                        // Send scrollback replay (tail N bytes)
                        if let Some(n) = tail {
                            if let Ok(sb) = scrollback.lock() {
                                let data = sb.read_tail(n);
                                if !data.is_empty() {
                                    let frame = serde_json::to_string(&AttachFrame::Output {
                                        data: String::from_utf8_lossy(&data).to_string(),
                                    })
                                    .unwrap_or_default();
                                    let _ = stream.write_all(frame.as_bytes());
                                    let _ = stream.write_all(b"\n");
                                }
                            }
                        } else {
                            // Send all scrollback
                            if let Ok(sb) = scrollback.lock() {
                                let data = sb.read_all();
                                if !data.is_empty() {
                                    let frame = serde_json::to_string(&AttachFrame::Output {
                                        data: String::from_utf8_lossy(&data).to_string(),
                                    })
                                    .unwrap_or_default();
                                    let _ = stream.write_all(frame.as_bytes());
                                    let _ = stream.write_all(b"\n");
                                }
                            }
                        }

                        // In a real implementation, we'd stream live output here.
                        // For now, send exit and close.
                        let frame = serde_json::to_string(&AttachFrame::Exit { code: 0 })
                            .unwrap_or_default();
                        let _ = stream.write_all(frame.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                debug!("Read error on control connection: {}", e);
                break;
            }
        }
    }
}

fn read_current_state(sandbox_dir: &Path) -> SupervisorState {
    let state_path = sandbox_dir.join("state.json");
    std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(SupervisorState {
            sandbox_name: String::new(),
            state: SandboxState::Error,
            pid: None,
            exit_code: None,
            started_at: String::new(),
            config_hash: String::new(),
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scrollback_ring_basic() {
        let mut ring = ScrollbackRing::new(16);
        ring.push(b"hello world");
        let data = ring.read_all();
        assert_eq!(String::from_utf8(data).unwrap(), "hello world");
    }

    #[test]
    fn test_scrollback_ring_wraparound() {
        let mut ring = ScrollbackRing::new(8);
        ring.push(b"12345678");
        ring.push(b"ABCD");
        let data = ring.read_all();
        // After wraparound: first 4 bytes overwritten by "ABCD"
        assert_eq!(String::from_utf8(data).unwrap(), "5678ABCD");
    }

    #[test]
    fn test_scrollback_ring_tail() {
        let mut ring = ScrollbackRing::new(16);
        ring.push(b"hello world");
        let tail = ring.read_tail(5);
        assert_eq!(String::from_utf8(tail).unwrap(), "world");
    }

    #[test]
    fn test_scrollback_ring_empty() {
        let ring = ScrollbackRing::new(16);
        assert!(ring.read_all().is_empty());
        assert!(ring.read_tail(10).is_empty());
    }

    #[test]
    fn test_scrollback_ring_capacity_exact() {
        let mut ring = ScrollbackRing::new(5);
        ring.push(b"hello");
        ring.push(b"world");
        let data = ring.read_all();
        assert_eq!(data.len(), 5);
        assert_eq!(String::from_utf8(data).unwrap(), "world");
    }

    #[test]
    fn test_control_request_deserialize() {
        let json = r#"{"type": "ping"}"#;
        let req: ControlRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, ControlRequest::Ping {}));

        let json = r#"{"type": "status"}"#;
        let req: ControlRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, ControlRequest::Status {}));

        let json = r#"{"type": "stop", "force": true}"#;
        let req: ControlRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, ControlRequest::Stop { force: Some(true) }));

        let json = r#"{"type": "attach"}"#;
        let req: ControlRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, ControlRequest::Attach {}));

        let json = r#"{"type": "logs_follow", "tail": 100}"#;
        let req: ControlRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, ControlRequest::LogsFollow { tail: Some(100) }));
    }

    #[test]
    fn test_control_response_serialize() {
        let resp = ControlResponse::Ok {
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"ok""#));
        assert!(json.contains(r#""version":"0.1.0""#));
    }

    #[test]
    fn test_attach_frame_serialize() {
        let frame = AttachFrame::Output {
            data: "hello".to_string(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""type":"output""#));
        assert!(json.contains(r#""data":"hello""#));
    }

    #[test]
    fn test_attach_input_deserialize() {
        let json = r#"{"type": "input", "data": "ls -la\n"}"#;
        let input: AttachInput = serde_json::from_str(json).unwrap();
        assert!(matches!(input, AttachInput::Input { .. }));

        let json = r#"{"type": "resize", "cols": 80, "rows": 24}"#;
        let input: AttachInput = serde_json::from_str(json).unwrap();
        assert!(matches!(input, AttachInput::Resize { cols: 80, rows: 24 }));
    }

    #[test]
    fn test_log_rotation_paths() {
        // Just verify the rotation logic doesn't panic
        let tmp = std::env::temp_dir().join("nanosb-test-rotation");
        let _ = std::fs::create_dir_all(&tmp);

        // Create a test log file
        let log_path = tmp.join("console.log");
        std::fs::write(&log_path, b"test data").ok();

        rotate_logs(&tmp);

        // After rotation, console.log should be gone (renamed to console.log.1)
        assert!(!log_path.exists());
        assert!(tmp.join("console.log.1").exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
