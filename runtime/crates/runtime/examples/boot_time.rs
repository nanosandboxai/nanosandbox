use runtime::Sandbox;
use runtime::config::SandboxConfig;
use runtime::runtime::handle_boot_vm_subprocess;

fn main() {
    // Handle subprocess mode (Windows spawns the same exe with "internal-boot-vm" arg)
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

    let config = SandboxConfig::builder()
        .name("boot-time-test")
        .image("ghcr.io/nanosandboxai/agents-registry/claude:0.2.0-rc3")
        .cpus(1)
        .memory_mb(256)
        .build();

    let start = std::time::Instant::now();

    let mut sandbox = match Sandbox::create(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[{:.1}s] Failed to create sandbox: {}", start.elapsed().as_secs_f64(), e);
            return;
        }
    };
    println!("[{:.1}s] Sandbox created", start.elapsed().as_secs_f64());

    if let Err(e) = sandbox.start().await {
        eprintln!("[{:.1}s] Failed to start: {}", start.elapsed().as_secs_f64(), e);
        let _ = sandbox.stop().await;
        let _ = sandbox.destroy().await;
        return;
    }
    println!("[{:.1}s] Sandbox started (VM running)", start.elapsed().as_secs_f64());

    // Note: exec is now handled by the gateway crate in the sandbox layer.
    // This example only measures VM boot time.

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;
    println!("[{:.1}s] Done", start.elapsed().as_secs_f64());
}
