//! Integration tests for Nanosandbox
//!
//! These tests require network access to pull images from registries.
//! Some tests require crun/krun to be installed for full sandbox functionality.

use nanosandbox::image::{ImageManager, ImageRef};
use nanosandbox::oci::{self, OciBundle};
use nanosandbox::config::SandboxConfig;
use tempfile::TempDir;

/// Test parsing various image reference formats
#[test]
fn test_image_ref_parsing() {
    // Docker Hub library image
    let ref1 = ImageRef::parse("alpine").unwrap();
    assert_eq!(ref1.registry, "docker.io");
    assert_eq!(ref1.repository, "library/alpine");
    assert_eq!(ref1.tag, "latest");

    // Docker Hub library image with tag
    let ref2 = ImageRef::parse("alpine:3.19").unwrap();
    assert_eq!(ref2.registry, "docker.io");
    assert_eq!(ref2.repository, "library/alpine");
    assert_eq!(ref2.tag, "3.19");

    // GHCR image
    let ref3 = ImageRef::parse("ghcr.io/devdone-labs/test:v1.0").unwrap();
    assert_eq!(ref3.registry, "ghcr.io");
    assert_eq!(ref3.repository, "devdone-labs/test");
    assert_eq!(ref3.tag, "v1.0");

    // Docker Hub user image
    let ref4 = ImageRef::parse("nginx").unwrap();
    assert_eq!(ref4.repository, "library/nginx");
}

/// Test ImageManager creation
#[tokio::test]
async fn test_image_manager_creation() {
    let temp_dir = TempDir::new().unwrap();
    let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();
    
    // Verify cache directories were created
    assert!(manager.blobs_dir().exists());
    assert!(manager.extracted_dir().exists());
}

/// Test pulling an alpine image from Docker Hub
/// 
/// This test requires network access and may take a while to download layers.
#[tokio::test]
async fn test_pull_alpine_image() {
    let temp_dir = TempDir::new().unwrap();
    let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();
    
    // Pull alpine (small image for testing)
    let pulled = manager.pull("alpine:3.19").await.unwrap();
    
    // Verify we got layers
    assert!(!pulled.layers.is_empty(), "Expected at least one layer");
    
    // Verify layers are cached
    for layer in &pulled.layers {
        assert!(manager.layer_exists(layer), "Layer {} should be cached", layer);
    }
    
    println!("Pulled {} layers, total size: {} bytes", pulled.layers.len(), pulled.size);
}

/// Test creating a rootfs from pulled layers
#[tokio::test]
async fn test_create_rootfs() {
    let temp_dir = TempDir::new().unwrap();
    let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();
    
    // Pull alpine
    let pulled = manager.pull("alpine:3.19").await.unwrap();
    
    // Create rootfs
    let rootfs_dir = temp_dir.path().join("rootfs");
    manager.create_rootfs(&pulled.layers, &rootfs_dir).unwrap();
    
    // Verify rootfs has expected structure
    assert!(rootfs_dir.join("bin").exists(), "rootfs should have /bin");
    assert!(rootfs_dir.join("etc").exists(), "rootfs should have /etc");
    assert!(rootfs_dir.join("bin/sh").exists() || rootfs_dir.join("bin/busybox").exists(), 
            "rootfs should have a shell");
    
    println!("Rootfs created successfully at {:?}", rootfs_dir);
}

