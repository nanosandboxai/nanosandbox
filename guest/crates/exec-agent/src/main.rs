//! Minimal in-guest exec agent for nanosandbox.
//!
//! Listens on a vsock port bridged to a host unix socket by libkrun
//! (`krun_add_vsock_port2(listen=true)`). This is the dedicated host<->guest
//! channel: it does not use the network stack, so exec works even with
//! networking disabled.
//!
//! Protocol (length-prefixed JSON frames, u32 LE length prefix):
//!   host -> guest: an `Inbound` (either `ExecRequest` to start, or
//!                  `ExecControl` to drive an in-flight session)
//!   guest -> host: one `ExecEvent` per frame

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

mod proto;
mod vsock;

use nanosb_protocol::{FsEntry, FsRequest, FsResponse, FsStat};
use proto::{ExecControl, ExecEvent, ExecRequest};

fn main() {
    let port: u32 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(1024);

    if let Err(e) = run(port) {
        diag(&format!("exec-agent: {}", e));
        std::process::exit(1);
    }
}

/// Write a diagnostic line straight to fd 2 (unbuffered, bypasses stdio).
fn diag(msg: &str) {
    let mut line = String::from(msg);
    line.push('\n');
    let bytes = line.as_bytes();
    unsafe {
        libc::write(2, bytes.as_ptr() as *const libc::c_void, bytes.len());
    }
}

fn rlimit_resource(name: &str) -> Option<libc::c_int> {
    let name = name
        .strip_prefix("RLIMIT_")
        .unwrap_or(name)
        .to_ascii_uppercase();
    Some(match name.as_str() {
        "NOFILE" | "OFILE" => libc::RLIMIT_NOFILE,
        "NPROC" => libc::RLIMIT_NPROC,
        "FSIZE" => libc::RLIMIT_FSIZE,
        "AS" => libc::RLIMIT_AS,
        "CPU" => libc::RLIMIT_CPU,
        "CORE" => libc::RLIMIT_CORE,
        "DATA" => libc::RLIMIT_DATA,
        "STACK" => libc::RLIMIT_STACK,
        "MEMLOCK" => libc::RLIMIT_MEMLOCK,
        _ => return None,
    })
}

fn parse_limit(value: &str) -> libc::rlim_t {
    let v = value.trim();
    if v.is_empty()
        || v == "-"
        || v.eq_ignore_ascii_case("infinity")
        || v.eq_ignore_ascii_case("unlimited")
    {
        libc::RLIM_INFINITY
    } else {
        v.parse::<libc::rlim_t>().unwrap_or(libc::RLIM_INFINITY)
    }
}

fn parse_rlimits(specs: &[String]) -> Vec<(libc::c_int, libc::rlimit)> {
    let mut out = Vec::new();
    for spec in specs {
        let Some((resource, values)) = spec.split_once('=') else {
            continue;
        };
        let Some(code) = rlimit_resource(resource) else {
            diag(&format!("exec-agent: unknown rlimit '{}'", resource));
            continue;
        };
        let (soft, hard) = match values.split_once(':') {
            Some((s, h)) => (s, h),
            None => (values, values),
        };
        out.push((
            code,
            libc::rlimit {
                rlim_cur: parse_limit(soft),
                rlim_max: parse_limit(hard),
            },
        ));
    }
    out
}

#[derive(serde::Deserialize)]
struct MountPlan {
    target: String,
    kind: String,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    fstype: Option<String>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    gid: Option<u32>,
    #[serde(default)]
    readonly: bool,
}

