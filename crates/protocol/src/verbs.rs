//! The control protocol verb set.
//!
//! `Request` is client -> helper; `Response` is helper -> client. One request
//! yields one immediate `Response`; verbs that stream (exec_stream, attach,
//! logs follow, metrics_stream, fs write/read stream) then emit additional
//! `Response::Guest` / `Response::Event` frames terminated by an exit/done.
//!
//! Sandbox-scoped verbs carry an optional `name`. On a per-sandbox socket the
//! name may be omitted (the socket is already bound to one sandbox); on the
//! global helper socket it is required. This keeps the legacy per-sandbox
//! control frames wire-compatible.

use serde::{Deserialize, Serialize};

use crate::error::ProtocolError;
use crate::hello::{HelloRequest, HelloResponse};
use crate::types::{
    ExecResult, FsEntry, FsStat, GuestEvent, LogLine, LogSource, Metrics, ModifySpec, MountSpec,
    Rlimit, SandboxInfo, SandboxList, SandboxSpec, SandboxState, SecretEntry, SnapshotInfo,
    VolumeInfo, VolumeSpec,
};

/// Client -> helper request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    // ── handshake ──────────────────────────────────────────────────────
    Hello(HelloRequest),
    Ping,

    // ── lifecycle & handles ────────────────────────────────────────────
    Status {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Create(Box<CreateRequest>),
    Start {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        force: bool,
    },
    Kill {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// POSIX signal to deliver. `None` (or `SIGKILL`) force-stops; `SIGTERM`
        /// requests a graceful stop. Any other signal is `unsupported`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
    },
    /// Block until the sandbox reaches a terminal state (`stopped`/`error`).
    Wait {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Max seconds to wait; `None` waits indefinitely.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    Remove {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        force: bool,
    },
    Get {
        name: String,
    },
    List {
        #[serde(default)]
        all: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Modify {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        spec: ModifySpec,
    },
    Drain {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Touch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Pause {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Resume {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },

    // ── execution ──────────────────────────────────────────────────────
    Exec(Box<ExecRequest>),
    ExecStream(Box<ExecRequest>),
    ExecDefault(Box<ExecDefaultRequest>),
    Shell(Box<ShellRequest>),
    Attach {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    AttachDefault {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    AttachInput {
        data: String,
    },
    AttachResize {
        cols: u16,
        rows: u16,
    },
    /// Write raw bytes to the workload's console stdin (PID 1 stdin).
    ConsoleInput {
        data: String,
    },
    /// Close the workload's console stdin (signal EOF, e.g. so `cat` exits).
    ConsoleInputClose,
    /// Body frame for a tty `exec_stream`: live stdin bytes.
    ExecInput {
        data: String,
    },
    /// Body frame for a tty `exec_stream`: PTY resize.
    ExecResize {
        cols: u16,
        rows: u16,
    },

    // ── filesystem ─────────────────────────────────────────────────────
    FsRead(FsPathSpec),
    FsReadToString(FsPathSpec),
    FsWrite(FsWriteSpec),
    FsWriteStream(FsWriteStreamSpec),
    /// Body frame for `FsWriteStream` (base64) or `AttachInput`-style streams.
    FsData {
        data_base64: String,
        eof: bool,
    },
    FsList(FsPathSpec),
    FsStat(FsPathSpec),
    FsExists(FsPathSpec),
    FsMkdir(FsMkdirSpec),
    FsRemove(FsPathSpec),
    FsRemoveDir(FsRemoveDirSpec),
    FsCopy(FsCopySpec),
    FsRename(FsCopySpec),
    FsCopyFromHost(FsTransferSpec),
    FsCopyToHost(FsTransferSpec),

    // ── observability ──────────────────────────────────────────────────
    Logs(LogsSpec),
    Metrics {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    MetricsStream {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
    },

    // ── storage / volumes ──────────────────────────────────────────────
    VolumeCreate(VolumeCreateSpec),
    VolumeList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    VolumeGet(VolumeRef),
    VolumeRemove(VolumeRef),
    /// Mount a volume into a running sandbox.
    MountAdd(MountAddSpec),

    // ── secrets ────────────────────────────────────────────────────────
    SecretSet(SecretSetSpec),
    SecretList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    SecretRemove(SecretRef),

    // ── snapshots / pause-resume ───────────────────────────────────────
    SnapshotCreate(SnapshotCreateSpec),
    SnapshotList,
    SnapshotRestore(SnapshotRestoreSpec),
    SnapshotFork(SnapshotRestoreSpec),
    SnapshotRemove {
        snapshot: String,
    },
}

/// Helper -> client response. Exactly one non-stream `Response` answers each
/// request; streaming verbs then emit `Guest`/`Event` frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    // handshake
    HelloOk(HelloResponse),
    Pong {
        version: String,
    },

    // generic acks (wire-compatible with the legacy `ok`)
    Ok {
        version: String,
    },
    Error(ProtocolError),

    // lifecycle
    Status {
        state: SandboxState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_hash: Option<String>,
    },
    /// Answer to `wait`: the terminal exit code, when the sandbox exited, and
    /// the terminal state.
    Wait {
        exit_code: i32,
        exited_at_ms: u64,
        state: SandboxState,
    },
    Sandbox(SandboxInfo),
    List(SandboxList),

    // execution
    ExecResult(ExecResult),

    // filesystem
    FsData {
        data_base64: String,
        eof: bool,
    },
    FsContent {
        content: String,
    },
    FsEntries {
        entries: Vec<FsEntry>,
    },
    FsStat(FsStat),
    FsExists {
        exists: bool,
    },

    // observability
    Metrics(Metrics),
    Logs {
        lines: Vec<LogLine>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_cursor: Option<u64>,
    },

    // storage / secrets
    Volumes {
        volumes: Vec<VolumeInfo>,
    },
    Volume(VolumeInfo),
    Secrets {
        secrets: Vec<SecretEntry>,
    },

    // snapshots
    Snapshot(SnapshotInfo),
    Snapshots {
        snapshots: Vec<SnapshotInfo>,
    },

    // streaming
    /// A proxied guest event (exec / attach).
    Guest(GuestEvent),
    /// A generic stream event (logs follow, metrics_stream).
    Event(StreamEvent),
}

/// A generic streaming event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    Log(LogLine),
    Metrics(Metrics),
    /// The stream ended cleanly.
    Done,
}

/// The `create` request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateRequest {
    pub spec: SandboxSpec,
}

/// A fully-qualified execution request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub shell: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default)]
    pub rlimits: Vec<Rlimit>,
    /// Allocate a PTY for the process (interactive programs).
    #[serde(default)]
    pub tty: bool,
    /// Initial PTY columns when `tty` is set.
    #[serde(default)]
    pub cols: u16,
    /// Initial PTY rows when `tty` is set.
    #[serde(default)]
    pub rows: u16,
}

impl ExecRequest {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            name: None,
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: std::collections::HashMap::new(),
            user: None,
            shell: false,
            timeout_secs: None,
            stdin: None,
            rlimits: Vec::new(),
            tty: false,
            cols: 0,
            rows: 0,
        }
    }
}

/// Execute the sandbox's configured default workload, with optional overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecDefaultRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Extra arguments appended to the default command.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub rlimits: Vec<Rlimit>,
}

