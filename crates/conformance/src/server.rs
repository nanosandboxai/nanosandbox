//! The conformance server: a standalone superset of the in-process FakeHelper.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nanosb_protocol::{write_message, Capability, HelloResponse, Response, PROTOCOL_VERSION};
use serde_json::Value;

use crate::matcher::{is_terminal_frame, partial_match, required_capability};
use crate::scenario::{Scenario, Step};

/// A running conformance server on a Unix stream socket.
pub struct ConformanceServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ConformanceServer {
    /// Start a server for `scenario`. `socket` overrides the default temp path;
    /// `caps` overrides the scenario's advertised capabilities.
    pub fn start(
        scenario: Scenario,
        socket: Option<PathBuf>,
        caps: Option<Vec<Capability>>,
    ) -> std::io::Result<Self> {
        let path = match socket {
            Some(p) => p,
            None => default_socket_path(&scenario.id),
        };
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;

        let caps = caps.unwrap_or_else(|| parse_caps(&scenario.capabilities));
        let steps = Arc::new(scenario.steps);
        let cursor = Arc::new(Mutex::new(0usize));
        let stop = Arc::new(AtomicBool::new(false));

        let caps_t = caps.clone();
        let steps_t = Arc::clone(&steps);
        let cursor_t = Arc::clone(&cursor);
        let stop_t = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            serve(listener, caps_t, steps_t, cursor_t, stop_t);
        });

        Ok(Self {
            path,
            stop,
            handle: Some(handle),
        })
    }

    /// The socket path drivers should dial.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ConformanceServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.path);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve(
    listener: UnixListener,
    caps: Vec<Capability>,
    steps: Arc<Vec<Step>>,
    cursor: Arc<Mutex<usize>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let caps = caps.clone();
                let steps = Arc::clone(&steps);
                let cursor = Arc::clone(&cursor);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || handle_conn(stream, caps, steps, cursor, stop));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn handle_conn(
    stream: UnixStream,
    caps: Vec<Capability>,
    steps: Arc<Vec<Step>>,
    cursor: Arc<Mutex<usize>>,
    stop: Arc<AtomicBool>,
) {
    // BSD/macOS accept() inherits O_NONBLOCK from the non-blocking listener; the
    // reader thread below needs a blocking socket.
    let _ = stream.set_nonblocking(false);
    let mut writer = stream.try_clone().expect("clone stream");
    let read_socket = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    // A dedicated blocking reader thread feeds lines over a channel. This
    // avoids SO_RCVTIMEO + BufReader, which on macOS can wedge a socket after a
    // timed-out read; deadlines are enforced with `recv_timeout` instead.
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(read_socket);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(std::mem::take(&mut line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let req = match next_request(&rx, &steps, &cursor, &mut writer) {
            Some(r) => r,
            None => return,
        };
        let ty = req.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "hello" {
            if write_message(&mut writer, &hello_response(&caps)).is_err() {
                return;
            }
            continue;
        }
        if let Some(need) = required_capability(ty) {
            if !caps.contains(&need) {
                if write_frame(
                    &mut writer,
                    &error_frame("unsupported", &format!("verb `{ty}` is not advertised")),
                )
                .is_err()
                {
                    return;
                }
                continue;
            }
        }
        let mut guard = cursor.lock().unwrap();
        let idx = find_step(&steps, *guard, &req);
        match idx {
            Some(i) => {
                let step = &steps[i];
                *guard = i + 1;
                drop(guard);
                if let Some(d) = step.delay_ms {
                    std::thread::sleep(Duration::from_millis(d));
                }
                if !send_respond(&mut writer, &step.respond) {
                    return;
                }
            }
            None => {
                drop(guard);
                if write_frame(
                    &mut writer,
                    &error_frame("internal", "no scripted response"),
                )
                .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Read one request line from the reader channel. Returns `None` on EOF or a
/// malformed transport. When the current step has a `timeout_ms`, a wait that
/// exceeds it answers with a `timeout` error frame and advances the cursor.
fn next_request(
    rx: &mpsc::Receiver<String>,
    steps: &[Step],
    cursor: &Mutex<usize>,
    writer: &mut UnixStream,
) -> Option<Value> {
    const POLL: Duration = Duration::from_millis(50);
    let mut wait_cursor: Option<usize> = None;
    let mut deadline: Option<Instant> = None;
    loop {
        let cur = *cursor.lock().unwrap();
        if wait_cursor != Some(cur) {
            wait_cursor = Some(cur);
            deadline = steps
                .get(cur)
                .and_then(|s| s.timeout_ms)
                .map(|ms| Instant::now() + Duration::from_millis(ms));
        }
        match rx.recv_timeout(POLL) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(v) => return Some(v),
                    Err(_) => {
                        if write_frame(writer, &error_frame("bad_request", "malformed frame"))
                            .is_err()
                        {
                            return None;
                        }
                        continue;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        let mut guard = cursor.lock().unwrap();
                        if steps.get(*guard).and_then(|s| s.timeout_ms).is_some() {
                            if write_frame(
                                writer,
                                &error_frame("timeout", "step wait budget elapsed"),
                            )
                            .is_err()
                            {
                                return None;
                            }
                            *guard += 1;
                        }
                        continue;
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// First step at or after `from` whose `expect` matches `req`.
fn find_step(steps: &[Step], from: usize, req: &Value) -> Option<usize> {
    (from..steps.len()).find(|&i| match &steps[i].expect {
        Some(needle) => partial_match(needle, req),
        None => true,
    })
}

/// Write a step's response frames. A list is flushed back-to-back, stopping
/// early at a terminal frame (the streaming contract).
fn send_respond(writer: &mut UnixStream, frames: &[Value]) -> bool {
    for frame in frames {
        if write_frame(writer, frame).is_err() {
            return false;
        }
        if is_terminal_frame(frame) {
            break;
        }
    }
    true
}

fn write_frame(writer: &mut UnixStream, frame: &Value) -> std::io::Result<()> {
    let line = serde_json::to_string(frame)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn hello_response(caps: &[Capability]) -> Response {
    Response::HelloOk(HelloResponse {
        protocol: PROTOCOL_VERSION.to_string(),
        helper_version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: caps.to_vec(),
    })
}

fn error_frame(code: &str, message: &str) -> Value {
    serde_json::json!({ "type": "error", "code": code, "message": message })
}

fn parse_caps(names: &[String]) -> Vec<Capability> {
    names
        .iter()
        .filter_map(|n| Capability::all().iter().find(|c| c.as_str() == n).copied())
        .collect()
}

fn default_socket_path(id: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let safe: String = id
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!(
        "nanosb-conf-{}-{}-{}.sock",
        std::process::id(),
        n,
        safe
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Scenario;
    use nanosb_protocol::{read_message, HelloRequest, Request};
    use std::time::Instant;

    fn scenario_with(steps: Vec<Value>, caps: Vec<&str>) -> Scenario {
        Scenario {
            id: "test".into(),
            description: String::new(),
            capabilities: caps.into_iter().map(str::to_string).collect(),
            steps: steps
                .into_iter()
                .map(|v| serde_json::from_value(v).unwrap())
                .collect(),
            assert: Value::Null,
        }
    }

    fn connect(path: &Path) -> (UnixStream, BufReader<UnixStream>) {
        let stream = UnixStream::connect(path).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        (stream, reader)
    }

    fn hello(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>) -> Response {
        let req = Request::Hello(HelloRequest::new("test", PROTOCOL_VERSION));
        write_message(stream, &req).unwrap();
        let mut scratch = String::new();
        read_message::<_, Response>(reader, &mut scratch)
            .unwrap()
            .unwrap()
    }

    fn send_req(stream: &mut UnixStream, req: &Value) {
        let line = serde_json::to_string(req).unwrap();
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
    }

    fn read_resp(reader: &mut BufReader<UnixStream>) -> Value {
        let mut scratch = String::new();
        read_message::<_, Value>(reader, &mut scratch)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn hello_handshake_advertises_version_and_capabilities() {
        let s = scenario_with(vec![], vec!["lifecycle", "exec"]);
        let server = ConformanceServer::start(s, None, None).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        let resp = hello(&mut stream, &mut reader);
        let Response::HelloOk(h) = resp else {
            panic!("expected hello_ok, got {resp:?}");
        };
        assert_eq!(h.protocol, PROTOCOL_VERSION);
        assert_eq!(
            h.capabilities,
            vec![Capability::Lifecycle, Capability::Exec]
        );
    }

    #[test]
    fn ordered_replay_consumes_steps_in_order() {
        let s = scenario_with(
            vec![
                serde_json::json!({"expect": {"type": "ping"}, "respond": {"type": "pong", "version": "1"}}),
                serde_json::json!({"expect": {"type": "status"}, "respond": {"type": "status", "state": "running"}}),
            ],
            vec!["lifecycle"],
        );
        let server = ConformanceServer::start(s, None, None).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        hello(&mut stream, &mut reader);

        send_req(&mut stream, &serde_json::json!({"type": "ping"}));
        assert_eq!(read_resp(&mut reader)["type"], "pong");

        send_req(&mut stream, &serde_json::json!({"type": "status"}));
        assert_eq!(read_resp(&mut reader)["state"], "running");
    }

    #[test]
    fn non_matching_request_skips_to_next_step() {
        let s = scenario_with(
            vec![
                serde_json::json!({"expect": {"type": "exec", "command": "echo"}, "respond": {"type": "exec_result", "exit_code": 0, "stdout": "hi", "stderr": "", "duration_ms": 1}}),
            ],
            vec!["exec"],
        );
        let server = ConformanceServer::start(s, None, None).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        hello(&mut stream, &mut reader);

        send_req(
            &mut stream,
            &serde_json::json!({"type": "exec", "command": "ls"}),
        );
        let resp = read_resp(&mut reader);
        assert_eq!(resp["type"], "error");
        assert_eq!(resp["code"], "internal");
    }

    #[test]
    fn streaming_flush_sends_all_frames_and_stops_at_terminal() {
        let s = scenario_with(
            vec![serde_json::json!({
                "expect": {"type": "exec_stream"},
                "respond": [
                    {"type": "ok", "version": "1"},
                    {"type": "guest", "event": "stdout", "data": "a"},
                    {"type": "guest", "event": "exit", "code": 0},
                    {"type": "guest", "event": "stdout", "data": "never"}
                ]
            })],
            vec!["exec"],
        );
        let server = ConformanceServer::start(s, None, None).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        hello(&mut stream, &mut reader);

        send_req(
            &mut stream,
            &serde_json::json!({"type": "exec_stream", "command": "x"}),
        );
        assert_eq!(read_resp(&mut reader)["type"], "ok");
        assert_eq!(read_resp(&mut reader)["event"], "stdout");
        let last = read_resp(&mut reader);
        assert_eq!(last["event"], "exit");
        assert_eq!(last["code"], 0);
    }

    #[test]
    fn capability_gating_rejects_unadvertised_verb() {
        let s = scenario_with(vec![], vec!["lifecycle"]);
        let server = ConformanceServer::start(s, None, None).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        hello(&mut stream, &mut reader);

        send_req(
            &mut stream,
            &serde_json::json!({"type": "exec", "command": "x"}),
        );
        let resp = read_resp(&mut reader);
        assert_eq!(resp["type"], "error");
        assert_eq!(resp["code"], "unsupported");
    }

    #[test]
    fn per_step_timeout_answers_timeout_error() {
        let s = scenario_with(
            vec![serde_json::json!({
                "expect": {"type": "ping"},
                "respond": {"type": "pong", "version": "1"},
                "timeout_ms": 100
            })],
            vec!["lifecycle"],
        );
        let server = ConformanceServer::start(s, None, None).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        hello(&mut stream, &mut reader);
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let start = Instant::now();
        let resp = read_resp(&mut reader);
        assert_eq!(resp["type"], "error");
        assert_eq!(resp["code"], "timeout");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn request_with_no_scripted_response_gets_internal_error() {
        let s = scenario_with(vec![], vec!["lifecycle"]);
        let server = ConformanceServer::start(s, None, None).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        hello(&mut stream, &mut reader);

        send_req(&mut stream, &serde_json::json!({"type": "ping"}));
        let resp = read_resp(&mut reader);
        assert_eq!(resp["type"], "error");
        assert_eq!(resp["code"], "internal");
    }

    #[test]
    fn capabilities_override_wins_over_scenario() {
        let s = scenario_with(vec![], vec!["exec"]);
        let server = ConformanceServer::start(s, None, Some(vec![Capability::Lifecycle])).unwrap();
        let (mut stream, mut reader) = connect(server.path());
        let resp = hello(&mut stream, &mut reader);
        let Response::HelloOk(h) = resp else {
            panic!("expected hello_ok");
        };
        assert_eq!(h.capabilities, vec![Capability::Lifecycle]);
    }
}
