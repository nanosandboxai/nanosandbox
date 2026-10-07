//! Client library for the supervisor control socket.
//!
//! Provides connect/request/attach streams for CLI and TUI use.

use crate::supervisor::{
    AttachFrame, ControlRequest, ControlResponse, SupervisorState, SANDBOXES_DIR,
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
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let sandbox_dir = PathBuf::from(&home)
            .join(SANDBOXES_DIR)
            .join(sandbox_name);
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

    /// Check if the supervisor is running (control socket exists).
    pub fn is_running(&self) -> bool {
        self.control_socket_path().exists()
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
