//! Next-mode integration tests for vanilla-image boot: console I/O, extra
//! virtiofs mounts, and microVM-layer networking.
//!
//! Requires: patched libkrun (`runtime/scripts/patches/`), gvproxy, and the
//! Hypervisor entitlement on the test binary (`runtime/scripts/codesign-and-run.sh`).
//! Run: cargo test -p runtime --test next_mode_test -- --ignored --nocapture

use runtime::config::SandboxConfig;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore]
async fn test_next_mode_console_boot() {
    use runtime::config::{ConsoleSpec, RuntimeMode};
    use runtime::runtime::handle_boot_vm_subprocess;

    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        handle_boot_vm_subprocess();
    }

    let (output_rx, output_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_stderr_rx, stderr_tx) = std::os::unix::net::UnixStream::pair().unwrap();

    let console = ConsoleSpec {
        stdin_fd: 0,
        stdout_fd: output_tx.as_raw_fd(),
        stderr_fd: stderr_tx.as_raw_fd(),
        tty: false,
    };

    let mut config = SandboxConfig::builder()
        .name("test-next-console")
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .runtime_mode(RuntimeMode::Next)
        .console(console)
        .timeout_secs(120)
        .build();

    config.command = Some("/bin/sh".to_string());
    config.command_args = vec![
        "-c".to_string(),
        "echo 'NEXT_MODE_CONSOLE_OK'; sleep 1".to_string(),
    ];

    let start = Instant::now();
    let mut sandbox = runtime::Sandbox::create(config).await.unwrap();
    println!("[{:.1}s] Sandbox created", start.elapsed().as_secs_f64());

    let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let output_clone = output.clone();
    let _handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut reader = std::io::BufReader::new(output_rx);
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut out = output_clone.lock().unwrap();
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });

    sandbox.start().await.unwrap();
    println!("[{:.1}s] VM started", start.elapsed().as_secs_f64());

    tokio::time::sleep(Duration::from_secs(15)).await;

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;

    let output_str = output.lock().unwrap().clone();
    println!(
        "[{:.1}s] Console output: {:?}",
        start.elapsed().as_secs_f64(),
        output_str
    );
    assert!(
        output_str.contains("NEXT_MODE_CONSOLE_OK"),
        "Expected console output not found: {:?}",
        output_str
    );
}

#[tokio::test]
#[ignore]
async fn test_next_mode_extra_mounts() {
    use runtime::config::{ConsoleSpec, RuntimeMode};
    use runtime::runtime::handle_boot_vm_subprocess;

    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        handle_boot_vm_subprocess();
    }

    let host_dir =
        std::env::temp_dir().join(format!("nanosb-next-mount-{}", std::process::id()));
    std::fs::create_dir_all(&host_dir).unwrap();
    std::fs::write(host_dir.join("hello.txt"), "MOUNT_MARKER_OK\n").unwrap();

    let (output_rx, output_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_stderr_rx, stderr_tx) = std::os::unix::net::UnixStream::pair().unwrap();

    let console = ConsoleSpec {
        stdin_fd: 0,
        stdout_fd: output_tx.as_raw_fd(),
        stderr_fd: stderr_tx.as_raw_fd(),
        tty: false,
    };

    let mut config = SandboxConfig::builder()
        .name("test-next-mounts")
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .runtime_mode(RuntimeMode::Next)
        .console(console)
        .extra_mount(
            "testmount",
            host_dir.to_string_lossy(),
            "/mnt/test",
            false,
        )
        .timeout_secs(120)
        .build();

    config.command = Some("/bin/sh".to_string());
    config.command_args = vec![
        "-c".to_string(),
        "echo 'MOUNT_CHECK'; cat /mnt/test/hello.txt 2>&1 || echo 'MOUNT_NOT_VISIBLE'; sleep 1"
            .to_string(),
    ];

    let start = Instant::now();
    let mut sandbox = runtime::Sandbox::create(config).await.unwrap();
    println!("[{:.1}s] Sandbox created", start.elapsed().as_secs_f64());

    let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let output_clone = output.clone();
    let _handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut reader = std::io::BufReader::new(output_rx);
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut out = output_clone.lock().unwrap();
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });

    sandbox.start().await.unwrap();
    println!("[{:.1}s] VM started", start.elapsed().as_secs_f64());

    tokio::time::sleep(Duration::from_secs(15)).await;

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;
    let _ = std::fs::remove_dir_all(&host_dir);

    let output_str = output.lock().unwrap().clone();
    println!(
        "[{:.1}s] Console output: {:?}",
        start.elapsed().as_secs_f64(),
        output_str
    );
    assert!(
        output_str.contains("MOUNT_MARKER_OK"),
        "Expected mounted marker file content: {:?}",
        output_str
    );
}