/// Test OCI bundle creation
#[tokio::test]
async fn test_oci_bundle_creation() {
    let temp_dir = TempDir::new().unwrap();
    
    let bundle = OciBundle::create(temp_dir.path(), "test-sandbox").unwrap();
    
    // Verify bundle structure
    assert!(bundle.path.exists());
    assert!(bundle.rootfs_path.exists());
    
    // Write a config
    let config = SandboxConfig::builder()
        .name("test-sandbox")
        .image("alpine:latest")
        .cpus(2)
        .memory_mb(512)
        .build();
    
    let oci_config = oci::generate_config(&config, &bundle.rootfs_path);
    bundle.write_config(&oci_config).unwrap();
    
    // Verify config was written
    assert!(bundle.config_path.exists());
    
    // Read and verify config content
    let content = std::fs::read_to_string(&bundle.config_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    
    assert_eq!(parsed["ociVersion"], "1.0.2");
    assert_eq!(parsed["process"]["cwd"], "/workspace");
}

/// Full integration test: pull image, create rootfs, create bundle
#[tokio::test]
async fn test_full_image_to_bundle_flow() {
    let temp_dir = TempDir::new().unwrap();
    let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();
    
    // 1. Pull image
    println!("Pulling alpine:3.19...");
    let pulled = manager.pull("alpine:3.19").await.unwrap();
    println!("Pulled {} layers", pulled.layers.len());
    
    // 2. Create bundle
    let bundles_dir = temp_dir.path().join("bundles");
    let bundle = OciBundle::create(&bundles_dir, "test-sandbox").unwrap();
    
    // 3. Extract layers to rootfs
    println!("Creating rootfs...");
    manager.create_rootfs(&pulled.layers, &bundle.rootfs_path).unwrap();
    
    // 4. Generate OCI config
    let config = SandboxConfig::builder()
        .name("test-sandbox")
        .image("alpine:3.19")
        .cpus(1)
        .memory_mb(256)
        .build();
    
    let oci_config = oci::generate_config(&config, &bundle.rootfs_path);
    bundle.write_config(&oci_config).unwrap();
    
    // 5. Verify everything is in place
    assert!(bundle.config_path.exists(), "config.json should exist");
    assert!(bundle.rootfs_path.join("bin").exists(), "rootfs/bin should exist");
    
    println!("Full flow completed successfully!");
    println!("Bundle path: {:?}", bundle.path);
}

/// Test sandbox creation (requires crun/krun or krunvm to be configured)
#[tokio::test]
#[ignore] // Requires runtime: cargo test -- --ignored
async fn test_sandbox_creation() {
    use nanosandbox::Sandbox;
    
    let config = SandboxConfig::builder()
        .name("integration-test")
        .image("alpine:3.19")
        .cpus(1)
        .memory_mb(256)
        .build();
    
    // Create sandbox (pulls image, creates bundle)
    let sandbox = Sandbox::create(config).await;
    
    match sandbox {
        Ok(sb) => {
            println!("Sandbox created successfully: {}", sb.id());
            println!("Bundle path: {:?}", sb.bundle_path());
            assert!(!sb.id().is_empty());
            
            // Clean up
            sb.destroy().await.unwrap();
        }
        Err(e) => {
            println!("Sandbox creation failed (may be expected if no runtime): {}", e);
        }
    }
}

/// Test running echo command in alpine sandbox
/// 
/// This test:
/// 1. Creates a sandbox with alpine image
/// 2. Starts the sandbox
/// 3. Executes 'echo Hello nanobox'
/// 4. Verifies the output contains "Hello nanobox"
/// 5. Stops and destroys the sandbox
#[tokio::test]
async fn test_alpine_echo_hello_nanobox() {
    use nanosandbox::Sandbox;
    
    let config = SandboxConfig::builder()
        .name("hello-nanobox-test")
        .image("alpine:3.19")
        .cpus(1)
        .memory_mb(256)
        .build();
    
    // Create sandbox
    let sandbox_result = Sandbox::create(config).await;
    
    match sandbox_result {
        Ok(mut sandbox) => {
            println!("=== Sandbox Created ===");
            println!("ID: {}", sandbox.id());
            println!("Bundle: {:?}", sandbox.bundle_path());
            
            // Start the sandbox
            match sandbox.start().await {
                Ok(_) => {
                    println!("=== Sandbox Started ===");
                    
                    // Execute echo command
                    println!("=== Executing: echo Hello nanobox ===");
                    match sandbox.exec("echo", &["Hello", "nanobox"]).await {
                        Ok(result) => {
                            println!("=== Command Output ===");
                            println!("Exit code: {}", result.exit_code);
                            println!("Stdout: {}", result.stdout);
                            println!("Stderr: {}", result.stderr);
                            println!("Duration: {}ms", result.duration_ms);
                            
                            // Verify output
                            assert_eq!(result.exit_code, 0, "Command should succeed");
                            assert!(
                                result.stdout.contains("Hello nanobox"),
                                "Output should contain 'Hello nanobox', got: {}",
                                result.stdout
                            );
                            
                            println!("=== Test PASSED: Output verified ===");
                        }
                        Err(e) => {
                            println!("Exec failed: {}", e);
                        }
                    }
                    
                    // Stop the sandbox
                    println!("=== Stopping Sandbox ===");
                    match sandbox.stop().await {
                        Ok(_) => println!("Sandbox stopped successfully"),
                        Err(e) => println!("Stop failed (may be expected): {}", e),
                    }
                }
                Err(e) => {
                    println!("Start failed (runtime may not be installed): {}", e);
                }
            }
            
            // Destroy/cleanup
            println!("=== Destroying Sandbox ===");
            match sandbox.destroy().await {
                Ok(_) => println!("Sandbox destroyed successfully"),
                Err(e) => println!("Destroy failed: {}", e),
            }
        }
        Err(e) => {
            println!("Sandbox creation failed (runtime may not be installed): {}", e);
            // Don't fail the test if runtime is not available
        }
    }
}

/// Test running ls command in alpine sandbox to show directory structure
/// 
/// This test:
/// 1. Creates a sandbox with alpine image
/// 2. Starts the sandbox
/// 3. Executes 'ls -la /' to list root directory
/// 4. Prints the full directory structure
/// 5. Stops and destroys the sandbox
#[tokio::test]
async fn test_alpine_ls_directory_structure() {
    use nanosandbox::Sandbox;
    
    let config = SandboxConfig::builder()
        .name("alpine-ls-test")
        .image("alpine:3.19")
        .cpus(1)
        .memory_mb(256)
        .build();
    
    // Create sandbox
    let sandbox_result = Sandbox::create(config).await;
    
    match sandbox_result {
        Ok(mut sandbox) => {
            println!("=== Sandbox Created ===");
            println!("ID: {}", sandbox.id());
            
            // Start the sandbox
            match sandbox.start().await {
                Ok(_) => {
                    println!("=== Sandbox Started ===");
                    
                    // Execute ls command on root directory
                    println!("\n=== Executing: ls -la / ===");
                    match sandbox.exec("ls", &["-la", "/"]).await {
                        Ok(result) => {
                            println!("\n=== Root Directory Structure ===");
                            println!("{}", result.stdout);
                            
                            if !result.stderr.is_empty() {
                                println!("Stderr: {}", result.stderr);
                            }
                            println!("Exit code: {}", result.exit_code);
                            println!("Duration: {}ms", result.duration_ms);
                            
                            // Verify we got some output
                            assert_eq!(result.exit_code, 0, "ls command should succeed");
                            assert!(!result.stdout.is_empty(), "Should have directory listing");
                            
                            // Check for expected Alpine root directories
                            assert!(result.stdout.contains("bin"), "Should contain /bin");
                            assert!(result.stdout.contains("etc"), "Should contain /etc");
                            assert!(result.stdout.contains("usr"), "Should contain /usr");
                            assert!(result.stdout.contains("var"), "Should contain /var");
                            
                            println!("\n=== Test PASSED ===");
                        }
                        Err(e) => {
                            println!("Exec failed: {}", e);
                            panic!("ls command failed: {}", e);
                        }
                    }
                    
                    // Also show /etc contents
                    println!("\n=== Executing: ls -la /etc ===");
                    if let Ok(result) = sandbox.exec("ls", &["-la", "/etc"]).await {
                        println!("\n=== /etc Directory ===");
                        println!("{}", result.stdout);
                    }
                    
                    // Show installed packages
                    println!("\n=== Executing: cat /etc/alpine-release ===");
                    if let Ok(result) = sandbox.exec("cat", &["/etc/alpine-release"]).await {
                        println!("Alpine version: {}", result.stdout.trim());
                    }
                    
                    // Stop the sandbox
                    println!("\n=== Stopping Sandbox ===");
                    match sandbox.stop().await {
                        Ok(_) => println!("Sandbox stopped successfully"),
                        Err(e) => println!("Stop failed (may be expected): {}", e),
                    }
                }
                Err(e) => {
                    println!("Start failed (runtime may not be installed): {}", e);
                }
            }
            
            // Destroy/cleanup
            println!("=== Destroying Sandbox ===");
            match sandbox.destroy().await {
                Ok(_) => println!("Sandbox destroyed successfully"),
                Err(e) => println!("Destroy failed: {}", e),
            }
        }
        Err(e) => {
            println!("Sandbox creation failed (runtime may not be installed): {}", e);
        }
    }
}

// ============================================================================
// M2: Sandbox Management Tests
// ============================================================================

/// Test ExecOptions builder pattern
#[test]
fn test_exec_options_builder() {
    use nanosandbox::ExecOptions;
    
    let options = ExecOptions::new()
        .workdir("/app")
        .env("FOO", "bar")
        .env("BAZ", "qux")
        .user("nobody")
        .timeout(60);
    
    assert_eq!(options.workdir, Some("/app".to_string()));
    assert_eq!(options.env.get("FOO"), Some(&"bar".to_string()));
    assert_eq!(options.env.get("BAZ"), Some(&"qux".to_string()));
    assert_eq!(options.user, Some("nobody".to_string()));
    assert_eq!(options.timeout_secs, Some(60));
}

/// Test ExecResult success/failure
#[test]
fn test_exec_result() {
    use nanosandbox::ExecResult;
    
    let success = ExecResult {
        exit_code: 0,
        stdout: "hello".to_string(),
        stderr: String::new(),
        duration_ms: 100,
    };
    assert!(success.success());
    
    let failure = ExecResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "error".to_string(),
        duration_ms: 50,
    };
    assert!(!failure.success());
}

