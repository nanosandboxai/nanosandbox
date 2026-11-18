//! Conformance driver for the reference `nanosb-client`.
//!
//! For each scenario in `sdk/conformance/scenarios/`, spawns the
//! `nanosb-conformance serve` binary (via `CARGO_BIN_EXE_nanosb-conformance`),
//! reads the `READY <socket>` line from its stdout, connects `nanosb-client` to
//! that socket, drives the scripted steps per the driver contract in
//! `sdk/conformance/SPEC.md`, and evaluates the scenario's `assert` block. A
//! scenario passes when the driver completes without an assertion failure.
//!
//! allow: SIZE_OK — a data-driven verb→operation mapping table plus the assert
//! evaluator; splitting it would scatter one driver across files.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use nanosb_client::connection::Connection;
use nanosb_client::{Error, SandboxClient};
use nanosb_conformance::matcher::required_capability;
use nanosb_conformance::Scenario;
use nanosb_protocol::{
    base64_decode, base64_encode, Capability, ErrorCode, ExecRequest, ExecResult, FsEntry, FsStat,
    GuestEvent, LogLine, LogSource, LogsSpec, Metrics, ModifySpec, MountSpec, Request, Response,
    SandboxInfo, SandboxSpec, SandboxState, SecretEntry, SnapshotInfo, StreamEvent, VolumeInfo,
    VolumeSpec,
};
use serde_json::Value;

/// Directory holding the scenario corpus, relative to this crate's manifest.
const SCENARIOS_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdk/conformance/scenarios"
);

/// Accumulated observations from driving a scenario, consumed by the assert
/// evaluator.
#[derive(Default)]
struct DriverState {
    caps: Vec<Capability>,
    errors: Vec<ErrorCode>,
    error_messages: Vec<String>,
    acked: HashSet<&'static str>,
    last_exec: Option<ExecResult>,
    stream_exit: Option<i32>,
    stream_stdout: String,
    stream_stderr: String,
    get_info: Option<SandboxInfo>,
    list_infos: Option<Vec<SandboxInfo>>,
    fs_entries: Option<Vec<FsEntry>>,
    fs_stat: Option<FsStat>,
    fs_exists: Option<bool>,
    fs_read_bytes: Option<Vec<u8>>,
    fs_read_to_string: Option<String>,
    logs_lines: Option<Vec<LogLine>>,
    resume_logs_lines: Option<Vec<LogLine>>,
    follow_logs_lines: Vec<LogLine>,
    next_cursor: Option<u64>,
    metrics: Option<Metrics>,
    metrics_samples: Vec<Metrics>,
    volume_created: Option<String>,
    volumes: Option<Vec<VolumeInfo>>,
    volume_get: Option<VolumeInfo>,
    secrets: Option<Vec<SecretEntry>>,
    snapshot_created: Option<String>,
    snapshots: Option<Vec<SnapshotInfo>>,
    attach_events: Vec<String>,
    attach_exit_code: Option<i32>,
    chunks_acked: usize,
    ping_ok: bool,
    timeout_code: Option<String>,
    wait_exit_code: Option<i32>,
    wait_state: Option<String>,
}

/// Run a client call, recording a protocol error into `state` instead of
/// failing (the `assert` block decides whether an error was expected).
fn drive<T>(
    state: &mut DriverState,
    f: impl FnOnce() -> Result<T, Error>,
) -> Result<Option<T>, String> {
    match f() {
        Ok(v) => Ok(Some(v)),
        Err(Error::Protocol(p)) => {
            state.errors.push(p.code);
            state.error_messages.push(p.message.clone());
            Ok(None)
        }
        Err(other) => Err(format!("unexpected client error: {other}")),
    }
}

/// Drive an ack-style verb, recording the ack on success.
fn ack(
    state: &mut DriverState,
    f: impl FnOnce() -> Result<(), Error>,
    key: &'static str,
) -> Result<(), String> {
    if drive(state, f)?.is_some() {
        state.acked.insert(key);
    }
    Ok(())
}

fn parse_log_source(s: &str) -> LogSource {
    match s {
        "stdout" => LogSource::Stdout,
        "stderr" => LogSource::Stderr,
        "pty" => LogSource::Pty,
        "system" => LogSource::System,
        other => panic!("unknown log source {other}"),
    }
}

fn state_str(s: &SandboxState) -> &'static str {
    match s {
        SandboxState::Starting => "starting",
        SandboxState::Running => "running",
        SandboxState::Stopped => "stopped",
        SandboxState::Error => "error",
    }
}

fn source_str(s: &LogSource) -> &'static str {
    match s {
        LogSource::Stdout => "stdout",
        LogSource::Stderr => "stderr",
        LogSource::Pty => "pty",
        LogSource::System => "system",
    }
}

/// Build an `ExecRequest` from an `exec`/`exec_stream` expect object.
fn exec_request(expect: &Value, name: &str) -> ExecRequest {
    let mut req = ExecRequest::new(expect["command"].as_str().unwrap());
    req.name = Some(name.to_string());
    req.args = expect["args"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default();
    req.cwd = expect["cwd"].as_str().map(String::from);
    req.env = expect["env"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect()
        })
        .unwrap_or_default();
    req.user = expect["user"].as_str().map(String::from);
    req.tty = expect["tty"].as_bool().unwrap_or(false);
    req.cols = expect["cols"].as_u64().unwrap_or(0) as u16;
    req.rows = expect["rows"].as_u64().unwrap_or(0) as u16;
    req
}

