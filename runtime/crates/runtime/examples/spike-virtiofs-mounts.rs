//! Spike (b): Extra virtiofs mount visibility (RW + RO)
//!
//! Boots a vanilla Alpine rootfs in next mode with two extra virtiofs mounts:
//!   - A RW mount at /mnt/workspace (host temp dir)
//!   - A RO mount at /mnt/config (host temp dir)
//!
//! Verifies that:
//!   - Files written on the host appear in the guest at the target path
//!   - RO mount rejects writes inside the guest
//!
//! Usage:
//!   cargo run --example spike-virtiofs-mounts -- --image alpine:latest
//!
//! Requires the libkrun init patches to be applied (see runtime/scripts/patches/).
//! Without the patches, extra virtiofs tags are registered on the host side
//! but the guest init won't mount them.
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

    println!("=== Spike (b): Extra virtiofs mount visibility ===");
    println!("Image: {}", image);
    println!("");

    let workspace_dir = std::env::temp_dir().join(format!("nanosb-spike-workspace-{}", std::process::id()));
    let config_dir = std::env::temp_dir().join(format!("nanosb-spike-config-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();

    std::fs::write(workspace_dir.join("hello.txt"), "Hello from host workspace!").unwrap();
    std::fs::write(config_dir.join("config.yaml"), "key: value\n").unwrap();

    println!("Host workspace dir: {}", workspace_dir.display());
    println!("Host config dir: {}", config_dir.display());
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
        .name("spike-virtiofs-mounts")
        .image(&image)
        .cpus(1)
        .memory_mb(256)
        .runtime_mode(RuntimeMode::Next)
        .console(console)
        .extra_mount(
            "workspace",
            workspace_dir.to_string_lossy(),
            "/mnt/workspace",
            false,
        )
        .extra_mount(
            "config",
            config_dir.to_string_lossy(),
            "/mnt/config",
            true,
        )
        .timeout_secs(120)
        .build();

    config.command = Some("/bin/sh".to_string());
    config.command_args = vec![
        "-c".to_string(),
        "echo '=== Guest mount check ==='; ls -la /mnt/workspace/ 2>&1 || echo 'workspace NOT mounted'; ls -la /mnt/config/ 2>&1 || echo 'config NOT mounted'; echo '---'; cat /mnt/workspace/hello.txt 2>&1 || echo 'hello.txt not found'; cat /mnt/config/config.yaml 2>&1 || echo 'config.yaml not found'; echo '---'; echo 'Trying to write to RO mount...'; touch /mnt/config/test-write 2>&1 && echo 'RO WRITE SUCCEEDED (BUG!)' || echo 'RO write correctly rejected'; echo '---'; echo 'Trying to write to RW mount...'; echo 'guest-data' > /mnt/workspace/guest.txt 2>&1 && echo 'RW write OK' || echo 'RW write FAILED'; echo '=== Guest mount check complete ==='; sleep 2".to_string(),
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

    tokio::time::sleep(Duration::from_secs(15)).await;

    let guest_file = workspace_dir.join("guest.txt");
    if guest_file.exists() {
        let content = std::fs::read_to_string(&guest_file).unwrap();
        println!("[{:.1}s] Guest wrote to workspace: {:?}", start.elapsed().as_secs_f64(), content.trim());
    } else {
        println!("[{:.1}s] Guest file not found (init patches may not be applied)", start.elapsed().as_secs_f64());
    }

    println!("[{:.1}s] Stopping VM...", start.elapsed().as_secs_f64());
    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;

    let _ = std::fs::remove_dir_all(&workspace_dir);
    let _ = std::fs::remove_dir_all(&config_dir);

    println!("[{:.1}s] Done", start.elapsed().as_secs_f64());
    println!("=== Spike (b) complete ===");
}