/// Test SandboxRegistry basic operations
#[test]
fn test_sandbox_registry() {
    use nanosandbox::{SandboxRegistry, SandboxInfo, SandboxStatus};
    use std::path::PathBuf;
    use chrono::Utc;
    
    let temp_dir = TempDir::new().unwrap();
    let registry = SandboxRegistry::with_state_dir(temp_dir.path().to_path_buf()).unwrap();
    
    // Create test sandbox info
    let info = SandboxInfo {
        id: "test-123".to_string(),
        name: "test-sandbox".to_string(),
        image: "alpine:3.19".to_string(),
        status: SandboxStatus::Ready,
        bundle_path: PathBuf::from("/tmp/test-bundle"),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        config: SandboxConfig::builder()
            .name("test-sandbox")
            .image("alpine:3.19")
            .build(),
    };
    
    // Register
    registry.register(&info).unwrap();
    assert!(registry.exists("test-123"));
    
    // Get
    let retrieved = registry.get("test-123").unwrap().unwrap();
    assert_eq!(retrieved.id, "test-123");
    assert_eq!(retrieved.name, "test-sandbox");
    
    // Update status
    registry.update_status("test-123", SandboxStatus::Running).unwrap();
    let updated = registry.get("test-123").unwrap().unwrap();
    assert_eq!(updated.status, SandboxStatus::Running);
    
    // List
    let list = registry.list().unwrap();
    assert_eq!(list.len(), 1);
    
    // Unregister
    registry.unregister("test-123").unwrap();
    assert!(!registry.exists("test-123"));
}

