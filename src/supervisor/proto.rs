//! Serves the `nanosb-protocol` verb set on a supervisor control connection.
//!
//! A connection is served in "new" mode only when its opening frame is a
//! `hello`; a connection that opens with a legacy `ControlRequest` keeps the
//! original framing. This keeps the CLI/TUI wire-compatible while letting SDK
//! clients speak the versioned protocol.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nanosb_protocol::{
    base64_encode, Capability, ExecResult, FsWriteStreamSpec, GuestEvent, HelloRequest,
    HelloResponse, LogLine, LogSource, Metrics, ModifySpec, Request, Response, SandboxInfo,
    SecretRef, SecretSetSpec, StreamEvent, PROTOCOL_VERSION,
};
use runtime::exec::{ExecClient, ExecEvent, ExecHandle, ExecOptions};

use crate::supervisor::secrets::SecretStore;
use crate::supervisor::volumes::{VolumeError, VolumeRegistry};
use crate::supervisor::{read_current_state, now_ms, ScrollbackRing, Subscribers};

fn map_state(s: crate::supervisor::SandboxState) -> nanosb_protocol::SandboxState {
    use crate::supervisor::SandboxState as S;
    use nanosb_protocol::SandboxState as P;
    match s {
        S::Starting => P::Starting,
        S::Running => P::Running,
        S::Stopped => P::Stopped,
        S::Error => P::Error,
    }
}

/// Per-connection context borrowed from the supervisor.
pub struct Ctx<'a> {
    pub sandbox_dir: &'a std::path::Path,
    pub sandbox_name: &'a str,
    pub started_at: &'a str,
    pub config_hash: &'a str,
    pub scrollback: &'a Arc<Mutex<ScrollbackRing>>,
    pub subscribers: &'a Subscribers,
    pub stdin_writer: Arc<Mutex<std::io::PipeWriter>>,
    pub stop_requested: &'a AtomicBool,
    pub force_stop: &'a AtomicBool,
    pub running: &'a AtomicBool,
    pub next_subscriber_id: &'a Arc<AtomicU64>,
    pub secrets: &'a SecretStore,
    pub volumes: &'a VolumeRegistry,
    pub exec_session: &'a Mutex<Option<ExecHandle>>,
    /// In-flight `wait` handlers; the supervisor drains them before exiting.
    pub active_waiters: &'a AtomicUsize,
}

const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether this sandbox was started with the resident exec agent wired up.
///
/// Exec is advertised only when the exec channel is configured (the guest agent
/// is resident), so a client can fail loudly instead of hitting a reset.
fn exec_configured(ctx: &Ctx) -> bool {
    std::fs::read_to_string(ctx.sandbox_dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("vsock_socket").and_then(|s| s.as_str()).map(String::from))
        .is_some_and(|s| !s.is_empty())
}

fn capabilities(ctx: &Ctx) -> Vec<Capability> {
    let mut caps = vec![
        Capability::Lifecycle,
        Capability::Fs,
        Capability::Logs,
        Capability::Metrics,
        Capability::Networking,
        Capability::Handles,
        Capability::Ephemeral,
        Capability::Secrets,
        Capability::Volumes,
        Capability::Signal,
        Capability::Wait,
    ];
    if exec_configured(ctx) {
        caps.push(Capability::Exec);
        caps.push(Capability::ExecPty);
    }
    caps
}

/// Serve a "new mode" connection: `hello` already parsed from the first frame.
pub fn serve(
    mut stream: UnixStream,
    mut reader: BufReader<UnixStream>,
    ctx: &Ctx,
    hello: HelloRequest,
) {
    let _ = stream.set_nonblocking(false);

    if !hello.accepts(PROTOCOL_VERSION) {
        let err = Response::Error(nanosb_protocol::ProtocolError::new(
            nanosb_protocol::ErrorCode::IncompatibleVersion,
            format!(
                "helper speaks {} but client range is {}..{}",
                PROTOCOL_VERSION, hello.protocol_min, hello.protocol_max
            ),
        ));
        let _ = send(&mut stream, &err);
        return;
    }
    let hello_ok = Response::HelloOk(HelloResponse {
        protocol: PROTOCOL_VERSION.to_string(),
        helper_version: HELPER_VERSION.to_string(),
        capabilities: capabilities(ctx),
    });
    if send(&mut stream, &hello_ok).is_err() {
        return;
    }

    let mut scratch = String::new();
    loop {
        scratch.clear();
        match reader.read_line(&mut scratch) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = scratch.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let req: Request = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = send(
                            &mut stream,
                            &Response::Error(nanosb_protocol::ProtocolError::bad_request(format!(
                                "invalid request: {e}"
                            ))),
                        );
                        continue;
                    }
                };
                if handle(&mut stream, &mut reader, ctx, req).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn send(stream: &mut UnixStream, resp: &Response) -> std::io::Result<()> {
    let mut buf = serde_json::to_string(resp).unwrap_or_default().into_bytes();
    buf.push(b'\n');
    stream.write_all(&buf)?;
    stream.flush()
}

/// Write one frame while holding `out`'s lock, as a single `write_all`, so two
/// threads sharing the client socket cannot splice frames on the wire.
fn send_locked(out: &Mutex<UnixStream>, resp: &Response) -> std::io::Result<()> {
    let mut buf = serde_json::to_string(resp).unwrap_or_default().into_bytes();
    buf.push(b'\n');
    let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
    s.write_all(&buf)?;
    s.flush()
}

fn send_error(stream: &mut UnixStream, code: nanosb_protocol::ErrorCode, msg: impl Into<String>) {
    let _ = send(
        stream,
        &Response::Error(nanosb_protocol::ProtocolError::new(code, msg)),
    );
}

fn unsupported(stream: &mut UnixStream, msg: impl Into<String>) -> std::io::Result<()> {
    send_error(stream, nanosb_protocol::ErrorCode::Unsupported, msg);
    Ok(())
}

fn exec_client(ctx: &Ctx) -> ExecClient {
    ExecClient::new(ctx.sandbox_dir.join("exec.sock"))
}

