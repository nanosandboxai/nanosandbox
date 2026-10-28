//! `nanosb-runtime` — the slim protocol helper.
//!
//! A long-lived daemon that owns the sandbox control plane. It listens on a
//! Unix socket (`$HOME/.nanosandbox/helper.sock` by default) and serves the
//! `nanosb-protocol` verb set. Control-plane verbs (create/list/get/remove/…)
//! are handled here directly; sandbox-scoped verbs (exec/fs/logs/attach) are
//! served by that sandbox's supervisor over its own control socket, which this
//! daemon proxies verbatim.

use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use nanosb_protocol::{
    read_message, write_message, Capability, ErrorCode, HelloResponse, ProtocolError, Request,
    Response, SandboxInfo, SandboxList, PROTOCOL_VERSION,
};

const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn send(writer: &mut UnixStream, resp: &Response) -> anyhow::Result<()> {
    write_message(writer, resp)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("internal-boot-vm") {
        sandbox::handle_boot_vm_subprocess();
    }

    if args.get(1).map(String::as_str) == Some("__supervise") {
        let sandbox_name = args
            .get(2)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("__supervise requires sandbox_name"))?;
        let config_json = args
            .get(3)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("__supervise requires config_json"))?;
        let extra_mounts_json =
            named_arg(&args, "--extra-mounts-json").unwrap_or_else(|| "[]".into());
        let timeout_secs = named_arg(&args, "--timeout-secs")
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600);
        let foreground = args.iter().any(|a| a == "--foreground");
        nanosb_cli::supervisor::run_supervisor(nanosb_cli::supervisor::SuperviseArgs {
            sandbox_name,
            config_json,
            extra_mounts_json,
            timeout_secs,
            foreground,
        });
    }

    let socket = if let Some(path) = named_arg(&args, "--socket") {
        PathBuf::from(path)
    } else {
        match args.get(1).map(String::as_str) {
            Some("serve") | None => helper_socket(),
            Some(other) => {
                eprintln!("nanosb-runtime: unknown command '{other}'");
                eprintln!("usage: nanosb-runtime serve [--socket PATH]");
                std::process::exit(2);
            }
        }
    };
    serve(&socket)
}

fn named_arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn nanosb_home() -> PathBuf {
    if let Ok(dir) = std::env::var("NANOSB_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".nanosandbox")
}

fn helper_socket() -> PathBuf {
    nanosb_home().join(nanosb_protocol::HELPER_SOCKET_FILE)
}

fn sandboxes_dir() -> PathBuf {
    nanosb_home().join("sandboxes")
}

fn capabilities() -> Vec<Capability> {
    vec![
        Capability::Lifecycle,
        Capability::Exec,
        Capability::Fs,
        Capability::Logs,
        Capability::Metrics,
        Capability::Networking,
        Capability::Handles,
        Capability::Ephemeral,
        Capability::Secrets,
        Capability::Volumes,
        Capability::Snapshots,
        Capability::Docker,
        Capability::DockerBridge,
    ]
}

fn serve(socket: &Path) -> anyhow::Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    }
    eprintln!("nanosb-runtime: listening on {}", socket.display());
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(e) = handle(stream) {
                        eprintln!("nanosb-runtime: connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("nanosb-runtime: accept error: {e}"),
        }
    }
    Ok(())
}

fn handle(stream: UnixStream) -> anyhow::Result<()> {
    let _ = stream.set_nonblocking(false);
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut scratch = String::new();

    loop {
        let req: Request = match read_message(&mut reader, &mut scratch) {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(()),
            Err(e) => {
                let _ = write_message(&mut writer, &Response::Error(e));
                return Ok(());
            }
        };

        if let Request::Hello(hello) = req {
            if !hello.accepts(PROTOCOL_VERSION) {
                write_message(
                    &mut writer,
                    &Response::Error(ProtocolError::new(
                        ErrorCode::IncompatibleVersion,
                        format!(
                            "client range {}..{} incompatible",
                            hello.protocol_min, hello.protocol_max
                        ),
                    )),
                )?;
                return Ok(());
            }
            let ok = Response::HelloOk(HelloResponse {
                protocol: PROTOCOL_VERSION.to_string(),
                helper_version: HELPER_VERSION.to_string(),
                capabilities: capabilities(),
            });
            write_message(&mut writer, &ok)?;
            continue;
        }

        if let Err(e) = dispatch(&mut writer, req) {
            let _ = write_message(
                &mut writer,
                &Response::Error(ProtocolError::internal(e.to_string())),
            );
        }
    }
}