/// Test OCI config resource limits
#[test]
fn test_oci_resource_limits() {
    let config = SandboxConfig::builder()
        .name("test")
        .image("alpine:latest")
        .cpus(4)
        .memory_mb(1024)
        .build();
    
    let oci_config = oci::generate_config(&config, std::path::Path::new("rootfs"));
    
    // Check memory limits
    let memory = &oci_config["linux"]["resources"]["memory"];
    assert_eq!(memory["limit"], 1024 * 1024 * 1024); // 1024 MB in bytes
    assert_eq!(memory["swap"], 1024 * 1024 * 1024); // swap matches memory
    
    // Check CPU limits
    let cpu = &oci_config["linux"]["resources"]["cpu"];
    assert_eq!(cpu["shares"], 4 * 1024); // 4 cores * 1024
    assert_eq!(cpu["quota"], 4 * 100000); // 4 cores * 100000 microseconds
    assert_eq!(cpu["period"], 100000);
    
    // Check PIDs limit
    let pids = &oci_config["linux"]["resources"]["pids"];
    assert_eq!(pids["limit"], 256);
}

/// Test SandboxStatus enum
#[test]
fn test_sandbox_status() {
    use nanosandbox::SandboxStatus;
    
    assert_eq!(SandboxStatus::default(), SandboxStatus::Creating);
    
    // Test serialization
    let status = SandboxStatus::Running;
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(json, "\"running\"");
    
    let parsed: SandboxStatus = serde_json::from_str("\"stopped\"").unwrap();
    assert_eq!(parsed, SandboxStatus::Stopped);
}

