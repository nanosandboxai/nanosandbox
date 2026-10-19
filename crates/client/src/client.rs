//! `SandboxClient` and the `Sandbox` handle — the reference protocol client.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nanosb_protocol::{
    Capability, ExecDefaultRequest, ExecRequest, ExecResult, FsEntry, FsMkdirSpec, FsPathSpec,
    FsStat, FsTransferSpec, FsWriteSpec, FsWriteStreamSpec, GuestEvent, LogLine, LogSource,
    LogsSpec, Metrics, ModifySpec, MountSpec,
    Request::{self, *},
    Response, SandboxInfo, SandboxList, SandboxSpec, SandboxState, SecretEntry, SnapshotInfo,
    StreamEvent, VolumeInfo, VolumeSpec,
};

use crate::builder::SandboxBuilder;
use crate::connection::{default_helper_socket, nanosb_home, sandbox_control_socket, Connection};
use crate::error::{Error, Result};

/// The backend a client talks to.
#[derive(Debug, Clone)]
pub enum Backend {
    /// A local Unix-socket helper or sandbox control socket.
    Local {
        socket: PathBuf,
        /// When set, requests omit `name` (the socket is bound to one sandbox).
        sandbox: Option<String>,
    },
    /// A future remote HTTPS backend (D6).
    Cloud { endpoint: String },
}

/// A protocol client. Cloneable; each call dials the socket and re-negotiates.
#[derive(Debug, Clone)]
pub struct SandboxClient {
    backend: Backend,
    caps: Vec<Capability>,
}

impl SandboxClient {
    /// Connect to the default global helper daemon (`$HOME/.nanosandbox/helper.sock`).
    pub fn local() -> Result<Self> {
        Self::connect(default_helper_socket())
    }

    /// Connect to an arbitrary socket path.
    pub fn connect(socket: impl Into<PathBuf>) -> Result<Self> {
        let socket = socket.into();
        let conn = Connection::dial(&socket)?;
        let caps = conn.capabilities().capabilities.clone();
        Ok(Self {
            backend: Backend::Local {
                socket,
                sandbox: None,
            },
            caps,
        })
    }

    /// Connect to a specific sandbox's control socket.
    pub fn for_sandbox(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let socket = sandbox_control_socket(&name);
        let conn = Connection::dial(&socket)?;
        let caps = conn.capabilities().capabilities.clone();
        Ok(Self {
            backend: Backend::Local {
                socket,
                sandbox: Some(name),
            },
            caps,
        })
    }

    /// A stub Cloud client (D6). All verbs return `Unsupported` until WS-cloud.
    pub fn cloud(endpoint: impl Into<String>) -> Self {
        Self {
            backend: Backend::Cloud {
                endpoint: endpoint.into(),
            },
            caps: Vec::new(),
        }
    }

    /// Capabilities advertised by the connected helper.
    pub fn capabilities(&self) -> &[Capability] {
        &self.caps
    }

    /// Whether the connected helper supports `cap`.
    pub fn supports(&self, cap: Capability) -> bool {
        self.caps.contains(&cap)
    }

    /// Ensure the connected helper supports `cap`, else return an error.
    pub fn require(&self, cap: Capability) -> Result<()> {
        if self.supports(cap) {
            Ok(())
        } else {
            Err(Error::Unsupported(format!(
                "helper does not advertise capability '{}'",
                cap.as_str()
            )))
        }
    }

    fn socket(&self) -> Result<(&PathBuf, &Option<String>)> {
        match &self.backend {
            Backend::Local { socket, sandbox } => Ok((socket, sandbox)),
            Backend::Cloud { .. } => Err(Error::Unsupported(
                "cloud backend is not implemented yet".into(),
            )),
        }
    }

    /// Open a fresh negotiated connection.
    fn open(&self) -> Result<Connection> {
        let (socket, _) = self.socket()?;
        Connection::dial(socket)
    }

    /// Resolve a `name` for a request: use the explicit name, else the socket's
    /// bound sandbox, else error.
    fn resolve_name(&self, name: Option<&str>) -> Result<Option<String>> {
        if let Some(n) = name {
            return Ok(Some(n.to_string()));
        }
        let (_, bound) = self.socket()?;
        if let Some(b) = bound {
            return Ok(Some(b.clone()));
        }
        Ok(None)
    }

    fn name_arg(&self, name: Option<&str>) -> Result<Option<String>> {
        Ok(self
            .resolve_name(name)?
            .filter(|_| matches!(&self.backend, Backend::Local { sandbox: None, .. })))
    }

    fn call(&self, req: &Request) -> Result<Response> {
        self.open()?.call(req)
    }

    // ── lifecycle ──────────────────────────────────────────────────────