fn dispatch(writer: &mut UnixStream, req: Request) -> anyhow::Result<()> {
    match req {
        Request::Hello(_) => Ok(()),
        Request::Ping => send(
            writer,
            &Response::Pong {
                version: HELPER_VERSION.to_string(),
            },
        ),
        Request::List {
            all,
            limit,
            cursor,
            label,
        } => {
            let (sandboxes, next_cursor) =
                list_sandboxes(all, label.as_deref(), limit, cursor.as_deref());
            send(
                writer,
                &Response::List(SandboxList {
                    sandboxes,
                    next_cursor,
                }),
            )
        }
        Request::Get { name } => match sandbox_info(&name) {
            Some(info) => send(writer, &Response::Sandbox(info)),
            None => send(writer, &Response::Error(not_found(&name))),
        },
        Request::Create(spec) => create(writer, spec.spec),
        Request::Start { name } => {
            proxy(writer, require_name(name)?, Request::Start { name: None })
        }
        Request::Status { name } => {
            proxy(writer, require_name(name)?, Request::Status { name: None })
        }
        Request::Stop { name, force } => proxy(
            writer,
            require_name(name)?,
            Request::Stop { name: None, force },
        ),
        Request::Kill { name, signal } => proxy(
            writer,
            require_name(name)?,
            Request::Kill { name: None, signal },
        ),
        Request::Drain { name } => {
            proxy(writer, require_name(name)?, Request::Drain { name: None })
        }
        Request::Touch { name } => {
            proxy(writer, require_name(name)?, Request::Touch { name: None })
        }
        Request::Pause { name } => {
            proxy(writer, require_name(name)?, Request::Pause { name: None })
        }
        Request::Resume { name } => {
            proxy(writer, require_name(name)?, Request::Resume { name: None })
        }
        Request::Modify { name, spec } => proxy(
            writer,
            require_name(name)?,
            Request::Modify { name: None, spec },
        ),
        Request::Remove { name, force } => remove_sandbox(writer, &require_name(name)?, force),
        Request::Exec(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::Exec(s))
        }
        Request::ExecStream(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::ExecStream(s))
        }
        Request::ExecDefault(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::ExecDefault(s))
        }
        Request::Shell(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::Shell(s))
        }
        Request::Attach { name } => {
            proxy(writer, require_name(name)?, Request::Attach { name: None })
        }
        Request::AttachDefault { name } => proxy(
            writer,
            require_name(name)?,
            Request::AttachDefault { name: None },
        ),
        Request::Logs(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::Logs(s))
        }
        Request::Metrics { name } => {
            proxy(writer, require_name(name)?, Request::Metrics { name: None })
        }
        Request::MetricsStream { name, interval_ms } => proxy(
            writer,
            require_name(name)?,
            Request::MetricsStream {
                name: None,
                interval_ms,
            },
        ),
        Request::FsRead(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsRead(s))
        }
        Request::FsReadToString(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsReadToString(s))
        }
        Request::FsWrite(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsWrite(s))
        }
        Request::FsWriteStream(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsWriteStream(s))
        }
        Request::FsList(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsList(s))
        }
        Request::FsStat(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsStat(s))
        }
        Request::FsExists(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsExists(s))
        }
        Request::FsMkdir(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsMkdir(s))
        }
        Request::FsRemove(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsRemove(s))
        }
        Request::FsRemoveDir(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsRemoveDir(s))
        }
        Request::FsCopy(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsCopy(s))
        }
        Request::FsRename(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsRename(s))
        }
        Request::FsCopyFromHost(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsCopyFromHost(s))
        }
        Request::FsCopyToHost(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::FsCopyToHost(s))
        }
        Request::SecretSet(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::SecretSet(s))
        }
        Request::SecretList { name } => proxy(
            writer,
            require_name(name)?,
            Request::SecretList { name: None },
        ),
        Request::SecretRemove(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::SecretRemove(s))
        }
        Request::VolumeCreate(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::VolumeCreate(s))
        }
        Request::VolumeList { name } => proxy(
            writer,
            require_name(name)?,
            Request::VolumeList { name: None },
        ),
        Request::VolumeGet(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::VolumeGet(s))
        }
        Request::VolumeRemove(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::VolumeRemove(s))
        }
        Request::MountAdd(mut s) => {
            let n = require_name(s.name.clone())?;
            s.name = None;
            proxy(writer, n, Request::MountAdd(s))
        }
        Request::SnapshotCreate(spec) => snapshot_create(writer, spec),
        Request::SnapshotList => snapshot_list(writer),
        Request::SnapshotRemove { snapshot } => snapshot_remove(writer, &snapshot),
        Request::SnapshotRestore(spec) | Request::SnapshotFork(spec) => {
            snapshot_materialize(writer, spec)
        }
        other => send(
            writer,
            &Response::Error(ProtocolError::unsupported(format!(
                "helper does not serve {other:?}"
            ))),
        ),
    }
}

