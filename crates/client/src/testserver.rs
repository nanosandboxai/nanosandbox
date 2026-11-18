//! An in-process fake NDJSON helper for tests and the conformance suite.
//!
//! It binds a temporary Unix socket, completes the `hello` handshake, and
//! replays a queue of scripted responses in request order. This gives every SDK
//! (and this reference client) a real, byte-level protocol target that runs in
//! CI without a VM.

use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use nanosb_protocol::{
    read_message, write_message, Capability, HelloResponse, Request, Response, PROTOCOL_VERSION,
};

/// A scripted fake helper.
pub struct FakeHelper {
    path: PathBuf,
    queue: Arc<Mutex<VecDeque<String>>>,
    helper_version: String,
    handle: Option<JoinHandle<()>>,
    stop: Arc<Mutex<bool>>,
}

impl FakeHelper {
    /// Start a fake helper on a fresh temporary socket.
    pub fn start() -> Self {
        Self::with_version("0.3.0-test")
    }

    /// Start a fake helper advertising `version` and all capabilities.
    pub fn with_version(version: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("nanosb-fake-{}-{}.sock", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fake helper socket");
        listener.set_nonblocking(true).expect("set nonblocking");

        let queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(Mutex::new(false));
        let version = version.to_string();
        let q = Arc::clone(&queue);
        let s = Arc::clone(&stop);
        let v = version.clone();
        let handle = std::thread::spawn(move || {
            serve(listener, q, v, s);
        });

        Self {
            path,
            queue,
            helper_version: version,
            handle: Some(handle),
            stop,
        }
    }

    /// The socket path clients should dial.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Enqueue a raw JSON response line (replayed in order).
    pub fn push_script(&self, json: &str) {
        self.queue.lock().unwrap().push_back(json.to_string());
    }

    /// Enqueue a typed response.
    pub fn push_response(&self, resp: &Response) {
        let line = serde_json::to_string(resp).unwrap();
        self.push_script(&line);
    }

    /// The helper version this server reports.
    pub fn helper_version(&self) -> &str {
        &self.helper_version
    }
}

impl Drop for FakeHelper {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
        let _ = std::fs::remove_file(&self.path);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn hello_response(version: &str) -> Response {
    Response::HelloOk(HelloResponse {
        protocol: PROTOCOL_VERSION.to_string(),
        helper_version: version.to_string(),
        capabilities: Capability::all().to_vec(),
    })
}

fn serve(
    listener: UnixListener,
    queue: Arc<Mutex<VecDeque<String>>>,
    version: String,
    stop: Arc<Mutex<bool>>,
) {
    while !*stop.lock().unwrap() {
        match listener.accept() {
            Ok((stream, _)) => handle_conn(stream, &queue, &version, &stop),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
}

fn handle_conn(
    stream: UnixStream,
    queue: &Arc<Mutex<VecDeque<String>>>,
    version: &str,
    stop: &Arc<Mutex<bool>>,
) {
    stream
        .set_nonblocking(false)
        .expect("accepted socket is blocking");
    let mut writer = stream.try_clone().expect("clone stream");
    let mut reader = BufReader::new(stream);
    let mut scratch = String::new();
    loop {
        if *stop.lock().unwrap() {
            return;
        }
        match read_message::<_, Request>(&mut reader, &mut scratch) {
            Ok(Some(Request::Hello(_))) => {
                let resp = hello_response(version);
                if write_message(&mut writer, &resp).is_err() {
                    return;
                }
            }
            Ok(Some(req)) => {
                if !send_next(queue, &mut writer) {
                    return;
                }
                if is_streaming(&req) && !send_stream_frames(queue, &mut writer) {
                    return;
                }
            }
            Ok(None) => return,
            Err(_) => return,
        }
    }
}

fn send_next(queue: &Arc<Mutex<VecDeque<String>>>, writer: &mut UnixStream) -> bool {
    match queue.lock().unwrap().pop_front() {
        Some(line) => {
            writer.write_all(line.as_bytes()).is_ok()
                && writer.write_all(b"\n").is_ok()
                && writer.flush().is_ok()
        }
        None => {
            let err = Response::Error(nanosb_protocol::ProtocolError::internal(
                "fake helper: no scripted response",
            ));
            write_message(writer, &err).is_ok()
        }
    }
}

fn send_stream_frames(queue: &Arc<Mutex<VecDeque<String>>>, writer: &mut UnixStream) -> bool {
    loop {
        let line = queue.lock().unwrap().pop_front();
        match line {
            Some(l) => {
                let terminal = is_terminal(&l);
                if writer.write_all(l.as_bytes()).is_err()
                    || writer.write_all(b"\n").is_err()
                    || writer.flush().is_err()
                {
                    return false;
                }
                if terminal {
                    return true;
                }
            }
            None => return true,
        }
    }
}

fn is_streaming(req: &Request) -> bool {
    match req {
        Request::ExecStream(_) | Request::Attach { .. } | Request::MetricsStream { .. } => true,
        Request::Logs(spec) => spec.follow,
        _ => false,
    }
}

fn is_terminal(line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if ty == "guest" {
        matches!(
            v.get("event").and_then(|e| e.as_str()),
            Some("exit") | Some("error")
        )
    } else if ty == "event" {
        v.get("event").and_then(|e| e.as_str()) == Some("done")
    } else {
        false
    }
}
