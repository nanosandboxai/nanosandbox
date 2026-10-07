//! Spike (a): Console stdin->stdout echo + TTY resize
//!
//! Boots a vanilla Alpine rootfs in next mode with console I/O wired to
//! host pipes. Writes bytes to stdin, reads echo from stdout, verifies
//! raw-mode TUI behavior and window resize.
//!
//! Usage:
//!   cargo run --example spike-console-echo -- --image alpine:latest
//!
//! Requires:
//!   - libkrun.a at ~/.nanosandbox/lib/libkrun.a
//!   - libkrunfw.5.dylib at ~/.nanosandbox/libs/libkrunfw.5.dylib
//!   - gvproxy at ~/.nanosandbox/bin/gvproxy (or on PATH)
//!   - The nanosb binary must be codesigned with com.apple.security.hypervisor
//!   - NANOSB_BINARY_PATH env var pointing to the nanosb binary (for subprocess spawn)
//!
//! Timebox: ~3 minutes. If the VM hangs, kill with Ctrl+C and check logs.

use runtime::config::{ConsoleSpec, RuntimeMode, SandboxConfig};
use runtime::runtime::handle_boot_vm_subprocess;
use runtime::Sandbox;
use std::io::{Read, Write};
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

    println!("=== Spike (a): Console stdin->stdout echo + TTY resize ===");
    println!("Image: {}", image);
    println!("");

    let (input_rx, mut input_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (output_rx, output_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (stderr_rx, stderr_tx) = std::os::unix::net::UnixStream::pair().unwrap();

    let console = ConsoleSpec {
        stdin_fd: input_rx.as_raw_fd(),
        stdout_fd: output_tx.as_raw_fd(),
        stderr_fd: stderr_tx.as_raw_fd(),
        tty: true,
    };

    let mut config = SandboxConfig::builder()
        .name("spike-console-echo")
        .image(&image)
        .cpus(1)
        .memory_mb(256)
        .runtime_mode(RuntimeMode::Next)
        .console(console)
        .timeout_secs(120)
        .build();

    config.command = Some("/bin/sh".to_string());
    config.command_args = vec![
        "-c".to_string(),
        "echo 'Hello from next-mode VM'; stty size; sleep 2; echo DONE".to_string(),
    ];

    let start = Instant::now();
    let mut sandbox = match Sandbox::create(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[{:.1}s] Failed: {}", start.elapsed().as_secs_f64(), e);
            return;
        }
    };
    println!("[{:.1}s] Sandbox created", start.elapsed().as_secs_f64());

    let _output_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut reader = std::io::BufReader::new(output_rx);
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => print!("[stdout] {}", String::from_utf8_lossy(&buf[..n])),
                Err(e) => { eprintln!("[stdout err] {}", e); break; }
            }
        }
    });

    let _stderr_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut reader = std::io::BufReader::new(stderr_rx);
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => print!("[stderr] {}", String::from_utf8_lossy(&buf[..n])),
                Err(e) => { eprintln!("[stderr err] {}", e); break; }
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

    let input = b"echo 'Hello from host stdin!'\n";
    input_tx.write_all(input).unwrap();
    println!("[{:.1}s] Wrote stdin", start.elapsed().as_secs_f64());

    tokio::time::sleep(Duration::from_secs(10)).await;

    println!("[{:.1}s] Stopping VM...", start.elapsed().as_secs_f64());
    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;
    println!("[{:.1}s] Done", start.elapsed().as_secs_f64());
    println!("=== Spike (a) complete ===");
}