/// Drive one step: map `expect.type` to the `nanosb-client` operation and
/// record the outcome into `state`.
fn drive_step(
    expect: &Value,
    client: &SandboxClient,
    state: &mut DriverState,
    server_path: &Path,
) -> Result<(), String> {
    let ty = expect["type"].as_str().ok_or("step has no type")?;
    let name = expect["name"].as_str();
    let sb = name.map(|n| client.sandbox(n));

    match ty {
        "create" => {
            let spec: SandboxSpec = serde_json::from_value(expect["spec"].clone())
                .map_err(|e| format!("bad create spec: {e}"))?;
            if drive(state, || client.create(spec))?.is_some() {
                state.acked.insert("create_acked");
            }
        }
        "get" => {
            if let Some(info) = drive(state, || client.get(name.unwrap()))? {
                state.get_info = Some(info);
            }
        }
        "list" => {
            let all = expect["all"].as_bool().unwrap_or(false);
            if let Some(infos) = drive(state, || client.list(all))? {
                state.list_infos = Some(infos);
            }
        }
        "ping" => {
            if drive(state, || client.ping())?.is_some() {
                state.ping_ok = true;
            }
        }
        "start" => ack(state, || sb.unwrap().start(), "start_acked")?,
        "stop" => {
            let force = expect["force"].as_bool().unwrap_or(false);
            ack(state, || sb.unwrap().stop(force), "stop_acked")?;
        }
        "kill" => {
            if let Some(sig) = expect["signal"].as_i64() {
                ack(state, || sb.unwrap().kill_with(sig as i32), "kill_acked")?;
            } else {
                ack(state, || sb.unwrap().kill(), "kill_acked")?;
            }
        }
        "wait" => {
            let timeout = expect["timeout_secs"]
                .as_u64()
                .map(std::time::Duration::from_secs);
            if let Some((code, _at, st)) = drive(state, || sb.unwrap().wait(timeout))? {
                state.wait_exit_code = Some(code);
                state.wait_state = Some(state_str(&st).to_string());
            }
        }
        "remove" => {
            let force = expect["force"].as_bool().unwrap_or(false);
            ack(state, || sb.unwrap().remove(force), "remove_acked")?;
        }
        "drain" => ack(state, || sb.unwrap().drain(), "drain_acked")?,
        "touch" => ack(state, || sb.unwrap().touch(), "touch_acked")?,
        "pause" => ack(state, || sb.unwrap().pause(), "pause_acked")?,
        "resume" => ack(state, || sb.unwrap().resume(), "resume_acked")?,
        "modify" => {
            let spec: ModifySpec = serde_json::from_value(expect["spec"].clone())
                .map_err(|e| format!("bad modify spec: {e}"))?;
            ack(state, || sb.unwrap().modify(spec), "modify_acked")?;
        }
        "status" => {
            let _ = drive(state, || sb.unwrap().state())?;
        }
        "exec" => {
            let req = exec_request(expect, name.unwrap());
            if let Some(r) = drive(state, || sb.unwrap().exec_with(req))? {
                state.last_exec = Some(r);
            }
        }
        "exec_stream" => {
            let req = exec_request(expect, name.unwrap());
            let mut stdout = String::new();
            let mut stderr = String::new();
            let result = drive(state, || {
                sb.unwrap().exec_stream(req, |ev| match ev {
                    GuestEvent::Stdout { data } => stdout.push_str(&data),
                    GuestEvent::Stderr { data } => stderr.push_str(&data),
                    _ => {}
                })
            });
            if let Some(code) = result? {
                state.stream_exit = Some(code);
                state.stream_stdout = stdout;
                state.stream_stderr = stderr;
            }
        }
        "shell" => {
            let script = expect["script"].as_str().unwrap();
            if let Some(r) = drive(state, || sb.unwrap().shell(script))? {
                state.last_exec = Some(r);
            }
        }
        "attach" => {
            let mut events = Vec::new();
            let mut exit_code = None;
            if drive(state, || {
                sb.unwrap().attach(|ev| match ev {
                    GuestEvent::Started { .. } => events.push("started".to_string()),
                    GuestEvent::Stdout { .. } => events.push("stdout".to_string()),
                    GuestEvent::Stderr { .. } => events.push("stderr".to_string()),
                    GuestEvent::Exit { code } => {
                        events.push("exit".to_string());
                        exit_code = Some(code);
                    }
                    GuestEvent::Error { .. } => events.push("error".to_string()),
                })
            })?
            .is_some()
            {
                state.attach_events = events;
                state.attach_exit_code = exit_code;
            }
        }
        "attach_input" => {
            let data = expect["data"].as_str().unwrap();
            let mut conn = Connection::dial(server_path).map_err(|e| e.to_string())?;
            conn.call(&Request::AttachInput {
                data: data.to_string(),
            })
            .map_err(|e| e.to_string())?;
            state.acked.insert("input_acked");
        }
        "attach_resize" => {
            let cols = expect["cols"].as_u64().unwrap() as u16;
            let rows = expect["rows"].as_u64().unwrap() as u16;
            let mut conn = Connection::dial(server_path).map_err(|e| e.to_string())?;
            conn.call(&Request::AttachResize { cols, rows })
                .map_err(|e| e.to_string())?;
            state.acked.insert("resize_acked");
        }
        "exec_input" => {
            let data = expect["data"].as_str().unwrap();
            let mut conn = Connection::dial(server_path).map_err(|e| e.to_string())?;
            conn.call(&Request::ExecInput {
                data: data.to_string(),
            })
            .map_err(|e| e.to_string())?;
            state.acked.insert("input_acked");
        }
        "exec_resize" => {
            let cols = expect["cols"].as_u64().unwrap() as u16;
            let rows = expect["rows"].as_u64().unwrap() as u16;
            let mut conn = Connection::dial(server_path).map_err(|e| e.to_string())?;
            conn.call(&Request::ExecResize { cols, rows })
                .map_err(|e| e.to_string())?;
            state.acked.insert("resize_acked");
        }
        "fs_read" => {
            let path = expect["path"].as_str().unwrap();
            if let Some(bytes) = drive(state, || sb.unwrap().fs_read(path))? {
                state.fs_read_bytes = Some(bytes);
            }
        }
        "fs_read_to_string" => {
            let path = expect["path"].as_str().unwrap();
            if let Some(s) = drive(state, || sb.unwrap().fs_read_to_string(path))? {
                state.fs_read_to_string = Some(s);
            }
        }
        "fs_write" => {
            let path = expect["path"].as_str().unwrap();
            let data = base64_decode(expect["data_base64"].as_str().unwrap())
                .ok_or("invalid base64 in fs_write")?;
            ack(state, || sb.unwrap().fs_write(path, &data), "write_acked")?;
        }
        "fs_list" => {
            let path = expect["path"].as_str().unwrap();
            if let Some(entries) = drive(state, || sb.unwrap().fs_list(path))? {
                state.fs_entries = Some(entries);
            }
        }
        "fs_stat" => {
            let path = expect["path"].as_str().unwrap();
            if let Some(stat) = drive(state, || sb.unwrap().fs_stat(path))? {
                state.fs_stat = Some(stat);
            }
        }
        "fs_exists" => {
            let path = expect["path"].as_str().unwrap();
            if let Some(exists) = drive(state, || sb.unwrap().fs_exists(path))? {
                state.fs_exists = Some(exists);
            }
        }
        "fs_mkdir" => {
            let path = expect["path"].as_str().unwrap();
            let recursive = expect["recursive"].as_bool().unwrap_or(false);
            ack(
                state,
                || sb.unwrap().fs_mkdir(path, recursive),
                "mkdir_acked",
            )?;
        }
        "fs_remove" => {
            let path = expect["path"].as_str().unwrap();
            ack(state, || sb.unwrap().fs_remove(path), "remove_acked")?;
        }
        "fs_remove_dir" => {
            let path = expect["path"].as_str().unwrap();
            let recursive = expect["recursive"].as_bool().unwrap_or(false);
            ack(
                state,
                || sb.unwrap().fs_remove_dir(path, recursive),
                "remove_dir_acked",
            )?;
        }
        "fs_copy" => {
            let from = expect["from"].as_str().unwrap();
            let to = expect["to"].as_str().unwrap();
            ack(state, || sb.unwrap().fs_copy(from, to), "copy_acked")?;
        }
        "fs_rename" => {
            let from = expect["from"].as_str().unwrap();
            let to = expect["to"].as_str().unwrap();
            ack(state, || sb.unwrap().fs_rename(from, to), "rename_acked")?;
        }
        "fs_copy_from_host" => {
            let host = expect["host_path"].as_str().unwrap();
            let guest = expect["guest_path"].as_str().unwrap();
            ack(
                state,
                || sb.unwrap().fs_copy_from_host(host, guest),
                "copy_from_host_acked",
            )?;
        }
        "fs_copy_to_host" => {
            let guest = expect["guest_path"].as_str().unwrap();
            let host = expect["host_path"].as_str().unwrap();
            ack(
                state,
                || sb.unwrap().fs_copy_to_host(guest, host),
                "copy_to_host_acked",
            )?;
        }
        "logs" => {
            let follow = expect["follow"].as_bool().unwrap_or(false);
            let tail = expect["tail"].as_u64().map(|v| v as usize);
            let cursor = expect["cursor"].as_u64();
            let source = expect["source"].as_str().map(parse_log_source);
            let spec = LogsSpec {
                name: name.map(str::to_string),
                follow,
                tail,
                cursor,
                source,
            };
            let mut conn = Connection::dial(server_path).map_err(|e| e.to_string())?;
            if follow {
                conn.call_stream(&Request::Logs(spec))
                    .map_err(|e| e.to_string())?;
                let mut lines = Vec::new();
                loop {
                    match conn.next_response().map_err(|e| e.to_string())? {
                        Some(Response::Event(StreamEvent::Log(line))) => lines.push(line),
                        Some(Response::Event(StreamEvent::Done)) => break,
                        Some(Response::Error(e)) => {
                            return Err(format!("logs follow error: {}", e.code.as_str()))
                        }
                        Some(other) => {
                            return Err(format!("unexpected logs follow frame: {other:?}"))
                        }
                        None => return Err("logs follow stream ended without done".into()),
                    }
                }
                state.follow_logs_lines = lines;
            } else {
                match conn.call(&Request::Logs(spec)).map_err(|e| e.to_string())? {
                    Response::Logs { lines, next_cursor } => {
                        if state.logs_lines.is_none() {
                            state.logs_lines = Some(lines);
                            state.next_cursor = next_cursor;
                        } else {
                            state.resume_logs_lines = Some(lines);
                        }
                    }
                    other => return Err(format!("unexpected logs response: {other:?}")),
                }
            }
        }
        "metrics" => {
            if let Some(m) = drive(state, || sb.unwrap().metrics())? {
                state.metrics = Some(m);
            }
        }
        "metrics_stream" => {
            let interval = expect["interval_ms"].as_u64();
            let mut samples = Vec::new();
            if drive(state, || {
                sb.unwrap().metrics_stream(interval, |m| samples.push(m))
            })?
            .is_some()
            {
                state.metrics_samples = samples;
            }
        }
        "volume_create" => {
            let volume: VolumeSpec = serde_json::from_value(expect["volume"].clone())
                .map_err(|e| format!("bad volume spec: {e}"))?;
            if let Some(info) = drive(state, || sb.unwrap().volume_create(volume))? {
                state.volume_created = Some(info.name);
            }
        }
        "volume_list" => {
            if let Some(vols) = drive(state, || sb.unwrap().volume_list())? {
                state.volumes = Some(vols);
            }
        }
        "volume_get" => {
            let volume = expect["volume"].as_str().unwrap();
            if let Some(info) = drive(state, || sb.unwrap().volume_get(volume))? {
                state.volume_get = Some(info);
            }
        }
        "volume_remove" => {
            let volume = expect["volume"].as_str().unwrap();
            ack(state, || sb.unwrap().volume_remove(volume), "remove_acked")?;
        }
        "mount_add" => {
            let mount: MountSpec = serde_json::from_value(expect["mount"].clone())
                .map_err(|e| format!("bad mount spec: {e}"))?;
            ack(state, || sb.unwrap().mount_add(mount), "mount_add_acked")?;
        }
        "secret_set" => {
            let key = expect["key"].as_str().unwrap();
            let value = expect["value"].as_str().unwrap();
            ack(state, || sb.unwrap().secret_set(key, value), "set_acked")?;
        }
        "secret_list" => {
            if let Some(secrets) = drive(state, || sb.unwrap().secret_list())? {
                state.secrets = Some(secrets);
            }
        }
        "secret_remove" => {
            let key = expect["key"].as_str().unwrap();
            ack(state, || sb.unwrap().secret_remove(key), "remove_acked")?;
        }
        "snapshot_create" => {
            let dest = expect["dest"].as_str();
            if let Some(info) = drive(state, || sb.unwrap().snapshot_create(dest))? {
                state.snapshot_created = Some(info.id);
            }
        }
        "snapshot_list" => {
            if let Some(snaps) = drive(state, || client.snapshots())? {
                state.snapshots = Some(snaps);
            }
        }
        "snapshot_restore" => {
            let snapshot = expect["snapshot"].as_str().unwrap();
            let target = expect["name"].as_str().unwrap();
            ack(
                state,
                || sb.unwrap().snapshot_restore(snapshot, target),
                "restore_acked",
            )?;
        }
        "snapshot_fork" => {
            let snapshot = expect["snapshot"].as_str().unwrap();
            let target = expect["name"].as_str().unwrap();
            ack(
                state,
                || sb.unwrap().snapshot_fork(snapshot, target),
                "fork_acked",
            )?;
        }
        "snapshot_remove" => {
            // Not sandbox-scoped: the client's `snapshot_remove` never sends a
            // `name`, so the handle's bound name is irrelevant.
            let snapshot = expect["snapshot"].as_str().unwrap();
            let sb = client.sandbox("");
            ack(state, || sb.snapshot_remove(snapshot), "remove_acked")?;
        }
        other => return Err(format!("unmapped verb {other}")),
    }
    Ok(())
}

