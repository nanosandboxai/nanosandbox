//! Live-VM assertion for Epic 7 (Docker inside the microVM, Tier 1).
//!
//! Boots a real microVM with a Docker-enabled agent image, waits for the guest
//! `dockerd` bootstrap, and proves:
//! - the daemon runs on the dedicated ext4 data disk (`overlay2`);
//! - `docker run --network host hello-world` succeeds inside the sandbox.
//!
//! Opt-in (needs the helper, libkrun/gvproxy, and a cached Docker-enabled image):
//!
//!   NANOSB_TEST_DOCKER_IMAGE=localhost:5050/claude-docker:latest \
//!   cargo test -p nanosb-client --test epic7_docker_live -- --ignored --nocapture

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use nanosb_client::SandboxClient;
use nanosb_protocol::{DockerConfig, PullPolicy, SandboxSpec, SandboxState};

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/nanosb-runtime")
}

fn start_daemon() -> Daemon {
    let dir = std::env::temp_dir().join(format!("nsb-e7-{}", std::process::id()));
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

fn cleanup(name: &str) {
    let _ = std::fs::remove_dir_all(nanosb_client::sandbox_dir(name));
}

fn wait_for_docker(sandbox: &nanosb_client::Sandbox) -> String {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Ok(out) = sandbox.exec("docker", &["info"]) {
            if out.exit_code == 0 {
                return out.stdout;
            }
        }
        assert!(
            Instant::now() < deadline,
            "dockerd did not become ready within 90s"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "requires nanosb-runtime + libkrun + a cached Docker-enabled image"]
fn epic7_docker_live() {
    let daemon = start_daemon();
    let client = SandboxClient::connect(&daemon.socket).expect("connect");
    let image = std::env::var("NANOSB_TEST_DOCKER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/nanosandboxai/agents-registry/claude-docker:latest".into());
    let name = format!("e7-{}", std::process::id());

    let mut spec = SandboxSpec::new(&image);
    spec.name = Some(name.clone());
    spec.cpus = Some(2);
    spec.memory_mb = Some(3072);
    spec.pull_policy = PullPolicy::Never;
    spec.docker = Some(DockerConfig {
        enabled: true,
        data_size_bytes: 4 * 1024 * 1024 * 1024,
        ..Default::default()
    });

    let sandbox = client.create(spec).expect("create");
    assert_eq!(sandbox.state().unwrap(), SandboxState::Running);

    let info = wait_for_docker(&sandbox);
    assert!(
        info.contains("Storage Driver: overlay2"),
        "expected overlay2, got:\n{info}"
    );
    assert!(
        info.contains("Docker Root Dir: /var/lib/docker"),
        "expected /var/lib/docker, got:\n{info}"
    );

    let mount = sandbox
        .exec("findmnt", &["-no", "FSTYPE", "/var/lib/docker"])
        .expect("findmnt");
    assert_eq!(
        mount.stdout.trim(),
        "ext4",
        "docker data disk is not ext4: {:?}",
        mount.stdout
    );

    let run = sandbox
        .exec(
            "docker",
            &["run", "--rm", "--network", "host", "hello-world"],
        )
        .expect("docker run");
    assert_eq!(run.exit_code, 0, "docker run failed: {}", run.stderr);
    assert!(
        run.stdout.contains("Hello from Docker!"),
        "unexpected output:\n{}",
        run.stdout
    );

    // The code agent runs as the non-root `developer` user; it must be able to
    // use the root-owned daemon through the socket group.
    let socket_group = sandbox
        .exec("stat", &["-c", "%G", "/var/run/docker.sock"])
        .expect("stat socket");
    assert_eq!(socket_group.stdout.trim(), "developer");

    let agent_info = sandbox
        .exec("su", &["-s", "/bin/sh", "developer", "-c", "docker info"])
        .expect("agent docker info");
    assert_eq!(
        agent_info.exit_code, 0,
        "agent user cannot reach dockerd: {}",
        agent_info.stderr
    );

    let agent_run = sandbox
        .exec(
            "su",
            &[
                "-s",
                "/bin/sh",
                "developer",
                "-c",
                "docker run --rm --network host hello-world",
            ],
        )
        .expect("agent docker run");
    assert_eq!(agent_run.exit_code, 0, "agent docker run failed");
    assert!(agent_run.stdout.contains("Hello from Docker!"));

    let claude = sandbox
        .exec("su", &["-s", "/bin/sh", "developer", "-c", "claude --version"])
        .expect("claude version");
    assert_eq!(claude.exit_code, 0, "claude not runnable as developer");

    sandbox.stop(true).expect("stop");
    let _ = sandbox.remove(true);
    cleanup(&name);
}