/// Fail-closed OCI security-context check (Epic 9 C6 / #162): name the first
/// security-sensitive field this build cannot enforce in-guest, if any.
fn security_rejection(spec: &nanosb_protocol::SandboxSpec) -> Option<ProtocolError> {
    spec.security
        .as_ref()
        .and_then(|s| s.unenforced_field())
        .map(|field| {
            ProtocolError::unsupported(format!(
                "security context field '{field}' is unsupported: this build enforces no in-guest security context"
            ))
        })
}

fn create(writer: &mut UnixStream, spec: nanosb_protocol::SandboxSpec) -> anyhow::Result<()> {
    // This handler is the single central entry point where a protocol
    // `SandboxSpec` becomes a boot; the CLI/supervisor create path
    // (`cmd_run_next`/`cmd_apply`, `deploy_plan_for`) builds a
    // `runtime::config::SandboxConfig` directly and has no `SandboxSpec` surface.
    if let Some(err) = security_rejection(&spec) {
        return send(writer, &Response::Error(err));
    }
    let name = match spec.name.clone() {
        Some(n) => n,
        None => {
            return send(
                writer,
                &Response::Error(ProtocolError::bad_request("spec.name is required")),
            )
        }
    };
    let dir = sandboxes_dir().join(&name);
    if dir.join(nanosb_protocol::STATE_FILE).exists() {
        if !spec.replace {
            return send(
                writer,
                &Response::Error(ProtocolError::invalid_state(format!(
                    "sandbox '{name}' already exists (set replace=true)"
                ))),
            );
        }
        stop_sandbox(&name);
        let _ = std::fs::remove_dir_all(&dir);
    }
    nanosb_cli::helper::create_sandbox(&spec)?;
    send(
        writer,
        &Response::Ok {
            version: HELPER_VERSION.to_string(),
        },
    )
}

fn require_name(name: Option<String>) -> anyhow::Result<String> {
    name.ok_or_else(|| anyhow::anyhow!("sandbox name is required for this verb"))
}

fn not_found(name: &str) -> ProtocolError {
    ProtocolError::not_found(format!("sandbox '{name}' not found"))
}

fn snapshot_create(
    writer: &mut UnixStream,
    spec: nanosb_protocol::SnapshotCreateSpec,
) -> anyhow::Result<()> {
    let store = nanosb_cli::supervisor::snapshots::SnapshotStore::new();
    let name = require_name(spec.name.clone())?;
    let dir = sandboxes_dir().join(&name);
    if !dir.exists() {
        return send(writer, &Response::Error(not_found(&name)));
    }
    let image = std::fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("image").and_then(|i| i.as_str()).map(String::from));
    match store.create(&dir, &name, image, spec.dest) {
        Ok(info) => send(writer, &Response::Snapshot(info)),
        Err(e) => send(writer, &Response::Error(e.to_protocol())),
    }
}

