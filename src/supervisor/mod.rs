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
//! # State machine
//!
//! A sandbox moves through exactly one deterministic path:
//!
//! ```text
//! starting ──▶ running ──▶ (stopped | error)
//! ```
//!
//! `starting` is written before the VM boots; `running` once the VM pid is
//! known. A **terminal** state (`stopped`/`error`) MUST carry an `exit_code`
//! and an `exited_at_ms` timestamp. The supervisor writes the terminal state
//! before it exits, and [`reconcile`] recovers a stale `running` state whose VM
//! pid is dead into a terminal `stopped` with a deterministic exit code, so a
//! crash never leaves a sandbox stuck in `running`.
//!
//! # Protocol (control socket)
//!
//! Newline-delimited JSON over Unix stream socket.
//! See the plan §5 for verb definitions.

pub mod proto;
pub mod secrets;
pub mod snapshots;
pub mod volumes;

pub use nanosb_host::supervisor::client;
pub use nanosb_host::supervisor::{
    nanosb_home, sandboxes_root, AttachFrame, AttachInput, ControlRequest, ControlResponse,
    SandboxState, SupervisorState, SANDBOXES_DIR,
};

use runtime::config::{ConsoleSpec, ExtraMount};
use runtime::exec::ExecHandle;
use runtime::Sandbox as RuntimeSandbox;
use runtime::SandboxConfig;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum log file size before rotation (50 MB).
const LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Number of rotated log files to keep.
const LOG_ROTATION_COUNT: u32 = 3;

/// In-memory scrollback ring buffer capacity (in bytes).
const SCROLLBACK_CAPACITY: usize = 256 * 1024; // 256 KB

/// Control socket protocol version.
const PROTOCOL_VERSION: &str = "0.1.0";

// ---------------------------------------------------------------------------
// Control socket protocol types
// ---------------------------------------------------------------------------

