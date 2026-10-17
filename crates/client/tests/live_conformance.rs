//! Live-VM conformance for the protocol client against a real microVM.
//!
//! This is the AC5/AC6/AC9 evidence harness for Epic 1: it builds/spawns the
//! real `nanosb-runtime` helper (`serve`), connects `nanosb-client`, and drives
//! the operations that require a real microVM — lifecycle (create/get/exec/fs/
//! logs/metrics/list/stop/remove), secrets, pause/resume, plus volumes and
//! snapshot create/restore (which are blocked in this build; the exact blocker
//! is asserted so a change in behaviour is caught).
//!
//! Opt-in: requires the `nanosb-runtime` binary, libkrun/gvproxy, and a cached
//! image. Run with:
//!
//!   cargo build -p nanosb-cli --bin nanosb-runtime --no-default-features --features runtime-host
//!   cargo test -p nanosb-client --test live_conformance -- --ignored --nocapture
//!
//! Override the binary with `NANOSB_RUNTIME_BIN` and the image with
//! `NANOSB_TEST_IMAGE` (default `library/alpine:latest`). See
//! `docs/live-verification.md` for the recorded outcome.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use nanosb_client::{SandboxBuilder, SandboxClient, VolumeBuilder};
use nanosb_protocol::{Capability, ExecRequest, GuestEvent, SandboxState};

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
    name: String,
    sandbox_dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::remove_dir_all(&self.sandbox_dir);
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
    let dir = std::env::temp_dir().join(format!("nsb-live-{}", std::process::id()));
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
    let name = format!("sdk-live-{}", std::process::id());
    Daemon {
        child,
        socket,
        dir,
        name: name.clone(),
        sandbox_dir: nanosb_client::sandbox_dir(&name),
    }
}