fn handle(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    ctx: &Ctx,
    req: Request,
) -> std::io::Result<()> {
    use nanosb_protocol::ErrorCode;
    match req {
        Request::Hello(_) => {
            let resp = Response::HelloOk(HelloResponse {
                protocol: PROTOCOL_VERSION.to_string(),
                helper_version: HELPER_VERSION.to_string(),
                capabilities: capabilities(ctx),
            });
            send(stream, &resp)
        }
        Request::Ping => send(
            stream,
            &Response::Pong {
                version: HELPER_VERSION.to_string(),
            },
        ),
        Request::Status { .. } => {
            let state = read_current_state(ctx.sandbox_dir);
            send(
                stream,
                &Response::Status {
                    state: map_state(state.state),
                    pid: state.pid,
                    exit_code: state.exit_code,
                    started_at: Some(state.started_at),
                    config_hash: Some(state.config_hash),
                },
            )
        }
        Request::Get { .. } => {
            let info = sandbox_info(ctx);
            send(stream, &Response::Sandbox(info))
        }
        Request::List { .. } => send(
            stream,
            &Response::List(nanosb_protocol::SandboxList {
                sandboxes: vec![sandbox_info(ctx)],
                next_cursor: None,
            }),
        ),
        Request::Stop { force, .. } => ack_stop(stream, ctx, force),
        Request::Kill { signal, .. } => handle_kill(stream, ctx, signal),
        Request::Wait { timeout_secs, .. } => handle_wait(stream, ctx, timeout_secs),
        Request::Remove { force, .. } => ack_stop(stream, ctx, force),
        Request::Drain { .. } => ack_stop(stream, ctx, false),
        Request::Touch { .. } => send(
            stream,
            &Response::Ok {
                version: HELPER_VERSION.to_string(),
            },
        ),
        Request::Pause { .. } => signal_vm(stream, ctx, libc::SIGSTOP),
        Request::Resume { .. } => signal_vm(stream, ctx, libc::SIGCONT),
        Request::Exec(spec) => exec_buffered(
            stream,
            ctx,
            spec.command,
            spec.args,
            spec.cwd,
            spec.env,
            spec.shell,
            spec.timeout_secs,
            spec.rlimits,
        ),
        Request::Shell(spec) => exec_buffered(
            stream,
            ctx,
            spec.script,
            Vec::new(),
            spec.cwd,
            spec.env,
            true,
            spec.timeout_secs,
            Vec::new(),
        ),
        Request::ExecDefault(spec) => exec_default(stream, ctx, *spec),
        Request::ExecStream(spec) => {
            let opts = ExecOptions {
                workdir: spec.cwd.clone(),
                env: spec.env.clone(),
                shell: spec.shell,
                timeout_secs: spec.timeout_secs,
                rlimits: spec
                    .rlimits
                    .iter()
                    .map(|r| {
                        format!(
                            "{}={}:{}",
                            r.resource.to_uppercase(),
                            r.soft.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                            r.hard.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
                        )
                    })
                    .collect(),
                tty: spec.tty,
                cols: spec.cols,
                rows: spec.rows,
                ..ExecOptions::default()
            };
            let args: Vec<String> = spec.args.clone();
            let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
            stream_exec_stream(stream, reader, ctx, &spec.command, &argrefs, opts)
        }
        Request::Attach { .. } | Request::AttachDefault { .. } => {
            stream_attach(stream, reader, ctx)
        }
        Request::Logs(spec) => handle_logs(stream, ctx, spec),
        Request::Metrics { .. } => send(stream, &Response::Metrics(sample_metrics(ctx))),
        Request::FsRead(p) => {
            let c = exec_client(ctx);
            match c.fs_read(&p.path, p.offset, p.length) {
                Ok(bytes) => send(
                    stream,
                    &Response::FsData {
                        data_base64: base64_encode(&bytes),
                        eof: true,
                    },
                ),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsReadToString(p) => {
            let c = exec_client(ctx);
            match c.fs_read_to_string(&p.path) {
                Ok(content) => send(stream, &Response::FsContent { content }),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsWrite(w) => {
            let data = match nanosb_protocol::base64_decode(&w.data_base64) {
                Some(d) => d,
                None => {
                    send_error(stream, ErrorCode::BadRequest, "invalid base64 payload");
                    return Ok(());
                }
            };
            let c = exec_client(ctx);
            match c.fs_write(&w.path, &data, w.append, w.mode) {
                Ok(()) => ack_ok(stream),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsWriteStream(spec) => fs_write_stream(stream, reader, ctx, spec),
        Request::FsData { .. } => {
            send_error(
                stream,
                ErrorCode::InvalidState,
                "fs_data frame without an open fs_write_stream",
            );
            Ok(())
        }
        Request::FsList(p) => {
            let c = exec_client(ctx);
            match c.fs_list(&p.path) {
                Ok(entries) => send(stream, &Response::FsEntries { entries }),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsStat(p) => {
            let c = exec_client(ctx);
            match c.fs_stat(&p.path) {
                Ok(stat) => send(stream, &Response::FsStat(stat)),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsExists(p) => {
            let c = exec_client(ctx);
            match c.fs_exists(&p.path) {
                Ok(exists) => send(stream, &Response::FsExists { exists }),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsMkdir(p) => {
            let c = exec_client(ctx);
            match c.fs_mkdir(&p.path, p.recursive, p.mode) {
                Ok(()) => ack_ok(stream),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsRemove(p) => {
            let c = exec_client(ctx);
            match c.fs_remove(&p.path) {
                Ok(()) => ack_ok(stream),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsRemoveDir(p) => {
            let c = exec_client(ctx);
            match c.fs_remove_dir(&p.path, p.recursive) {
                Ok(()) => ack_ok(stream),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsCopy(p) => {
            let c = exec_client(ctx);
            match c.fs_copy(&p.from, &p.to) {
                Ok(()) => ack_ok(stream),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsRename(p) => {
            let c = exec_client(ctx);
            match c.fs_rename(&p.from, &p.to) {
                Ok(()) => ack_ok(stream),
                Err(e) => Err(send_agent_error(stream, e)),
            }
        }
        Request::FsCopyFromHost(p) => fs_copy_from_host(stream, ctx, &p.host_path, &p.guest_path),
        Request::FsCopyToHost(p) => fs_copy_to_host(stream, ctx, &p.guest_path, &p.host_path),
        Request::Create(_) => unsupported(stream, "verb is served by the helper daemon"),
        Request::Start { .. } => handle_start(stream, ctx),
        Request::Modify { spec, .. } => handle_modify(stream, ctx, spec),
        Request::VolumeCreate(spec) => handle_volume_create(stream, ctx, spec),
        Request::VolumeList { .. } => send(
            stream,
            &Response::Volumes {
                volumes: ctx.volumes.list(),
            },
        ),
        Request::VolumeGet(r) => match ctx.volumes.get(&r.volume) {
            Ok(v) => send(stream, &Response::Volume(v)),
            Err(e) => send_volume_error(stream, e),
        },
        Request::VolumeRemove(r) => match ctx.volumes.remove(&r.volume) {
            Ok(()) => ack_ok(stream),
            Err(e) => send_volume_error(stream, e),
        },
        Request::MountAdd(_) => unsupported(
            stream,
            "live mounts are not supported by this engine; declare mounts at create time",
        ),
        Request::SecretSet(spec) => handle_secret_set(stream, ctx, spec),
        Request::SecretList { .. } => {
            let secrets = ctx.secrets.list();
            send(stream, &Response::Secrets { secrets })
        }
        Request::SecretRemove(spec) => handle_secret_remove(stream, ctx, spec),
        Request::SnapshotCreate(_)
        | Request::SnapshotRestore(_)
        | Request::SnapshotFork(_)
        | Request::SnapshotRemove { .. } => {
            unsupported(stream, "snapshots are not available in this build")
        }
        Request::SnapshotList => send(
            stream,
            &Response::Snapshots {
                snapshots: Vec::new(),
            },
        ),
        Request::MetricsStream { interval_ms, .. } => stream_metrics(stream, ctx, interval_ms),
        Request::AttachInput { data } => attach_input(stream, ctx, data),
        Request::AttachResize { cols, rows } => attach_resize(stream, ctx, cols, rows),
        Request::ConsoleInput { data } => console_input(stream, ctx, data),
        Request::ConsoleInputClose => console_input_close(stream, ctx),
        Request::ExecInput { .. } => {
            send_error(
                stream,
                ErrorCode::InvalidState,
                "exec_input frame without an open tty exec_stream",
            );
            Ok(())
        }
        Request::ExecResize { .. } => {
            send_error(
                stream,
                ErrorCode::InvalidState,
                "exec_resize frame without an open tty exec_stream",
            );
            Ok(())
        }
    }
}

fn ack_ok(stream: &mut UnixStream) -> std::io::Result<()> {
    send(
        stream,
        &Response::Ok {
            version: HELPER_VERSION.to_string(),
        },
    )
}

fn ack_stop(stream: &mut UnixStream, ctx: &Ctx, force: bool) -> std::io::Result<()> {
    if force {
        ctx.force_stop.store(true, Ordering::SeqCst);
    }
    ctx.stop_requested.store(true, Ordering::SeqCst);
    ack_ok(stream)
}

/// `kill` with an optional signal. `None`/`SIGKILL` force-stop; `SIGTERM`
/// requests a graceful stop; any other signal is `unsupported` (never silently
/// ignored).
fn handle_kill(stream: &mut UnixStream, ctx: &Ctx, signal: Option<i32>) -> std::io::Result<()> {
    match signal {
        None | Some(libc::SIGKILL) => ack_stop(stream, ctx, true),
        Some(libc::SIGTERM) => ack_stop(stream, ctx, false),
        Some(other) => {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::Unsupported,
                format!(
                    "kill signal {other} is not supported; use SIGTERM (15) or SIGKILL (9)"
                ),
            );
            Ok(())
        }
    }
}

/// Block until the sandbox reaches a terminal state (`stopped`/`error`),
/// polling the persisted state every ~50ms. Returns the terminal exit code and
/// exit timestamp; an already-terminal sandbox returns immediately. A
/// `timeout_secs` budget that elapses answers `timeout`.
fn handle_wait(
    stream: &mut UnixStream,
    ctx: &Ctx,
    timeout_secs: Option<u64>,
) -> std::io::Result<()> {
    ctx.active_waiters.fetch_add(1, Ordering::SeqCst);
    let deadline = timeout_secs.map(|s| Instant::now() + Duration::from_secs(s));
    loop {
        let state = read_current_state(ctx.sandbox_dir);
        if matches!(
            state.state,
            crate::supervisor::SandboxState::Stopped | crate::supervisor::SandboxState::Error
        ) {
            let exited_at_ms = state.exited_at_ms.unwrap_or_else(now_ms);
            let exit_code = state.exit_code.unwrap_or(255);
            ctx.active_waiters.fetch_sub(1, Ordering::SeqCst);
            return send(
                stream,
                &Response::Wait {
                    exit_code,
                    exited_at_ms,
                    state: map_state(state.state),
                },
            );
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                ctx.active_waiters.fetch_sub(1, Ordering::SeqCst);
                send_error(stream, nanosb_protocol::ErrorCode::Timeout, "wait timed out");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn signal_vm(stream: &mut UnixStream, ctx: &Ctx, sig: i32) -> std::io::Result<()> {
    let state = read_current_state(ctx.sandbox_dir);
    match state.pid {
        Some(pid) if pid > 0 => {
            unsafe {
                libc::kill(pid, sig);
            }
            ack_ok(stream)
        }
        _ => {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::InvalidState,
                "sandbox has no running VM",
            );
            Ok(())
        }
    }
}

fn handle_start(stream: &mut UnixStream, ctx: &Ctx) -> std::io::Result<()> {
    let state = read_current_state(ctx.sandbox_dir);
    match state.state {
        crate::supervisor::SandboxState::Running | crate::supervisor::SandboxState::Starting => {
            ack_ok(stream)
        }
        _ => {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::InvalidState,
                "sandbox is not running and cannot be restarted by this engine",
            );
            Ok(())
        }
    }
}

fn handle_modify(stream: &mut UnixStream, ctx: &Ctx, spec: ModifySpec) -> std::io::Result<()> {
    match apply_modify(ctx.sandbox_dir, &spec) {
        Ok(()) => ack_ok(stream),
        Err(e) => {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::Internal,
                format!("modify failed: {e}"),
            );
            Ok(())
        }
    }
}

fn apply_modify(sandbox_dir: &std::path::Path, spec: &ModifySpec) -> std::io::Result<()> {
    let path = sandbox_dir.join("config.json");
    let mut cfg: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(v) = spec.idle_timeout_secs {
        cfg["idle_timeout_secs"] = serde_json::json!(v);
    }
    if let Some(v) = spec.max_duration_secs {
        cfg["max_duration_secs"] = serde_json::json!(v);
    }
    if !spec.labels.is_empty() {
        let labels = cfg
            .as_object_mut()
            .map(|o| o.entry("labels").or_insert_with(|| serde_json::json!({})));
        if let Some(serde_json::Value::Object(map)) = labels {
            for (k, v) in &spec.labels {
                map.insert(k.clone(), serde_json::json!(v));
            }
        }
    }
    let json = serde_json::to_string_pretty(&cfg)?;
    std::fs::write(&path, json)
}

fn handle_secret_set(
    stream: &mut UnixStream,
    ctx: &Ctx,
    spec: SecretSetSpec,
) -> std::io::Result<()> {
    ctx.secrets.set(spec.key, spec.value);
    ack_ok(stream)
}

fn handle_volume_create(
    stream: &mut UnixStream,
    ctx: &Ctx,
    spec: nanosb_protocol::VolumeCreateSpec,
) -> std::io::Result<()> {
    match ctx.volumes.create(&spec.volume) {
        Ok(v) => send(stream, &Response::Volume(v)),
        Err(e) => send_volume_error(stream, e),
    }
}

fn send_volume_error(stream: &mut UnixStream, e: VolumeError) -> std::io::Result<()> {
    send(stream, &Response::Error(e.to_protocol()))
}

fn handle_secret_remove(
    stream: &mut UnixStream,
    ctx: &Ctx,
    spec: SecretRef,
) -> std::io::Result<()> {
    ctx.secrets.remove(&spec.key);
    ack_ok(stream)
}

fn ensure_exec_session(ctx: &Ctx) -> Result<(), String> {
    let c = exec_client(ctx);
    if !c.is_available() {
        return Err("exec channel is not available".to_string());
    }
    let mut session = ctx
        .exec_session
        .lock()
        .map_err(|_| "interactive session lock poisoned".to_string())?;
    if session.is_none() {
        let handle = c
            .start("/bin/sh", &[], ExecOptions::default().tty(true))
            .map_err(|e| format!("cannot start interactive session: {e}"))?;
        *session = Some(handle);
    }
    Ok(())
}

fn drain_session(h: &mut ExecHandle) {
    loop {
        match h.next_event(Some(Duration::ZERO)) {
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break,
        }
    }
}

fn attach_input(stream: &mut UnixStream, ctx: &Ctx, data: String) -> std::io::Result<()> {
    if let Err(msg) = ensure_exec_session(ctx) {
        send_error(stream, nanosb_protocol::ErrorCode::InvalidState, msg);
        return Ok(());
    }
    let write_result = {
        let mut session = match ctx.exec_session.lock() {
            Ok(s) => s,
            Err(_) => {
                send_error(
                    stream,
                    nanosb_protocol::ErrorCode::InvalidState,
                    "interactive session lock poisoned",
                );
                return Ok(());
            }
        };
        let Some(h) = session.as_mut() else {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::InvalidState,
                "interactive session unavailable",
            );
            return Ok(());
        };
        let r = h.write_stdin(&data);
        drain_session(h);
        r
    };
    if let Err(e) = write_result {
        send_error(
            stream,
            nanosb_protocol::ErrorCode::InvalidState,
            format!("stdin write failed: {e}"),
        );
        return Ok(());
    }
    ack_ok(stream)
}

fn console_input(stream: &mut UnixStream, ctx: &Ctx, data: String) -> std::io::Result<()> {
    let result = match ctx.stdin_writer.lock() {
        Ok(mut w) => w.write_all(data.as_bytes()).and_then(|_| w.flush()),
        Err(_) => Err(std::io::Error::other("stdin writer poisoned")),
    };
    match result {
        Ok(()) => ack_ok(stream),
        Err(e) => {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::Io,
                format!("stdin write failed: {e}"),
            );
            Ok(())
        }
    }
}

fn console_input_close(stream: &mut UnixStream, ctx: &Ctx) -> std::io::Result<()> {
    let Ok(mut w) = ctx.stdin_writer.lock() else {
        send_error(
            stream,
            nanosb_protocol::ErrorCode::InvalidState,
            "stdin writer poisoned",
        );
        return Ok(());
    };
    let (_, dummy) = std::io::pipe()?;
    let old = std::mem::replace(&mut *w, dummy);
    drop(old);
    drop(w);
    ack_ok(stream)
}

fn attach_resize(stream: &mut UnixStream, ctx: &Ctx, cols: u16, rows: u16) -> std::io::Result<()> {
    if let Err(msg) = ensure_exec_session(ctx) {
        send_error(stream, nanosb_protocol::ErrorCode::InvalidState, msg);
        return Ok(());
    }
    let resize_result = {
        let mut session = match ctx.exec_session.lock() {
            Ok(s) => s,
            Err(_) => {
                send_error(
                    stream,
                    nanosb_protocol::ErrorCode::InvalidState,
                    "interactive session lock poisoned",
                );
                return Ok(());
            }
        };
        let Some(h) = session.as_mut() else {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::InvalidState,
                "interactive session unavailable",
            );
            return Ok(());
        };
        h.resize(cols, rows)
    };
    if let Err(e) = resize_result {
        send_error(
            stream,
            nanosb_protocol::ErrorCode::InvalidState,
            format!("resize failed: {e}"),
        );
        return Ok(());
    }
    ack_ok(stream)
}

fn sandbox_info(ctx: &Ctx) -> SandboxInfo {
    let state = read_current_state(ctx.sandbox_dir);
    let config: serde_json::Value = std::fs::read_to_string(ctx.sandbox_dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let live = matches!(state.state, crate::supervisor::SandboxState::Running)
        && state.pid.map(|p| p > 0).unwrap_or(false);
    SandboxInfo {
        name: ctx.sandbox_name.to_string(),
        state: map_state(state.state),
        image: config
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        pid: state.pid,
        exit_code: state.exit_code,
        started_at: Some(state.started_at),
        config_hash: Some(state.config_hash),
        detached: !live,
        ephemeral: config
            .get("ephemeral")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        labels: std::collections::HashMap::new(),
    }
}

fn exec_buffered(
    stream: &mut UnixStream,
    ctx: &Ctx,
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    env: std::collections::HashMap<String, String>,
    shell: bool,
    timeout_secs: Option<u64>,
    rlimits: Vec<nanosb_protocol::Rlimit>,
) -> std::io::Result<()> {
    let c = exec_client(ctx);
    if !c.is_available() {
        send_error(
            stream,
            nanosb_protocol::ErrorCode::InvalidState,
            "exec channel is not available (start the sandbox with --exec)",
        );
        return Ok(());
    }
    let opts = ExecOptions {
        workdir: cwd,
        env,
        shell,
        timeout_secs,
        rlimits: rlimits
            .iter()
            .map(|r| {
                format!(
                    "{}={}:{}",
                    r.resource.to_uppercase(),
                    r.soft.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                    r.hard.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
                )
            })
            .collect(),
        ..ExecOptions::default()
    };
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    match c.exec_with(&command, &argrefs, opts) {
        Ok(r) => send(
            stream,
            &Response::ExecResult(ExecResult {
                exit_code: r.exit_code,
                stdout: r.stdout,
                stderr: r.stderr,
                duration_ms: r.duration_ms,
            }),
        ),
        Err(e) => Err(send_agent_error(stream, e)),
    }
}

/// Execute the sandbox's configured default workload with optional overrides.
fn exec_default(
    stream: &mut UnixStream,
    ctx: &Ctx,
    spec: nanosb_protocol::ExecDefaultRequest,
) -> std::io::Result<()> {
    let config: serde_json::Value = std::fs::read_to_string(ctx.sandbox_dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let configured = config
        .get("command")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && !s.ends_with("exec-agent"));
    let (command, mut args) = match configured {
        Some(cmd) => (
            cmd.to_string(),
            config
                .get("command_args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        ),
        None => ("/bin/sh".to_string(), Vec::new()),
    };
    args.extend(spec.args);
    let env = spec.env;
    exec_buffered(
        stream,
        ctx,
        command,
        args,
        spec.cwd,
        env,
        false,
        spec.timeout_secs,
        spec.rlimits,
    )
}

fn stream_exec_stream(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    ctx: &Ctx,
    command: &str,
    args: &[&str],
    opts: ExecOptions,
) -> std::io::Result<()> {
    let c = exec_client(ctx);
    if !c.is_available() {
        send_error(
            stream,
            nanosb_protocol::ErrorCode::InvalidState,
            "exec channel is not available",
        );
        return Ok(());
    }
    ack_ok(stream)?;
    let tty = opts.tty;
    let mut handle = match c.start(command, args, opts) {
        Ok(h) => h,
        Err(e) => {
            send_error(
                stream,
                nanosb_protocol::ErrorCode::Io,
                format!("cannot start exec: {e}"),
            );
            return Ok(());
        }
    };

    // All client-bound frames (guest output, input acks, terminal) go through
    // one locked writer so the output loop and the tty input reader cannot
    // interleave frames on the wire.
    let out = Arc::new(Mutex::new(stream.try_clone()?));
    let done = Arc::new(AtomicBool::new(false));

    if tty {
        let writer = match handle.try_clone_writer() {
            Ok(w) => w,
            Err(e) => {
                send_error(
                    stream,
                    nanosb_protocol::ErrorCode::Io,
                    format!("cannot clone exec writer: {e}"),
                );
                return Ok(());
            }
        };
        // Share `serve`'s BufReader so frames pipelined behind the exec_stream
        // request (still in its buffer) are not lost to a raw-fd clone.
        let _ = reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(200)));
        let out_for_input = out.clone();
        let done_for_input = done.clone();
        let input_reader = &mut *reader;
        std::thread::scope(|scope| -> std::io::Result<()> {
            scope.spawn(move || {
                exec_input_reader(input_reader, writer, &out_for_input, &done_for_input);
            });
            let result = exec_output_loop(&mut handle, &out);
            done.store(true, Ordering::SeqCst);
            result
        })?;
        let _ = reader.get_ref().set_read_timeout(None);
    } else {
        exec_output_loop(&mut handle, &out)?;
    }
    Ok(())
}

/// Stream guest output frames to the client until the exec terminates.
fn exec_output_loop(handle: &mut ExecHandle, out: &Mutex<UnixStream>) -> std::io::Result<()> {
    let (code, err_msg) = loop {
        match handle.next_event(None) {
            Ok(Some(ExecEvent::Output(chunk))) => {
                let ev = match chunk.stream {
                    runtime::exec::Stream::Stdout => GuestEvent::Stdout { data: chunk.data },
                    runtime::exec::Stream::Stderr => GuestEvent::Stderr { data: chunk.data },
                };
                send_locked(out, &Response::Guest(ev))?;
            }
            Ok(Some(ExecEvent::Exit { code: c })) => break (Some(c), None),
            Ok(Some(ExecEvent::Error { message })) => break (None, Some(message)),
            Ok(Some(ExecEvent::Started { .. })) => {}
            Ok(None) => break (None, None),
            Err(e) => break (None, Some(e.to_string())),
        }
    };
    let exit = match (code, err_msg) {
        (Some(c), _) => GuestEvent::Exit { code: c },
        (None, Some(m)) => GuestEvent::Error { message: m },
        (None, None) => GuestEvent::Error {
            message: "exec stream closed before exit".to_string(),
        },
    };
    send_locked(out, &Response::Guest(exit))
}

/// Forward `exec_input`/`exec_resize` frames from the shared control reader to
/// the exec handle while [`exec_output_loop`] streams. Acks only a successful
/// write; a failed write errors the client instead of a false `ok`.
fn exec_input_reader(
    reader: &mut BufReader<UnixStream>,
    mut writer: runtime::exec::ExecWriter,
    out: &Mutex<UnixStream>,
    done: &AtomicBool,
) {
    let mut line = String::new();
    loop {
        if done.load(Ordering::SeqCst) {
            break;
        }
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let parsed = serde_json::from_str::<Request>(line.trim());
                line.clear();
                let Ok(req) = parsed else { continue };
                let result = match req {
                    Request::ExecInput { data } => writer.write_stdin(&data),
                    Request::ExecResize { cols, rows } => writer.resize(cols, rows),
                    _ => continue,
                };
                match result {
                    Ok(()) => {
                        let _ = send_locked(
                            out,
                            &Response::Ok {
                                version: HELPER_VERSION.to_string(),
                            },
                        );
                    }
                    Err(e) => {
                        let _ = send_locked(
                            out,
                            &Response::Error(nanosb_protocol::ProtocolError::io(format!(
                                "exec input failed: {e}"
                            ))),
                        );
                        break;
                    }
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue
            }
            Err(_) => break,
        }
    }
}

fn stream_attach(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    ctx: &Ctx,
) -> std::io::Result<()> {
    ack_ok(stream)?;
    if let Ok(sb) = ctx.scrollback.lock() {
        let data = sb.read_all();
        if !data.is_empty() {
            send(
                stream,
                &Response::Guest(GuestEvent::Stdout {
                    data: String::from_utf8_lossy(&data).to_string(),
                }),
            )?;
        }
    }
    let subscriber_id = ctx.next_subscriber_id.fetch_add(1, Ordering::SeqCst);
    let mut registered = false;
    if let Ok(clone) = stream.try_clone() {
        let _ = clone.set_write_timeout(Some(Duration::from_secs(5)));
        if let Ok(mut subs) = ctx.subscribers.lock() {
            subs.push((subscriber_id, Arc::new(Mutex::new(clone))));
            registered = true;
        }
    }
    let mut input = String::new();
    loop {
        input.clear();
        match reader.read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {
                if let Ok(parsed) = serde_json::from_str::<nanosb_protocol::Request>(input.trim()) {
                    match parsed {
                        Request::AttachInput { data } => {
                            if let Ok(mut w) = ctx.stdin_writer.lock() {
                                let _ = w.write_all(data.as_bytes());
                                let _ = w.flush();
                            }
                            ack_ok(stream)?;
                        }
                        Request::AttachResize { cols, rows } => {
                            attach_resize(stream, ctx, cols, rows)?
                        }
                        _ => {}
                    }
                }
            }
            Err(_) => break,
        }
    }
    if registered {
        if let Ok(mut subs) = ctx.subscribers.lock() {
            subs.retain(|(id, _)| *id != subscriber_id);
        }
    }
    Ok(())
}

fn log_source_name(source: LogSource) -> &'static str {
    match source {
        LogSource::Stdout => "stdout",
        LogSource::Stderr => "stderr",
        LogSource::Pty => "pty",
        LogSource::System => "system",
    }
}

fn log_source_from_name(name: &str) -> LogSource {
    match name {
        "stderr" => LogSource::Stderr,
        "pty" => LogSource::Pty,
        "system" => LogSource::System,
        _ => LogSource::Stdout,
    }
}

/// Parse the `console.jsonl` sidecar (one `{cursor,source,data}` record per line).
fn read_jsonl_logs(path: &std::path::Path) -> Vec<LogLine> {
    std::fs::read_to_string(path)
        .map(|content| {
            content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .map(|v| LogLine {
                    cursor: v.get("cursor").and_then(|x| x.as_u64()).unwrap_or(0),
                    timestamp_ms: 0,
                    source: log_source_from_name(
                        v.get("source").and_then(|x| x.as_str()).unwrap_or("stdout"),
                    ),
                    data: v
                        .get("data")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn handle_logs(
    stream: &mut UnixStream,
    ctx: &Ctx,
    spec: nanosb_protocol::LogsSpec,
) -> std::io::Result<()> {
    let logs_dir = ctx.sandbox_dir.join("logs");
    let page = |all: Vec<LogLine>| -> (Vec<LogLine>, u64) {
        let start = match spec.tail {
            Some(n) if all.len() > n => all.len() - n,
            _ => 0,
        };
        let start = spec
            .cursor
            .map(|c| (c as usize).min(all.len()))
            .unwrap_or(start);
        let next = all.len() as u64;
        (all[start..].to_vec(), next)
    };

    let mut lines = match spec.source {
        Some(src) => {
            let path = logs_dir.join(format!("{}.log", log_source_name(src)));
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .enumerate()
                .map(|(i, l)| LogLine {
                    cursor: i as u64,
                    timestamp_ms: 0,
                    source: src,
                    data: l.to_string(),
                })
                .collect::<Vec<_>>()
        }
        None => read_jsonl_logs(&logs_dir.join("console.jsonl")),
    };

    if !spec.follow {
        let (page_lines, next) = page(lines);
        return send(
            stream,
            &Response::Logs {
                next_cursor: Some(next),
                lines: page_lines,
            },
        );
    }

    let (page_lines, _) = page(lines.clone());
    ack_ok(stream)?;
    for line in &page_lines {
        send(stream, &Response::Event(StreamEvent::Log(line.clone())))?;
    }
    let mut seen = lines.len();
    lines.clear();
    while ctx.running.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        let fresh = match spec.source {
            Some(src) => {
                std::fs::read_to_string(logs_dir.join(format!("{}.log", log_source_name(src))))
                    .unwrap_or_default()
                    .lines()
                    .enumerate()
                    .map(|(i, l)| LogLine {
                        cursor: i as u64,
                        timestamp_ms: 0,
                        source: src,
                        data: l.to_string(),
                    })
                    .collect::<Vec<_>>()
            }
            None => read_jsonl_logs(&logs_dir.join("console.jsonl")),
        };
        if fresh.len() > seen {
            for line in &fresh[seen..] {
                send(stream, &Response::Event(StreamEvent::Log(line.clone())))?;
            }
            seen = fresh.len();
        }
    }
    send(stream, &Response::Event(StreamEvent::Done))
}

fn stream_metrics(
    stream: &mut UnixStream,
    ctx: &Ctx,
    interval_ms: Option<u64>,
) -> std::io::Result<()> {
    ack_ok(stream)?;
    let interval = Duration::from_millis(interval_ms.unwrap_or(1000).max(100));
    while ctx.running.load(Ordering::SeqCst) {
        send(
            stream,
            &Response::Event(StreamEvent::Metrics(sample_metrics(ctx))),
        )?;
        std::thread::sleep(interval);
    }
    send(stream, &Response::Event(StreamEvent::Done))
}

fn sample_metrics(ctx: &Ctx) -> Metrics {
    let state = read_current_state(ctx.sandbox_dir);
    let (rss_bytes, cpu_percent) = match state.pid {
        Some(pid) if pid > 0 => sample_process(pid),
        _ => (0, 0.0),
    };
    Metrics {
        cpu_percent,
        memory_used_bytes: rss_bytes,
        memory_total_bytes: 0,
        disk_used_bytes: dir_size(ctx.sandbox_dir),
        disk_total_bytes: 0,
        net_rx_bytes: 0,
        net_tx_bytes: 0,
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    }
}

fn sample_process(pid: i32) -> (u64, f64) {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=,%cpu=", "-p", &pid.to_string()])
        .output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.split_whitespace();
        let rss_kb: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let cpu: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        return (rss_kb * 1024, cpu);
    }
    (0, 0.0)
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += dir_size(&p);
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

fn fs_copy_from_host(
    stream: &mut UnixStream,
    ctx: &Ctx,
    host_path: &str,
    guest_path: &str,
) -> std::io::Result<()> {
    let c = exec_client(ctx);
    let host = std::path::Path::new(host_path);
    if !host.exists() {
        send_error(
            stream,
            nanosb_protocol::ErrorCode::NotFound,
            format!("host path {host_path} does not exist"),
        );
        return Ok(());
    }
    if host.is_dir() {
        if let Err(e) = c.fs_mkdir(guest_path, true, None) {
            return Err(send_agent_error(stream, e));
        }
        for entry in walk(host) {
            let rel = entry.strip_prefix(host).unwrap_or(&entry);
            let target = std::path::Path::new(guest_path).join(rel);
            let target_str = target.to_string_lossy().to_string();
            if entry.is_dir() {
                let _ = c.fs_mkdir(&target_str, true, None);
            } else if let Ok(data) = std::fs::read(&entry) {
                if let Err(e) = c.fs_write(&target_str, &data, false, None) {
                    return Err(send_agent_error(stream, e));
                }
            }
        }
    } else {
        let data = std::fs::read(host).map_err(std::io::Error::other)?;
        if let Err(e) = c.fs_write(guest_path, &data, false, None) {
            return Err(send_agent_error(stream, e));
        }
    }
    ack_ok(stream)
}

fn fs_copy_to_host(
    stream: &mut UnixStream,
    ctx: &Ctx,
    guest_path: &str,
    host_path: &str,
) -> std::io::Result<()> {
    let c = exec_client(ctx);
    match c.fs_stat(guest_path) {
        Ok(stat) if stat.is_dir => {
            let _ = std::fs::create_dir_all(host_path);
            if let Ok(entries) = c.fs_list(guest_path) {
                for e in entries {
                    if e.is_dir {
                        let _ =
                            std::fs::create_dir_all(std::path::Path::new(host_path).join(&e.name));
                    } else if !e.is_symlink {
                        if let Ok(data) = c.fs_read(&e.path, None, None) {
                            let _ =
                                std::fs::write(std::path::Path::new(host_path).join(&e.name), data);
                        }
                    }
                }
            }
        }
        Ok(_) => {
            let data = c
                .fs_read(guest_path, None, None)
                .map_err(std::io::Error::other)?;
            std::fs::write(host_path, data).map_err(std::io::Error::other)?;
        }
        Err(e) => return Err(send_agent_error(stream, e)),
    }
    ack_ok(stream)
}

/// Serve a streamed fs write: open the guest stream, then read `FsData`
/// frames from the connection until `eof`, acking each chunk.
fn fs_write_stream(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    ctx: &Ctx,
    spec: FsWriteStreamSpec,
) -> std::io::Result<()> {
    let c = exec_client(ctx);
    if !c.is_available() {
        send_error(
            stream,
            nanosb_protocol::ErrorCode::InvalidState,
            "exec channel is not available",
        );
        return Ok(());
    }
    let mut ws = match c.fs_write_stream_open(&spec.path, spec.append, spec.mode) {
        Ok(ws) => ws,
        Err(e) => return Err(send_agent_error(stream, e)),
    };
    ack_ok(stream)?;
    let mut input = String::new();
    loop {
        input.clear();
        match reader.read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let req: Request = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        send_error(
                            stream,
                            nanosb_protocol::ErrorCode::BadRequest,
                            format!("invalid request during fs_write_stream: {e}"),
                        );
                        break;
                    }
                };
                match req {
                    Request::FsData { data_base64, eof } => {
                        let bytes = match nanosb_protocol::base64_decode(&data_base64) {
                            Some(d) => d,
                            None => {
                                send_error(
                                    stream,
                                    nanosb_protocol::ErrorCode::BadRequest,
                                    "invalid base64 payload",
                                );
                                break;
                            }
                        };
                        if let Err(e) = ws.write_chunk(&bytes) {
                            return Err(send_agent_error(stream, e));
                        }
                        if eof {
                            if let Err(e) = ws.close() {
                                return Err(send_agent_error(stream, e));
                            }
                            return ack_ok(stream);
                        }
                        ack_ok(stream)?;
                    }
                    other => {
                        send_error(
                            stream,
                            nanosb_protocol::ErrorCode::InvalidState,
                            format!(
                                "expected fs_data frame during fs_write_stream, got {}",
                                serde_json::to_string(&other).unwrap_or_default()
                            ),
                        );
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    Ok(())
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                out.push(p.clone());
                if p.is_dir() {
                    stack.push(p);
                }
            }
        }
    }
    out
}

fn send_agent_error(stream: &mut UnixStream, e: runtime::exec::ExecError) -> std::io::Error {
    let msg = e.to_string();
    let _ = send(
        stream,
        &Response::Error(nanosb_protocol::ProtocolError::io(msg)),
    );
    std::io::Error::other("guest agent error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_input_close_signals_eof() {
        use std::io::Read;
        let (mut reader, writer) = std::io::pipe().unwrap();
        let stdin_writer = Arc::new(Mutex::new(writer));
        stdin_writer.lock().unwrap().write_all(b"hi").unwrap();
        {
            let mut w = stdin_writer.lock().unwrap();
            let (_, dummy) = std::io::pipe().unwrap();
            let old = std::mem::replace(&mut *w, dummy);
            drop(old);
        }
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"hi");
    }

    struct TestHarness {
        dir: std::path::PathBuf,
        stop_requested: AtomicBool,
        force_stop: AtomicBool,
        running: AtomicBool,
        active_waiters: AtomicUsize,
        scrollback: Arc<Mutex<ScrollbackRing>>,
        subscribers: Subscribers,
        stdin_writer: Arc<Mutex<std::io::PipeWriter>>,
        next_subscriber_id: Arc<AtomicU64>,
        secrets: SecretStore,
        volumes: VolumeRegistry,
        exec_session: Mutex<Option<ExecHandle>>,
    }

    impl TestHarness {
        fn new() -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("nsb-proto-{}-{}", std::process::id(), n));
            let _ = std::fs::create_dir_all(&dir);
            let scrollback = Arc::new(Mutex::new(ScrollbackRing::new(1024)));
            let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
            let (_, stdin_writer) = std::io::pipe().unwrap();
            let stop_requested = AtomicBool::new(false);
            let force_stop = AtomicBool::new(false);
            let running = AtomicBool::new(true);
            let next_subscriber_id = Arc::new(AtomicU64::new(0));
            let secrets = SecretStore::new();
            let volumes = VolumeRegistry::with_root(dir.join("volumes"));
            let exec_session = Mutex::new(None::<ExecHandle>);
            let active_waiters = AtomicUsize::new(0);
            Self {
                dir,
                stop_requested,
                force_stop,
                running,
                active_waiters,
                scrollback,
                subscribers,
                stdin_writer: Arc::new(Mutex::new(stdin_writer)),
                next_subscriber_id,
                secrets,
                volumes,
                exec_session,
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                sandbox_dir: &self.dir,
                sandbox_name: "t",
                started_at: "now",
                config_hash: "hash",
                scrollback: &self.scrollback,
                subscribers: &self.subscribers,
                stdin_writer: self.stdin_writer.clone(),
                stop_requested: &self.stop_requested,
                force_stop: &self.force_stop,
                running: &self.running,
                next_subscriber_id: &self.next_subscriber_id,
                secrets: &self.secrets,
                volumes: &self.volumes,
                exec_session: &self.exec_session,
                active_waiters: &self.active_waiters,
            }
        }

        fn write_state(&self, state: crate::supervisor::SandboxState, exit_code: Option<i32>) {
            let s = crate::supervisor::SupervisorState {
                sandbox_name: "t".into(),
                state,
                pid: None,
                exit_code,
                started_at: "now".into(),
                config_hash: "hash".into(),
                exited_at_ms: Some(1234),
                supervisor_pid: None,
            };
            crate::supervisor::write_state(&self.dir, &s);
        }
    }

    impl Drop for TestHarness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().unwrap()
    }

    fn recv_resp(stream: &mut UnixStream) -> Response {
        let mut line = String::new();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[test]
    fn kill_with_unsupported_signal_errors() {
        let h = TestHarness::new();
        let ctx = h.ctx();
        let (mut server, mut client) = pair();
        handle_kill(&mut server, &ctx, Some(99)).unwrap();
        let resp = recv_resp(&mut client);
        assert!(matches!(
            resp,
            Response::Error(e) if e.code == nanosb_protocol::ErrorCode::Unsupported
        ));
        assert!(!h.stop_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn kill_with_sigterm_is_graceful() {
        let h = TestHarness::new();
        let ctx = h.ctx();
        let (mut server, mut client) = pair();
        handle_kill(&mut server, &ctx, Some(libc::SIGTERM)).unwrap();
        assert!(matches!(recv_resp(&mut client), Response::Ok { .. }));
        assert!(h.stop_requested.load(Ordering::SeqCst));
        assert!(!h.force_stop.load(Ordering::SeqCst));
    }

    #[test]
    fn kill_without_signal_force_stops() {
        let h = TestHarness::new();
        let ctx = h.ctx();
        let (mut server, mut client) = pair();
        handle_kill(&mut server, &ctx, None).unwrap();
        assert!(matches!(recv_resp(&mut client), Response::Ok { .. }));
        assert!(h.stop_requested.load(Ordering::SeqCst));
        assert!(h.force_stop.load(Ordering::SeqCst));
    }

    #[test]
    fn wait_on_already_terminal_state_returns_immediately() {
        let h = TestHarness::new();
        h.write_state(crate::supervisor::SandboxState::Stopped, Some(137));
        let ctx = h.ctx();
        let (mut server, mut client) = pair();
        handle_wait(&mut server, &ctx, Some(5)).unwrap();
        let resp = recv_resp(&mut client);
        assert!(matches!(
            resp,
            Response::Wait {
                exit_code: 137,
                exited_at_ms: 1234,
                state: nanosb_protocol::SandboxState::Stopped
            }
        ));
        assert_eq!(h.active_waiters.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn wait_times_out_when_not_terminal() {
        let h = TestHarness::new();
        h.write_state(crate::supervisor::SandboxState::Running, None);
        let ctx = h.ctx();
        let (mut server, mut client) = pair();
        handle_wait(&mut server, &ctx, Some(1)).unwrap();
        let resp = recv_resp(&mut client);
        assert!(matches!(
            resp,
            Response::Error(e) if e.code == nanosb_protocol::ErrorCode::Timeout
        ));
        assert_eq!(h.active_waiters.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn jsonl_logs_preserve_per_line_source() {
        let dir = std::env::temp_dir().join(format!("nsb-logsrc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("console.jsonl");
        std::fs::write(
            &path,
            "{\"cursor\":0,\"source\":\"stdout\",\"data\":\"out\"}\n\
             {\"cursor\":1,\"source\":\"stderr\",\"data\":\"err\"}\n",
        )
        .unwrap();
        let logs = read_jsonl_logs(&path);
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].source, LogSource::Stdout);
        assert_eq!(logs[1].source, LogSource::Stderr);
        assert_eq!(logs[1].data, "err");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_source_name_roundtrips() {
        for s in [
            LogSource::Stdout,
            LogSource::Stderr,
            LogSource::Pty,
            LogSource::System,
        ] {
            assert_eq!(log_source_from_name(log_source_name(s)), s);
        }
    }

    #[test]
    fn apply_modify_writes_mutable_fields() {
        let dir = std::env::temp_dir().join(format!("nsb-modify-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("config.json"),
            r#"{"name":"t","image":"alpine","labels":{"a":"1"}}"#,
        )
        .unwrap();

        let mut spec = ModifySpec::default();
        spec.idle_timeout_secs = Some(120);
        spec.max_duration_secs = Some(3600);
        spec.labels.insert("b".to_string(), "2".to_string());
        apply_modify(&dir, &spec).unwrap();

        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(cfg["idle_timeout_secs"], 120);
        assert_eq!(cfg["max_duration_secs"], 3600);
        assert_eq!(cfg["labels"]["a"], "1");
        assert_eq!(cfg["labels"]["b"], "2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_modify_preserves_other_fields() {
        let dir = std::env::temp_dir().join(format!("nsb-modify2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("config.json"),
            r#"{"name":"t","image":"alpine","cpus":2}"#,
        )
        .unwrap();

        let mut spec = ModifySpec::default();
        spec.labels.insert("env".to_string(), "prod".to_string());
        apply_modify(&dir, &spec).unwrap();

        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(cfg["name"], "t");
        assert_eq!(cfg["image"], "alpine");
        assert_eq!(cfg["cpus"], 2);
        assert_eq!(cfg["labels"]["env"], "prod");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fs_write_stream_streams_chunks_to_guest() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("nsb-fsws-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // Fake guest exec agent on the sandbox's exec socket.
        let exec_sock = dir.join("exec.sock");
        let listener = UnixListener::bind(&exec_sock).unwrap();
        let agent = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
            s.read_exact(&mut buf).unwrap();
            let open: serde_json::Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(open["type"], "fs_write_stream_open");
            assert_eq!(open["path"], "/tmp/guest.bin");
            let resp = serde_json::to_vec(&serde_json::json!({"type":"ok"})).unwrap();
            s.write_all(&(resp.len() as u32).to_le_bytes()).unwrap();
            s.write_all(&resp).unwrap();
            s.flush().unwrap();

            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
            s.read_exact(&mut buf).unwrap();
            let chunk: serde_json::Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(chunk["type"], "fs_write_chunk");
            let data =
                nanosb_protocol::base64_decode(chunk["data_base64"].as_str().unwrap()).unwrap();
            assert_eq!(data, b"hello ");
            let resp = serde_json::to_vec(&serde_json::json!({"type":"ok"})).unwrap();
            s.write_all(&(resp.len() as u32).to_le_bytes()).unwrap();
            s.write_all(&resp).unwrap();
            s.flush().unwrap();

            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
            s.read_exact(&mut buf).unwrap();
            let chunk: serde_json::Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(chunk["type"], "fs_write_chunk");
            let data =
                nanosb_protocol::base64_decode(chunk["data_base64"].as_str().unwrap()).unwrap();
            assert_eq!(data, b"world");
            let resp = serde_json::to_vec(&serde_json::json!({"type":"ok"})).unwrap();
            s.write_all(&(resp.len() as u32).to_le_bytes()).unwrap();
            s.write_all(&resp).unwrap();
            s.flush().unwrap();

            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
            s.read_exact(&mut buf).unwrap();
            let close: serde_json::Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(close["type"], "fs_write_close");
            let resp = serde_json::to_vec(&serde_json::json!({"type":"ok"})).unwrap();
            s.write_all(&(resp.len() as u32).to_le_bytes()).unwrap();
            s.write_all(&resp).unwrap();
            s.flush().unwrap();
        });

        let scrollback = Arc::new(Mutex::new(ScrollbackRing::new(1024)));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let (_, stdin_writer) = std::io::pipe().unwrap();
        let stop_requested = AtomicBool::new(false);
        let force_stop = AtomicBool::new(false);
        let running = AtomicBool::new(true);
        let next_subscriber_id = Arc::new(AtomicU64::new(0));
        let secrets = SecretStore::new();
        let volumes = VolumeRegistry::with_root(dir.join("volumes"));
        let exec_session = Mutex::new(None::<ExecHandle>);
        let active_waiters = AtomicUsize::new(0);
        let ctx = Ctx {
            sandbox_dir: &dir,
            sandbox_name: "t",
            started_at: "now",
            config_hash: "hash",
            scrollback: &scrollback,
            subscribers: &subscribers,
            stdin_writer: Arc::new(Mutex::new(stdin_writer)),
            stop_requested: &stop_requested,
            force_stop: &force_stop,
            running: &running,
            next_subscriber_id: &next_subscriber_id,
            secrets: &secrets,
            volumes: &volumes,
            exec_session: &exec_session,
            active_waiters: &active_waiters,
        };

        let (server, client) = UnixStream::pair().unwrap();
        let server_clone = server.try_clone().unwrap();
        let reader = BufReader::new(server_clone);
        let hello_server = HelloRequest::new("test", PROTOCOL_VERSION);

        let mut client = client;
        let mut client_reader = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        let send_req = |client: &mut UnixStream, req: &Request| {
            client
                .write_all(serde_json::to_string(req).unwrap().as_bytes())
                .unwrap();
            client.write_all(b"\n").unwrap();
            client.flush().unwrap();
        };
        let recv_resp =
            |client_reader: &mut BufReader<UnixStream>, line: &mut String| -> Response {
                line.clear();
                client_reader.read_line(line).unwrap();
                serde_json::from_str(line.trim()).unwrap()
            };

        std::thread::scope(|scope| {
            scope.spawn(move || {
                serve(server, reader, &ctx, hello_server);
            });

            // `serve` answers the handshake immediately (the caller consumed
            // the hello frame), so the client only reads the HelloOk.
            assert!(matches!(
                recv_resp(&mut client_reader, &mut line),
                Response::HelloOk(_)
            ));

            send_req(
                &mut client,
                &Request::FsWriteStream(FsWriteStreamSpec {
                    name: None,
                    path: "/tmp/guest.bin".to_string(),
                    append: false,
                    mode: None,
                }),
            );
            assert!(matches!(
                recv_resp(&mut client_reader, &mut line),
                Response::Ok { .. }
            ));

            send_req(
                &mut client,
                &Request::FsData {
                    data_base64: nanosb_protocol::base64_encode(b"hello "),
                    eof: false,
                },
            );
            assert!(matches!(
                recv_resp(&mut client_reader, &mut line),
                Response::Ok { .. }
            ));

            send_req(
                &mut client,
                &Request::FsData {
                    data_base64: nanosb_protocol::base64_encode(b"world"),
                    eof: true,
                },
            );
            assert!(matches!(
                recv_resp(&mut client_reader, &mut line),
                Response::Ok { .. }
            ));

            drop(client);
            drop(client_reader);
        });
        agent.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