fn apply_one_mount(plan: &MountPlan) -> Result<(), String> {
    let _ = std::fs::create_dir_all(&plan.target);
    let target = std::ffi::CString::new(plan.target.clone()).map_err(|e| e.to_string())?;
    let mut flags: libc::c_ulong = 0;
    for opt in &plan.options {
        match opt.as_str() {
            "noexec" => flags |= libc::MS_NOEXEC as libc::c_ulong,
            "nosuid" => flags |= libc::MS_NOSUID as libc::c_ulong,
            "nodev" => flags |= libc::MS_NODEV as libc::c_ulong,
            "ro" => flags |= libc::MS_RDONLY as libc::c_ulong,
            _ => {}
        }
    }
    if plan.readonly {
        flags |= libc::MS_RDONLY as libc::c_ulong;
    }
    let (source, fstype, extra) = match plan.kind.as_str() {
        "tmpfs" => ("tmpfs".to_string(), "tmpfs".to_string(), 0u64),
        "virtiofs" => (
            plan.tag.clone().unwrap_or_default(),
            "virtiofs".to_string(),
            0u64,
        ),
        "file" => (
            plan.source.clone().unwrap_or_default(),
            String::new(),
            libc::MS_BIND as u64,
        ),
        "disk" => (
            plan.source.clone().unwrap_or_default(),
            plan.fstype.clone().unwrap_or_else(|| "auto".to_string()),
            0u64,
        ),
        other => return Err(format!("unknown mount kind '{other}'")),
    };
    // tmpfs consumes size/mode in the data string; virtiofs rejects bind
    // options such as rbind/rprivate and is already mounted at the share root.
    let data = match plan.kind.as_str() {
        "tmpfs" => plan.options.join(","),
        _ => String::new(),
    };
    let c_source = std::ffi::CString::new(source).map_err(|e| e.to_string())?;
    let c_fstype = std::ffi::CString::new(fstype).map_err(|e| e.to_string())?;
    let c_data = std::ffi::CString::new(data).map_err(|e| e.to_string())?;
    let rc = unsafe {
        libc::mount(
            c_source.as_ptr(),
            target.as_ptr(),
            c_fstype.as_ptr(),
            flags | extra,
            c_data.as_ptr() as *const libc::c_void,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if plan.uid.is_some() || plan.gid.is_some() {
        let uid = plan.uid.map(|u| u as libc::uid_t).unwrap_or(u32::MAX);
        let gid = plan.gid.map(|g| g as libc::gid_t).unwrap_or(u32::MAX);
        unsafe {
            libc::chown(target.as_ptr(), uid, gid);
        }
    }
    Ok(())
}

fn apply_startup_config() {
    if let Ok(specs) = std::env::var("NANOSB_RLIMITS") {
        let limits: Vec<String> = serde_json::from_str(&specs).unwrap_or_default();
        for (resource, limit) in parse_rlimits(&limits) {
            unsafe {
                if libc::setrlimit(resource, &limit) != 0 {
                    diag("exec-agent: startup setrlimit failed");
                }
            }
        }
    }
    if let Ok(hostname) = std::env::var("NANOSB_HOSTNAME") {
        if !hostname.is_empty() {
            if let Ok(c) = std::ffi::CString::new(hostname.clone()) {
                unsafe {
                    if libc::sethostname(c.as_ptr(), hostname.len()) != 0 {
                        diag("exec-agent: sethostname failed");
                    }
                }
            }
        }
    }
    if let Ok(mounts) = std::env::var("NANOSB_MOUNTS") {
        match serde_json::from_str::<Vec<MountPlan>>(&mounts) {
            Ok(plans) => {
                for plan in &plans {
                    if let Err(e) = apply_one_mount(plan) {
                        diag(&format!("exec-agent: mount {} failed: {}", plan.target, e));
                    }
                }
            }
            Err(e) => diag(&format!("exec-agent: bad NANOSB_MOUNTS: {}", e)),
        }
    }
    if let Ok(docker) = std::env::var("NANOSB_DOCKER") {
        match serde_json::from_str::<serde_json::Value>(&docker) {
            Ok(v) => {
                let enabled = v.get("enabled").and_then(|j| j.as_bool()).unwrap_or(false);
                let runtime = v.get("runtime").and_then(|j| j.as_str()).unwrap_or_default();
                let network = v.get("network").and_then(|j| j.as_str()).unwrap_or_default();
                let socket = v.get("socket").and_then(|j| j.as_str()).unwrap_or_default();
                diag(&format!(
                    "exec-agent: docker config present (enabled={}, runtime={}, network={}, socket={})",
                    enabled, runtime, network, socket
                ));
            }
            Err(e) => diag(&format!("exec-agent: bad NANOSB_DOCKER: {}", e)),
        }
    }
}

fn run(port: u32) -> Result<(), String> {
    apply_startup_config();
    diag(&format!("exec-agent: listening on vsock port {}", port));
    // Match smolvm: the guest SERVES a vsock listener and the host connects in
    // (libkrun's `listen=true` acceptor bridges the host's unix socket to us).
    let listener = vsock::VsockListener::bind(port)?;
    loop {
        let stream = listener.accept()?;
        diag("exec-agent: host connected");
        if let Err(e) = serve(stream) {
            diag(&format!("exec-agent: session error: {}", e));
        }
    }
}

/// Shared, mutex-guarded writer so the output threads and the main loop can
/// interleave event frames safely.
#[derive(Clone)]
struct Writer {
    stream: Arc<Mutex<vsock::VsockStream>>,
}

impl Writer {
    fn send<T: serde::Serialize>(&self, value: &T) -> Result<(), String> {
        let json = serde_json::to_vec(value).map_err(|e| format!("serialize: {}", e))?;
        let len = (json.len() as u32).to_le_bytes();
        let mut s = self.stream.lock().unwrap();
        s.write_all(&len)
            .and_then(|_| s.write_all(&json))
            .and_then(|_| s.flush())
            .map_err(|e| format!("write: {}", e))
    }

    fn event(&self, event: &ExecEvent) -> Result<(), String> {
        self.send(event)
    }
}

/// Serve one host connection: handle requests and their in-flight controls.
fn serve(stream: vsock::VsockStream) -> Result<(), String> {
    let mut reader = stream.try_clone()?;
    let writer = Writer {
        stream: Arc::new(Mutex::new(stream)),
    };

    // The currently running session, if any.
    let mut active: Option<Session> = None;
    // An open streamed fs write, if any.
    let mut fs_stream: Option<FsStreamState> = None;

    loop {
        let frame = read_frame(&mut reader)?;
        let Some(bytes) = frame else {
            if let Some(mut s) = active.take() {
                let _ = s.kill();
            }
            fs_stream.take();
            return Ok(());
        };

        // A frame is either a new request or a control for the active session.
        if let Ok(ctl) = serde_json::from_slice::<ExecControl>(&bytes) {
            if let Some(s) = active.as_mut() {
                if let Err(e) = s.apply(ctl) {
                    writer.event(&ExecEvent::Error { message: e })?;
                }
            }
            continue;
        }

        if let Ok(fs) = serde_json::from_slice::<FsRequest>(&bytes) {
            if let Err(e) = handle_fs(fs, &mut fs_stream, &writer) {
                writer.send(&FsResponse::error(e))?;
            }
            continue;
        }

        let req: ExecRequest = match serde_json::from_slice(&bytes) {
            Ok(r) => r,
            Err(e) => {
                writer.event(&ExecEvent::Error { message: e.to_string() })?;
                continue;
            }
        };

        // The waiter thread emitted `Exit` already once the previous command
        // finished; here we only need to drop it so a new one can start.
        if let Some(s) = active.as_ref() {
            if !s.is_finished() {
                writer.event(&ExecEvent::Error {
                    message: "a command is already running".to_string(),
                })?;
                continue;
            }
            active = None;
        }

        match Session::start(req, writer.clone()) {
            Ok(s) => active = Some(s),
            Err(e) => {
                writer.event(&ExecEvent::Error { message: e })?;
                writer.event(&ExecEvent::Exit { code: 127 })?;
            }
        }
    }
}

/// An open streamed fs write: the file handle is kept across `FsWriteChunk`
/// frames until `FsWriteClose`.
struct FsStreamState {
    file: std::fs::File,
    path: String,
}

fn handle_fs(req: FsRequest, fs_stream: &mut Option<FsStreamState>, w: &Writer) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    match req {
        FsRequest::FsRead { path, offset, length } => {
            let data = std::fs::read(&path).map_err(|e| format!("read {}: {}", path, e))?;
            let start = offset.unwrap_or(0) as usize;
            let end = match length {
                Some(l) => start.saturating_add(l as usize),
                None => data.len(),
            };
            let slice = data.get(start..end).unwrap_or(&[]);
            w.send(&FsResponse::FsData {
                data_base64: nanosb_protocol::base64_encode(slice),
            })
        }
        FsRequest::FsWrite { path, data_base64, append, mode } => {
            let bytes = nanosb_protocol::base64_decode(&data_base64)
                .ok_or_else(|| "invalid base64 payload".to_string())?;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true);
            if append {
                opts.append(true);
            } else {
                opts.truncate(true);
            }
            let mut file = opts.open(&path).map_err(|e| format!("write {}: {}", path, e))?;
            file.write_all(&bytes).map_err(|e| format!("write {}: {}", path, e))?;
            if let Some(m) = mode {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m));
            }
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsWriteStreamOpen { path, append, mode } => {
            if fs_stream.is_some() {
                return Err("a streamed write is already open".to_string());
            }
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true);
            if append {
                opts.append(true);
            } else {
                opts.truncate(true);
            }
            let file = opts
                .open(&path)
                .map_err(|e| format!("open stream {}: {}", path, e))?;
            if let Some(m) = mode {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m));
            }
            *fs_stream = Some(FsStreamState { file, path });
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsWriteChunk { data_base64 } => {
            let bytes = nanosb_protocol::base64_decode(&data_base64)
                .ok_or_else(|| "invalid base64 payload".to_string())?;
            let st = fs_stream
                .as_mut()
                .ok_or_else(|| "no open streamed write".to_string())?;
            st.file
                .write_all(&bytes)
                .map_err(|e| format!("stream write {}: {}", st.path, e))?;
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsWriteClose => {
            let mut st = fs_stream
                .take()
                .ok_or_else(|| "no open streamed write".to_string())?;
            st.file
                .flush()
                .map_err(|e| format!("flush {}: {}", st.path, e))?;
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsList { path } => {
            let mut entries = Vec::new();
            let dir = std::fs::read_dir(&path).map_err(|e| format!("list {}: {}", path, e))?;
            for entry in dir {
                let entry = entry.map_err(|e| format!("list {}: {}", path, e))?;
                let p = entry.path();
                let meta = entry.metadata().map_err(|e| format!("stat {}: {}", p.display(), e))?;
                let sym = std::fs::symlink_metadata(&p).ok();
                entries.push(FsEntry {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path: p.to_string_lossy().to_string(),
                    is_dir: meta.is_dir(),
                    is_symlink: sym.map(|m| m.file_type().is_symlink()).unwrap_or(false),
                    size: meta.len(),
                    mode: Some(meta.permissions().mode()),
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            w.send(&FsResponse::FsEntries { entries })
        }
        FsRequest::FsStat { path } => {
            let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("stat {}: {}", path, e))?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64);
            w.send(&FsResponse::FsStat {
                stat: FsStat {
                    path,
                    is_dir: meta.is_dir(),
                    is_symlink: meta.file_type().is_symlink(),
                    size: meta.len(),
                    mode: Some(meta.permissions().mode()),
                    modified_ms,
                },
            })
        }
        FsRequest::FsExists { path } => w.send(&FsResponse::FsExists {
            exists: Path::new(&path).exists(),
        }),
        FsRequest::FsMkdir { path, recursive, mode } => {
            if recursive {
                std::fs::create_dir_all(&path).map_err(|e| format!("mkdir {}: {}", path, e))?;
            } else {
                std::fs::create_dir(&path).map_err(|e| format!("mkdir {}: {}", path, e))?;
            }
            if let Some(m) = mode {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m));
            }
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsRemove { path } => {
            std::fs::remove_file(&path).map_err(|e| format!("remove {}: {}", path, e))?;
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsRemoveDir { path, recursive } => {
            if recursive {
                std::fs::remove_dir_all(&path).map_err(|e| format!("rmdir {}: {}", path, e))?;
            } else {
                std::fs::remove_dir(&path).map_err(|e| format!("rmdir {}: {}", path, e))?;
            }
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsCopy { from, to } => {
            copy_path(std::path::PathBuf::from(&from), std::path::PathBuf::from(&to))?;
            w.send(&FsResponse::Ok)
        }
        FsRequest::FsRename { from, to } => {
            std::fs::rename(&from, &to).map_err(|e| format!("rename {}: {}", from, e))?;
            w.send(&FsResponse::Ok)
        }
    }
}

fn copy_path(from: std::path::PathBuf, to: std::path::PathBuf) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(&from).map_err(|e| format!("copy {}: {}", from.display(), e))?;
    if meta.is_dir() {
        std::fs::create_dir_all(&to).map_err(|e| format!("copy dir {}: {}", to.display(), e))?;
        for entry in std::fs::read_dir(&from).map_err(|e| format!("copy {}: {}", from.display(), e))? {
            let entry = entry.map_err(|e| format!("copy {}: {}", from.display(), e))?;
            copy_path(entry.path(), to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(&from, &to)
            .map(|_| ())
            .map_err(|e| format!("copy {}: {}", from.display(), e))
    }
}

/// A running (or just-finished) command.
struct Session {
    pid: u32,
    stdin: Option<std::process::ChildStdin>,
    pty_master: Option<std::os::fd::RawFd>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl Session {
    fn start(req: ExecRequest, writer: Writer) -> Result<Self, String> {
        use std::os::fd::FromRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let (program, args) = if req.shell {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), req.command.clone()],
            )
        } else {
            (req.command.clone(), req.args.clone())
        };

        // PTY mode: one master fd carries merged stdout/stderr and accepts
        // stdin/resize; the child gets the slave as its controlling terminal.
        let mut pty_master: Option<std::os::fd::RawFd> = None;
        let mut pty_slave: Option<std::os::fd::RawFd> = None;
        let mut cmd = Command::new(&program);
        cmd.args(&args);

        let parsed_limits = parse_rlimits(&req.rlimits);
        if !parsed_limits.is_empty() {
            unsafe {
                cmd.pre_exec(move || {
                    for (resource, limit) in &parsed_limits {
                        if libc::setrlimit(*resource, limit) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }

        if req.tty {
            let (master, slave) = openpty(req.cols.max(1), req.rows.max(1))?;
            // Child's stdio all point at the slave; `pre_exec` makes it the
            // controlling terminal via fd 0 (already the slave at that point).
            unsafe {
                cmd.stdin(Stdio::from_raw_fd(libc::dup(slave)));
                cmd.stdout(Stdio::from_raw_fd(libc::dup(slave)));
                cmd.stderr(Stdio::from_raw_fd(libc::dup(slave)));
                cmd.pre_exec(move || {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
                // Parent keeps only the master; close its slave copy after spawn.
            }
            pty_master = Some(master);
            pty_slave = Some(slave);
        } else {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }

        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &req.env {
            cmd.env(k, v);
        }

        // Announce acceptance *before* spawning the child. The host retries a
        // new exec while the agent is still binding (the vsock socket exists
        // before the guest listener does); if a reset arrives after the host has
        // read `Started`, it will not retry, so a command is never run twice.
        writer.event(&ExecEvent::Started { pid: 0 })?;

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {}: {}", program, e))?;

        // Close the parent's slave copy now that the child owns its own dups.
        if let Some(slave) = pty_slave {
            unsafe { libc::close(slave) };
        }

        let pid = child.id();

        if let Some(master) = pty_master {
            // Stream the PTY master as stdout (stderr is merged by the kernel).
            let w = writer.clone();
            let mfd = master;
            std::thread::spawn(move || {
                let mut f = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(mfd) };
                let mut buf = [0u8; 4096];
                while let Ok(n) = f.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let _ = w.event(&ExecEvent::Stdout {
                        data: String::from_utf8_lossy(&buf[..n]).to_string(),
                    });
                }
            });

            let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let done = done.clone();
                let w = writer.clone();
                let timeout = req.timeout_secs;
                std::thread::spawn(move || {
                    if timeout > 0 {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(timeout);
                        loop {
                            if done.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            if std::time::Instant::now() >= deadline {
                                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                    let code = match child.wait() {
                        Ok(st) => st.code().unwrap_or(-1),
                        Err(_) => -1,
                    };
                    let _ = w.event(&ExecEvent::Exit { code });
                    done.store(true, std::sync::atomic::Ordering::Relaxed);
                });
            }

            return Ok(Session {
                pid,
                stdin: None,
                pty_master: Some(master),
                done,
            });
        }

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        if let Some(mut s) = stdout {
            let w = writer.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let _ = w.event(&ExecEvent::Stdout {
                        data: String::from_utf8_lossy(&buf[..n]).to_string(),
                    });
                }
            });
        }
        if let Some(mut s) = stderr {
            let w = writer.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let _ = w.event(&ExecEvent::Stderr {
                        data: String::from_utf8_lossy(&buf[..n]).to_string(),
                    });
                }
            });
        }

        // Waiter thread: owns the child, emits Exit when it finishes. Signals and
        // kills are delivered by PID so this thread stays the sole reaper. The
        // session `active` flag is cleared via the shared `done` handle.
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let done = done.clone();
            let w = writer.clone();
            let timeout = req.timeout_secs;
            std::thread::spawn(move || {
                if timeout > 0 {
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs(timeout);
                    loop {
                        if done.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        if std::time::Instant::now() >= deadline {
                            unsafe {
                                libc::kill(pid as i32, libc::SIGKILL);
                            }
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
                let code = match child.wait() {
                    Ok(st) => st.code().unwrap_or(-1),
                    Err(_) => -1,
                };
                let _ = w.event(&ExecEvent::Exit { code });
                done.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        }

        Ok(Session {
            pid,
            stdin,
            pty_master: None,
            done,
        })
    }

    fn apply(&mut self, ctl: ExecControl) -> Result<(), String> {
        match ctl {
            ExecControl::Stdin { data } => {
                if let Some(mfd) = self.pty_master {
                    let bytes = data.as_bytes();
                    let mut off = 0;
                    while off < bytes.len() {
                        let n = unsafe {
                            libc::write(
                                mfd,
                                bytes[off..].as_ptr() as *const libc::c_void,
                                bytes.len() - off,
                            )
                        };
                        if n <= 0 {
                            break;
                        }
                        off += n as usize;
                    }
                } else if let Some(si) = self.stdin.as_mut() {
                    si.write_all(data.as_bytes())
                        .map_err(|e| format!("stdin write: {}", e))?;
                    si.flush().ok();
                }
            }
            ExecControl::StdinClose => {
                self.stdin = None;
            }
            ExecControl::Signal { signal } => {
                unsafe { libc::kill(self.pid as i32, signal) };
            }
            ExecControl::Kill => {
                unsafe { libc::kill(self.pid as i32, libc::SIGKILL) };
            }
            ExecControl::Resize { cols, rows } => {
                if let Some(mfd) = self.pty_master {
                    let ws = libc::winsize {
                        ws_row: rows,
                        ws_col: cols,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    };
                    unsafe {
                        libc::ioctl(mfd, libc::TIOCSWINSZ, &ws);
                    }
                }
            }
        }
        Ok(())
    }

    /// True once the waiter thread has reported the process exit.
    fn is_finished(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn kill(&mut self) -> Result<(), String> {
        unsafe { libc::kill(self.pid as i32, libc::SIGKILL) };
        Ok(())
    }
}

/// Allocate a pseudo-terminal pair, sized `cols` x `rows`.
fn openpty(cols: u16, rows: u16) -> Result<(std::os::fd::RawFd, std::os::fd::RawFd), String> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut ws,
        )
    };
    if ret < 0 {
        return Err(format!("openpty: {}", std::io::Error::last_os_error()));
    }
    Ok((master, slave))
}

/// Read one length-prefixed frame; `None` on clean EOF.
fn read_frame(reader: &mut vsock::VsockStream) -> Result<Option<Vec<u8>>, String> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(format!("read len: {}", e)),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 64 * 1024 * 1024 {
        return Err(format!("frame too large: {}", len));
    }
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("read body: {}", e))?;
    Ok(Some(buf))
}