fn snapshot_list(writer: &mut UnixStream) -> anyhow::Result<()> {
    let store = nanosb_cli::supervisor::snapshots::SnapshotStore::new();
    send(
        writer,
        &Response::Snapshots {
            snapshots: store.list(),
        },
    )
}

fn snapshot_remove(writer: &mut UnixStream, id: &str) -> anyhow::Result<()> {
    let store = nanosb_cli::supervisor::snapshots::SnapshotStore::new();
    match store.remove(id) {
        Ok(()) => send(
            writer,
            &Response::Ok {
                version: HELPER_VERSION.to_string(),
            },
        ),
        Err(e) => send(writer, &Response::Error(e.to_protocol())),
    }
}

fn snapshot_materialize(
    writer: &mut UnixStream,
    spec: nanosb_protocol::SnapshotRestoreSpec,
) -> anyhow::Result<()> {
    let store = nanosb_cli::supervisor::snapshots::SnapshotStore::new();
    let new_dir = sandboxes_dir().join(&spec.name);
    match store.materialize(&spec.snapshot, &spec.name, &new_dir) {
        Ok(_) => send(
            writer,
            &Response::Ok {
                version: HELPER_VERSION.to_string(),
            },
        ),
        Err(e) => send(writer, &Response::Error(e.to_protocol())),
    }
}

fn sandbox_facing_socket(name: &str) -> PathBuf {
    sandboxes_dir()
        .join(name)
        .join(nanosb_protocol::CONTROL_SOCKET_FILE)
}

/// Forward a sandbox-scoped request to the target sandbox's control socket and
/// relay every response frame back verbatim.
fn proxy(writer: &mut UnixStream, name: String, req: Request) -> anyhow::Result<()> {
    let socket = sandbox_facing_socket(&name);
    let stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(e) => {
            return send(
                writer,
                &Response::Error(ProtocolError::invalid_state(format!(
                    "sandbox '{name}' is not running ({e})"
                ))),
            )
        }
    };
    let mut upstream_writer = stream.try_clone()?;
    let mut upstream_reader = BufReader::new(stream);
    let mut scratch = String::new();
    write_message(
        &mut upstream_writer,
        &Request::Hello(nanosb_protocol::HelloRequest::new(
            "nanosb-runtime",
            PROTOCOL_VERSION,
        )),
    )?;
    let _hello_ok = read_message::<_, Response>(&mut upstream_reader, &mut scratch)?;
    write_message(&mut upstream_writer, &req)?;
    let streaming = request_streams(&req);
    let mut first = true;
    while let Some(resp) = read_message::<_, Response>(&mut upstream_reader, &mut scratch)? {
        let terminal = if streaming {
            is_stream_terminal(&resp)
        } else {
            first
        };
        first = false;
        write_message(writer, &resp)?;
        if terminal {
            break;
        }
    }
    Ok(())
}

/// A request that yields a multi-frame stream (an opening `ok` ack followed by
/// events until a terminal frame), as opposed to a single response frame.
fn request_streams(req: &Request) -> bool {
    matches!(
        req,
        Request::ExecStream(_)
            | Request::Attach { .. }
            | Request::MetricsStream { .. }
            | Request::FsWriteStream(_)
            | Request::Logs(nanosb_protocol::LogsSpec { follow: true, .. })
    )
}

/// The frame that ends a streamed exchange. The opening `ok` ack is NOT
/// terminal — treating it as terminal truncates every streamed verb.
fn is_stream_terminal(resp: &Response) -> bool {
    matches!(
        resp,
        Response::Guest(nanosb_protocol::GuestEvent::Exit { .. })
            | Response::Guest(nanosb_protocol::GuestEvent::Error { .. })
            | Response::Event(nanosb_protocol::StreamEvent::Done)
            | Response::Error(_)
    )
}