#[tokio::test]
#[ignore]
async fn test_next_mode_network() {
    use runtime::config::{ConsoleSpec, RuntimeMode};
    use runtime::runtime::handle_boot_vm_subprocess;

    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        handle_boot_vm_subprocess();
    }

    let (output_rx, output_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_stderr_rx, stderr_tx) = std::os::unix::net::UnixStream::pair().unwrap();

    let console = ConsoleSpec {
        stdin_fd: 0,
        stdout_fd: output_tx.as_raw_fd(),
        stderr_fd: stderr_tx.as_raw_fd(),
        tty: false,
    };

    let mut config = SandboxConfig::builder()
        .name("test-next-network")
        .image("alpine:latest")
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
        "echo 'NET_CHECK'; ip addr show eth0 2>&1 || echo 'NO_ETH0'; sleep 1".to_string(),
    ];

    let start = Instant::now();
    let mut sandbox = runtime::Sandbox::create(config).await.unwrap();
    println!("[{:.1}s] Sandbox created", start.elapsed().as_secs_f64());

    let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let output_clone = output.clone();
    let _handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut reader = std::io::BufReader::new(output_rx);
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut out = output_clone.lock().unwrap();
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });

    sandbox.start().await.unwrap();
    println!("[{:.1}s] VM started", start.elapsed().as_secs_f64());

    tokio::time::sleep(Duration::from_secs(15)).await;

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;

    let output_str = output.lock().unwrap().clone();
    println!(
        "[{:.1}s] Console output: {:?}",
        start.elapsed().as_secs_f64(),
        output_str
    );
    assert!(
        output_str.contains("192.168.127.2"),
        "Expected static IP on eth0: {:?}",
        output_str
    );
}

/// Boot a next-mode sandbox running `cmd`, with `user` set, and return console output.
async fn run_next_with_user(name: &str, user: &str, cmd: &str) -> String {
    use runtime::config::{ConsoleSpec, RuntimeMode};
    use runtime::runtime::handle_boot_vm_subprocess;

    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        handle_boot_vm_subprocess();
    }

    let (output_rx, output_tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_stderr_rx, stderr_tx) = std::os::unix::net::UnixStream::pair().unwrap();

    let console = ConsoleSpec {
        stdin_fd: 0,
        stdout_fd: output_tx.as_raw_fd(),
        stderr_fd: stderr_tx.as_raw_fd(),
        tty: false,
    };

    let mut config = SandboxConfig::builder()
        .name(name)
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .runtime_mode(RuntimeMode::Next)
        .console(console)
        .timeout_secs(120)
        .user(user)
        .build();

    config.command = Some("/bin/sh".to_string());
    config.command_args = vec!["-c".to_string(), cmd.to_string()];

    let mut sandbox = runtime::Sandbox::create(config).await.unwrap();

    let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let output_clone = output.clone();
    let _handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut reader = std::io::BufReader::new(output_rx);
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            output_clone
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });

    sandbox.start().await.unwrap();
    tokio::time::sleep(Duration::from_secs(12)).await;
    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;

    let out = output.lock().unwrap().clone();
    out
}

#[tokio::test]
#[ignore]
async fn test_next_mode_user_drop_numeric() {
    let out = run_next_with_user("test-next-user-numeric", "1000", "id").await;
    println!("console: {:?}", out);
    assert!(
        out.contains("uid=1000"),
        "expected dropped uid 1000 in: {:?}",
        out
    );
}

#[tokio::test]
#[ignore]
async fn test_next_mode_user_home_set() {
    let out = run_next_with_user("test-next-user-home", "developer", "echo HOME=$HOME").await;
    println!("console: {:?}", out);
    assert!(
        out.contains("HOME=/home/developer"),
        "expected /home/developer in: {:?}",
        out
    );
}
