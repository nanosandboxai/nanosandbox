//! Client library for the supervisor control socket.
//!
//! Provides connect/request/attach streams for CLI and TUI use.

use crate::supervisor::{
    sandboxes_root, AttachFrame, ControlRequest, ControlResponse, SupervisorState,
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Error type for supervisor client operations.
#[derive(Debug)]
pub enum ClientError {
    NotRunning(String),
    ConnectionFailed(String),
    RequestFailed(String),
    ProtocolError(String),
    IoError(std::io::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotRunning(msg) => write!(f, "sandbox not running: {}", msg),
            ClientError::ConnectionFailed(msg) => write!(f, "connection failed: {}", msg),
            ClientError::RequestFailed(msg) => write!(f, "request failed: {}", msg),
            ClientError::ProtocolError(msg) => write!(f, "protocol error: {}", msg),
            ClientError::IoError(e) => write!(f, "io error: {}", e),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::IoError(e)
    }
}

/// Result type for supervisor client operations.
pub type ClientResult<T> = Result<T, ClientError>;

/// Supervisor client for communicating with a running supervisor.
pub struct SupervisorClient {
    sandbox_name: String,
    sandbox_dir: PathBuf,
}

impl SupervisorClient {
    /// Create a new client for the given sandbox name.
    pub fn new(sandbox_name: &str) -> Self {
        let sandbox_dir = sandboxes_root().join(sandbox_name);
        Self {
            sandbox_name: sandbox_name.to_string(),
            sandbox_dir,
        }
    }

    /// Get the sandbox directory path.
    pub fn sandbox_dir(&self) -> &Path {
        &self.sandbox_dir
    }

    /// Get the control socket path.
    pub fn control_socket_path(&self) -> PathBuf {
        self.sandbox_dir.join("control.sock")
    }

    /// Get the console log path.
    pub fn console_log_path(&self) -> PathBuf {
        self.sandbox_dir.join("logs").join("console.log")
    }

    /// Check whether the supervisor is actually alive: a connectable control
    /// socket, or a recorded supervisor PID that is still alive.
    pub fn is_running(&self) -> bool {
        if UnixStream::connect(self.control_socket_path()).is_ok() {
            return true;
        }
        self.supervisor_pid_alive()
    }

    fn supervisor_pid_alive(&self) -> bool {
        let Some(pid) = self.read_state().and_then(|state| state.supervisor_pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // kill(pid, 0) sends no signal; EPERM still means the PID exists.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Read the current state from state.json (works even when supervisor is stopped).
    pub fn read_state(&self) -> Option<SupervisorState> {
        let state_path = self.sandbox_dir.join("state.json");
        std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Connect to the control socket.
    fn connect(&self) -> ClientResult<UnixStream> {
        let sock_path = self.control_socket_path();
        if !sock_path.exists() {
            return Err(ClientError::NotRunning(format!(
                "supervisor for '{}' is not running (no control socket at {})",
                self.sandbox_name,
                sock_path.display()
            )));
        }
        let stream = UnixStream::connect(&sock_path)
            .map_err(|e| ClientError::ConnectionFailed(format!("{}: {}", sock_path.display(), e)))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok();
        Ok(stream)
    }

    /// Send a request and read the response.
    fn send_request(&self, request: &ControlRequest) -> ClientResult<ControlResponse> {
        let mut stream = self.connect()?;
        let json = serde_json::to_string(request)
            .map_err(|e| ClientError::ProtocolError(format!("serialize: {}", e)))?;

        stream
            .write_all(json.as_bytes())
            .map_err(|e| ClientError::RequestFailed(format!("write: {}", e)))?;
        stream
            .write_all(b"\n")
            .map_err(|e| ClientError::RequestFailed(format!("write newline: {}", e)))?;

        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| ClientError::RequestFailed(format!("read: {}", e)))?;

        serde_json::from_str(line.trim())
            .map_err(|e| ClientError::ProtocolError(format!("deserialize: {}", e)))
    }

    /// Ping the supervisor.
    pub fn ping(&self) -> ClientResult<String> {
        match self.send_request(&ControlRequest::Ping {})? {
            ControlResponse::Ok { version } => Ok(version),
            ControlResponse::Error { message } => Err(ClientError::RequestFailed(message)),
            _ => Err(ClientError::ProtocolError("unexpected response".to_string())),
        }
    }

    /// Get sandbox status.
    pub fn status(&self) -> ClientResult<ControlResponse> {
        self.send_request(&ControlRequest::Status {})
    }

    /// Stop the sandbox.
    pub fn stop(&self, force: bool) -> ClientResult<()> {
        match self.send_request(&ControlRequest::Stop {
            force: Some(force),
        })? {
            ControlResponse::Ok { .. } => Ok(()),
            ControlResponse::Error { message } => Err(ClientError::RequestFailed(message)),
            _ => Err(ClientError::ProtocolError("unexpected response".to_string())),
        }
    }

    /// Attach to the console (returns scrollback replay).
    /// In a full implementation, this would return a stream.
    pub fn attach(&self) -> ClientResult<Vec<AttachFrame>> {
        let mut stream = self.connect()?;
        let json = serde_json::to_string(&ControlRequest::Attach {})
            .map_err(|e| ClientError::ProtocolError(format!("serialize: {}", e)))?;

        stream
            .write_all(json.as_bytes())
            .map_err(|e| ClientError::RequestFailed(format!("write: {}", e)))?;
        stream
            .write_all(b"\n")
            .map_err(|e| ClientError::RequestFailed(format!("write newline: {}", e)))?;

        let mut reader = BufReader::new(&stream);
        let mut frames = Vec::new();
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(frame) = serde_json::from_str::<AttachFrame>(trimmed) {
                        let is_exit = matches!(frame, AttachFrame::Exit { .. });
                        frames.push(frame);
                        if is_exit {
                            break;
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(ClientError::RequestFailed(format!("read: {}", e))),
            }
        }

        Ok(frames)
    }

    /// Read console log file directly (works even when supervisor is stopped).
    pub fn read_log_file(&self) -> ClientResult<String> {
        let log_path = self.console_log_path();
        std::fs::read_to_string(&log_path)
            .map_err(|e| ClientError::IoError(e))
    }

    /// Read the last N bytes from the console log file.
    pub fn read_log_tail(&self, n: usize) -> ClientResult<String> {
        let log_path = self.console_log_path();
        let content = std::fs::read(&log_path).map_err(ClientError::IoError)?;
        let len = content.len();
        let start = if len > n { len - n } else { 0 };
        Ok(String::from_utf8_lossy(&content[start..]).to_string())
    }
}

/// A live console attach connection: replay + streamed output + input forwarding.
pub struct AttachConnection {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl AttachConnection {
    pub fn open(client: &SupervisorClient) -> ClientResult<Self> {
        let stream = client.connect()?;
        stream.set_read_timeout(None).ok();
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
        let mut writer = stream.try_clone()?;
        let json = serde_json::to_string(&ControlRequest::Attach {})
            .map_err(|e| ClientError::ProtocolError(format!("serialize: {}", e)))?;
        writer
            .write_all(json.as_bytes())
            .map_err(|e| ClientError::RequestFailed(format!("write: {}", e)))?;
        writer
            .write_all(b"\n")
            .map_err(|e| ClientError::RequestFailed(format!("write newline: {}", e)))?;
        Ok(Self {
            writer,
            reader: BufReader::new(stream),
        })
    }

    pub fn writer_clone(&self) -> ClientResult<UnixStream> {
        self.writer
            .try_clone()
            .map_err(|e| ClientError::ConnectionFailed(format!("clone: {}", e)))
    }

    pub fn next_frame(&mut self) -> ClientResult<Option<AttachFrame>> {
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Ok(None),
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(frame) = serde_json::from_str::<AttachFrame>(trimmed) {
                        return Ok(Some(frame));
                    }
                }
                Err(e) => return Err(ClientError::RequestFailed(format!("read: {}", e))),
            }
        }
    }

    pub fn send_input(&mut self, data: &str) -> ClientResult<()> {
        let frame = serde_json::json!({ "type": "input", "data": data }).to_string();
        self.writer
            .write_all(frame.as_bytes())
            .map_err(|e| ClientError::RequestFailed(format!("write: {}", e)))?;
        self.writer
            .write_all(b"\n")
            .map_err(|e| ClientError::RequestFailed(format!("write newline: {}", e)))?;
        Ok(())
    }
}

#[cfg(test)]
impl SupervisorClient {
    fn with_sandbox_dir(sandbox_name: &str, sandbox_dir: PathBuf) -> Self {
        Self {
            sandbox_name: sandbox_name.to_string(),
            sandbox_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn reaped_pid() -> i32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn child");
        let pid = child.id() as i32;
        let _ = child.wait();
        pid
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nanosb-host-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn client_with_state(name: &str, supervisor_pid: Option<i32>) -> (SupervisorClient, PathBuf) {
        let dir = temp_dir(name);
        let state = serde_json::json!({
            "sandbox_name": name,
            "state": "stopped",
            "pid": null,
            "exit_code": null,
            "started_at": "0",
            "config_hash": "",
            "supervisor_pid": supervisor_pid,
        });
        std::fs::write(dir.join("state.json"), state.to_string()).unwrap();
        (SupervisorClient::with_sandbox_dir(name, dir.clone()), dir)
    }

    #[test]
    fn is_running_uses_supervisor_liveness_not_socket_existence() {
        let (dead, dead_dir) = client_with_state("dead", Some(reaped_pid()));
        std::fs::write(dead_dir.join("control.sock"), b"").unwrap();
        assert!(!dead.is_running());

        let (live, live_dir) = client_with_state("live", Some(std::process::id() as i32));
        assert!(live.is_running());

        let bound_dir = temp_dir("bound");
        let _listener = UnixListener::bind(bound_dir.join("control.sock")).unwrap();
        let bound = SupervisorClient::with_sandbox_dir("bound", bound_dir.clone());
        assert!(bound.is_running());

        let _ = std::fs::remove_dir_all(&dead_dir);
        let _ = std::fs::remove_dir_all(&live_dir);
        let _ = std::fs::remove_dir_all(&bound_dir);
    }
}