#[test]
#[ignore = "requires nanosb-runtime + libkrun + a cached image"]
fn live_conformance_smoke() {
    let daemon = start_daemon();
    let client = SandboxClient::connect(&daemon.socket).expect("connect");
    assert!(!client.capabilities().is_empty());

    let image =
        std::env::var("NANOSB_TEST_IMAGE").unwrap_or_else(|_| "library/alpine:latest".into());
    let name = daemon.name.clone();

    // ── lifecycle: create boots a real microVM ─────────────────────────
    let sandbox = client
        .create_with(
            SandboxBuilder::new(&image)
                .name(&name)
                .cpus(2)
                .memory_mb(1024),
        )
        .expect("create");
    assert_eq!(sandbox.state().unwrap(), SandboxState::Running);

    // get
    let info = client.get(&name).expect("get");
    assert_eq!(info.name, name);
    assert_eq!(info.state, SandboxState::Running);
    assert_eq!(info.image, image);

    // exec
    let out = sandbox.exec("echo", &["hello-from-live"]).expect("exec");
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout.trim(), "hello-from-live");

    // fs
    sandbox
        .fs_write("/tmp/live-conformance", b"written-by-live")
        .expect("fs_write");
    assert_eq!(
        sandbox.fs_read_to_string("/tmp/live-conformance").unwrap(),
        "written-by-live"
    );
    assert!(sandbox.fs_exists("/tmp/live-conformance").unwrap());
    sandbox.fs_mkdir("/tmp/live-conformance-dir", true).unwrap();
    assert!(sandbox.fs_stat("/tmp/live-conformance-dir").unwrap().is_dir);

    // logs + metrics
    assert!(!sandbox.logs(Some(10)).unwrap().is_empty());
    let metrics = sandbox.metrics().unwrap();
    assert!(metrics.disk_used_bytes > 0);

    // list
    assert!(client.list(true).unwrap().iter().any(|s| s.name == name));

    // ── secrets (AC7: values are never returned) ───────────────────────
    sandbox
        .secret_set("LIVE_TOKEN", "s3cr3t")
        .expect("secret_set");
    let secrets = sandbox.secret_list().unwrap();
    assert!(secrets.iter().any(|s| s.key == "LIVE_TOKEN"));
    assert!(secrets.iter().all(|s| s.value.is_none()));
    sandbox.secret_remove("LIVE_TOKEN").expect("secret_remove");
    assert!(sandbox
        .secret_list()
        .unwrap()
        .iter()
        .all(|s| s.key != "LIVE_TOKEN"));

    // ── pause / resume (SIGSTOP / SIGCONT on the VM process) ───────────
    sandbox.pause().expect("pause");
    assert_eq!(sandbox.state().unwrap(), SandboxState::Running);
    sandbox.resume().expect("resume");
    let after = sandbox
        .exec("echo", &["after-resume"])
        .expect("exec after resume");
    assert_eq!(after.exit_code, 0);
    assert_eq!(after.stdout.trim(), "after-resume");

    // ── volumes (AC5): served through the helper socket ────────────────
    let vol_name = format!("vol-{}", std::process::id());
    let vol_dir = daemon.dir.join("vol-data");
    assert!(
        client.supports(Capability::Volumes),
        "the helper must advertise the Volumes capability"
    );
    let vol = sandbox
        .volume_create(
            VolumeBuilder::new(&vol_name)
                .dir(vol_dir.to_string_lossy().to_string())
                .build(),
        )
        .expect("volume_create via helper socket");
    assert_eq!(vol.name, vol_name);
    assert!(sandbox
        .volume_list()
        .unwrap()
        .iter()
        .any(|v| v.name == vol_name));
    assert_eq!(sandbox.volume_get(&vol_name).unwrap().name, vol_name);
    sandbox.volume_remove(&vol_name).expect("volume_remove");
    assert!(sandbox
        .volume_list()
        .unwrap()
        .iter()
        .all(|v| v.name != vol_name));

    // ── snapshots (AC6): disk-only, served through the helper socket ────
    assert!(
        client.supports(Capability::Snapshots),
        "the helper must advertise the Snapshots capability"
    );
    let snap = sandbox
        .snapshot_create(None)
        .expect("snapshot_create via helper socket");
    assert_eq!(snap.sandbox, name);
    assert!(!snap.includes_memory);
    assert!(client.snapshots().unwrap().iter().any(|s| s.id == snap.id));
    let restored = format!("{}-restored", name);
    sandbox
        .snapshot_restore(&snap.id, &restored)
        .expect("snapshot_restore via helper socket");
    sandbox.snapshot_remove(&snap.id).expect("snapshot_remove");
    assert!(client.snapshots().unwrap().iter().all(|s| s.id != snap.id));
    // The restore materialised a new, independent sandbox directory; tear it
    // down too so the run leaves nothing behind (AC-E5).
    client
        .sandbox(&restored)
        .remove(true)
        .expect("remove restored sandbox");

    // ── exec_stream (AC5): streamed stdout + exit code ─────────────────
    let mut stream_out = String::new();
    let mut req = ExecRequest::new("echo");
    req.name = Some(name.clone());
    req.args = vec!["hello-stream".to_string()];
    let code = sandbox
        .exec_stream(req, |ev| {
            if let GuestEvent::Stdout { data } = ev {
                stream_out.push_str(&data);
            }
        })
        .expect("exec_stream");
    assert_eq!(code, 0);
    assert_eq!(stream_out.trim(), "hello-stream");

    // ── shell (AC5): buffered shell script ─────────────────────────────
    let sh = sandbox.shell("echo hello-shell").expect("shell");
    assert_eq!(sh.exit_code, 0);
    assert_eq!(sh.stdout.trim(), "hello-shell");

    // ── shell_stream (AC5): streamed shell script ──────────────────────
    let mut sh_stream_out = String::new();
    let sh_code = sandbox
        .shell_stream("echo hello-shell-stream", |ev| {
            if let GuestEvent::Stdout { data } = ev {
                sh_stream_out.push_str(&data);
            }
        })
        .expect("shell_stream");
    assert_eq!(sh_code, 0);
    assert_eq!(sh_stream_out.trim(), "hello-shell-stream");

    // ── attach (AC5): open the console stream via the helper ───────────
    let session = sandbox.attach_session().expect("attach_session via helper");
    drop(session);

    // ── attach input/resize: driven on the sandbox control socket ──────
    // The helper relays the console stream outward; client->guest attach
    // control frames are served by the supervisor on the sandbox socket.
    let direct = SandboxClient::for_sandbox(&name).expect("connect to sandbox control socket");
    let mut direct_session = direct
        .sandbox(&name)
        .attach_session()
        .expect("attach_session via sandbox socket");
    direct_session
        .send_input(b"echo hello-attach\n")
        .expect("attach_input");
    direct_session.resize(120, 40).expect("attach_resize");
    drop(direct_session);

    // ── lifecycle teardown ─────────────────────────────────────────────
    sandbox.stop(true).expect("stop");
    client.sandbox(&name).remove(true).expect("remove");
    assert!(client.list(true).unwrap().iter().all(|s| s.name != name));
}