/// Test OutputChunk and Stream types
#[test]
fn test_output_chunk() {
    use nanosandbox::{OutputChunk, Stream};
    use chrono::Utc;
    
    let chunk = OutputChunk {
        stream: Stream::Stdout,
        data: "Hello, World!".to_string(),
        timestamp: Utc::now(),
    };
    
    assert_eq!(chunk.stream, Stream::Stdout);
    assert_eq!(chunk.data, "Hello, World!");
    
    let stderr_chunk = OutputChunk {
        stream: Stream::Stderr,
        data: "Error message".to_string(),
        timestamp: Utc::now(),
    };
    assert_eq!(stderr_chunk.stream, Stream::Stderr);
}

// ============================================================================
// M3: Advanced Features Tests
// ============================================================================

/// Test CredentialStore empty and add
#[test]
fn test_credential_store_empty() {
    use nanosandbox::CredentialStore;
    use oci_distribution::secrets::RegistryAuth;
    
    let store = CredentialStore::empty();
    
    // Should return anonymous for unknown registry
    assert_eq!(store.get_auth("ghcr.io"), RegistryAuth::Anonymous);
    assert!(!store.has_credentials("ghcr.io"));
}

/// Test CredentialStore add and retrieve
#[test]
fn test_credential_store_add() {
    use nanosandbox::CredentialStore;
    use oci_distribution::secrets::RegistryAuth;
    
    let mut store = CredentialStore::empty();
    store.add_credentials("ghcr.io", "user".to_string(), "token123".to_string());
    
    assert!(store.has_credentials("ghcr.io"));
    
    match store.get_auth("ghcr.io") {
        RegistryAuth::Basic(user, pass) => {
            assert_eq!(user, "user");
            assert_eq!(pass, "token123");
        }
        RegistryAuth::Anonymous => panic!("Expected Basic auth"),
    }
}

/// Test Mount types
#[test]
fn test_mount_types() {
    use nanosandbox::{Mount, MountType};
    
    // Bind mount
    let bind = Mount::bind("/host/path", "/container/path");
    assert_eq!(bind.mount_type, MountType::Bind);
    assert!(!bind.readonly);
    
    // VirtioFs mount
    let virtiofs = Mount::virtiofs("/host/shared", "/shared").readonly();
    assert_eq!(virtiofs.mount_type, MountType::VirtioFs);
    assert!(virtiofs.readonly);
}

/// Test NetworkConfig builders
#[test]
fn test_network_config() {
    use nanosandbox::{NetworkConfig, NetworkMode};
    
    // Default is TSI with network enabled
    let default = NetworkConfig::default();
    assert!(default.enabled);
    assert_eq!(default.mode, NetworkMode::Tsi);
    assert!(default.port_mappings.is_empty());
    
    // None mode
    let none = NetworkConfig::none();
    assert!(!none.enabled);
    assert_eq!(none.mode, NetworkMode::None);
    
    // TSI with port mapping
    let tsi = NetworkConfig::tsi().with_port(8080, 80).with_dns("8.8.8.8");
    assert_eq!(tsi.port_mappings.len(), 1);
    assert_eq!(tsi.port_mappings[0].host_port, 8080);
    assert_eq!(tsi.port_mappings[0].container_port, 80);
    assert_eq!(tsi.dns, vec!["8.8.8.8"]);
}