fn check_eq<T: PartialEq + std::fmt::Debug>(
    key: &str,
    expected: T,
    actual: T,
) -> Result<(), String> {
    if expected == actual {
        Ok(())
    } else {
        Err(format!(
            "assert {key}: expected {expected:?}, got {actual:?}"
        ))
    }
}

/// Evaluate the scenario's `assert` block against the recorded observations.
fn evaluate_assert(assert: &Value, state: &DriverState) -> Result<(), String> {
    let a = assert.as_object().ok_or("assert must be an object")?;

    if let Some(expected) = a.get("exit_code").and_then(Value::as_i64) {
        let actual = state
            .last_exec
            .as_ref()
            .map(|e| e.exit_code as i64)
            .or(state.stream_exit.map(|c| c as i64))
            .ok_or("assert exit_code: no exec result recorded")?;
        check_eq("exit_code", expected, actual)?;
    }
    if let Some(expected) = a.get("wait_exit_code").and_then(Value::as_i64) {
        let actual = state
            .wait_exit_code
            .map(|c| c as i64)
            .ok_or("assert wait_exit_code: no wait result")?;
        check_eq("wait_exit_code", expected, actual)?;
    }
    if let Some(expected) = a.get("wait_state").and_then(Value::as_str) {
        let actual = state
            .wait_state
            .as_deref()
            .ok_or("assert wait_state: no wait result")?;
        check_eq("wait_state", expected, actual)?;
    }
    if let Some(expected) = a.get("stdout_equals").and_then(Value::as_str) {
        let actual = state
            .last_exec
            .as_ref()
            .map(|e| e.stdout.as_str())
            .ok_or("assert stdout_equals: no exec result")?;
        check_eq("stdout_equals", expected, actual)?;
    }
    if let Some(expected) = a.get("stderr_equals").and_then(Value::as_str) {
        let actual = state
            .last_exec
            .as_ref()
            .map(|e| e.stderr.as_str())
            .ok_or("assert stderr_equals: no exec result")?;
        check_eq("stderr_equals", expected, actual)?;
    }
    if let Some(needles) = a.get("stdout_contains").and_then(Value::as_array) {
        let stdout = state
            .last_exec
            .as_ref()
            .map(|e| e.stdout.as_str())
            .unwrap_or(&state.stream_stdout);
        for n in needles {
            let n = n.as_str().unwrap();
            if !stdout.contains(n) {
                return Err(format!("assert stdout_contains: {n:?} not in {stdout:?}"));
            }
        }
    }
    if let Some(needles) = a.get("stderr_contains").and_then(Value::as_array) {
        let stderr = state
            .last_exec
            .as_ref()
            .map(|e| e.stderr.as_str())
            .unwrap_or(&state.stream_stderr);
        for n in needles {
            let n = n.as_str().unwrap();
            if !stderr.contains(n) {
                return Err(format!("assert stderr_contains: {n:?} not in {stderr:?}"));
            }
        }
    }

    const ACK_KEYS: &[&str] = &[
        "create_acked",
        "start_acked",
        "stop_acked",
        "kill_acked",
        "remove_acked",
        "drain_acked",
        "touch_acked",
        "pause_acked",
        "resume_acked",
        "modify_acked",
        "write_acked",
        "mkdir_acked",
        "remove_dir_acked",
        "copy_acked",
        "rename_acked",
        "copy_from_host_acked",
        "copy_to_host_acked",
        "mount_add_acked",
        "set_acked",
        "restore_acked",
        "fork_acked",
        "input_acked",
        "resize_acked",
        "write_stream_acked",
    ];
    for key in ACK_KEYS {
        if a.get(*key).and_then(Value::as_bool) == Some(true) && !state.acked.contains(key) {
            return Err(format!("assert {key}: operation did not ack"));
        }
    }

    if let Some(expected) = a.get("get_name").and_then(Value::as_str) {
        let info = state
            .get_info
            .as_ref()
            .ok_or("assert get_name: no get result")?;
        check_eq("get_name", expected, info.name.as_str())?;
    }
    if let Some(expected) = a.get("get_state").and_then(Value::as_str) {
        let info = state
            .get_info
            .as_ref()
            .ok_or("assert get_state: no get result")?;
        check_eq("get_state", expected, state_str(&info.state))?;
    }
    if let Some(expected) = a.get("get_image").and_then(Value::as_str) {
        let info = state
            .get_info
            .as_ref()
            .ok_or("assert get_image: no get result")?;
        check_eq("get_image", expected, info.image.as_str())?;
    }
    if let Some(expected) = a.get("list_names").and_then(Value::as_array) {
        let infos = state
            .list_infos
            .as_ref()
            .ok_or("assert list_names: no list result")?;
        let actual: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
        let expected: Vec<&str> = expected.iter().map(|v| v.as_str().unwrap()).collect();
        check_eq("list_names", expected, actual)?;
    }
    if let Some(expected) = a.get("list_states").and_then(Value::as_array) {
        let infos = state
            .list_infos
            .as_ref()
            .ok_or("assert list_states: no list result")?;
        let actual: Vec<&str> = infos.iter().map(|i| state_str(&i.state)).collect();
        let expected: Vec<&str> = expected.iter().map(|v| v.as_str().unwrap()).collect();
        check_eq("list_states", expected, actual)?;
    }
    if let Some(needles) = a.get("entries_contain").and_then(Value::as_array) {
        let entries = state
            .fs_entries
            .as_ref()
            .ok_or("assert entries_contain: no fs_list result")?;
        for n in needles {
            let n = n.as_str().unwrap();
            if !entries.iter().any(|e| e.name == n) {
                return Err(format!("assert entries_contain: {n:?} not in entries"));
            }
        }
    }
    if let Some(expected) = a.get("stat_is_dir").and_then(Value::as_bool) {
        let stat = state
            .fs_stat
            .as_ref()
            .ok_or("assert stat_is_dir: no fs_stat result")?;
        check_eq("stat_is_dir", expected, stat.is_dir)?;
    }
    if let Some(expected) = a.get("exists").and_then(Value::as_bool) {
        let actual = state
            .fs_exists
            .ok_or("assert exists: no fs_exists result")?;
        check_eq("exists", expected, actual)?;
    }
    if let Some(expected) = a.get("read_bytes_base64").and_then(Value::as_str) {
        let bytes = state
            .fs_read_bytes
            .as_ref()
            .ok_or("assert read_bytes_base64: no fs_read result")?;
        check_eq("read_bytes_base64", expected, base64_encode(bytes).as_str())?;
    }
    if let Some(expected) = a.get("content_equals").and_then(Value::as_str) {
        let content = state
            .fs_read_to_string
            .as_ref()
            .ok_or("assert content_equals: no fs_read_to_string result")?;
        check_eq("content_equals", expected, content.as_str())?;
    }
    if let Some(expected) = a.get("chunks_acked").and_then(Value::as_u64) {
        check_eq("chunks_acked", expected as usize, state.chunks_acked)?;
    }
    if let Some(needles) = a.get("lines_contain").and_then(Value::as_array) {
        let lines = state
            .logs_lines
            .as_ref()
            .ok_or("assert lines_contain: no logs result")?;
        for n in needles {
            let n = n.as_str().unwrap();
            if !lines.iter().any(|l| l.data.contains(n)) {
                return Err(format!("assert lines_contain: {n:?} not in log lines"));
            }
        }
    }
    if let Some(needles) = a.get("resume_lines_contain").and_then(Value::as_array) {
        let lines = state
            .resume_logs_lines
            .as_ref()
            .ok_or("assert resume_lines_contain: no resume logs result")?;
        for n in needles {
            let n = n.as_str().unwrap();
            if !lines.iter().any(|l| l.data.contains(n)) {
                return Err(format!(
                    "assert resume_lines_contain: {n:?} not in resume lines"
                ));
            }
        }
    }
    if let Some(needles) = a.get("follow_lines_contain").and_then(Value::as_array) {
        for n in needles {
            let n = n.as_str().unwrap();
            if !state.follow_logs_lines.iter().any(|l| l.data.contains(n)) {
                return Err(format!(
                    "assert follow_lines_contain: {n:?} not in follow lines"
                ));
            }
        }
    }
    if let Some(expected) = a.get("next_cursor").and_then(Value::as_u64) {
        let actual = state
            .next_cursor
            .ok_or("assert next_cursor: no logs result")?;
        check_eq("next_cursor", expected, actual)?;
    }
    if let Some(expected) = a.get("all_source").and_then(Value::as_str) {
        let lines = state
            .logs_lines
            .as_ref()
            .ok_or("assert all_source: no logs result")?;
        for line in lines {
            if source_str(&line.source) != expected {
                return Err(format!(
                    "assert all_source: line source {} != {expected}",
                    source_str(&line.source)
                ));
            }
        }
    }
    if let Some(expected) = a.get("cpu_percent").and_then(Value::as_f64) {
        let m = state
            .metrics
            .as_ref()
            .ok_or("assert cpu_percent: no metrics result")?;
        if (m.cpu_percent - expected).abs() > 1e-9 {
            return Err(format!(
                "assert cpu_percent: expected {expected}, got {}",
                m.cpu_percent
            ));
        }
    }
    if let Some(expected) = a.get("memory_used_bytes").and_then(Value::as_u64) {
        let m = state
            .metrics
            .as_ref()
            .ok_or("assert memory_used_bytes: no metrics result")?;
        check_eq("memory_used_bytes", expected, m.memory_used_bytes)?;
    }
    if let Some(fields) = a.get("has_fields").and_then(Value::as_array) {
        state
            .metrics
            .as_ref()
            .ok_or("assert has_fields: no metrics result")?;
        for f in fields {
            let f = f.as_str().unwrap();
            let present = matches!(
                f,
                "cpu_percent"
                    | "memory_used_bytes"
                    | "memory_total_bytes"
                    | "disk_used_bytes"
                    | "disk_total_bytes"
                    | "net_rx_bytes"
                    | "net_tx_bytes"
                    | "timestamp_ms"
            );
            if !present {
                return Err(format!("assert has_fields: missing field {f}"));
            }
        }
    }
    if let Some(expected) = a.get("stream_samples").and_then(Value::as_u64) {
        check_eq(
            "stream_samples",
            expected as usize,
            state.metrics_samples.len(),
        )?;
    }
    if let Some(expected) = a.get("first_cpu_percent").and_then(Value::as_f64) {
        let first = state
            .metrics_samples
            .first()
            .ok_or("assert first_cpu_percent: no samples")?;
        if (first.cpu_percent - expected).abs() > 1e-9 {
            return Err(format!(
                "assert first_cpu_percent: expected {expected}, got {}",
                first.cpu_percent
            ));
        }
    }
    if let Some(expected) = a.get("volume_created").and_then(Value::as_str) {
        let actual = state
            .volume_created
            .as_ref()
            .ok_or("assert volume_created: no volume_create result")?;
        check_eq("volume_created", expected, actual.as_str())?;
    }
    if let Some(needles) = a.get("volumes_contain").and_then(Value::as_array) {
        let vols = state
            .volumes
            .as_ref()
            .ok_or("assert volumes_contain: no volume_list result")?;
        for n in needles {
            let n = n.as_str().unwrap();
            if !vols.iter().any(|v| v.name == n) {
                return Err(format!("assert volumes_contain: {n:?} not in volumes"));
            }
        }
    }
    if let Some(expected) = a.get("volume_get_name").and_then(Value::as_str) {
        let info = state
            .volume_get
            .as_ref()
            .ok_or("assert volume_get_name: no volume_get result")?;
        check_eq("volume_get_name", expected, info.name.as_str())?;
    }
    if let Some(needles) = a.get("secrets_contain").and_then(Value::as_array) {
        let secrets = state
            .secrets
            .as_ref()
            .ok_or("assert secrets_contain: no secret_list result")?;
        for n in needles {
            let n = n.as_str().unwrap();
            if !secrets.iter().any(|s| s.key == n) {
                return Err(format!("assert secrets_contain: {n:?} not in secrets"));
            }
        }
    }
    if a.get("secret_values_redacted").and_then(Value::as_bool) == Some(true) {
        let secrets = state
            .secrets
            .as_ref()
            .ok_or("assert secret_values_redacted: no secret_list result")?;
        for s in secrets {
            if s.value.as_deref().map(|v| !v.is_empty()).unwrap_or(false) {
                return Err("assert secret_values_redacted: value not redacted".into());
            }
        }
    }
    if let Some(expected) = a.get("snapshot_created").and_then(Value::as_str) {
        let actual = state
            .snapshot_created
            .as_ref()
            .ok_or("assert snapshot_created: no snapshot_create result")?;
        check_eq("snapshot_created", expected, actual.as_str())?;
    }
    if let Some(needles) = a.get("snapshots_contain").and_then(Value::as_array) {
        let snaps = state
            .snapshots
            .as_ref()
            .ok_or("assert snapshots_contain: no snapshot_list result")?;
        for n in needles {
            let n = n.as_str().unwrap();
            if !snaps.iter().any(|s| s.id == n) {
                return Err(format!("assert snapshots_contain: {n:?} not in snapshots"));
            }
        }
    }
    if let Some(expected) = a.get("attach_events").and_then(Value::as_array) {
        let actual: Vec<&str> = state.attach_events.iter().map(|s| s.as_str()).collect();
        let expected: Vec<&str> = expected.iter().map(|v| v.as_str().unwrap()).collect();
        check_eq("attach_events", expected, actual)?;
    }
    if let Some(expected) = a.get("attach_exit_code").and_then(Value::as_i64) {
        let actual = state
            .attach_exit_code
            .ok_or("assert attach_exit_code: no attach exit")?;
        check_eq("attach_exit_code", expected, actual as i64)?;
    }
    if let Some(expected) = a.get("error_code").and_then(Value::as_str) {
        let actual = state
            .errors
            .first()
            .map(|c| c.as_str())
            .ok_or("assert error_code: no error recorded")?;
        check_eq("error_code", expected, actual)?;
    }
    if let Some(expected) = a.get("second_error_code").and_then(Value::as_str) {
        let actual = state
            .errors
            .get(1)
            .map(|c| c.as_str())
            .ok_or("assert second_error_code: no second error recorded")?;
        check_eq("second_error_code", expected, actual)?;
    }
    if let Some(expected) = a.get("error_message_contains").and_then(Value::as_str) {
        let msg = state
            .error_messages
            .first()
            .ok_or("assert error_message_contains: no error message")?;
        if !msg.contains(expected) {
            return Err(format!(
                "assert error_message_contains: {expected:?} not in {msg:?}"
            ));
        }
    }
    if a.get("expects_timeout").and_then(Value::as_bool) == Some(true) {
        if let Some(expected) = a.get("timeout_code").and_then(Value::as_str) {
            let actual = state
                .timeout_code
                .as_ref()
                .ok_or("assert timeout_code: no timeout observed")?;
            check_eq("timeout_code", expected, actual.as_str())?;
        }
    }
    if a.get("ping_ok").and_then(Value::as_bool) == Some(true) && !state.ping_ok {
        return Err("assert ping_ok: ping did not succeed".into());
    }
    if a.get("expects_unsupported").and_then(Value::as_bool) == Some(true) {
        let verb = a
            .get("unsupported_verb")
            .and_then(Value::as_str)
            .ok_or("assert expects_unsupported: missing unsupported_verb")?;
        let cap = required_capability(verb).ok_or("assert expects_unsupported: unknown verb")?;
        if state.caps.contains(&cap) {
            return Err(format!(
                "assert expects_unsupported: {verb} capability was advertised"
            ));
        }
        if state.errors.first().map(|c| c.as_str()) != Some("unsupported") {
            return Err("assert expects_unsupported: no unsupported error recorded".into());
        }
    }
    Ok(())
}

