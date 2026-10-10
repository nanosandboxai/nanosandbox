//! Host-side exec client for next-mode sandboxes over the vsock bridge.
//!
//! libkrun exposes the guest's vsock exec port as a host unix socket
//! (`SandboxConfig::vsock_socket`). This client connects to that socket and
//! speaks the length-prefixed JSON frame protocol implemented by the guest
//! `exec-agent` (`guest/crates/exec-agent`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Which output stream a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// A chunk of streamed process output.
#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub stream: Stream,
    pub data: String,
}

/// Options for a single execution.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Working directory inside the guest.
    pub workdir: Option<String>,
    /// Environment variables for the child process.
    pub env: HashMap<String, String>,
    /// Run through `/bin/sh -c` instead of executing directly.
    pub shell: bool,
    /// Kill the process after this many seconds (None/0 = no timeout).
    pub timeout_secs: Option<u64>,
    /// Allocate a PTY (Phase 6).
    pub tty: bool,
    /// Initial PTY columns when `tty` is set.
    pub cols: u16,
    /// Initial PTY rows when `tty` is set.
    pub rows: u16,
}

impl ExecOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = Some(workdir.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn shell(mut self, shell: bool) -> Self {
        self.shell = shell;
        self
    }

    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Allocate a PTY for the process (interactive programs).
    pub fn tty(mut self, tty: bool) -> Self {
        self.tty = tty;
        if tty && (self.cols == 0 || self.rows == 0) {
            self.cols = 80;
            self.rows = 24;
        }
        self
    }

    /// Set the initial PTY size (used with `tty(true)`).
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }
}

/// The result of a completed (buffered) execution.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl ExecResult {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Error type for exec client operations.
#[derive(Debug)]
pub enum ExecError {
    /// The host socket for the exec channel is missing or unreachable.
    NotAvailable(String),
    /// A frame could not be read/written or was malformed.
    Protocol(String),
    /// The guest agent reported an error.
    Agent(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::NotAvailable(m) => write!(f, "exec channel unavailable: {}", m),
            ExecError::Protocol(m) => write!(f, "exec protocol error: {}", m),
            ExecError::Agent(m) => write!(f, "agent error: {}", m),
            ExecError::Io(e) => write!(f, "io error: {}", e),
        }
    }
}

impl std::error::Error for ExecError {}

impl From<std::io::Error> for ExecError {
    fn from(e: std::io::Error) -> Self {
        ExecError::Io(e)
    }
}

/// Result alias for exec client operations.
pub type ExecResultT<T> = Result<T, ExecError>;

/// A live streaming execution handle.
///
/// Feed control with [`ExecHandle::write_stdin`] / [`ExecHandle::signal`] /
/// [`ExecHandle::kill`]; drain output events with [`ExecHandle::next_chunk`].
pub struct ExecHandle {
    stream: UnixStream,
    applied_timeout: Option<Duration>,
}

/// One decoded guest event.
#[derive(Debug, Clone)]
pub enum ExecEvent {
    Started { pid: u32 },
    Output(OutputChunk),
    Exit { code: i32 },
    Error { message: String },
}

#[derive(Serialize)]
struct ExecRequestWire<'a> {
    command: &'a str,
    args: &'a [String],
    shell: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    env: &'a HashMap<String, String>,
    timeout_secs: u64,
    tty: bool,
    cols: u16,
    rows: u16,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ExecEventWire {
    Started { pid: u32 },
    Stdout { data: String },
    Stderr { data: String },
    Exit { code: i32 },
    Error { message: String },
}

/// Host-side exec client bound to one sandbox's exec socket.
pub struct ExecClient {
    socket: PathBuf,
}

impl ExecClient {
    /// Create a client for the sandbox's exec socket path.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// True if the host socket exists (the VM has booted with the exec bridge).
    pub fn is_available(&self) -> bool {
        self.socket.exists()
    }

    /// Run a command and buffer all output.
    pub fn exec(&self, command: &str, args: &[&str]) -> ExecResultT<ExecResult> {
        self.exec_with(command, args, ExecOptions::default())
    }