fn list_sandboxes(
    all: bool,
    label: Option<&str>,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> (Vec<SandboxInfo>, Option<String>) {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sandboxes_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(info) = sandbox_info(&name) {
                if !all && !matches!(info.state, nanosb_protocol::SandboxState::Running) {
                    continue;
                }
                if let Some(sel) = label {
                    if !selector_matches(&info.labels, sel) {
                        continue;
                    }
                }
                out.push(info);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    let start = match cursor {
        Some(c) => out
            .iter()
            .position(|s| s.name.as_str() > c)
            .unwrap_or(out.len()),
        None => 0,
    };
    let mut page: Vec<SandboxInfo> = out[start..].to_vec();
    let next_cursor = match limit {
        Some(n) if page.len() > n => {
            page.truncate(n);
            page.last().map(|s| s.name.clone())
        }
        _ => None,
    };
    (page, next_cursor)
}

fn selector_matches(labels: &std::collections::HashMap<String, String>, selector: &str) -> bool {
    for clause in selector.split(',') {
        let (k, v) = clause.split_once('=').unwrap_or((clause, ""));
        match labels.get(k.trim()) {
            Some(val) if v.is_empty() || val == v.trim() => {}
            _ => return false,
        }
    }
    true
}

fn sandbox_info(name: &str) -> Option<SandboxInfo> {
    let dir = sandboxes_dir().join(name);
    let state_raw = std::fs::read_to_string(dir.join(nanosb_protocol::STATE_FILE)).ok()?;
    let state: serde_json::Value = serde_json::from_str(&state_raw).ok()?;
    let config: serde_json::Value = std::fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let state_enum = match state
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("error")
    {
        "starting" => nanosb_protocol::SandboxState::Starting,
        "running" => nanosb_protocol::SandboxState::Running,
        "stopped" => nanosb_protocol::SandboxState::Stopped,
        _ => nanosb_protocol::SandboxState::Error,
    };
    let labels: std::collections::HashMap<String, String> = config
        .get("labels")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let pid = state.get("pid").and_then(|v| v.as_i64()).map(|v| v as i32);
    let live = matches!(state_enum, nanosb_protocol::SandboxState::Running)
        && pid.map(|p| p > 0).unwrap_or(false);
    Some(SandboxInfo {
        name: name.to_string(),
        state: state_enum,
        image: config
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        pid,
        exit_code: state
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        started_at: state
            .get("started_at")
            .and_then(|v| v.as_str())
            .map(String::from),
        config_hash: state
            .get("config_hash")
            .and_then(|v| v.as_str())
            .map(String::from),
        detached: !live,
        ephemeral: config
            .get("ephemeral")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        labels,
    })
}

fn remove_sandbox(writer: &mut UnixStream, name: &str, force: bool) -> anyhow::Result<()> {
    if force {
        stop_sandbox(name);
    }
    let dir = sandboxes_dir().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
        send(
            writer,
            &Response::Ok {
                version: HELPER_VERSION.to_string(),
            },
        )
    } else {
        send(writer, &Response::Error(not_found(name)))
    }
}

fn stop_sandbox(name: &str) {
    let socket = sandbox_facing_socket(name);
    let Ok(stream) = UnixStream::connect(&socket) else {
        return;
    };
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let _ = write_message(
        &mut writer,
        &Request::Stop {
            name: None,
            force: true,
        },
    );
    let mut scratch = String::new();
    let _ = read_message::<_, Response>(&mut reader, &mut scratch);
    for _ in 0..50 {
        if !socket.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::security_rejection;
    use nanosb_protocol::{ErrorCode, SandboxSpec, SecurityContext};

    #[test]
    fn security_rejection_names_the_unenforced_field() {
        let mut spec = SandboxSpec::new("alpine");
        spec.security = Some(SecurityContext {
            seccomp_profile: Some("default".into()),
            ..SecurityContext::default()
        });
        let err = security_rejection(&spec).expect("should reject");
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.message.contains("seccomp_profile"), "{}", err.message);
    }

    #[test]
    fn security_rejection_allows_default_and_run_as_user() {
        let mut spec = SandboxSpec::new("alpine");
        assert!(security_rejection(&spec).is_none());
        spec.security = Some(SecurityContext {
            run_as_user: Some("1000".into()),
            ..SecurityContext::default()
        });
        assert!(security_rejection(&spec).is_none());
    }
}