impl Default for ExecDefaultRequest {
    fn default() -> Self {
        Self {
            name: None,
            args: Vec::new(),
            cwd: None,
            env: std::collections::HashMap::new(),
            user: None,
            timeout_secs: None,
            rlimits: Vec::new(),
        }
    }
}

/// A shell-script execution request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub script: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// A path-scoped filesystem request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsPathSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
}

/// A buffered write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWriteSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
    pub data_base64: String,
    #[serde(default)]
    pub append: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// The opening frame of a streamed write; `FsData` frames follow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWriteStreamSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
    #[serde(default)]
    pub append: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// mkdir with recursion + mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsMkdirSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// rmdir with recursion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsRemoveDirSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
}

/// copy / rename inside the guest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsCopySpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub from: String,
    pub to: String,
}

/// host <-> guest transfer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsTransferSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub host_path: String,
    pub guest_path: String,
}

/// Logs query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogsSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub follow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<LogSource>,
}

/// Create a named volume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeCreateSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub volume: VolumeSpec,
}

/// Reference a named volume within a sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub volume: String,
}

/// Mount an existing volume into a running sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountAddSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub mount: MountSpec,
}

/// Set a named secret (value is never persisted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretSetSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub key: String,
    pub value: String,
}

/// Reference a named secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub key: String,
}

/// Create a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotCreateSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
    #[serde(default)]
    pub include_memory: bool,
}