    /// Run a command with options and buffer all output.
    pub fn exec_with(
        &self,
        command: &str,
        args: &[&str],
        options: ExecOptions,
    ) -> ExecResultT<ExecResult> {
        let start = Instant::now();
        let mut handle = self.start(command, args, options)?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        loop {
            match handle.next_event(None)? {
                Some(ExecEvent::Output(chunk)) => match chunk.stream {
                    Stream::Stdout => stdout.push_str(&chunk.data),
                    Stream::Stderr => stderr.push_str(&chunk.data),
                },
                Some(ExecEvent::Exit { code }) => {
                    return Ok(ExecResult {
                        exit_code: code,
                        stdout,
                        stderr,
                        duration_ms: start.elapsed().as_millis() as u64,
                    });
                }
                Some(ExecEvent::Error { message }) => return Err(ExecError::Agent(message)),
                Some(ExecEvent::Started { .. }) => {}
                None => {
                    return Err(ExecError::Protocol(
                        "connection closed before exit".to_string(),
                    ))
                }
            }
        }
    }

    /// Run a command, invoking `on_chunk` for each output chunk; returns exit code.
    pub fn exec_stream<F>(
        &self,
        command: &str,
        args: &[&str],
        options: ExecOptions,
        on_chunk: F,
    ) -> ExecResultT<i32>
    where
        F: FnMut(OutputChunk),
    {
        let mut handle = self.start(command, args, options)?;
        let mut on_chunk = on_chunk;
        loop {
            match handle.next_event(None)? {
                Some(ExecEvent::Output(chunk)) => on_chunk(chunk),
                Some(ExecEvent::Exit { code }) => return Ok(code),
                Some(ExecEvent::Error { message }) => return Err(ExecError::Agent(message)),
                Some(ExecEvent::Started { .. }) => {}
                None => {
                    return Err(ExecError::Protocol(
                        "connection closed before exit".to_string(),
                    ))
                }
            }
        }
    }

    /// Start a command and return a streaming handle.
    pub fn start(
        &self,
        command: &str,
        args: &[&str],
        options: ExecOptions,
    ) -> ExecResultT<ExecHandle> {
        let stream = self.connect()?;
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let req = ExecRequestWire {
            command,
            args: &args,
            shell: options.shell,
            cwd: options.workdir.as_deref(),
            env: &options.env,
            timeout_secs: options.timeout_secs.unwrap_or(0),
            tty: options.tty,
            cols: options.cols,
            rows: options.rows,
        };
        let mut handle = ExecHandle {
            stream,
            applied_timeout: None,
        };
        handle.write_frame(&serde_json::to_vec(&req).map_err(|e| {
            ExecError::Protocol(format!("serialize request: {}", e))
        })?)?;
        Ok(handle)
    }

    /// Connect to the host exec socket, retrying while the VM finishes booting.
    fn connect(&self) -> ExecResultT<UnixStream> {
        if !self.socket.exists() {
            return Err(ExecError::NotAvailable(format!(
                "no exec socket at {}",
                self.socket.display()
            )));
        }
        let stream = UnixStream::connect(&self.socket).map_err(|e| {
            ExecError::NotAvailable(format!("connect {}: {}", self.socket.display(), e))
        })?;
        Ok(stream)
    }
}