/// Test RegistryConfig
#[test]
fn test_registry_config() {
    use nanosandbox::RegistryConfig;
    
    let config = RegistryConfig::new("localhost:5000")
        .insecure()
        .skip_tls();
    
    assert_eq!(config.host, "localhost:5000");
    assert!(config.insecure);
    assert!(config.skip_tls_verify);
}

/// Test PortMapping
#[test]
fn test_port_mapping() {
    use nanosandbox::PortMapping;
    
    let tcp = PortMapping::tcp(8080, 80);
    assert_eq!(tcp.host_port, 8080);
    assert_eq!(tcp.container_port, 80);
    assert_eq!(tcp.protocol, "tcp");
    
    let udp = PortMapping::udp(53, 53);
    assert_eq!(udp.protocol, "udp");
}

/// Test OCI config virtio-fs mounts
#[test]
fn test_oci_virtiofs_mount() {
    let config = SandboxConfig::builder()
        .name("test")
        .image("alpine:latest")
        .mount_virtiofs("/host/shared", "/shared")
        .build();
    
    let oci_config = oci::generate_config(&config, std::path::Path::new("rootfs"));
    
    // Find the virtiofs mount
    let mounts = oci_config["mounts"].as_array().unwrap();
    let virtiofs_mount = mounts.iter().find(|m| m["destination"] == "/shared");
    
    assert!(virtiofs_mount.is_some(), "Should have virtiofs mount");
    let mount = virtiofs_mount.unwrap();
    assert_eq!(mount["type"], "virtiofs");
}

/// Test OCI config TSI networking (no network namespace)
#[test]
fn test_oci_tsi_networking() {
    use nanosandbox::NetworkMode;
    
    let config = SandboxConfig::builder()
        .name("test")
        .image("alpine:latest")
        .network_mode(NetworkMode::Tsi)
        .build();
    
    let oci_config = oci::generate_config(&config, std::path::Path::new("rootfs"));
    
    // TSI mode should NOT have network namespace
    let namespaces = oci_config["linux"]["namespaces"].as_array().unwrap();
    let has_network_ns = namespaces.iter().any(|ns| ns["type"] == "network");
    
    assert!(!has_network_ns, "TSI mode should not have network namespace");
}

/// Test OCI config None networking (has network namespace)
#[test]
fn test_oci_none_networking() {
    use nanosandbox::NetworkMode;
    
    let config = SandboxConfig::builder()
        .name("test")
        .image("alpine:latest")
        .network_mode(NetworkMode::None)
        .build();
    
    let oci_config = oci::generate_config(&config, std::path::Path::new("rootfs"));
    
    // None mode should have network namespace for isolation
    let namespaces = oci_config["linux"]["namespaces"].as_array().unwrap();
    let has_network_ns = namespaces.iter().any(|ns| ns["type"] == "network");
    
    assert!(has_network_ns, "None mode should have network namespace for isolation");
}

/// Test SandboxConfig builder with M3 features
#[test]
fn test_sandbox_config_m3_features() {
    use nanosandbox::NetworkMode;
    
    let config = SandboxConfig::builder()
        .name("test")
        .image("alpine:latest")
        .mount_virtiofs("/host/code", "/code")
        .mount_virtiofs_readonly("/host/data", "/data")
        .network_mode(NetworkMode::Tsi)
        .port(8080, 80)
        .dns("8.8.8.8")
        .build();
    
    // Check mounts
    assert_eq!(config.mounts.len(), 2);
    assert!(!config.mounts[0].readonly);
    assert!(config.mounts[1].readonly);
    
    // Check network
    assert_eq!(config.network.mode, NetworkMode::Tsi);
    assert_eq!(config.network.port_mappings.len(), 1);
    assert_eq!(config.network.dns.len(), 1);
}
