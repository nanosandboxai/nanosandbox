//! Real end-to-end conformance for the protocol helper against a live VM.
//!
//! Opt-in: requires the `nanosb-runtime` binary, libkrun/gvproxy, and a cached
//! image. Run with:
//!
//!   cargo build -p nanosb-cli --bin nanosb-runtime
//!   cargo test -p nanosb-client --test helper_e2e -- --ignored --nocapture
//!
//! Override the binary with `NANOSB_RUNTIME_BIN` and the image with
//! `NANOSB_TEST_IMAGE` (default `library/alpine:latest`).

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use nanosb_client::{SandboxBuilder, SandboxClient};

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn runtime_bin() -> PathBuf {
    if let Ok(p) = std::env::var("NANOSB_RUNTIME_BIN") {
        return PathBuf::from(p);
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../");
    workspace.join("target/debug/nanosb-runtime")
}

fn start_daemon() -> Daemon {
    let dir = std::env::temp_dir().join(format!("nsb-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("helper.sock");
    let child = Command::new(runtime_bin())
        .arg("serve")
        .arg("--socket")
        .arg(&socket)
        .spawn()
        .expect("spawn nanosb-runtime");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "helper socket never appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon { child, socket, dir }
}

#[test]
#[ignore = "requires nanosb-runtime + libkrun + a cached image"]
fn end_to_end_lifecycle_exec_fs_logs_metrics() {
    let daemon = start_daemon();
    let client = SandboxClient::connect(&daemon.socket).expect("connect");
    assert!(!client.capabilities().is_empty());

    let image =
        std::env::var("NANOSB_TEST_IMAGE").unwrap_or_else(|_| "library/alpine:latest".into());
    let name = format!("sdk-e2e-{}", std::process::id());
    let sandbox = client
        .create_with(
            SandboxBuilder::new(&image)
                .name(&name)
                .cpus(2)
                .memory_mb(1024),
        )
        .expect("create");

    assert_eq!(
        sandbox.state().unwrap(),
        nanosb_protocol::SandboxState::Running
    );

    let out = sandbox.exec("echo", &["hello-from-sdk"]).expect("exec");
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout.trim(), "hello-from-sdk");

    sandbox
        .fs_write("/tmp/nanosc", b"written-by-sdk")
        .expect("fs_write");
    assert_eq!(
        sandbox.fs_read_to_string("/tmp/nanosc").unwrap(),
        "written-by-sdk"
    );
    assert!(sandbox.fs_exists("/tmp/nanosc").unwrap());
    sandbox.fs_mkdir("/tmp/nanosc-dir", true).unwrap();
    assert!(sandbox.fs_stat("/tmp/nanosc-dir").unwrap().is_dir);

    assert!(!sandbox.logs(Some(10)).unwrap().is_empty());
    let metrics = sandbox.metrics().unwrap();
    assert!(metrics.disk_used_bytes > 0);

    assert!(client.list(true).unwrap().iter().any(|s| s.name == name));

    sandbox.stop(true).expect("stop");
    client.sandbox(&name).remove(true).expect("remove");
    assert!(client.list(true).unwrap().iter().all(|s| s.name != name));
}