/// Attached console clients: (subscriber id, stream).
pub type Subscribers = Arc<Mutex<Vec<(u64, Arc<Mutex<UnixStream>>)>>>;

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
    pub extra_mounts_json: String,
    pub timeout_secs: u64,
    /// Keep the supervisor in the caller's process tree (no `setsid`).
    pub foreground: bool,
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
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

    // Detach from the parent process group unless the caller owns us
    // (`nanosb supervise --foreground` keeps us in the caller's tree).
    if !args.foreground {
        unsafe {
            libc::setsid();
        }
    }

    let sandbox_dir = sandboxes_root().join(&args.sandbox_name);

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

    // Merge the boot env delivered out-of-band via the process environment
    // (secrets are never written to config.json/deploy.json or the argv), then
    // clear it so it is not inherited by the VM subprocess.
    let mut config = config;
    let ephemeral = config.ephemeral;
    if let Ok(boot_env_json) = std::env::var("NANOSB_BOOT_ENV") {
        if let Ok(extra) =
            serde_json::from_str::<std::collections::HashMap<String, String>>(&boot_env_json)
        {
            config.env.extend(extra);
        }
        std::env::remove_var("NANOSB_BOOT_ENV");
    }

    // Parse extra mounts
    let extra_mounts: Vec<ExtraMount> =
        serde_json::from_str(&args.extra_mounts_json).unwrap_or_default();

    // Console pipes (supervisor-owned): guest reads stdin_read and writes
    // stdout_write/stderr_write; the supervisor keeps the other ends.
    let (stdin_read, stdin_write) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to create console pipes: {}", e);
            std::process::exit(1);
        }
    };
    let (stdout_read, stdout_write) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to create console pipes: {}", e);
            std::process::exit(1);
        }
    };
    let (stderr_read, stderr_write) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to create console pipes: {}", e);
            std::process::exit(1);
        }
    };

    let console = Some(ConsoleSpec {
        stdin_fd: stdin_read.as_raw_fd(),
        stdout_fd: stdout_write.as_raw_fd(),
        stderr_fd: stderr_write.as_raw_fd(),
        tty: false,
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
    let supervisor_pid = Some(std::process::id() as i32);

    // Write initial state
    let state = SupervisorState {
        sandbox_name: args.sandbox_name.clone(),
        state: SandboxState::Starting,
        pid: None,
        exit_code: None,
        started_at: started_at.clone(),
        config_hash: config_hash.clone(),
        exited_at_ms: None,
        supervisor_pid,
    };
    write_state(&sandbox_dir, &state);

    // Boot the VM through the runtime SDK (image pull, gvproxy, VM subprocess).
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            error!("Failed to create tokio runtime: {}", e);
            write_error_state(&sandbox_dir, &args.sandbox_name, &started_at, &config_hash);
            std::process::exit(1);
        }
    };

    let mut sandbox = match rt.block_on(RuntimeSandbox::create(config)) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to create sandbox: {}", e);
            write_error_state(&sandbox_dir, &args.sandbox_name, &started_at, &config_hash);
            std::process::exit(1);
        }
    };

    if let Err(e) = rt.block_on(sandbox.start_next(console, extra_mounts)) {
        error!("Failed to start sandbox (next mode): {}", e);
        let _ = rt.block_on(sandbox.destroy());
        write_error_state(&sandbox_dir, &args.sandbox_name, &started_at, &config_hash);
        std::process::exit(1);
    }

    // The VM child has duplicated copies of the console fds; drop the
    // supervisor's guest-side ends so EOF propagates when the VM exits.
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);

    let vm_pid = sandbox.vm_pid();

    // Update state with PID
    let state = SupervisorState {
        sandbox_name: args.sandbox_name.clone(),
        state: SandboxState::Running,
        pid: vm_pid,
        exit_code: None,
        started_at: started_at.clone(),
        config_hash: config_hash.clone(),
        exited_at_ms: None,
        supervisor_pid,
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
    let stop_requested = Arc::new(AtomicBool::new(false));
    let force_stop = Arc::new(AtomicBool::new(false));
    let log_file = Arc::new(Mutex::new(log_file));
    let current_log_size = Arc::new(Mutex::new(0u64));
    let stdin_writer = Arc::new(Mutex::new(stdin_write));
    let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
    let next_subscriber_id = Arc::new(AtomicU64::new(1));
    let secrets = Arc::new(secrets::SecretStore::new());
    let volumes = Arc::new(volumes::VolumeRegistry::new());
    let exec_session: Arc<Mutex<Option<ExecHandle>>> = Arc::new(Mutex::new(None));
    // In-flight `wait` handlers; the supervisor drains them before exiting so a
    // blocking wait always receives its terminal response.
    let active_waiters = Arc::new(AtomicUsize::new(0));

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

    let vm_threads = vec![
        spawn_output_reader(
            stdout_read,
            running.clone(),
            scrollback.clone(),
            log_file.clone(),
            current_log_size.clone(),
            logs_dir.clone(),
            subscribers.clone(),
            nanosb_protocol::LogSource::Stdout,
        ),
        spawn_output_reader(
            stderr_read,
            running.clone(),
            scrollback.clone(),
            log_file.clone(),
            current_log_size.clone(),
            logs_dir.clone(),
            subscribers.clone(),
            nanosb_protocol::LogSource::Stderr,
        ),
    ];

    let _accept_thread = listener.map(|listener| {
        let running = running.clone();
        let stop_requested = stop_requested.clone();
        let force_stop = force_stop.clone();
        let scrollback = scrollback.clone();
        let sandbox_dir = sandbox_dir.clone();
        let sandbox_name = args.sandbox_name.clone();
        let started_at = started_at.clone();
        let config_hash = config_hash.clone();
        let stdin_writer = stdin_writer.clone();
        let subscribers = subscribers.clone();
        let next_subscriber_id = next_subscriber_id.clone();
        let secrets = secrets.clone();
        let volumes = volumes.clone();
        let exec_session = exec_session.clone();
        let active_waiters = active_waiters.clone();
        std::thread::spawn(move || {
            accept_connections(
                &listener,
                running,
                stop_requested,
                force_stop,
                &scrollback,
                &sandbox_dir,
                &sandbox_name,
                &started_at,
                &config_hash,
                stdin_writer,
                subscribers,
                next_subscriber_id,
                secrets,
                volumes,
                exec_session,
                active_waiters,
            );
        })
    });

    // Wait for VM exit (console EOF), a stop request, or timeout.
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        if stop_requested.load(Ordering::SeqCst) {
            info!("Stop requested for sandbox '{}'", args.sandbox_name);
            break;
        }
        if vm_threads.iter().all(|t| t.is_finished()) {
            info!("VM exited for sandbox '{}'", args.sandbox_name);
            break;
        }
        if Instant::now() > deadline {
            warn!(
                "Sandbox '{}' timed out after {}s, stopping VM",
                args.sandbox_name, args.timeout_secs
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Cleanup
    running.store(false, Ordering::SeqCst);
    let _ = rt.block_on(sandbox.stop());
    for t in vm_threads {
        let _ = t.join();
    }

    // Give the boot thread a moment to record the VM exit code, then read it.
    let mut vm_exit_code = None;
    for _ in 0..20 {
        if let Some(code) = sandbox.vm_exit_code() {
            vm_exit_code = Some(code);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = rt.block_on(sandbox.destroy());

    let exit_code = vm_exit_code.unwrap_or(0);

    broadcast_exit(&subscribers, exit_code);

    let _ = std::fs::remove_file(&control_sock_path);
    let _ = std::fs::remove_file(sandbox_dir.join("supervisor.pid"));

    let final_state = SupervisorState {
        sandbox_name: args.sandbox_name.clone(),
        state: SandboxState::Stopped,
        pid: None,
        exit_code: Some(exit_code),
        started_at,
        config_hash,
        exited_at_ms: Some(now_ms()),
        supervisor_pid,
    };
    write_state(&sandbox_dir, &final_state);

    // Drain in-flight `wait` handlers so a blocking wait receives its terminal
    // response before the process exits (bounded to avoid a wedged client).
    let drain_deadline = Instant::now() + Duration::from_secs(5);
    while active_waiters.load(Ordering::SeqCst) > 0 && Instant::now() < drain_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    cleanup_after_stop(&sandbox_dir, ephemeral);

    std::process::exit(exit_code);
}

fn cleanup_after_stop(sandbox_dir: &Path, ephemeral: bool) {
    if ephemeral {
        info!(
            "ephemeral sandbox stopped; removing {}",
            sandbox_dir.display()
        );
        let _ = std::fs::remove_dir_all(sandbox_dir);
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn write_state(sandbox_dir: &Path, state: &SupervisorState) {
    let state_path = sandbox_dir.join("state.json");
    if let Ok(json) = serde_json::to_string_pretty(state) {
        // Atomic write: a crash mid-write leaves the previous state.json intact
        // rather than a truncated file.
        let tmp_path = sandbox_dir.join("state.json.tmp");
        if std::fs::write(&tmp_path, &json).is_ok() {
            let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
            let _ = std::fs::rename(&tmp_path, &state_path);
        }
    }
}

fn write_error_state(sandbox_dir: &Path, name: &str, started_at: &str, config_hash: &str) {
    let state = SupervisorState {
        sandbox_name: name.to_string(),
        state: SandboxState::Error,
        pid: None,
        exit_code: Some(-1),
        started_at: started_at.to_string(),
        config_hash: config_hash.to_string(),
        exited_at_ms: Some(now_ms()),
        supervisor_pid: Some(std::process::id() as i32),
    };
    write_state(sandbox_dir, &state);
}

/// Unix epoch milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Whether a pid is alive (or exists but is not signalable, i.e. EPERM).
fn pid_alive(pid: i32) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Decode a `waitpid` status into a deterministic exit code: `128 + signal`
/// when the process was killed by a signal (SIGKILL → 137), the exit status
/// when it exited, else 255.
fn decode_status(status: libc::c_int) -> i32 {
    if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        255
    }
}

/// Reconcile a persisted state against the live VM pid. A `running` state whose
/// pid is dead is terminal: return `stopped` with a deterministic exit code so
/// a crashed supervisor never leaves a sandbox stuck in `running`.
fn reconcile(state: SupervisorState) -> SupervisorState {
    if state.state != SandboxState::Running {
        return state;
    }
    // While the owning supervisor is alive it is mid-shutdown and will write the
    // real terminal state; never synthesize one that races that write.
    if state.supervisor_pid.map(pid_alive).unwrap_or(false) {
        return state;
    }
    let Some(pid) = state.pid.filter(|&p| p > 0) else {
        return state;
    };
    if pid_alive(pid) {
        // The process exists; a zombie is terminal and carries its real status.
        let mut status: libc::c_int = 0;
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc != pid {
            return state;
        }
        let mut s = state;
        s.state = SandboxState::Stopped;
        s.exit_code = Some(decode_status(status));
        s.exited_at_ms = Some(now_ms());
        return s;
    }
    // kill(pid, 0) failed (ESRCH): the process is gone and already reaped, so
    // the cause is unknowable — use the deterministic fallback.
    let mut s = state;
    s.state = SandboxState::Stopped;
    s.exit_code = Some(255);
    s.exited_at_ms = Some(now_ms());
    s
}

fn broadcast_output(subscribers: &Subscribers, data: &[u8]) {
    if let Ok(frame) = serde_json::to_string(&AttachFrame::Output {
        data: String::from_utf8_lossy(data).to_string(),
    }) {
        broadcast_line(subscribers, &frame);
    }
}

fn broadcast_exit(subscribers: &Subscribers, code: i32) {
    if let Ok(frame) = serde_json::to_string(&AttachFrame::Exit { code }) {
        broadcast_line(subscribers, &frame);
    }
}

fn broadcast_line(subscribers: &Subscribers, line: &str) {
    let mut payload = line.as_bytes().to_vec();
    payload.push(b'\n');
    let mut dead: Vec<u64> = Vec::new();
    if let Ok(subs) = subscribers.lock() {
        debug!("broadcast: {} subscriber(s)", subs.len());
        for (id, stream) in subs.iter() {
            if let Ok(mut s) = stream.lock() {
                if let Err(e) = s.write_all(&payload) {
                    warn!("broadcast: write failed for subscriber {}: {}", id, e);
                    dead.push(*id);
                }
            }
        }
    }
    if !dead.is_empty() {
        if let Ok(mut subs) = subscribers.lock() {
            subs.retain(|(id, _)| !dead.contains(id));
        }
    }
}

fn log_source_str(source: nanosb_protocol::LogSource) -> &'static str {
    match source {
        nanosb_protocol::LogSource::Stdout => "stdout",
        nanosb_protocol::LogSource::Stderr => "stderr",
        nanosb_protocol::LogSource::Pty => "pty",
        nanosb_protocol::LogSource::System => "system",
    }
}

fn spawn_output_reader<R: Read + Send + 'static>(
    mut reader: R,
    running: Arc<AtomicBool>,
    scrollback: Arc<Mutex<ScrollbackRing>>,
    log_file: Arc<Mutex<std::fs::File>>,
    current_log_size: Arc<Mutex<u64>>,
    logs_dir: PathBuf,
    subscribers: Subscribers,
    source: nanosb_protocol::LogSource,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let source_name = log_source_str(source);
        let mut source_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs_dir.join(format!("{source_name}.log")))
            .ok();
        let mut jsonl_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs_dir.join("console.jsonl"))
            .ok();
        let mut line_cursor: u64 = 0;
        let mut pending = String::new();
        let mut buf = [0u8; 4096];
        loop {
            if !running.load(Ordering::SeqCst) {
                let _ = reader.read(&mut buf);
                break;
            }

            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = &buf[..n];

                    if let Ok(mut sb) = scrollback.lock() {
                        sb.push(data);
                    }

                    if let Some(f) = source_file.as_mut() {
                        let _ = f.write_all(data);
                        let _ = f.flush();
                    }

                    pending.push_str(&String::from_utf8_lossy(data));
                    while let Some(idx) = pending.find('\n') {
                        let line: String = pending.drain(..=idx).collect();
                        let line = line.trim_end_matches(['\n', '\r']);
                        if let Some(j) = jsonl_file.as_mut() {
                            let rec = serde_json::json!({
                                "cursor": line_cursor,
                                "source": source_name,
                                "data": line,
                            });
                            let _ = writeln!(j, "{}", rec);
                            let _ = j.flush();
                        }
                        line_cursor += 1;
                    }

                    if let Ok(lf) = log_file.lock() {
                        if let Ok(mut size) = current_log_size.lock() {
                            *size += n as u64;
                            if *size >= LOG_MAX_BYTES {
                                drop(lf);
                                drop(size);
                                rotate_logs(&logs_dir);
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
                                if let Ok(mut lf2) = log_file.lock() {
                                    let _ = lf2.write_all(data);
                                    let _ = lf2.flush();
                                }
                            }
                        }
                    }

                    broadcast_output(&subscribers, data);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

fn accept_connections(
    listener: &UnixListener,
    running: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    force_stop: Arc<AtomicBool>,
    scrollback: &Arc<Mutex<ScrollbackRing>>,
    sandbox_dir: &Path,
    sandbox_name: &str,
    started_at: &str,
    config_hash: &str,
    stdin_writer: Arc<Mutex<std::io::PipeWriter>>,
    subscribers: Subscribers,
    next_subscriber_id: Arc<AtomicU64>,
    secrets: Arc<secrets::SecretStore>,
    volumes: Arc<volumes::VolumeRegistry>,
    exec_session: Arc<Mutex<Option<ExecHandle>>>,
    active_waiters: Arc<AtomicUsize>,
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
                let stop_requested = stop_requested.clone();
                let force_stop = force_stop.clone();
                let sandbox_dir = sandbox_dir.clone();
                let sandbox_name = sandbox_name.clone();
                let started_at = started_at.clone();
                let config_hash = config_hash.clone();
                let stdin_writer = stdin_writer.clone();
                let subscribers = subscribers.clone();
                let next_subscriber_id = next_subscriber_id.clone();
                let secrets = secrets.clone();
                let volumes = volumes.clone();
                let exec_session = exec_session.clone();
                let active_waiters = active_waiters.clone();

                std::thread::spawn(move || {
                    handle_connection(
                        stream,
                        &running,
                        &stop_requested,
                        &force_stop,
                        &scrollback,
                        &sandbox_dir,
                        &sandbox_name,
                        &started_at,
                        &config_hash,
                        stdin_writer,
                        subscribers,
                        next_subscriber_id,
                        secrets,
                        volumes,
                        exec_session,
                        active_waiters,
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
    _running: &AtomicBool,
    stop_requested: &AtomicBool,
    force_stop: &AtomicBool,
    scrollback: &Arc<Mutex<ScrollbackRing>>,
    sandbox_dir: &Path,
    _sandbox_name: &str,
    _started_at: &str,
    _config_hash: &str,
    stdin_writer: Arc<Mutex<std::io::PipeWriter>>,
    subscribers: Subscribers,
    next_subscriber_id: Arc<AtomicU64>,
    secrets: Arc<secrets::SecretStore>,
    volumes: Arc<volumes::VolumeRegistry>,
    exec_session: Arc<Mutex<Option<ExecHandle>>>,
    active_waiters: Arc<AtomicUsize>,
) {
    let _ = stream.set_nonblocking(false);

    // Check peer credentials (same UID only)
    #[cfg(target_os = "macos")]
    {
        // macOS: peer_cred is not available on UnixStream directly
        // We rely on socket permissions (0600) for access control
    }
    #[cfg(target_os = "linux")]
    {
        // Stable replacement for the unstable `SocketExt::peer_cred`:
        // getsockopt(SO_PEERCRED) yields the peer's pid/uid/gid.
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc == 0 {
            let our_uid = unsafe { libc::getuid() };
            if cred.uid != our_uid {
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
    let mut first = true;

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if first {
                    first = false;
                    if let Ok(nanosb_protocol::Request::Hello(hello)) =
                        serde_json::from_str::<nanosb_protocol::Request>(trimmed)
                    {
                        let ctx = proto::Ctx {
                            sandbox_dir,
                            sandbox_name: _sandbox_name,
                            started_at: _started_at,
                            config_hash: _config_hash,
                            scrollback,
                            subscribers: &subscribers,
                            stdin_writer: stdin_writer.clone(),
                            stop_requested,
                            force_stop,
                            running: _running,
                            next_subscriber_id: &next_subscriber_id,
                            secrets: secrets.as_ref(),
                            volumes: volumes.as_ref(),
                            exec_session: exec_session.as_ref(),
                            active_waiters: active_waiters.as_ref(),
                        };
                        proto::serve(stream, reader, &ctx, hello);
                        return;
                    }
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
                        if force.unwrap_or(false) {
                            force_stop.store(true, Ordering::SeqCst);
                        }
                        stop_requested.store(true, Ordering::SeqCst);
                        let resp = serde_json::to_string(&ControlResponse::Ok {
                            version: PROTOCOL_VERSION.to_string(),
                        })
                        .unwrap_or_default();
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                    ControlRequest::Attach {} => {
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

                        let subscriber_id = next_subscriber_id.fetch_add(1, Ordering::SeqCst);
                        let mut registered = false;
                        if let Ok(clone) = stream.try_clone() {
                            let _ = clone.set_write_timeout(Some(Duration::from_secs(5)));
                            if let Ok(mut subs) = subscribers.lock() {
                                subs.push((subscriber_id, Arc::new(Mutex::new(clone))));
                                registered = true;
                                info!(
                                    "attach: subscriber {} registered (total {})",
                                    subscriber_id,
                                    subs.len()
                                );
                            }
                        }

                        let mut input_line = String::new();
                        loop {
                            input_line.clear();
                            match reader.read_line(&mut input_line) {
                                Ok(0) => break,
                                Ok(_) => {
                                    if let Ok(AttachInput::Input { data }) =
                                        serde_json::from_str::<AttachInput>(input_line.trim())
                                    {
                                        if let Ok(mut w) = stdin_writer.lock() {
                                            let _ = w.write_all(data.as_bytes());
                                            let _ = w.flush();
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }

                        if registered {
                            if let Ok(mut subs) = subscribers.lock() {
                                subs.retain(|(id, _)| *id != subscriber_id);
                            }
                            info!("attach: subscriber {} unregistered", subscriber_id);
                        }
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
    let state = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(SupervisorState {
            sandbox_name: String::new(),
            state: SandboxState::Error,
            pid: None,
            exit_code: None,
            started_at: String::new(),
            config_hash: String::new(),
            exited_at_ms: None,
            supervisor_pid: None,
        });
    reconcile(state)
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
        assert!(matches!(
            req,
            ControlRequest::LogsFollow { tail: Some(100) }
        ));
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
    fn test_ephemeral_cleanup_removes_dir() {
        let tmp = std::env::temp_dir().join(format!("nsb-ephemeral-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("state.json"), "{}").unwrap();
        cleanup_after_stop(&tmp, true);
        assert!(!tmp.exists());
    }

    #[test]
    fn test_non_ephemeral_cleanup_keeps_dir() {
        let tmp = std::env::temp_dir().join(format!("nsb-persistent-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("state.json"), "{}").unwrap();
        cleanup_after_stop(&tmp, false);
        assert!(tmp.exists());
        let _ = std::fs::remove_dir_all(&tmp);
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

    fn running_state(pid: i32) -> SupervisorState {
        SupervisorState {
            sandbox_name: "t".into(),
            state: SandboxState::Running,
            pid: Some(pid),
            exit_code: None,
            started_at: "now".into(),
            config_hash: "h".into(),
            exited_at_ms: None,
            supervisor_pid: None,
        }
    }

    #[test]
    fn reconcile_keeps_live_running_state() {
        let state = running_state(std::process::id() as i32);
        let reconciled = reconcile(state.clone());
        assert_eq!(reconciled.state, SandboxState::Running);
        assert_eq!(reconciled.exit_code, None);
    }

    #[test]
    fn reconcile_keeps_terminal_state() {
        let state = SupervisorState {
            sandbox_name: "t".into(),
            state: SandboxState::Stopped,
            pid: None,
            exit_code: Some(3),
            started_at: "now".into(),
            config_hash: "h".into(),
            exited_at_ms: Some(42),
            supervisor_pid: None,
        };
        let reconciled = reconcile(state.clone());
        assert_eq!(reconciled, state);
    }

    #[test]
    fn reconcile_keeps_running_state_while_supervisor_alive() {
        let mut state = running_state(999_999_999);
        state.supervisor_pid = Some(std::process::id() as i32);
        let reconciled = reconcile(state.clone());
        assert_eq!(reconciled, state);
    }

    #[test]
    fn reconcile_marks_dead_running_pid_as_stopped() {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 7")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        assert_eq!(child.wait().unwrap().code(), Some(7));
        // The pid is now dead and reaped; kill(pid, 0) fails with ESRCH.
        let reconciled = reconcile(running_state(pid));
        assert_eq!(reconciled.state, SandboxState::Stopped);
        assert_eq!(reconciled.exit_code, Some(255));
        assert!(reconciled.exited_at_ms.is_some());
    }

    #[test]
    fn reconcile_uses_128_plus_signal_for_killed_zombie() {
        // A zombie killed by SIGKILL must reconcile to 137 (128 + SIGKILL).
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
                libc::_exit(1);
            }
        }
        assert!(pid > 0, "fork failed");
        let mut tries = 0;
        while !is_zombie(pid) && tries < 200 {
            std::thread::sleep(Duration::from_millis(10));
            tries += 1;
        }
        assert!(is_zombie(pid), "child never became a zombie");
        let reconciled = reconcile(running_state(pid));
        assert_eq!(reconciled.state, SandboxState::Stopped);
        assert_eq!(reconciled.exit_code, Some(137));
        let mut status: libc::c_int = 0;
        unsafe {
            libc::waitpid(pid, &mut status, 0);
        }
    }

    /// Whether `pid` is a zombie (exited but not yet reaped), via `ps`.
    fn is_zombie(pid: i32) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'),
            Err(_) => false,
        }
    }

    #[test]
    fn write_state_is_atomic_and_roundtrips() {
        let tmp = std::env::temp_dir().join(format!("nsb-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let state = SupervisorState {
            sandbox_name: "t".into(),
            state: SandboxState::Stopped,
            pid: None,
            exit_code: Some(137),
            started_at: "now".into(),
            config_hash: "h".into(),
            exited_at_ms: Some(99),
            supervisor_pid: None,
        };
        write_state(&tmp, &state);
        assert!(!tmp.join("state.json.tmp").exists());
        let read: SupervisorState =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("state.json")).unwrap())
                .unwrap();
        assert_eq!(read, state);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
