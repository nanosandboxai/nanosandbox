//! Spike (c): Network/DNS from a vanilla rootfs
//!
//! Boots a vanilla Alpine rootfs in next mode with gvproxy networking and
//! static IP configuration (192.168.127.2/24). Verifies that:
//!   - eth0 is up with the correct IP
//!   - Default gateway is reachable (192.168.127.1 = gvproxy)
//!   - DNS resolution works (via gvproxy's built-in DNS)
//!   - Outbound HTTP works (wget/curl to a known endpoint)
//!
//! Usage:
//!   cargo run --example spike-network-dns -- --image alpine:latest
//!
//! Requires the libkrun init patches to be applied (see runtime/scripts/patches/).
//! Without the patches, the virtio-net device is registered but the guest won't
//! configure eth0.
//!
//! Timebox: ~3 minutes.

use runtime::config::{ConsoleSpec, RuntimeMode, SandboxConfig};
use runtime::runtime::handle_boot_vm_subprocess;
use runtime::Sandbox;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

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

    let image = std::env::args()
        .position(|a| a == "--image")
        .and_then(|i| std::env::args().nth(i + 1))
        .unwrap_or_else(|| "alpine:latest".to_string());

    println!("=== Spike (c): Network/DNS from vanilla rootfs ===");
    println!("Image: {}", image);
    println!("");

    let (input_rx, _input_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (output_rx, output_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_stderr_rx, stderr_tx) = std::os::unix::net::UnixStream::pair().unwrap();

    let console = ConsoleSpec {
        stdin_fd: input_rx.as_raw_fd(),
        stdout_fd: output_tx.as_raw_fd(),
        stderr_fd: stderr_tx.as_raw_fd(),
        tty: false,
    };

    let mut config = SandboxConfig::builder()
        .name("spike-network-dns")
        .image(&image)
        .cpus(1)
        .memory_mb(256)
        .runtime_mode(RuntimeMode::Next)
        .console(console)
        .network_enabled(true)
        .timeout_secs(120)
        .build();

    config.command = Some("/bin/sh".to_string());
    config.command_args = vec![
        "-c".to_string(),
        "echo '=== Guest network check ==='; echo '--- IP config ---'; ip addr show eth0 2>&1 || ifconfig eth0 2>&1 || echo 'no ip cmd'; echo '--- Gateway ---'; ip route show default 2>&1 || route -n 2>&1 || echo 'no route cmd'; echo '--- DNS ---'; cat /etc/resolv.conf 2>&1 || echo 'no resolv.conf'; echo '--- Ping gateway ---'; ping -c 1 -W 3 192.168.127.1 2>&1 || echo 'ping failed'; echo '--- DNS resolution ---'; nslookup google.com 2>&1 || host google.com 2>&1 || echo 'no dns tools'; echo '--- HTTP test ---'; wget -q -O - http://httpbin.org/ip 2>&1 || curl -s http://httpbin.org/ip 2>&1 || echo 'http failed'; echo '=== Guest network check complete ==='; sleep 2".to_string(),
    ];

    let start = Instant::now();
    let mut sandbox = match Sandbox::create(config).await {
        Ok(s) => s,
        Err(e) => { eprintln!("[{:.1}s] Failed: {}", start.elapsed().as_secs_f64(), e); return; }
    };
    println!("[{:.1}s] Sandbox created", start.elapsed().as_secs_f64());

    let _output_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut reader = std::io::BufReader::new(output_rx);
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => print!("{}", String::from_utf8_lossy(&buf[..n])),
                Err(e) => { eprintln!("[read err] {}", e); break; }
            }
        }
    });

    if let Err(e) = sandbox.start().await {
        eprintln!("[{:.1}s] Start failed: {}", start.elapsed().as_secs_f64(), e);
        let _ = sandbox.stop().await;
        let _ = sandbox.destroy().await;
        return;
    }
    println!("[{:.1}s] VM started", start.elapsed().as_secs_f64());

    tokio::time::sleep(Duration::from_secs(20)).await;

    println!("[{:.1}s] Stopping VM...", start.elapsed().as_secs_f64());
    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;
    println!("[{:.1}s] Done", start.elapsed().as_secs_f64());
    println!("=== Spike (c) complete ===");
}
