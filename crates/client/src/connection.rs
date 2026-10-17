//! The wire connection: dial, handshake, and request/response exchange.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nanosb_protocol::{
    read_message, write_message, Capability, HelloRequest, HelloResponse, Request, Response,
    PROTOCOL_VERSION,
};

use crate::error::{Error, Result};

/// The default per-request timeout for non-streaming calls.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A single connection to a helper or sandbox control socket.
pub struct Connection {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    capabilities: HelloResponse,
    scratch: String,
}

impl Connection {
    /// Dial `path` and complete the `hello` handshake.
    pub fn dial(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .map_err(|e| Error::connect(format!("{}: {e}", path.display())))?;
        stream.set_read_timeout(Some(DEFAULT_TIMEOUT)).ok();
        stream.set_write_timeout(Some(DEFAULT_TIMEOUT)).ok();
        let reader = BufReader::new(stream.try_clone()?);
        let mut conn = Self {
            stream,
            reader,
            capabilities: HelloResponse {
                protocol: PROTOCOL_VERSION.to_string(),
                helper_version: String::new(),
                capabilities: Vec::new(),
            },
            scratch: String::new(),
        };
        conn.handshake(nanosb_protocol::CLIENT_NAME)?;
        Ok(conn)
    }

    /// Send the opening frame and validate the negotiated protocol version.
    pub fn handshake(&mut self, client: &str) -> Result<&HelloResponse> {
        let hello = HelloRequest::new(client, PROTOCOL_VERSION);
        write_message(&mut self.stream, &Request::Hello(hello))?;
        let resp = self.recv_response()?;
        match resp {
            Response::HelloOk(h) => {
                if !nanosb_protocol::version_in_range(
                    &h.protocol,
                    PROTOCOL_VERSION,
                    PROTOCOL_VERSION,
                ) {
                    return Err(Error::Unsupported(format!(
                        "helper speaks protocol {} but this client requires {}",
                        h.protocol, PROTOCOL_VERSION
                    )));
                }
                self.capabilities = h;
                Ok(&self.capabilities)
            }
            Response::Error(e) => Err(Error::Protocol(e)),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// The negotiated capability set.
    pub fn capabilities(&self) -> &HelloResponse {
        &self.capabilities
    }

    /// Ensure the helper supports `cap`, else return an error.
    pub fn require(&self, cap: Capability) -> Result<()> {
        if self.capabilities.supports(cap) {
            Ok(())
        } else {
            Err(Error::Unsupported(format!(
                "helper does not advertise capability '{}'",
                cap.as_str()
            )))
        }
    }

    fn recv_response(&mut self) -> Result<Response> {
        match read_message::<_, Response>(&mut self.reader, &mut self.scratch)? {
            Some(r) => Ok(r),
            None => Err(Error::Unexpected("connection closed".into())),
        }
    }

    /// Send a request without reading a response.
    pub fn send(&mut self, req: &Request) -> Result<()> {
        write_message(&mut self.stream, req)?;
        Ok(())
    }

    /// Read the next response frame (`None` on clean EOF).
    pub fn next_response(&mut self) -> Result<Option<Response>> {
        Ok(read_message::<_, Response>(
            &mut self.reader,
            &mut self.scratch,
        )?)
    }

    /// Send a request and read exactly one response; a `Response::Error` becomes
    /// `Err`.
    pub fn call(&mut self, req: &Request) -> Result<Response> {
        self.send(req)?;
        match self.recv_response()? {
            Response::Error(e) => Err(Error::Protocol(e)),
            other => Ok(other),
        }
    }

    /// Send a streaming request, read its opening ack, and return `Ok(())` so
    /// the caller can drain frames with [`Connection::next_response`].
    pub fn call_stream(&mut self, req: &Request) -> Result<()> {
        self.send(req)?;
        match self.recv_response()? {
            Response::Ok { .. } => Ok(()),
            Response::Error(e) => Err(Error::Protocol(e)),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Disable read timeouts (for long-lived streams).
    pub fn clear_read_timeout(&mut self) -> Result<()> {
        self.stream.set_read_timeout(None)?;
        Ok(())
    }
}

/// Resolve the default helper daemon socket path (`$HOME/.nanosandbox/helper.sock`).
pub fn default_helper_socket() -> PathBuf {
    nanosb_home().join(nanosb_protocol::HELPER_SOCKET_FILE)
}

/// Resolve a sandbox's control socket path
/// (`$HOME/.nanosandbox/sandboxes/<name>/control.sock`).
pub fn sandbox_control_socket(name: &str) -> PathBuf {
    sandbox_dir(name).join(nanosb_protocol::CONTROL_SOCKET_FILE)
}

/// Resolve a sandbox's state directory.
pub fn sandbox_dir(name: &str) -> PathBuf {
    nanosb_home().join("sandboxes").join(name)
}

/// `$HOME/.nanosandbox` (overridable with `NANOSB_HOME`).
pub fn nanosb_home() -> PathBuf {
    if let Ok(dir) = std::env::var("NANOSB_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".nanosandbox")
}