/// Restore or fork from a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRestoreSpec {
    pub snapshot: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(req: &Request) -> Request {
        let s = serde_json::to_string(req).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn roundtrip_resp(resp: &Response) -> Response {
        let s = serde_json::to_string(resp).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn legacy_status_frame_still_parses() {
        // The legacy CLI sends `{"type":"status"}` with no name.
        let req: Request = serde_json::from_str("{\"type\":\"status\"}").unwrap();
        assert_eq!(req, Request::Status { name: None });
    }

    #[test]
    fn legacy_kill_frame_still_parses() {
        // A kill frame written before the `signal` field must still decode.
        let req: Request = serde_json::from_str("{\"type\":\"kill\"}").unwrap();
        assert_eq!(
            req,
            Request::Kill {
                name: None,
                signal: None
            }
        );
        let req: Request =
            serde_json::from_str("{\"type\":\"kill\",\"name\":\"box\"}").unwrap();
        assert_eq!(
            req,
            Request::Kill {
                name: Some("box".into()),
                signal: None
            }
        );
    }

    #[test]
    fn kill_with_signal_roundtrips() {
        let req = Request::Kill {
            name: Some("box".into()),
            signal: Some(15),
        };
        assert_eq!(roundtrip(&req), req);
        // The signal is omitted when None (additive wire compatibility).
        let json = serde_json::to_string(&Request::Kill {
            name: None,
            signal: None,
        })
        .unwrap();
        assert!(!json.contains("signal"), "{json}");
    }

    #[test]
    fn wait_request_roundtrips() {
        for req in [
            Request::Wait {
                name: None,
                timeout_secs: None,
            },
            Request::Wait {
                name: Some("box".into()),
                timeout_secs: Some(30),
            },
        ] {
            assert_eq!(roundtrip(&req), req);
        }
        let req: Request = serde_json::from_str("{\"type\":\"wait\"}").unwrap();
        assert_eq!(
            req,
            Request::Wait {
                name: None,
                timeout_secs: None
            }
        );
    }

    #[test]
    fn exec_input_and_resize_roundtrip() {
        for req in [
            Request::ExecInput {
                data: "ls\n".into(),
            },
            Request::ExecResize { cols: 120, rows: 40 },
        ] {
            assert_eq!(roundtrip(&req), req);
        }
    }

    #[test]
    fn exec_request_tty_fields_roundtrip_and_default() {
        let mut e = ExecRequest::new("cat");
        e.tty = true;
        e.cols = 120;
        e.rows = 40;
        assert_eq!(roundtrip(&Request::Exec(Box::new(e))), Request::Exec(Box::new(
            ExecRequest {
                name: None,
                command: "cat".into(),
                args: Vec::new(),
                cwd: None,
                env: std::collections::HashMap::new(),
                user: None,
                shell: false,
                timeout_secs: None,
                stdin: None,
                rlimits: Vec::new(),
                tty: true,
                cols: 120,
                rows: 40,
            }
        )));
        // A legacy exec frame without tty/cols/rows must decode with defaults.
        let legacy = r#"{"type":"exec","command":"echo","args":[]}"#;
        let req: Request = serde_json::from_str(legacy).unwrap();
        let Request::Exec(spec) = req else {
            panic!("expected exec");
        };
        assert!(!spec.tty);
        assert_eq!(spec.cols, 0);
        assert_eq!(spec.rows, 0);
    }

    #[test]
    fn wait_response_roundtrips() {
        let resp = Response::Wait {
            exit_code: 137,
            exited_at_ms: 1234,
            state: SandboxState::Stopped,
        };
        assert_eq!(roundtrip_resp(&resp), resp);
    }

    #[test]
    fn legacy_ping_and_ok_and_error() {
        assert_eq!(
            serde_json::from_str::<Request>("{\"type\":\"ping\"}").unwrap(),
            Request::Ping
        );
        let ok: Response = serde_json::from_str("{\"type\":\"ok\",\"version\":\"1.0.0\"}").unwrap();
        assert_eq!(
            ok,
            Response::Ok {
                version: "1.0.0".into()
            }
        );
        // Legacy error had only `message`; `code` is a required superset field,
        // so a legacy bare error must still decode with a defaulted code.
        let err: Response =
            serde_json::from_str("{\"type\":\"error\",\"code\":\"internal\",\"message\":\"boom\"}")
                .unwrap();
        assert!(matches!(err, Response::Error(e) if e.message == "boom"));
    }

    #[test]
    fn request_variants_roundtrip() {
        let mut e = ExecRequest::new("echo");
        e.args = vec!["hi".into()];
        e.shell = true;
        for req in [
            Request::Ping,
            Request::Hello(HelloRequest::new("test", "1.0.0")),
            Request::Status { name: None },
            Request::Status {
                name: Some("s".into()),
            },
            Request::Exec(Box::new(e)),
            Request::FsList(FsPathSpec {
                name: None,
                path: "/etc".into(),
                offset: None,
                length: None,
            }),
            Request::SnapshotList,
        ] {
            assert_eq!(roundtrip(&req), req);
        }
    }

    #[test]
    fn response_variants_roundtrip() {
        for resp in [
            Response::Pong {
                version: "1".into(),
            },
            Response::FsExists { exists: true },
            Response::ExecResult(ExecResult {
                exit_code: 0,
                stdout: "x".into(),
                stderr: String::new(),
                duration_ms: 3,
            }),
            Response::Guest(GuestEvent::Stdout { data: "y".into() }),
            Response::Event(StreamEvent::Done),
        ] {
            assert_eq!(roundtrip_resp(&resp), resp);
        }
    }
}