/// Spawn `nanosb-conformance serve --scenario <file>` and read the `READY
/// <socket>` line from its stdout (bounded wait). Returns the child and the
/// socket path.
fn spawn_harness(scenario_file: &Path) -> Result<(Child, PathBuf), String> {
    let bin = env!("CARGO_BIN_EXE_nanosb-conformance");
    let mut child = Command::new(bin)
        .arg("serve")
        .arg("--scenario")
        .arg(scenario_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn harness: {e}"))?;
    let stdout = child.stdout.take().expect("harness stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap_or(0);
        let _ = tx.send((n, line));
    });
    let (n, line) = rx.recv_timeout(Duration::from_secs(10)).map_err(|_| {
        let _ = child.kill();
        "harness did not print READY within 10s".to_string()
    })?;
    if n == 0 {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        let _ = child.kill();
        return Err(format!("harness exited before READY: {stderr}"));
    }
    let line = line.trim();
    let path = line
        .strip_prefix("READY ")
        .ok_or_else(|| format!("expected READY line, got: {line}"))?;
    Ok((child, PathBuf::from(path)))
}

/// Drive one scenario against the harness socket: run the steps, then evaluate
/// the assert block.
fn drive_scenario(
    scenario: &Scenario,
    client: &SandboxClient,
    state: &mut DriverState,
    server_path: &Path,
) -> Result<(), String> {
    let expects_timeout = scenario
        .assert
        .get("expects_timeout")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let expects_unsupported = scenario
        .assert
        .get("expects_unsupported")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if expects_unsupported {
        // Capability-gating scenario: no steps. Verify the handshake omitted
        // the capability, then invoke the verb and expect an `unsupported`
        // error.
        let verb = scenario.assert["unsupported_verb"].as_str().unwrap();
        let cap = required_capability(verb).ok_or_else(|| format!("unknown verb {verb}"))?;
        if client.supports(cap) {
            return Err(format!(
                "scenario expects {verb} unsupported but capability was advertised"
            ));
        }
        let expect = serde_json::json!({ "type": verb, "name": "box", "command": "x" });
        drive_step(&expect, client, state, server_path)?;
    } else {
        let steps = &scenario.steps;
        let mut i = 0;
        while i < steps.len() {
            let step = &steps[i];
            // Timeout step: the driver must NOT send the request; the server
            // answers a `timeout` error, observed below.
            if expects_timeout && step.timeout_ms.is_some() {
                break;
            }
            let expect = step
                .expect
                .as_ref()
                .ok_or_else(|| format!("step {i} has no expect"))?;
            let ty = expect["type"]
                .as_str()
                .ok_or_else(|| format!("step {i} has no type"))?;
            if ty == "fs_write_stream" {
                // The streamed write consumes the following `fs_data` steps.
                let name = expect["name"].as_str().unwrap();
                let path = expect["path"].as_str().unwrap();
                let sb = client.sandbox(name);
                let mut data = Vec::new();
                let mut chunk_size = 0usize;
                let mut j = i + 1;
                while j < steps.len() {
                    let Some(nexp) = steps[j].expect.as_ref() else {
                        break;
                    };
                    if nexp["type"].as_str() != Some("fs_data") {
                        break;
                    }
                    let chunk = base64_decode(nexp["data_base64"].as_str().unwrap())
                        .ok_or("invalid base64 in fs_data step")?;
                    if chunk_size == 0 {
                        chunk_size = chunk.len();
                    }
                    data.extend_from_slice(&chunk);
                    j += 1;
                }
                if chunk_size == 0 {
                    chunk_size = 1;
                }
                sb.fs_write_stream(path, &data, chunk_size)
                    .map_err(|e| format!("fs_write_stream failed: {e}"))?;
                state.acked.insert("write_stream_acked");
                state.chunks_acked = data.chunks(chunk_size).count();
                i = j;
                continue;
            }
            drive_step(expect, client, state, server_path)?;
            i += 1;
        }
    }

    // Timeout scenario: wait for the server's `timeout` error on a fresh
    // connection (the driver never sent the timed-out request).
    if expects_timeout {
        let mut conn = Connection::dial(server_path).map_err(|e| e.to_string())?;
        match conn.next_response().map_err(|e| e.to_string())? {
            Some(Response::Error(e)) => state.timeout_code = Some(e.code.as_str().to_string()),
            other => return Err(format!("expected timeout error, got {other:?}")),
        }
    }

    evaluate_assert(&scenario.assert, state)
}