    /// Ping the helper; returns its version string.
    pub fn ping(&self) -> Result<String> {
        match self.call(&Ping)? {
            Response::Pong { version } | Response::Ok { version } => Ok(version),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Create a sandbox and return a handle bound to it.
    pub fn create(&self, spec: SandboxSpec) -> Result<Sandbox> {
        let name = spec
            .name
            .clone()
            .ok_or_else(|| Error::unexpected("spec.name is required to create"))?;
        let req = Create(Box::new(nanosb_protocol::CreateRequest { spec }));
        match self.call(&req)? {
            Response::Ok { .. } | Response::Sandbox(_) => Ok(Sandbox::bound(self.clone(), name)),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Create from a [`SandboxBuilder`].
    pub fn create_with(&self, builder: SandboxBuilder) -> Result<Sandbox> {
        self.create(builder.build())
    }

    /// Get a handle for an existing sandbox by name.
    pub fn sandbox(&self, name: impl Into<String>) -> Sandbox {
        Sandbox::bound(self.clone(), name.into())
    }

    /// List sandboxes (running only unless `all`).
    pub fn list(&self, all: bool) -> Result<Vec<SandboxInfo>> {
        match self.call(&List {
            all,
            limit: None,
            cursor: None,
            label: None,
        })? {
            Response::List(l) => Ok(l.sandboxes),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// List sandboxes filtered by a label selector.
    pub fn list_with_label(&self, label: &str, all: bool) -> Result<Vec<SandboxInfo>> {
        match self.call(&List {
            all,
            limit: None,
            cursor: None,
            label: Some(label.to_string()),
        })? {
            Response::List(l) => Ok(l.sandboxes),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// List one page of sandboxes with `limit`/`cursor`/`label`; returns the
    /// page plus the `next_cursor` for the following page.
    pub fn list_page(
        &self,
        limit: Option<usize>,
        cursor: Option<String>,
        label: Option<&str>,
    ) -> Result<SandboxList> {
        match self.call(&List {
            all: true,
            limit,
            cursor,
            label: label.map(String::from),
        })? {
            Response::List(l) => Ok(l),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Get a sandbox's info.
    pub fn get(&self, name: &str) -> Result<SandboxInfo> {
        match self.call(&Get {
            name: name.to_string(),
        })? {
            Response::Sandbox(info) => Ok(info),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    // ── volumes (sandbox-independent surface mirrored per language) ─────

    /// List snapshots known to the helper.
    pub fn snapshots(&self) -> Result<Vec<SnapshotInfo>> {
        match self.call(&SnapshotList)? {
            Response::Snapshots { snapshots } => Ok(snapshots),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }
}

fn is_stream_end(ev: &GuestEvent) -> bool {
    matches!(ev, GuestEvent::Exit { .. } | GuestEvent::Error { .. })
}

/// A handle bound to a specific sandbox.
#[derive(Debug, Clone)]
pub struct Sandbox {
    client: SandboxClient,
    name: String,
}

impl Sandbox {
    fn bound(client: SandboxClient, name: String) -> Self {
        Self { client, name }
    }

    /// The sandbox name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The underlying client.
    pub fn client(&self) -> &SandboxClient {
        &self.client
    }

    fn name_arg(&self) -> Result<Option<String>> {
        self.client.name_arg(Some(&self.name))
    }

    // ── lifecycle ──────────────────────────────────────────────────────

    /// Current lifecycle state.
    pub fn state(&self) -> Result<SandboxState> {
        match self.client.call(&Status {
            name: self.name_arg()?,
        })? {
            Response::Status { state, .. } => Ok(state),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Full info (`get`).
    pub fn info(&self) -> Result<SandboxInfo> {
        self.client.get(&self.name)
    }

    pub fn start(&self) -> Result<()> {
        self.ack(Start {
            name: self.name_arg()?,
        })
    }

    pub fn stop(&self, force: bool) -> Result<()> {
        self.ack(Stop {
            name: self.name_arg()?,
            force,
        })
    }

    pub fn kill(&self) -> Result<()> {
        self.ack(Kill {
            name: self.name_arg()?,
            signal: None,
        })
    }

    /// Kill the sandbox with an explicit POSIX signal. `SIGTERM` requests a
    /// graceful stop; `SIGKILL` force-stops. Requires the `signal` capability.
    pub fn kill_with(&self, signal: i32) -> Result<()> {
        self.client.require(Capability::Signal)?;
        self.ack(Kill {
            name: self.name_arg()?,
            signal: Some(signal),
        })
    }

    /// Block until the sandbox reaches a terminal state (`stopped`/`error`),
    /// returning `(exit_code, exited_at_ms, state)`. `timeout` bounds the wait;
    /// `None` waits indefinitely. Requires the `wait` capability.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<(i32, u64, SandboxState)> {
        self.client.require(Capability::Wait)?;
        let timeout_secs = timeout.map(|d| d.as_secs().max(1));
        match self.client.call(&Wait {
            name: self.name_arg()?,
            timeout_secs,
        })? {
            Response::Wait {
                exit_code,
                exited_at_ms,
                state,
            } => Ok((exit_code, exited_at_ms, state)),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    pub fn remove(&self, force: bool) -> Result<()> {
        self.ack(Remove {
            name: self.name_arg()?,
            force,
        })
    }

    /// Request a graceful drain.
    pub fn drain(&self) -> Result<()> {
        self.ack(Drain {
            name: self.name_arg()?,
        })
    }

    /// Keepalive touch.
    pub fn touch(&self) -> Result<()> {
        self.ack(Touch {
            name: self.name_arg()?,
        })
    }

    pub fn pause(&self) -> Result<()> {
        self.ack(Pause {
            name: self.name_arg()?,
        })
    }

    pub fn resume(&self) -> Result<()> {
        self.ack(Resume {
            name: self.name_arg()?,
        })
    }

    /// Modify mutable sandbox settings.
    pub fn modify(&self, spec: ModifySpec) -> Result<()> {
        self.ack(Modify {
            name: self.name_arg()?,
            spec,
        })
    }

    /// Poll `state()` until the sandbox is `Stopped`, or return `Timeout` once
    /// `timeout` elapses. An `Error` state is surfaced immediately.
    pub fn wait_until_stopped(&self, timeout: Duration) -> Result<SandboxState> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.state() {
                Ok(SandboxState::Stopped) => return Ok(SandboxState::Stopped),
                Ok(SandboxState::Error) => {
                    return Err(Error::unexpected(
                        "sandbox entered error state while waiting",
                    ))
                }
                Ok(_) => {}
                // A stopped sandbox's control socket is gone; any transport
                // failure (direct or proxied by the helper) means it stopped.
                Err(_) => return Ok(SandboxState::Stopped),
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "sandbox '{}' did not stop within {timeout:?}",
                    self.name
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn ack(&self, req: Request) -> Result<()> {
        match self.client.call(&req)? {
            Response::Ok { .. } => Ok(()),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    // ── execution ──────────────────────────────────────────────────────

    /// Run a command and buffer its output.
    pub fn exec(&self, command: &str, args: &[&str]) -> Result<ExecResult> {
        let mut req = ExecRequest::new(command);
        req.name = self.name_arg()?;
        req.args = args.iter().map(|s| s.to_string()).collect();
        self.exec_with(req)
    }

    /// Run a command with full options.
    pub fn exec_with(&self, req: ExecRequest) -> Result<ExecResult> {
        match self.client.call(&Exec(Box::new(req)))? {
            Response::ExecResult(r) => Ok(r),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Run a script through the guest shell.
    pub fn shell(&self, script: &str) -> Result<ExecResult> {
        let req = nanosb_protocol::ShellRequest {
            name: self.name_arg()?,
            script: script.to_string(),
            cwd: None,
            env: HashMap::new(),
            user: None,
            timeout_secs: None,
        };
        match self.client.call(&Shell(Box::new(req)))? {
            Response::ExecResult(r) => Ok(r),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Run a script through the guest shell, streaming events via `exec_stream`
    /// with the `shell` flag set.
    pub fn shell_stream<F>(&self, script: &str, on_event: F) -> Result<i32>
    where
        F: FnMut(GuestEvent),
    {
        let mut req = ExecRequest::new(script);
        req.name = self.name_arg()?;
        req.shell = true;
        self.exec_stream(req, on_event)
    }

    /// Run a command, invoking `on_event` for each streamed guest event and
    /// returning the exit code.
    pub fn exec_stream<F>(&self, req: ExecRequest, mut on_event: F) -> Result<i32>
    where
        F: FnMut(GuestEvent),
    {
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&ExecStream(Box::new(req)))?;
        let mut code = -1;
        while let Some(resp) = conn.next_response()? {
            match resp {
                Response::Guest(ev) => {
                    let end = is_stream_end(&ev);
                    if let GuestEvent::Exit { code: c } = ev {
                        code = c;
                    }
                    if let GuestEvent::Error { ref message } = ev {
                        let msg = message.clone();
                        on_event(ev);
                        return Err(Error::unexpected(msg));
                    }
                    on_event(ev);
                    if end {
                        break;
                    }
                }
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(code)
    }

    /// Attach to the sandbox console, invoking `on_event` for each frame.
    pub fn attach<F>(&self, mut on_event: F) -> Result<()>
    where
        F: FnMut(GuestEvent),
    {
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&Attach {
            name: self.name_arg()?,
        })?;
        while let Some(resp) = conn.next_response()? {
            match resp {
                Response::Guest(ev) => {
                    let end = is_stream_end(&ev);
                    on_event(ev);
                    if end {
                        break;
                    }
                }
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Execute the sandbox's configured default workload, with optional
    /// overrides (extra args, env, cwd, rlimits).
    pub fn exec_default(&self, req: ExecDefaultRequest) -> Result<ExecResult> {
        let mut req = req;
        req.name = self.name_arg()?;
        match self.client.call(&Request::ExecDefault(Box::new(req)))? {
            Response::ExecResult(r) => Ok(r),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Attach to the sandbox's default interactive workload.
    pub fn attach_default<F>(&self, mut on_event: F) -> Result<()>
    where
        F: FnMut(GuestEvent),
    {
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&Request::AttachDefault {
            name: self.name_arg()?,
        })?;
        while let Some(resp) = conn.next_response()? {
            match resp {
                Response::Guest(ev) => {
                    let end = is_stream_end(&ev);
                    on_event(ev);
                    if end {
                        break;
                    }
                }
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Open a live attach session, returning a handle that can drive input and
    /// resize on the same connection while reading console events.
    pub fn attach_session(&self) -> Result<AttachSession> {
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&Attach {
            name: self.name_arg()?,
        })?;
        Ok(AttachSession {
            conn,
            pending: std::collections::VecDeque::new(),
        })
    }

    /// Open a tty `exec_stream` session: the command runs on a PTY and the
    /// returned handle can send live stdin and resize frames on the same
    /// connection while reading guest events. Requires the `exec_pty`
    /// capability. The request's `tty` flag is forced on; set `cols`/`rows`
    /// for the initial PTY size.
    pub fn exec_pty(&self, mut req: ExecRequest) -> Result<ExecPtySession> {
        self.client.require(Capability::ExecPty)?;
        req.name = self.name_arg()?;
        req.tty = true;
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&ExecStream(Box::new(req)))?;
        Ok(ExecPtySession {
            conn,
            pending: std::collections::VecDeque::new(),
        })
    }

    /// Write raw bytes to the workload's console stdin (PID 1 stdin).
    pub fn write_stdin(&self, data: &str) -> Result<()> {
        self.ack(Request::ConsoleInput {
            data: data.to_string(),
        })
    }

    /// Close the workload's console stdin, signalling EOF (so `cat` exits).
    pub fn close_stdin(&self) -> Result<()> {
        self.ack(Request::ConsoleInputClose)
    }

    // ── observability ──────────────────────────────────────────────────

    /// Fetch the current metrics snapshot.
    pub fn metrics(&self) -> Result<Metrics> {
        match self.client.call(&Request::Metrics {
            name: self.name_arg()?,
        })? {
            Response::Metrics(m) => Ok(m),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Stream metrics snapshots, invoking `on_event` until the stream ends.
    pub fn metrics_stream<F>(&self, interval_ms: Option<u64>, mut on_event: F) -> Result<()>
    where
        F: FnMut(Metrics),
    {
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&MetricsStream {
            name: self.name_arg()?,
            interval_ms,
        })?;
        while let Some(resp) = conn.next_response()? {
            match resp {
                Response::Event(StreamEvent::Metrics(m)) => on_event(m),
                Response::Event(StreamEvent::Done) => break,
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Read buffered log lines (respecting `tail`/`cursor`).
    pub fn logs(&self, tail: Option<usize>) -> Result<Vec<LogLine>> {
        let req = LogsSpec {
            name: self.name_arg()?,
            follow: false,
            tail,
            cursor: None,
            source: None,
        };
        match self.client.call(&Logs(req))? {
            Response::Logs { lines, .. } => Ok(lines),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Follow logs, invoking `on_line` until the stream ends.
    pub fn logs_follow<F>(&self, tail: Option<usize>, mut on_line: F) -> Result<()>
    where
        F: FnMut(LogLine),
    {
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&Logs(LogsSpec {
            name: self.name_arg()?,
            follow: true,
            tail,
            cursor: None,
            source: None,
        }))?;
        while let Some(resp) = conn.next_response()? {
            match resp {
                Response::Event(StreamEvent::Log(line)) => on_line(line),
                Response::Event(StreamEvent::Done) => break,
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Read buffered log lines with `tail`/`cursor`/`source` filters.
    pub fn logs_with(
        &self,
        tail: Option<usize>,
        cursor: Option<u64>,
        source: Option<LogSource>,
    ) -> Result<Vec<LogLine>> {
        let req = LogsSpec {
            name: self.name_arg()?,
            follow: false,
            tail,
            cursor,
            source,
        };
        match self.client.call(&Logs(req))? {
            Response::Logs { lines, .. } => Ok(lines),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Follow logs with `tail`/`cursor`/`source` filters, invoking `on_line`
    /// until the stream ends.
    pub fn logs_follow_with<F>(
        &self,
        tail: Option<usize>,
        cursor: Option<u64>,
        source: Option<LogSource>,
        mut on_line: F,
    ) -> Result<()>
    where
        F: FnMut(LogLine),
    {
        let mut conn = self.client.open()?;
        conn.clear_read_timeout()?;
        conn.call_stream(&Logs(LogsSpec {
            name: self.name_arg()?,
            follow: true,
            tail,
            cursor,
            source,
        }))?;
        while let Some(resp) = conn.next_response()? {
            match resp {
                Response::Event(StreamEvent::Log(line)) => on_line(line),
                Response::Event(StreamEvent::Done) => break,
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(())
    }

    // ── filesystem ─────────────────────────────────────────────────────

    fn fs_path(&self, path: &str) -> Result<FsPathSpec> {
        Ok(FsPathSpec {
            name: self.name_arg()?,
            path: path.to_string(),
            offset: None,
            length: None,
        })
    }

    /// Read a file's bytes.
    pub fn fs_read(&self, path: &str) -> Result<Vec<u8>> {
        let mut spec = self.fs_path(path)?;
        spec.offset = None;
        spec.length = None;
        match self.client.call(&FsRead(spec))? {
            Response::FsData { data_base64, .. } => nanosb_protocol::base64_decode(&data_base64)
                .ok_or_else(|| Error::unexpected("invalid base64 from helper")),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Read a file's bytes between `offset` and `offset+length`.
    pub fn fs_read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>> {
        let mut spec = self.fs_path(path)?;
        spec.offset = Some(offset);
        spec.length = Some(length);
        match self.client.call(&FsRead(spec))? {
            Response::FsData { data_base64, .. } => nanosb_protocol::base64_decode(&data_base64)
                .ok_or_else(|| Error::unexpected("invalid base64 from helper")),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Read a file in `chunk_size`-byte chunks via `FsRead` offset/length,
    /// invoking `on_chunk` for each chunk until the file is exhausted.
    pub fn fs_read_stream<F>(&self, path: &str, chunk_size: usize, mut on_chunk: F) -> Result<()>
    where
        F: FnMut(&[u8]),
    {
        let chunk_size = chunk_size.max(1) as u64;
        let mut offset = 0u64;
        loop {
            let chunk = self.fs_read_range(path, offset, chunk_size)?;
            if chunk.is_empty() {
                break;
            }
            on_chunk(&chunk);
            if (chunk.len() as u64) < chunk_size {
                break;
            }
            offset += chunk.len() as u64;
        }
        Ok(())
    }

    /// Read a file as UTF-8 text.
    pub fn fs_read_to_string(&self, path: &str) -> Result<String> {
        match self.client.call(&FsReadToString(self.fs_path(path)?))? {
            Response::FsContent { content } => Ok(content),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Write bytes to a file (creating it).
    pub fn fs_write(&self, path: &str, data: &[u8]) -> Result<()> {
        self.fs_write_opts(path, data, false, None)
    }

    fn fs_write_opts(
        &self,
        path: &str,
        data: &[u8],
        append: bool,
        mode: Option<u32>,
    ) -> Result<()> {
        let spec = FsWriteSpec {
            name: self.name_arg()?,
            path: path.to_string(),
            data_base64: nanosb_protocol::base64_encode(data),
            append,
            mode,
        };
        self.ack(FsWrite(spec))
    }

    /// Write a stream, chunking the payload.
    pub fn fs_write_stream(&self, path: &str, data: &[u8], chunk_size: usize) -> Result<()> {
        let mut conn = self.client.open()?;
        conn.call_stream(&FsWriteStream(FsWriteStreamSpec {
            name: self.name_arg()?,
            path: path.to_string(),
            append: false,
            mode: None,
        }))?;
        let chunk_size = chunk_size.max(1);
        let chunks: Vec<&[u8]> = data.chunks(chunk_size).collect();
        if chunks.is_empty() {
            conn.call(&FsData {
                data_base64: String::new(),
                eof: true,
            })?;
            return Ok(());
        }
        for (i, chunk) in chunks.iter().enumerate() {
            let last = i + 1 == chunks.len();
            match conn.call(&FsData {
                data_base64: nanosb_protocol::base64_encode(chunk),
                eof: last,
            })? {
                Response::Ok { .. } => {}
                other => return Err(Error::unexpected(format!("{other:?}"))),
            }
        }
        Ok(())
    }

    /// List a directory.
    pub fn fs_list(&self, path: &str) -> Result<Vec<FsEntry>> {
        match self.client.call(&FsList(self.fs_path(path)?))? {
            Response::FsEntries { entries } => Ok(entries),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Stat a path.
    pub fn fs_stat(&self, path: &str) -> Result<FsStat> {
        match self.client.call(&FsStat(self.fs_path(path)?))? {
            Response::FsStat(s) => Ok(s),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Whether a path exists.
    pub fn fs_exists(&self, path: &str) -> Result<bool> {
        match self.client.call(&FsExists(self.fs_path(path)?))? {
            Response::FsExists { exists } => Ok(exists),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// Create a directory.
    pub fn fs_mkdir(&self, path: &str, recursive: bool) -> Result<()> {
        self.ack(FsMkdir(FsMkdirSpec {
            name: self.name_arg()?,
            path: path.to_string(),
            recursive,
            mode: None,
        }))
    }

    /// Remove a file.
    pub fn fs_remove(&self, path: &str) -> Result<()> {
        self.ack(FsRemove(self.fs_path(path)?))
    }

    /// Remove a directory.
    pub fn fs_remove_dir(&self, path: &str, recursive: bool) -> Result<()> {
        self.ack(FsRemoveDir(nanosb_protocol::FsRemoveDirSpec {
            name: self.name_arg()?,
            path: path.to_string(),
            recursive,
        }))
    }

    /// Copy a path inside the guest.
    pub fn fs_copy(&self, from: &str, to: &str) -> Result<()> {
        self.ack(FsCopy(nanosb_protocol::FsCopySpec {
            name: self.name_arg()?,
            from: from.to_string(),
            to: to.to_string(),
        }))
    }

    /// Rename a path inside the guest.
    pub fn fs_rename(&self, from: &str, to: &str) -> Result<()> {
        self.ack(FsRename(nanosb_protocol::FsCopySpec {
            name: self.name_arg()?,
            from: from.to_string(),
            to: to.to_string(),
        }))
    }

    /// Copy a host file/dir into the sandbox.
    pub fn fs_copy_from_host(&self, host_path: &str, guest_path: &str) -> Result<()> {
        self.ack(FsCopyFromHost(FsTransferSpec {
            name: self.name_arg()?,
            host_path: host_path.to_string(),
            guest_path: guest_path.to_string(),
        }))
    }

    /// Copy a guest file/dir out to the host.
    pub fn fs_copy_to_host(&self, guest_path: &str, host_path: &str) -> Result<()> {
        self.ack(FsCopyToHost(FsTransferSpec {
            name: self.name_arg()?,
            host_path: host_path.to_string(),
            guest_path: guest_path.to_string(),
        }))
    }

    // ── volumes ────────────────────────────────────────────────────────

    pub fn volume_create(&self, volume: VolumeSpec) -> Result<VolumeInfo> {
        match self
            .client
            .call(&VolumeCreate(nanosb_protocol::VolumeCreateSpec {
                name: self.name_arg()?,
                volume,
            }))? {
            Response::Volume(v) => Ok(v),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    pub fn volume_list(&self) -> Result<Vec<VolumeInfo>> {
        match self.client.call(&VolumeList {
            name: self.name_arg()?,
        })? {
            Response::Volumes { volumes } => Ok(volumes),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    pub fn volume_get(&self, volume: &str) -> Result<VolumeInfo> {
        match self.client.call(&VolumeGet(nanosb_protocol::VolumeRef {
            name: self.name_arg()?,
            volume: volume.to_string(),
        }))? {
            Response::Volume(v) => Ok(v),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    /// The host directory backing a volume: `VolumeInfo.dir` when the helper
    /// reports one, else the resolved `$NANOSB_HOME/volumes/<name>` path.
    pub fn volume_fs(&self, name: &str) -> Result<PathBuf> {
        let info = self.volume_get(name)?;
        if let Some(dir) = info.dir {
            return Ok(PathBuf::from(dir));
        }
        Ok(nanosb_home().join("volumes").join(name))
    }

    pub fn volume_remove(&self, volume: &str) -> Result<()> {
        self.ack(VolumeRemove(nanosb_protocol::VolumeRef {
            name: self.name_arg()?,
            volume: volume.to_string(),
        }))
    }

    /// Mount an existing volume/dir into the running sandbox.
    pub fn mount_add(&self, mount: MountSpec) -> Result<()> {
        self.ack(MountAdd(nanosb_protocol::MountAddSpec {
            name: self.name_arg()?,
            mount,
        }))
    }

    // ── secrets ────────────────────────────────────────────────────────

    pub fn secret_set(&self, key: &str, value: &str) -> Result<()> {
        self.ack(SecretSet(nanosb_protocol::SecretSetSpec {
            name: self.name_arg()?,
            key: key.to_string(),
            value: value.to_string(),
        }))
    }

    pub fn secret_list(&self) -> Result<Vec<SecretEntry>> {
        match self.client.call(&SecretList {
            name: self.name_arg()?,
        })? {
            Response::Secrets { secrets } => Ok(secrets),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    pub fn secret_remove(&self, key: &str) -> Result<()> {
        self.ack(SecretRemove(nanosb_protocol::SecretRef {
            name: self.name_arg()?,
            key: key.to_string(),
        }))
    }

    // ── snapshots ──────────────────────────────────────────────────────

    pub fn snapshot_create(&self, dest: Option<&str>) -> Result<SnapshotInfo> {
        match self
            .client
            .call(&SnapshotCreate(nanosb_protocol::SnapshotCreateSpec {
                name: self.name_arg()?,
                dest: dest.map(String::from),
                include_memory: false,
            }))? {
            Response::Snapshot(s) => Ok(s),
            other => Err(Error::unexpected(format!("{other:?}"))),
        }
    }

    pub fn snapshot_restore(&self, snapshot: &str, name: &str) -> Result<()> {
        self.ack(SnapshotRestore(nanosb_protocol::SnapshotRestoreSpec {
            snapshot: snapshot.to_string(),
            name: name.to_string(),
        }))
    }

    pub fn snapshot_fork(&self, snapshot: &str, name: &str) -> Result<()> {
        self.ack(SnapshotFork(nanosb_protocol::SnapshotRestoreSpec {
            snapshot: snapshot.to_string(),
            name: name.to_string(),
        }))
    }

    pub fn snapshot_remove(&self, snapshot: &str) -> Result<()> {
        self.ack(SnapshotRemove {
            snapshot: snapshot.to_string(),
        })
    }
}

/// A live attach session: drive console input/resize and read guest events on
/// the same connection. `read_events` returns `None` once the stream ends.
pub struct AttachSession {
    conn: Connection,
    pending: std::collections::VecDeque<GuestEvent>,
}

impl AttachSession {
    /// Send raw bytes to the guest console.
    pub fn send_input(&mut self, bytes: &[u8]) -> Result<()> {
        let data = String::from_utf8_lossy(bytes).into_owned();
        self.control(&AttachInput { data })
    }

    /// Resize the attached PTY.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.control(&AttachResize { cols, rows })
    }

    /// Send a control request and wait for its `ok` ack, buffering any
    /// interleaved console events so they are not lost or mistaken for the ack.
    fn control(&mut self, req: &Request) -> Result<()> {
        self.conn.send(req)?;
        loop {
            match self.conn.next_response()? {
                None => return Err(Error::unexpected("attach stream closed before ack")),
                Some(Response::Ok { .. }) => return Ok(()),
                Some(Response::Error(e)) => return Err(Error::Protocol(e)),
                Some(Response::Guest(ev)) => {
                    if is_stream_end(&ev) {
                        return Err(Error::unexpected("attach stream ended before ack"));
                    }
                    self.pending.push_back(ev);
                }
                Some(_) => {}
            }
        }
    }

    /// Read the next guest event, or `None` when the attach stream has ended.
    pub fn read_events(&mut self) -> Result<Option<GuestEvent>> {
        if let Some(ev) = self.pending.pop_front() {
            return Ok(Some(ev));
        }
        while let Some(resp) = self.conn.next_response()? {
            match resp {
                Response::Guest(ev) => {
                    if is_stream_end(&ev) {
                        return Ok(None);
                    }
                    return Ok(Some(ev));
                }
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(None)
    }
}

/// A live tty exec session: drive stdin/resize and read guest events on the
/// same connection. `read_events` returns `None` once the stream ends.
pub struct ExecPtySession {
    conn: Connection,
    pending: std::collections::VecDeque<GuestEvent>,
}

impl ExecPtySession {
    /// Send raw bytes to the guest process's stdin.
    pub fn send_input(&mut self, bytes: &[u8]) -> Result<()> {
        let data = String::from_utf8_lossy(bytes).into_owned();
        self.control(&ExecInput { data })
    }

    /// Resize the exec PTY.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.control(&ExecResize { cols, rows })
    }

    /// Send a control request and wait for its `ok` ack, buffering any
    /// interleaved guest events so they are not lost or mistaken for the ack.
    fn control(&mut self, req: &Request) -> Result<()> {
        self.conn.send(req)?;
        loop {
            match self.conn.next_response()? {
                None => return Err(Error::unexpected("exec pty stream closed before ack")),
                Some(Response::Ok { .. }) => return Ok(()),
                Some(Response::Error(e)) => return Err(Error::Protocol(e)),
                Some(Response::Guest(ev)) => {
                    if is_stream_end(&ev) {
                        return Err(Error::unexpected("exec pty stream ended before ack"));
                    }
                    self.pending.push_back(ev);
                }
                Some(_) => {}
            }
        }
    }

    /// Read the next guest event, or `None` when the exec stream has ended.
    pub fn read_events(&mut self) -> Result<Option<GuestEvent>> {
        if let Some(ev) = self.pending.pop_front() {
            return Ok(Some(ev));
        }
        while let Some(resp) = self.conn.next_response()? {
            match resp {
                Response::Guest(ev) => {
                    if is_stream_end(&ev) {
                        return Ok(None);
                    }
                    return Ok(Some(ev));
                }
                Response::Error(e) => return Err(Error::Protocol(e)),
                _ => {}
            }
        }
        Ok(None)
    }
}

/// The default `$NANOSB_HOME` root, exposed for SDK parity.
pub fn home() -> PathBuf {
    nanosb_home()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::FakeHelper;

    #[test]
    fn client_handshakes_and_pings() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"pong","version":"0.3.0-test"}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        assert!(c.supports(Capability::Lifecycle));
        assert_eq!(c.ping().unwrap(), "0.3.0-test");
    }

    #[test]
    fn client_lists_sandboxes() {
        let h = FakeHelper::start();
        h.push_script(
            r#"{"type":"list","sandboxes":[{"name":"a","state":"running","image":"alpine","detached":false,"ephemeral":false,"labels":{}}]}"#,
        );
        let c = SandboxClient::connect(h.path()).unwrap();
        let list = c.list(false).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "a");
        assert_eq!(list[0].state, SandboxState::Running);
    }

    #[test]
    fn sandbox_exec_round_trips() {
        let h = FakeHelper::start();
        h.push_script(
            r#"{"type":"exec_result","exit_code":0,"stdout":"hi\n","stderr":"","duration_ms":2}"#,
        );
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let r = sb.exec("echo", &["hi"]).unwrap();
        assert_eq!(r.stdout, "hi\n");
        assert_eq!(r.exit_code, 0);
    }

    #[test]
    fn fs_write_then_read() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#);
        h.push_script(r#"{"type":"fs_data","data_base64":"aGVsbG8=","eof":true}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        sb.fs_write("/tmp/x", b"hello").unwrap();
        assert_eq!(sb.fs_read("/tmp/x").unwrap(), b"hello");
    }

    #[test]
    fn exec_stream_collects_events() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#);
        h.push_script(r#"{"type":"guest","event":"stdout","data":"a"}"#);
        h.push_script(r#"{"type":"guest","event":"exit","code":0}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let mut out = String::new();
        let code = sb
            .exec_stream(ExecRequest::new("x"), |ev| {
                if let GuestEvent::Stdout { data } = ev {
                    out.push_str(&data);
                }
            })
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(out, "a");
    }

    #[test]
    fn metrics_stream_collects_snapshots() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#);
        h.push_script(r#"{"type":"event","event":"metrics","cpu_percent":1.5,"memory_used_bytes":10,"memory_total_bytes":100,"disk_used_bytes":0,"disk_total_bytes":0,"net_rx_bytes":0,"net_tx_bytes":0,"timestamp_ms":0}"#);
        h.push_script(r#"{"type":"event","event":"done"}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let mut count = 0;
        sb.metrics_stream(Some(100), |m| {
            if m.cpu_percent > 0.0 {
                count += 1;
            }
        })
        .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn attach_session_drives_input_resize_and_reads_events() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#); // attach ack
        h.push_script(r#"{"type":"ok","version":"1"}"#); // send_input ack
        h.push_script(r#"{"type":"ok","version":"1"}"#); // resize ack
        h.push_script(r#"{"type":"guest","event":"stdout","data":"hi"}"#);
        h.push_script(r#"{"type":"guest","event":"exit","code":0}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let mut session = sb.attach_session().unwrap();
        session.send_input(b"x").unwrap();
        session.resize(80, 24).unwrap();
        let mut out = String::new();
        while let Some(ev) = session.read_events().unwrap() {
            if let GuestEvent::Stdout { data } = ev {
                out.push_str(&data);
            }
        }
        assert_eq!(out, "hi");
    }

    #[test]
    fn kill_with_sends_signal() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        sb.kill_with(15).unwrap();
    }

    #[test]
    fn wait_returns_terminal_exit() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"wait","exit_code":137,"exited_at_ms":1234,"state":"stopped"}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let (code, at, state) = sb.wait(Some(Duration::from_secs(30))).unwrap();
        assert_eq!(code, 137);
        assert_eq!(at, 1234);
        assert_eq!(state, SandboxState::Stopped);
    }

    #[test]
    fn exec_pty_session_drives_input_resize_and_reads_events() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#); // exec_stream ack
        h.push_script(r#"{"type":"ok","version":"1"}"#); // send_input ack
        h.push_script(r#"{"type":"ok","version":"1"}"#); // resize ack
        h.push_script(r#"{"type":"guest","event":"stdout","data":"echo"}"#);
        h.push_script(r#"{"type":"guest","event":"exit","code":0}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let mut req = ExecRequest::new("cat");
        req.cols = 120;
        req.rows = 40;
        let mut session = sb.exec_pty(req).unwrap();
        session.send_input(b"x").unwrap();
        session.resize(120, 40).unwrap();
        let mut out = String::new();
        while let Some(ev) = session.read_events().unwrap() {
            if let GuestEvent::Stdout { data } = ev {
                out.push_str(&data);
            }
        }
        assert_eq!(out, "echo");
    }

    #[test]
    fn logs_with_cursor_and_source() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"logs","lines":[{"cursor":5,"timestamp_ms":0,"source":"stdout","data":"x"}],"next_cursor":6}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let lines = sb
            .logs_with(Some(10), Some(5), Some(LogSource::Stdout))
            .unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].cursor, 5);
    }

    #[test]
    fn logs_follow_with_cursor_and_source() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#);
        h.push_script(r#"{"type":"event","event":"log","cursor":5,"timestamp_ms":0,"source":"stdout","data":"x"}"#);
        h.push_script(r#"{"type":"event","event":"done"}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let mut out = String::new();
        sb.logs_follow_with(Some(10), Some(5), Some(LogSource::Stdout), |l| {
            out.push_str(&l.data)
        })
        .unwrap();
        assert_eq!(out, "x");
    }

    #[test]
    fn list_page_returns_next_cursor() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"list","sandboxes":[{"name":"a","state":"running","image":"alpine","detached":false,"ephemeral":false,"labels":{}}],"next_cursor":"abc"}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let page = c
            .list_page(Some(10), Some("prev".into()), Some("team=sdk"))
            .unwrap();
        assert_eq!(page.sandboxes.len(), 1);
        assert_eq!(page.next_cursor.as_deref(), Some("abc"));
    }

    #[test]
    fn wait_until_stopped_polls_until_stopped() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"status","state":"running"}"#);
        h.push_script(r#"{"type":"status","state":"stopped"}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let state = sb.wait_until_stopped(Duration::from_secs(5)).unwrap();
        assert_eq!(state, SandboxState::Stopped);
    }

    #[test]
    fn shell_stream_runs_script() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"ok","version":"1"}"#);
        h.push_script(r#"{"type":"guest","event":"stdout","data":"hi"}"#);
        h.push_script(r#"{"type":"guest","event":"exit","code":0}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let mut out = String::new();
        let code = sb
            .shell_stream("echo hi", |ev| {
                if let GuestEvent::Stdout { data } = ev {
                    out.push_str(&data);
                }
            })
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(out, "hi");
    }

    #[test]
    fn fs_read_stream_chunks() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"fs_data","data_base64":"aGVsbG8=","eof":true}"#);
        h.push_script(r#"{"type":"fs_data","data_base64":"","eof":true}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        let mut out = Vec::new();
        sb.fs_read_stream("/tmp/x", 3, |chunk| out.extend_from_slice(chunk))
            .unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn volume_fs_returns_host_dir() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"volume","name":"data","dir":"/tmp/vol-data","used_bytes":0}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        assert_eq!(
            sb.volume_fs("data").unwrap(),
            PathBuf::from("/tmp/vol-data")
        );
    }

    #[test]
    fn volume_fs_falls_back_to_home() {
        let h = FakeHelper::start();
        h.push_script(r#"{"type":"volume","name":"data","used_bytes":0}"#);
        let c = SandboxClient::connect(h.path()).unwrap();
        let sb = c.sandbox("a");
        assert_eq!(
            sb.volume_fs("data").unwrap(),
            nanosb_home().join("volumes").join("data")
        );
    }
}
