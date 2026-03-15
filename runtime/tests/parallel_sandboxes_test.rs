/// Test that multiple sandboxes can be created and started in parallel
/// without conflicts. Each sandbox should get its own:
/// - VM process (internal-boot-vm)
/// - gvproxy instance
/// - Network namespace
/// - Virtiofs mounts
///
/// Run with: cargo test --test parallel_sandboxes_test -- --ignored
/// NOTE: This test requires the nanosb binary to be built first:
///   cargo build --features cli

use nanosandbox::config::SandboxConfig;
use nanosandbox::sandbox::Sandbox;
use std::time::Duration;
use tokio::time::timeout;

/// Ensure the nanosb binary exists and is codesigned before running tests
fn ensure_nanosb_binary() {
    let binary_path = std::path::Path::new("target/debug/nanosb");
    if !binary_path.exists() {
        panic!("nanosb binary not found at {:?}. Run: cargo build --features cli", binary_path);
    }
    // Set the binary path as an environment variable for the runtime to use
    std::env::set_var("NANOSB_BINARY_PATH", binary_path.canonicalize().unwrap());
}

#[tokio::test]
#[ignore] // Requires libkrun and takes time
async fn test_parallel_sandbox_creation() {
    ensure_nanosb_binary();

    // Create two sandbox configs with different images
    let config1 = SandboxConfig::builder()
        .name("test-parallel-1")
        .image("localhost:5050/agents-registry/claude:latest")
        .cpus(1)
        .memory_mb(512)
        .build();

    let config2 = SandboxConfig::builder()
        .name("test-parallel-2")
        .image("localhost:5050/agents-registry/codex:latest")
        .cpus(1)
        .memory_mb(512)
        .build();

    // Spawn both sandbox creations in parallel
    let handle1 = tokio::spawn(async move {
        let mut sb1 = Sandbox::create(config1).await?;
        sb1.start().await?;
        Ok::<_, nanosandbox::error::Error>(sb1)
    });

    let handle2 = tokio::spawn(async move {
        let mut sb2 = Sandbox::create(config2).await?;
        sb2.start().await?;
        Ok::<_, nanosandbox::error::Error>(sb2)
    });

    // Wait for both with timeout
    let result1 = timeout(Duration::from_secs(120), handle1)
        .await
        .expect("Sandbox 1 timed out")
        .expect("Sandbox 1 task panicked");

    let result2 = timeout(Duration::from_secs(120), handle2)
        .await
        .expect("Sandbox 2 timed out")
        .expect("Sandbox 2 task panicked");

    // Both should succeed
    let sb1 = result1.expect("Sandbox 1 failed to create");
    let sb2 = result2.expect("Sandbox 2 failed to create");

    // Verify they have different IDs
    assert_ne!(sb1.id(), sb2.id(), "Sandboxes should have unique IDs");

    // Verify both can execute commands
    let output1 = sb1
        .exec("echo", &["hello1"])
        .await
        .expect("Sandbox 1 exec failed");
    assert!(output1.stdout.contains("hello1"));

    let output2 = sb2
        .exec("echo", &["hello2"])
        .await
        .expect("Sandbox 2 exec failed");
    assert!(output2.stdout.contains("hello2"));

    println!("✓ Both sandboxes created and running successfully");
    println!("  Sandbox 1 ID: {}", sb1.id());
    println!("  Sandbox 2 ID: {}", sb2.id());

    // Clean up
    drop(sb1);
    drop(sb2);
}

#[tokio::test]
#[ignore]
async fn test_rapid_sequential_sandbox_creation() {
    ensure_nanosb_binary();

    // Test that sandboxes can be created quickly one after another
    // without port conflicts or resource leaks

    for i in 0..3 {
        let config = SandboxConfig::builder()
            .name(format!("test-sequential-{}", i))
            .image("localhost:5050/agents-registry/claude:latest")
            .cpus(1)
            .memory_mb(512)
            .build();

        let mut sb = Sandbox::create(config)
            .await
            .expect(&format!("Sandbox {} failed to create", i));

        sb.start()
            .await
            .expect(&format!("Sandbox {} failed to start", i));

        let test_str = format!("test{}", i);
        let output = sb
            .exec("echo", &[&test_str])
            .await
            .expect(&format!("Sandbox {} exec failed", i));

        assert!(output.stdout.contains(&test_str));

        println!("✓ Sandbox {} ({}) created and tested successfully", i, sb.id());

        // Clean up before next iteration
        drop(sb);

        // Small delay to ensure cleanup completes
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
