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

fn run(port: u32) -> Result<(), String> {
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
    fn event(&self, event: &ExecEvent) -> Result<(), String> {
        let json = serde_json::to_vec(event).map_err(|e| format!("serialize: {}", e))?;
        let len = (json.len() as u32).to_le_bytes();
        let mut s = self.stream.lock().unwrap();
        s.write_all(&len)
            .and_then(|_| s.write_all(&json))
            .and_then(|_| s.flush())
            .map_err(|e| format!("write: {}", e))
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

    loop {
        let frame = read_frame(&mut reader)?;
        let Some(bytes) = frame else {
            if let Some(mut s) = active.take() {
                let _ = s.kill();
            }
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

/// A running (or just-finished) command.
struct Session {
    pid: u32,
    stdin: Option<std::process::ChildStdin>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl Session {
    fn start(req: ExecRequest, writer: Writer) -> Result<Self, String> {
        use std::process::{Command, Stdio};

        let (program, args) = if req.shell {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), req.command.clone()],
            )
        } else {
            (req.command.clone(), req.args.clone())
        };

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &req.env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {}: {}", program, e))?;

        writer.event(&ExecEvent::Started { pid: child.id() })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pid = child.id();

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
            done,
        })
    }

    fn apply(&mut self, ctl: ExecControl) -> Result<(), String> {
        match ctl {
            ExecControl::Stdin { data } => {
                if let Some(si) = self.stdin.as_mut() {
                    si.write_all(data.as_bytes())
                        .map_err(|e| format!("stdin write: {}", e))?;
                    si.flush().ok();
                }
            }
            ExecControl::Signal { signal } => {
                unsafe { libc::kill(self.pid as i32, signal) };
            }
            ExecControl::Kill => {
                unsafe { libc::kill(self.pid as i32, libc::SIGKILL) };
            }
            ExecControl::Resize { .. } => {
                // PTY resize is handled in the tty path (Phase 6).
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
