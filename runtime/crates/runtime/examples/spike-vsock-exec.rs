//! Spike: next-mode exec channel over virtio-vsock.
//!
//! Proves the dedicated host<->guest channel that the exec feature builds on:
//!   1. Host unix socket  <->  libkrun `krun_add_vsock_port`  <->  guest vsock
//!   2. The guest runs `exec-agent`, listening on the vsock port.
//!   3. Host sends a length-prefixed JSON `ExecRequest`, receives `ExecEvent`s.
//!
//! Usage:
//!   # 1. build the guest agent for the guest arch
//!   RUSTFLAGS="-C linker=rust-lld" cargo build -p exec-agent \
//!     --target aarch64-unknown-linux-musl --release
//!   # 2. run the spike
//!   cargo run --example spike-vsock-exec -- --image alpine:latest
//!
//! Requires: libkrun.a, libkrunfw, codesigned nanosb binary (NANOSB_BINARY_PATH).

use runtime::config::{ConsoleSpec, RuntimeMode, SandboxConfig};
use runtime::runtime::handle_boot_vm_subprocess;
use runtime::Sandbox;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

const VM_VSOCK_PORT: u32 = 1024;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        handle_boot_vm_subprocess();
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async_main());
}

async fn async_main() {
    tracing_subscriber::fmt()
        .with_env_filter("runtime=debug")
        .with_target(false)
        .init();

    let image = arg("--image").unwrap_or_else(|| "alpine:latest".to_string());
    let agent_host = arg("--agent")
        .unwrap_or_else(|| "target/aarch64-unknown-linux-musl/release/exec-agent".to_string());

    println!("=== Spike: next-mode exec over vsock ===");
    println!("Image: {}", image);
    println!("Agent: {}", agent_host);

    if !std::path::Path::new(&agent_host).exists() {
        eprintln!("agent binary not found: {}", agent_host);
        eprintln!("build it: RUSTFLAGS=\"-C linker=rust-lld\" cargo build -p exec-agent \\");
        eprintln!("            --target aarch64-unknown-linux-musl --release");
        return;
    }

    // Host dir shared into the guest at /agent (injected at boot, D3b).
    let agent_dir = std::env::temp_dir().join("nanosb-spike-vsock-agent");
    let _ = std::fs::remove_dir_all(&agent_dir);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::copy(&agent_host, agent_dir.join("exec-agent")).unwrap();

    // Host unix socket bridged to the guest vsock port by libkrun.
    let vsock_sock = std::env::temp_dir().join(format!("nanosb-vsock-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&vsock_sock);

    let (output_rx, output_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_stderr_rx, stderr_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let console = ConsoleSpec {
        stdin_fd: 0,
        stdout_fd: output_tx.as_raw_fd(),
        stderr_fd: stderr_tx.as_raw_fd(),
        tty: false,
    };

    let mut config = SandboxConfig::builder()
        .name("spike-vsock-exec")
        .image(&image)
        .cpus(1)
        .memory_mb(256)
        .runtime_mode(RuntimeMode::Next)
        .console(console)
        .extra_mount("agent", agent_dir.to_string_lossy(), "/agent", false)
        .vsock_bridge(vsock_sock.to_string_lossy(), VM_VSOCK_PORT)
        .timeout_secs(120)
        .build();

    // PID 1: run the injected agent directly from the virtiofs mount.
    config.command = Some("/bin/sh".to_string());
    config.command_args = vec![
        "-c".to_string(),
        "echo PROBE_START; /agent/exec-agent 1024; echo AGENT_EXIT=$?"
            .to_string(),
    ];

    let start = Instant::now();
    let mut sandbox = match Sandbox::create(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[{:.1}s] create failed: {}", start.elapsed().as_secs_f64(), e);
            return;
        }
    };

    let _out = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut r = std::io::BufReader::new(output_rx);
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            print!("[console] {}", String::from_utf8_lossy(&buf[..n]));
        }
    });

    if let Err(e) = sandbox.start().await {
        eprintln!("[{:.1}s] start failed: {}", start.elapsed().as_secs_f64(), e);
        let _ = sandbox.stop().await;
        let _ = sandbox.destroy().await;
        return;
    }
    println!("[{:.1}s] VM started", start.elapsed().as_secs_f64());

    // Give the guest agent a moment to bind vsock.
    tokio::time::sleep(Duration::from_secs(8)).await;

    // === The actual transport test ===
    let ok = match connect_and_exec(&vsock_sock) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("vsock exec FAILED: {}", e);
            false
        }
    };

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;
    println!("[{:.1}s] Done", start.elapsed().as_secs_f64());
    println!(
        "=== Spike {} ===",
        if ok { "PASSED" } else { "FAILED" }
    );
    let _ = std::fs::remove_file(&vsock_sock);
}

fn arg(name: &str) -> Option<String> {
    std::env::args()
        .position(|a| a == name)
        .and_then(|i| std::env::args().nth(i + 1))
}

/// Drive the sandbox through the real host-side `ExecClient` (Phase 3).
fn connect_and_exec(sock: &std::path::Path) -> Result<(), String> {
    // Wait for libkrun to expose the host socket (the VM boot + agent listen).
    let client = runtime::exec::ExecClient::new(sock);
    for _ in 0..50 {
        if client.is_available() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // 1. Buffered exec: `echo VSOCK_EXEC_OK`.
    let res = client
        .exec("echo", &["VSOCK_EXEC_OK"])
        .map_err(|e| format!("exec failed: {}", e))?;
    println!("[client] stdout={:?} exit={}", res.stdout, res.exit_code);
    if !res.stdout.contains("VSOCK_EXEC_OK") {
        return Err(format!("expected VSOCK_EXEC_OK, got {:?}", res.stdout));
    }
    if res.exit_code != 0 {
        return Err(format!("expected exit 0, got {}", res.exit_code));
    }

    // 2. Streaming exec: multi-line stdout + stderr, non-zero exit.
    let mut lines = String::new();
    let code = client
        .exec_stream(
            "echo line1; echo line2; echo err1 >&2; exit 7",
            &[],
            runtime::exec::ExecOptions::new().shell(true),
            |chunk| {
                if chunk.stream == runtime::exec::Stream::Stdout {
                    lines.push_str(&chunk.data);
                }
            },
        )
        .map_err(|e| format!("exec_stream failed: {}", e))?;
    if !lines.contains("line1") || !lines.contains("line2") {
        return Err(format!("shell streaming missing lines: {:?}", lines));
    }
    if code != 7 {
        return Err(format!("expected exit 7, got {}", code));
    }
    println!("[client] shell streaming OK (exit {})", code);

    Ok(())
}