/// Run one scenario end to end: spawn the harness binary, connect the client,
/// drive the steps, and evaluate the assert block.
fn run_scenario(file: &Path) -> Result<(), String> {
    let raw = std::fs::read_to_string(file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let scenario: Scenario =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", file.display()))?;
    let (mut child, socket) = spawn_harness(file)?;
    let result = (|| -> Result<(), String> {
        let client = SandboxClient::connect(socket.as_path())
            .map_err(|e| format!("connect {}: {e}", socket.display()))?;
        let mut state = DriverState {
            caps: client.capabilities().to_vec(),
            ..Default::default()
        };
        drive_scenario(&scenario, &client, &mut state, &socket)
    })();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket);
    result
}

#[test]
fn reference_client_passes_all_scenarios() {
    let dir = Path::new(SCENARIOS_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("scenarios dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no scenario files in {SCENARIOS_DIR}");

    let mut failures: Vec<(String, String)> = Vec::new();
    let mut passed = 0usize;
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        match run_scenario(file) {
            Ok(()) => {
                passed += 1;
                println!("PASS {name}");
            }
            Err(msg) => {
                failures.push((name.clone(), msg.clone()));
                println!("FAIL {name}: {msg}");
            }
        }
    }
    println!("conformance: {passed}/{} scenarios passed", files.len());
    if !failures.is_empty() {
        panic!("{} scenario(s) failed: {failures:?}", failures.len());
    }
}
