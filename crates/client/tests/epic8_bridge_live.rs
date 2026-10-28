//! Live-VM assertion for Epic 8 (Docker bridge networking, Tier 2).
//!
//! Boots a real microVM with Docker in `bridge` mode and a published host
//! port, pulls and runs nginx on Docker's default bridge, and proves the host
//! can reach the container through gvproxy port forwarding.
//!
//! Opt-in (needs the helper, libkrun/gvproxy, and a cached Docker-enabled image):
//!
//!   NANOSB_RUNTIME_BIN="$PWD/target/debug/nanosb-runtime" \
//!   NANOSB_TEST_DOCKER_IMAGE=localhost:5050/claude-docker:latest \
//!     cargo test -p nanosb-client --test epic8_bridge_live -- --ignored --nocapture

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use nanosb_client::SandboxClient;
use nanosb_protocol::{
    DockerConfig, DockerNetwork, ExecRequest, GuestEvent, PortPublish, PullPolicy, SandboxSpec,
    SandboxState,
};

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
    let dir = std::env::temp_dir().join(format!("nsb-e8-{}", std::process::id()));
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

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().expect("local addr").port()
}

fn wait_for_docker(sandbox: &nanosb_client::Sandbox) -> String {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(out) = sandbox.exec("docker", &["info"]) {
            if out.exit_code == 0 {
                return out.stdout;
            }
        }
        assert!(
            Instant::now() < deadline,
            "dockerd did not become ready within 120s"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Execute a command over the streaming channel so long-running Docker
/// operations are not cut off by the client's 30s per-request read timeout.
fn exec_stream_collect(
    sandbox: &nanosb_client::Sandbox,
    name: &str,
    command: &str,
    args: &[&str],
    timeout_secs: u64,
) -> (i32, String, String) {
    let mut req = ExecRequest::new(command);
    req.name = Some(name.to_string());
    req.args = args.iter().map(|s| s.to_string()).collect();
    req.timeout_secs = Some(timeout_secs);
    let mut stdout = String::new();
    let mut stderr = String::new();
    let code = sandbox
        .exec_stream(req, |ev| match ev {
            GuestEvent::Stdout { data } => stdout.push_str(&data),
            GuestEvent::Stderr { data } => stderr.push_str(&data),
            _ => {}
        })
        .expect("exec_stream");
    (code, stdout, stderr)
}

fn host_http_get(port: u16) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    Some(buf)
}

fn wait_for_http(port: u16, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(resp) = host_http_get(port) {
            return resp;
        }
        assert!(
            Instant::now() < deadline,
            "host port {} never became reachable",
            port
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "requires nanosb-runtime + libkrun + gvproxy + a cached Docker-enabled image"]
fn epic8_bridge_live() {
    let daemon = start_daemon();
    let client = SandboxClient::connect(&daemon.socket).expect("connect");
    let image = std::env::var("NANOSB_TEST_DOCKER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/nanosandboxai/agents-registry/claude-docker:latest".into());
    let name = format!("e8-{}", std::process::id());
    let host_port = free_port();

    let mut spec = SandboxSpec::new(&image);
    spec.name = Some(name.clone());
    spec.cpus = Some(2);
    spec.memory_mb = Some(3072);
    spec.pull_policy = PullPolicy::Never;
    spec.docker = Some(DockerConfig {
        enabled: true,
        data_size_bytes: 4 * 1024 * 1024 * 1024,
        network: DockerNetwork::Bridge,
        ..Default::default()
    });
    // `NetworkSpec::default()` is `enabled: false` (its `default_true` serde
    // attribute is deserialize-only); bridge mode needs the gvproxy NIC.
    spec.network.enabled = true;
    // gvproxy forwards host 127.0.0.1:<host_port> to guest 192.168.127.2:80.
    spec.network.publish.push(PortPublish {
        host_port,
        container_port: 80,
        protocol: "tcp".to_string(),
        bind_address: None,
    });
    // Use gvproxy's built-in DNS resolver so the guest can pull from Docker Hub.
    spec.network.dns = vec!["192.168.127.1".to_string()];

    let sandbox = client.create(spec).expect("create");
    assert_eq!(sandbox.state().unwrap(), SandboxState::Running);

    let info = wait_for_docker(&sandbox);
    assert!(
        info.contains("Storage Driver: overlay2"),
        "expected overlay2, got:\n{info}"
    );

    // The bridge network must exist when dockerd starts in bridge mode.
    let networks = sandbox
        .exec("docker", &["network", "ls", "--format", "{{.Name}}"])
        .expect("docker network ls");
    assert_eq!(
        networks.exit_code, 0,
        "network ls failed: {}",
        networks.stderr
    );
    assert!(
        networks.stdout.lines().any(|l| l == "bridge"),
        "Docker's bridge network missing:\n{}",
        networks.stdout
    );

    // Pull and run nginx on the default bridge, publishing guest port 80.
    let (code, out, err) =
        exec_stream_collect(&sandbox, &name, "docker", &["pull", "nginx:alpine"], 600);
    assert_eq!(code, 0, "docker pull failed: {err}\nstdout: {out}");

    let (code, out, err) = exec_stream_collect(
        &sandbox,
        &name,
        "docker",
        &[
            "run",
            "-d",
            "--name",
            "e8-nginx",
            "--network",
            "bridge",
            "-p",
            "80:80",
            "nginx:alpine",
        ],
        120,
    );
    assert_eq!(code, 0, "docker run failed: {err}\nstdout: {out}");

    let inspect = sandbox
        .exec(
            "docker",
            &[
                "inspect",
                "-f",
                "{{.State.Running}} {{.HostConfig.NetworkMode}}",
                "e8-nginx",
            ],
        )
        .expect("docker inspect");
    assert_eq!(inspect.exit_code, 0, "inspect failed: {}", inspect.stderr);
    assert!(
        inspect.stdout.trim().starts_with("true"),
        "container is not running: {}",
        inspect.stdout
    );

    // Prove end-to-end host reachability through gvproxy + Docker bridge NAT.
    let resp = wait_for_http(host_port, Duration::from_secs(90));
    assert!(
        resp.starts_with("HTTP/1.1 200") || resp.starts_with("HTTP/1.0 200"),
        "unexpected nginx response:\n{resp}"
    );
    assert!(
        resp.contains("Welcome to nginx"),
        "nginx welcome page missing:\n{resp}"
    );

    let _ = sandbox.exec("docker", &["rm", "-f", "e8-nginx"]);
    sandbox.stop(true).expect("stop");
    let _ = sandbox.remove(true);
    cleanup(&name);
}