impl ExecHandle {
    /// Read the next event. `timeout` bounds the wait for a single frame.
    ///
    /// The read timeout is applied once, at the first read: on macOS,
    /// `set_read_timeout` returns `EINVAL` if called again after the socket has
    /// already delivered data, so re-applying it per frame is not safe.
    pub fn next_event(&mut self, timeout: Option<Duration>) -> ExecResultT<Option<ExecEvent>> {
        let want = timeout.unwrap_or(Duration::ZERO);
        if self.applied_timeout != Some(want) {
            let res = if want.is_zero() {
                self.stream.set_read_timeout(None)
            } else {
                self.stream.set_read_timeout(Some(want))
            };
            if res.is_ok() {
                self.applied_timeout = Some(want);
            }
        }
        let len = match read_len(&mut self.stream) {
            Ok(Some(n)) => n,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut buf = vec![0u8; len];
        self.stream.read_exact(&mut buf)?;
        let wire: ExecEventWire = serde_json::from_slice(&buf)
            .map_err(|e| ExecError::Protocol(format!("decode event: {}", e)))?;
        Ok(Some(match wire {
            ExecEventWire::Started { pid } => ExecEvent::Started { pid },
            ExecEventWire::Stdout { data } => {
                ExecEvent::Output(OutputChunk { stream: Stream::Stdout, data })
            }
            ExecEventWire::Stderr { data } => {
                ExecEvent::Output(OutputChunk { stream: Stream::Stderr, data })
            }
            ExecEventWire::Exit { code } => ExecEvent::Exit { code },
            ExecEventWire::Error { message } => ExecEvent::Error { message },
        }))
    }

    /// Write bytes to the running process's stdin.
    pub fn write_stdin(&mut self, data: &str) -> ExecResultT<()> {
        let ctl = serde_json::json!({ "type": "stdin", "data": data });
        self.write_frame(&serde_json::to_vec(&ctl).unwrap())
    }

    /// Close the process's stdin (signal EOF to the child).
    pub fn close_stdin(&mut self) -> ExecResultT<()> {
        let ctl = serde_json::json!({ "type": "stdin_close" });
        self.write_frame(&serde_json::to_vec(&ctl).unwrap())
    }

    /// Send a POSIX signal number to the process.
    pub fn signal(&mut self, signal: i32) -> ExecResultT<()> {
        let ctl = serde_json::json!({ "type": "signal", "signal": signal });
        self.write_frame(&serde_json::to_vec(&ctl).unwrap())
    }

    /// Resize the PTY (Phase 6).
    pub fn resize(&mut self, cols: u16, rows: u16) -> ExecResultT<()> {
        let ctl = serde_json::json!({ "type": "resize", "cols": cols, "rows": rows });
        self.write_frame(&serde_json::to_vec(&ctl).unwrap())
    }

    /// Kill the process.
    pub fn kill(&mut self) -> ExecResultT<()> {
        let ctl = serde_json::json!({ "type": "kill" });
        self.write_frame(&serde_json::to_vec(&ctl).unwrap())
    }

    fn write_frame(&mut self, body: &[u8]) -> ExecResultT<()> {
        let len = (body.len() as u32).to_le_bytes();
        self.stream.write_all(&len)?;
        self.stream.write_all(body)?;
        self.stream.flush()?;
        Ok(())
    }
}

/// Read a 4-byte little-endian length prefix; `None` on clean EOF.
fn read_len(stream: &mut UnixStream) -> ExecResultT<Option<usize>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(ExecError::Io(e)),
    }
    Ok(Some(u32::from_le_bytes(len_buf) as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixListener;

    /// Spawn a fake agent on a temp socket; returns (path, join-handle).
    fn fake_agent<F>(handler: F) -> (PathBuf, std::thread::JoinHandle<()>)
    where
        F: FnOnce(&mut UnixStream) + Send + 'static,
    {
        // Unix socket paths are length-limited on macOS, so keep this short and
        // unique per test (an atomic counter avoids nanosecond collisions).
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nsb-x{}-{}.sock", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let path_c = path.clone();
        let h = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                handler(&mut s);
            }
        });
        (path_c, h)
    }

    fn write_frame(s: &mut UnixStream, v: &serde_json::Value) {
        let body = serde_json::to_vec(v).unwrap();
        s.write_all(&(body.len() as u32).to_le_bytes()).unwrap();
        s.write_all(&body).unwrap();
        s.flush().unwrap();
    }

    fn read_frame(s: &mut UnixStream) -> serde_json::Value {
        let mut len = [0u8; 4];
        s.read_exact(&mut len).unwrap();
        let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
        s.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    #[test]
    fn exec_unavailable_without_socket() {
        let client = ExecClient::new("/nonexistent/does-not-exist.sock");
        assert!(!client.is_available());
        let err = client.exec("echo", &["hi"]).unwrap_err();
        assert!(matches!(err, ExecError::NotAvailable(_)), "{:?}", err);
    }

    #[test]
    fn exec_buffers_stdout_and_exit() {
        let (path, h) = fake_agent(|s| {
            let _req = read_frame(s);
            write_frame(s, &serde_json::json!({"type":"started","pid":1}));
            write_frame(s, &serde_json::json!({"type":"stdout","data":"hello\n"}));
            write_frame(s, &serde_json::json!({"type":"exit","code":0}));
        });
        let client = ExecClient::new(&path);
        let res = client.exec("echo", &["hello"]).unwrap();
        assert_eq!(res.stdout, "hello\n");
        assert_eq!(res.exit_code, 0);
        assert!(res.success());
        h.join().unwrap();
    }

    #[test]
    fn exec_stream_reports_chunks_and_code() {
        let (path, h) = fake_agent(|s| {
            let _req = read_frame(s);
            write_frame(s, &serde_json::json!({"type":"stdout","data":"a"}));
            write_frame(s, &serde_json::json!({"type":"stderr","data":"e"}));
            write_frame(s, &serde_json::json!({"type":"exit","code":3}));
        });
        let client = ExecClient::new(&path);
        let mut out = String::new();
        let mut err = String::new();
        let code = client
            .exec_stream("x", &[], ExecOptions::default(), |c| match c.stream {
                Stream::Stdout => out.push_str(&c.data),
                Stream::Stderr => err.push_str(&c.data),
            })
            .unwrap();
        assert_eq!(out, "a");
        assert_eq!(err, "e");
        assert_eq!(code, 3);
        h.join().unwrap();
    }

    #[test]
    fn exec_surfaces_agent_error() {
        let (path, h) = fake_agent(|s| {
            let _req = read_frame(s);
            write_frame(s, &serde_json::json!({"type":"error","message":"boom"}));
        });
        let client = ExecClient::new(&path);
        let err = client.exec("x", &[]).unwrap_err();
        assert!(
            matches!(&err, ExecError::Agent(m) if m == "boom"),
            "{:?}",
            err
        );
        h.join().unwrap();
    }

    #[test]
    fn exec_request_serializes_shell_and_cwd() {
        let (path, h) = fake_agent(|s| {
            let req = read_frame(s);
            // echo the decoded request back as stdout for assertion
            let text = serde_json::to_string(&req).unwrap();
            write_frame(s, &serde_json::json!({"type":"stdout","data":text}));
            write_frame(s, &serde_json::json!({"type":"exit","code":0}));
        });
        let client = ExecClient::new(&path);
        let opts = ExecOptions::new()
            .shell(true)
            .workdir("/tmp")
            .env("K", "V")
            .timeout_secs(9);
        let res = client.exec_with("echo hi", &[], opts).unwrap();
        assert!(res.stdout.contains("\"shell\":true"), "{}", res.stdout);
        assert!(res.stdout.contains("\"cwd\":\"/tmp\""), "{}", res.stdout);
        assert!(res.stdout.contains("\"timeout_secs\":9"), "{}", res.stdout);
        assert!(res.stdout.contains("\"K\":\"V\""), "{}", res.stdout);
        h.join().unwrap();
    }

    #[test]
    fn write_stdin_sends_control_frame() {
        let (path, h) = fake_agent(|s| {
            let _req = read_frame(s);
            let ctl = read_frame(s);
            let text = serde_json::to_string(&ctl).unwrap();
            write_frame(s, &serde_json::json!({"type":"stdout","data":text}));
            write_frame(s, &serde_json::json!({"type":"exit","code":0}));
        });
        let client = ExecClient::new(&path);
        let mut handle = client.start("cat", &[], ExecOptions::default()).unwrap();
        handle.write_stdin("data-here").unwrap();
        // Drain until exit, capturing stdout.
        let mut out = String::new();
        loop {
            match handle.next_event(None).unwrap() {
                Some(ExecEvent::Output(c)) => out.push_str(&c.data),
                Some(ExecEvent::Exit { .. }) => break,
                Some(_) => {}
                None => break,
            }
        }
        assert!(out.contains("\"type\":\"stdin\""), "{}", out);
        assert!(out.contains("data-here"), "{}", out);
        h.join().unwrap();
    }
}





