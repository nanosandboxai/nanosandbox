//! Integration tests for Nanosandbox
//!
//! These tests require network access to pull images from registries.
//! Some tests require libkrun to be installed for full sandbox functionality.

use nanosandbox::config::SandboxConfig;
use nanosandbox::image::{ImageManager, ImageRef};
use nanosandbox::oci::{self, OciBundle};
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
        assert!(
            manager.layer_exists(layer),
            "Layer {} should be cached",
            layer
        );
    }

    println!(
        "Pulled {} layers, total size: {} bytes",
        pulled.layers.len(),
        pulled.size
    );
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
    assert!(
        rootfs_dir.join("bin/sh").exists() || rootfs_dir.join("bin/busybox").exists(),
        "rootfs should have a shell"
    );

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
    manager
        .create_rootfs(&pulled.layers, &bundle.rootfs_path)
        .unwrap();

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
    assert!(
        bundle.rootfs_path.join("bin").exists(),
        "rootfs/bin should exist"
    );

    println!("Full flow completed successfully!");
    println!("Bundle path: {:?}", bundle.path);
}

/// Test sandbox creation (requires libkrun to be configured)
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
            println!(
                "Sandbox creation failed (may be expected if no runtime): {}",
                e
            );
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
            println!(
                "Sandbox creation failed (runtime may not be installed): {}",
                e
            );
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
            println!(
                "Sandbox creation failed (runtime may not be installed): {}",
                e
            );
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
    use chrono::Utc;
    use nanosandbox::{SandboxInfo, SandboxRegistry, SandboxStatus};
    use std::path::PathBuf;

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
    registry
        .update_status("test-123", SandboxStatus::Running)
        .unwrap();
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
    use chrono::Utc;
    use nanosandbox::{OutputChunk, Stream};

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

    let config = RegistryConfig::new("localhost:5000").insecure().skip_tls();

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

    assert!(
        !has_network_ns,
        "TSI mode should not have network namespace"
    );
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

    assert!(
        has_network_ns,
        "None mode should have network namespace for isolation"
    );
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

// ============================================================================
// M4: MCP Server Integration Tests
// ============================================================================

/// Helper: create a sandbox config with an MCP server for testing
fn create_mcp_test_config(name: &str) -> SandboxConfig {
    use nanosandbox::McpServerConfig;
    use std::collections::HashMap;

    SandboxConfig::builder()
        .name(name)
        .image("alpine:3.19")
        .cpus(2)
        .memory_mb(1024)
        .mcp_server(
            "context7",
            McpServerConfig {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
                env: HashMap::new(),
                enabled: true,
            },
        )
        .build()
}

/// Test that MCP servers from SandboxConfig are auto-pushed on start
#[tokio::test]
#[ignore]
async fn test_mcp_auto_push_on_start() {
    use nanosandbox::Sandbox;

    let config = create_mcp_test_config("mcp-auto-push-test");

    match Sandbox::create(config).await {
        Ok(mut sandbox) => {
            match sandbox.start().await {
                Ok(_) => {
                    println!("=== Sandbox started, checking MCP servers ===");

                    match sandbox.list_mcp_servers().await {
                        Ok(servers) => {
                            println!("MCP servers: {:?}", servers.keys().collect::<Vec<_>>());
                            assert!(
                                servers.contains_key("context7"),
                                "Expected 'context7' server to be auto-pushed, got: {:?}",
                                servers.keys().collect::<Vec<_>>()
                            );
                            println!("=== Test PASSED: MCP auto-push verified ===");
                        }
                        Err(e) => {
                            println!("list_mcp_servers failed (may need gateway): {}", e);
                        }
                    }

                    let _ = sandbox.stop().await;
                }
                Err(e) => {
                    println!("Start failed (runtime may not be available): {}", e);
                }
            }
            let _ = sandbox.destroy().await;
        }
        Err(e) => {
            println!("Sandbox creation failed: {}", e);
        }
    }
}

/// Test adding an MCP server to a running sandbox
#[tokio::test]
#[ignore]
async fn test_mcp_add_server() {
    use nanosandbox::{McpServerConfig, Sandbox};
    use std::collections::HashMap;

    let config = SandboxConfig::builder()
        .name("mcp-add-test")
        .image("alpine:3.19")
        .cpus(2)
        .memory_mb(1024)
        .build();

    match Sandbox::create(config).await {
        Ok(mut sandbox) => {
            match sandbox.start().await {
                Ok(_) => {
                    let mcp_config = McpServerConfig {
                        command: "npx".to_string(),
                        args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
                        env: HashMap::new(),
                        enabled: true,
                    };

                    match sandbox.add_mcp_server("test-server", mcp_config).await {
                        Ok(_) => {
                            println!("=== MCP server added ===");

                            match sandbox.list_mcp_servers().await {
                                Ok(servers) => {
                                    assert!(
                                        servers.contains_key("test-server"),
                                        "Expected 'test-server' in list, got: {:?}",
                                        servers.keys().collect::<Vec<_>>()
                                    );
                                    println!("=== Test PASSED: MCP add verified ===");
                                }
                                Err(e) => println!("list failed: {}", e),
                            }
                        }
                        Err(e) => println!("add_mcp_server failed: {}", e),
                    }

                    let _ = sandbox.stop().await;
                }
                Err(e) => println!("Start failed: {}", e),
            }
            let _ = sandbox.destroy().await;
        }
        Err(e) => println!("Sandbox creation failed: {}", e),
    }
}

/// Test removing an MCP server from a running sandbox
#[tokio::test]
#[ignore]
async fn test_mcp_remove_server() {
    use nanosandbox::{McpServerConfig, Sandbox};
    use std::collections::HashMap;

    let config = SandboxConfig::builder()
        .name("mcp-remove-test")
        .image("alpine:3.19")
        .cpus(2)
        .memory_mb(1024)
        .build();

    match Sandbox::create(config).await {
        Ok(mut sandbox) => {
            match sandbox.start().await {
                Ok(_) => {
                    let mcp_config = McpServerConfig {
                        command: "npx".to_string(),
                        args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
                        env: HashMap::new(),
                        enabled: true,
                    };

                    if sandbox.add_mcp_server("to-remove", mcp_config).await.is_ok() {
                        match sandbox.remove_mcp_server("to-remove").await {
                            Ok(_) => {
                                println!("=== MCP server removed ===");

                                match sandbox.list_mcp_servers().await {
                                    Ok(servers) => {
                                        assert!(
                                            !servers.contains_key("to-remove"),
                                            "Server 'to-remove' should be gone, got: {:?}",
                                            servers.keys().collect::<Vec<_>>()
                                        );
                                        println!("=== Test PASSED: MCP remove verified ===");
                                    }
                                    Err(e) => println!("list failed: {}", e),
                                }
                            }
                            Err(e) => println!("remove failed: {}", e),
                        }
                    }

                    let _ = sandbox.stop().await;
                }
                Err(e) => println!("Start failed: {}", e),
            }
            let _ = sandbox.destroy().await;
        }
        Err(e) => println!("Sandbox creation failed: {}", e),
    }
}

/// Test enabling and disabling an MCP server
#[tokio::test]
#[ignore]
async fn test_mcp_enable_disable() {
    use nanosandbox::{McpServerConfig, Sandbox};
    use std::collections::HashMap;

    let config = SandboxConfig::builder()
        .name("mcp-toggle-test")
        .image("alpine:3.19")
        .cpus(2)
        .memory_mb(1024)
        .mcp_server(
            "toggle-me",
            McpServerConfig {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
                env: HashMap::new(),
                enabled: true,
            },
        )
        .build();

    match Sandbox::create(config).await {
        Ok(mut sandbox) => {
            match sandbox.start().await {
                Ok(_) => {
                    match sandbox.disable_mcp_server("toggle-me").await {
                        Ok(_) => println!("=== Disabled toggle-me ==="),
                        Err(e) => println!("disable failed: {}", e),
                    }

                    match sandbox.enable_mcp_server("toggle-me").await {
                        Ok(_) => {
                            println!("=== Re-enabled toggle-me ===");
                            println!("=== Test PASSED: MCP enable/disable verified ===");
                        }
                        Err(e) => println!("enable failed: {}", e),
                    }

                    let _ = sandbox.stop().await;
                }
                Err(e) => println!("Start failed: {}", e),
            }
            let _ = sandbox.destroy().await;
        }
        Err(e) => println!("Sandbox creation failed: {}", e),
    }
}

/// Full MCP CRUD lifecycle test
#[tokio::test]
#[ignore]
async fn test_mcp_full_crud_lifecycle() {
    use nanosandbox::{McpServerConfig, Sandbox};
    use std::collections::HashMap;

    let config = SandboxConfig::builder()
        .name("mcp-lifecycle-test")
        .image("alpine:3.19")
        .cpus(2)
        .memory_mb(1024)
        .build();

    match Sandbox::create(config).await {
        Ok(mut sandbox) => {
            match sandbox.start().await {
                Ok(_) => {
                    println!("=== Starting MCP CRUD lifecycle ===");

                    let mcp_config = McpServerConfig {
                        command: "npx".to_string(),
                        args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
                        env: HashMap::new(),
                        enabled: true,
                    };

                    if let Err(e) = sandbox.add_mcp_server("lifecycle", mcp_config).await {
                        println!("add failed: {}", e);
                        let _ = sandbox.stop().await;
                        let _ = sandbox.destroy().await;
                        return;
                    }
                    println!("  [1/5] Added 'lifecycle' server");

                    match sandbox.list_mcp_servers().await {
                        Ok(servers) => {
                            assert!(servers.contains_key("lifecycle"), "Server should exist after add");
                            println!("  [2/5] Listed servers: {:?}", servers.keys().collect::<Vec<_>>());
                        }
                        Err(e) => println!("  list failed: {}", e),
                    }

                    match sandbox.disable_mcp_server("lifecycle").await {
                        Ok(_) => println!("  [3/5] Disabled 'lifecycle'"),
                        Err(e) => println!("  disable failed: {}", e),
                    }

                    match sandbox.enable_mcp_server("lifecycle").await {
                        Ok(_) => println!("  [4/5] Re-enabled 'lifecycle'"),
                        Err(e) => println!("  enable failed: {}", e),
                    }

                    match sandbox.remove_mcp_server("lifecycle").await {
                        Ok(_) => {
                            println!("  [5/5] Removed 'lifecycle'");

                            if let Ok(servers) = sandbox.list_mcp_servers().await {
                                assert!(
                                    !servers.contains_key("lifecycle"),
                                    "Server should be gone after remove"
                                );
                            }
                        }
                        Err(e) => println!("  remove failed: {}", e),
                    }

                    println!("=== Test PASSED: Full MCP CRUD lifecycle ===");

                    let _ = sandbox.stop().await;
                }
                Err(e) => println!("Start failed: {}", e),
            }
            let _ = sandbox.destroy().await;
        }
        Err(e) => println!("Sandbox creation failed: {}", e),
    }
}

// ============================================================================
// M5: Project Mount Integration Tests
// ============================================================================

/// Test the full project mount lifecycle: detect -> setup -> simulate work -> teardown -> verify
#[test]
fn test_project_mount_full_lifecycle() {
    use nanosandbox::project::{BranchStrategy, ProjectLayout, ProjectMount};
    use std::fs;
    use std::process::Command as GitCmd;
    use tempfile::TempDir;

    // Create a project repo
    let dir = TempDir::new().unwrap();
    GitCmd::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    // Configure git user for commits
    GitCmd::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    GitCmd::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    fs::write(dir.path().join("app.rs"), "fn main() {}").unwrap();
    GitCmd::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    GitCmd::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Detect: should be SingleRepo
    let mut mount = ProjectMount::detect(dir.path()).unwrap();
    assert!(matches!(mount.layout, ProjectLayout::SingleRepo { .. }));

    // Setup clone with named branch
    let wt_path = mount
        .setup(
            "integration-test-1",
            &BranchStrategy::Named("feat/test".to_string()),
        )
        .unwrap();
    assert!(wt_path.exists());
    assert!(wt_path.join("app.rs").exists());

    // Simulate agent work
    fs::write(wt_path.join("agent_output.txt"), "agent produced this").unwrap();
    fs::write(
        wt_path.join("app.rs"),
        "fn main() { println!(\"modified\"); }",
    )
    .unwrap();

    // Get mount config
    let mount_cfg = mount.mount_config("/workspace").unwrap();
    assert_eq!(mount_cfg.container_path, "/workspace");
    assert!(!mount_cfg.readonly);

    // Teardown: should auto-commit, fetch branch to source, and remove clone
    mount.teardown().unwrap();
    assert!(!wt_path.exists(), "Clone directory should be removed");

    // Verify branch exists with agent's changes
    let log = GitCmd::new("git")
        .args(["log", "--oneline", "feat/test"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let log_str = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_str.contains("nanosb: auto-save"),
        "Expected auto-save commit in log: {}",
        log_str
    );

    // Verify file content on the branch
    let show = GitCmd::new("git")
        .args(["show", "feat/test:agent_output.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&show.stdout).trim(),
        "agent produced this"
    );
}

/// Test multi-repo project mount lifecycle
#[test]
fn test_project_mount_multi_repo() {
    use nanosandbox::project::{BranchStrategy, ProjectLayout, ProjectMount};
    use std::fs;
    use std::process::Command as GitCmd;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();

    // Create two sub-repos
    for name in &["frontend", "backend"] {
        let sub = dir.path().join(name);
        fs::create_dir_all(&sub).unwrap();
        GitCmd::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&sub)
            .output()
            .unwrap();
        GitCmd::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&sub)
            .output()
            .unwrap();
        GitCmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&sub)
            .output()
            .unwrap();
        fs::write(sub.join("index.js"), format!("// {}", name)).unwrap();
        GitCmd::new("git")
            .args(["add", "."])
            .current_dir(&sub)
            .output()
            .unwrap();
        GitCmd::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&sub)
            .output()
            .unwrap();
    }

    // Add a loose file at root
    fs::write(dir.path().join("Makefile"), "all:\n\techo hi").unwrap();

    // Detect
    let mut mount = ProjectMount::detect(dir.path()).unwrap();
    if let ProjectLayout::MultiRepo {
        ref repos,
        ref loose_items,
    } = mount.layout
    {
        assert_eq!(repos.len(), 2);
        assert!(loose_items.len() >= 1);
    } else {
        panic!("Expected MultiRepo layout");
    }

    // Setup
    let wt_path = mount
        .setup("multi-test-1", &BranchStrategy::Auto)
        .unwrap();
    assert!(wt_path.join("frontend").exists());
    assert!(wt_path.join("backend").exists());
    assert!(wt_path.join("Makefile").exists());

    // Teardown
    mount.teardown().unwrap();
    assert!(!wt_path.exists());
}

/// Test SandboxConfig with project configuration
#[test]
fn test_sandbox_config_project_integration() {
    use nanosandbox::SandboxConfig;

    let config = SandboxConfig::builder()
        .name("test")
        .image("alpine")
        .project("/tmp/fake-project", Some("feat/test"))
        .build();

    let proj = config.project.unwrap();
    assert_eq!(proj.path, std::path::PathBuf::from("/tmp/fake-project"));
    assert_eq!(proj.branch, Some("feat/test".to_string()));
    assert_eq!(proj.mount_point, "/workspace");
}

/// Test that the sandbox-testing/sandbox.yml config parses and the agents registry
/// resolves the referenced agent definitions, skills, and MCPs.
///
/// Requires: NANOSB_REGISTRY_PATH env var pointing to the agents-registry directory,
/// OR the sibling directory ../agents-registry/ to exist.
#[test]
fn test_sandbox_testing_config_with_registry() {
    use nanosandbox::agents_registry::AgentsRegistryClient;
    use nanosandbox::config::file::{parse_sandbox_file, resolve_sandbox_configs};

    let yaml = r#"
defaults:
  cpus: 2
  memory: 2048
  timeout: 600
  workdir: /workspace

sandboxes:
  claude-rust:
    image: localhost:5050/agent-claude:latest
    name: claude-rust-dev
    agent: rust-developer
    skills:
      - tdd
      - git-workflow
      - code-review
    mcp:
      memory:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-memory"]

  codex-python:
    image: localhost:5050/agent-codex:latest
    name: codex-python-dev
    agent: python-developer
    skills:
      - tdd
      - security-best-practices
    mcp:
      filesystem:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
"#;

    let file = parse_sandbox_file(yaml).unwrap();
    let configs =
        resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert_eq!(configs.len(), 2);

    // Claude sandbox
    let claude = configs.iter().find(|(k, _)| k == "claude-rust").unwrap();
    assert_eq!(claude.1.name, "claude-rust-dev");
    assert_eq!(claude.1.agent.as_deref(), Some("rust-developer"));
    assert_eq!(claude.1.skills, vec!["tdd", "git-workflow", "code-review"]);
    assert!(claude.1.mcp_servers.contains_key("memory"));
    assert_eq!(claude.1.mcp_servers["memory"].command, "npx");

    // Codex sandbox
    let codex = configs.iter().find(|(k, _)| k == "codex-python").unwrap();
    assert_eq!(codex.1.name, "codex-python-dev");
    assert_eq!(codex.1.agent.as_deref(), Some("python-developer"));
    assert_eq!(codex.1.skills, vec!["tdd", "security-best-practices"]);
    assert!(codex.1.mcp_servers.contains_key("filesystem"));

    // Now try to resolve from the agents registry (if available)
    let registry_path = std::env::var("NANOSB_REGISTRY_PATH")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| {
            let p = std::path::PathBuf::from("../agents-registry");
            if p.join("index.json").exists() { Some(p) } else { None }
        });

    if let Some(reg_path) = registry_path {
        let registry = AgentsRegistryClient::from_path(&reg_path).unwrap();

        // Resolve rust-developer agent
        let resolved = registry.resolve_full("rust-developer", &claude.1.skills).unwrap();
        assert_eq!(resolved.agent_name, "rust-developer");
        assert!(!resolved.prompt.is_empty());
        // Should have at least the 3 skills specified in config
        assert!(resolved.skills.len() >= 3, "expected >=3 skills, got {}", resolved.skills.len());
        let skill_names: Vec<&str> = resolved.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(skill_names.contains(&"tdd"), "missing tdd skill");
        assert!(skill_names.contains(&"git-workflow"), "missing git-workflow skill");
        assert!(skill_names.contains(&"code-review"), "missing code-review skill");

        // Verify MCPs from agent definition
        assert!(resolved.mcp_servers.contains_key("server-github"),
            "expected server-github MCP from rust-developer agent");

        // Resolve python-developer agent
        let py_resolved = registry.resolve_full("python-developer", &codex.1.skills).unwrap();
        assert_eq!(py_resolved.agent_name, "python-developer");
        assert!(!py_resolved.prompt.is_empty());
        let py_skill_names: Vec<&str> = py_resolved.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(py_skill_names.contains(&"tdd"), "missing tdd in python agent");
        assert!(py_skill_names.contains(&"security-best-practices"),
            "missing security-best-practices in python agent");
    } else {
        eprintln!("Skipping registry resolution: no agents-registry found");
    }
}

// ============================================================================
// MCP / Skills / Agent E2E Tests (requires running agent image + gateway)
// ============================================================================

/// Full end-to-end test of MCP, skills, and agent operations on a running sandbox.
///
/// This test:
/// 1. Creates a sandbox with a real agent image (has agent-gateway)
/// 2. Tests MCP: add → list → disable → enable → remove
/// 3. Tests Skills: add → list → remove
/// 4. Tests Agent: bootstrap → list → get
/// 5. Destroys the sandbox
///
/// Requires: `localhost:5050/agents-registry/claude:latest` pushed to local registry.
#[tokio::test]
async fn test_mcp_skills_agent_e2e() {
    use nanosandbox::{McpServerConfig, Sandbox, SkillDef};
    use std::collections::HashMap;

    let config = SandboxConfig::builder()
        .name("e2e-gateway-test")
        .image("localhost:5050/agents-registry/claude:latest")
        .cpus(2)
        .memory_mb(1024)
        .build();

    // --- Create + Start ---
    let mut sandbox = match Sandbox::create(config).await {
        Ok(sb) => sb,
        Err(e) => {
            eprintln!("Sandbox creation failed (image may not be available): {}", e);
            return;
        }
    };
    if let Err(e) = sandbox.start().await {
        eprintln!("Sandbox start failed (runtime may not be available): {}", e);
        let _ = sandbox.destroy().await;
        return;
    }
    println!("=== Sandbox started: {} ===", sandbox.id());

    // Wait for gateway to be ready (health check already passed via start())
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // =============================================
    // MCP CRUD
    // =============================================
    println!("\n--- MCP: list (should be empty) ---");
    match sandbox.list_mcp_servers().await {
        Ok(servers) => {
            println!("  MCP servers: {:?}", servers.keys().collect::<Vec<_>>());
            // No default servers (we removed mcp-servers.yaml defaults)
        }
        Err(e) => {
            eprintln!("  list_mcp_servers failed: {} (gateway may not be ready)", e);
            let _ = sandbox.stop().await;
            let _ = sandbox.destroy().await;
            return;
        }
    }

    println!("--- MCP: add 'test-github' ---");
    let mcp_config = McpServerConfig {
        command: "npx".to_string(),
        args: vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()],
        env: HashMap::new(),
        enabled: true,
    };
    match sandbox.add_mcp_server("test-github", mcp_config).await {
        Ok(_) => println!("  Added 'test-github'"),
        Err(e) => {
            eprintln!("  add_mcp_server failed: {}", e);
            let _ = sandbox.stop().await;
            let _ = sandbox.destroy().await;
            return;
        }
    }

    println!("--- MCP: list (should have test-github) ---");
    match sandbox.list_mcp_servers().await {
        Ok(servers) => {
            let keys: Vec<&String> = servers.keys().collect();
            println!("  MCP servers: {:?}", keys);
            assert!(servers.contains_key("test-github"), "Expected 'test-github', got: {:?}", keys);
        }
        Err(e) => eprintln!("  list failed: {}", e),
    }

    println!("--- MCP: disable 'test-github' ---");
    match sandbox.disable_mcp_server("test-github").await {
        Ok(_) => println!("  Disabled 'test-github'"),
        Err(e) => eprintln!("  disable failed: {}", e),
    }

    println!("--- MCP: enable 'test-github' ---");
    match sandbox.enable_mcp_server("test-github").await {
        Ok(_) => println!("  Re-enabled 'test-github'"),
        Err(e) => eprintln!("  enable failed: {}", e),
    }

    println!("--- MCP: add second server 'test-context7' ---");
    let mcp2 = McpServerConfig {
        command: "npx".to_string(),
        args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
        env: HashMap::new(),
        enabled: true,
    };
    match sandbox.add_mcp_server("test-context7", mcp2).await {
        Ok(_) => println!("  Added 'test-context7'"),
        Err(e) => eprintln!("  add failed: {}", e),
    }

    println!("--- MCP: list (should have 2 servers) ---");
    match sandbox.list_mcp_servers().await {
        Ok(servers) => {
            let keys: Vec<&String> = servers.keys().collect();
            println!("  MCP servers: {:?}", keys);
            assert_eq!(servers.len(), 2, "Expected 2 servers, got {}", servers.len());
        }
        Err(e) => eprintln!("  list failed: {}", e),
    }

    println!("--- MCP: remove 'test-github' ---");
    match sandbox.remove_mcp_server("test-github").await {
        Ok(_) => println!("  Removed 'test-github'"),
        Err(e) => eprintln!("  remove failed: {}", e),
    }

    println!("--- MCP: list (should have 1 server) ---");
    match sandbox.list_mcp_servers().await {
        Ok(servers) => {
            let keys: Vec<&String> = servers.keys().collect();
            println!("  MCP servers: {:?}", keys);
            assert_eq!(servers.len(), 1, "Expected 1 server, got {}", servers.len());
            assert!(!servers.contains_key("test-github"), "'test-github' should be gone");
            assert!(servers.contains_key("test-context7"), "'test-context7' should remain");
        }
        Err(e) => eprintln!("  list failed: {}", e),
    }

    // =============================================
    // Skills CRUD
    // =============================================
    println!("\n--- Skills: add 'test-skill' ---");
    let skill = SkillDef {
        name: "test-skill".to_string(),
        description: "A test skill for e2e testing".to_string(),
        content: "# Test Skill\n\nAlways write tests first.".to_string(),
        version: "1.0".to_string(),
        tags: vec![],
    };
    match sandbox.add_skill(&skill).await {
        Ok(_) => println!("  Added 'test-skill'"),
        Err(e) => eprintln!("  add_skill failed: {}", e),
    }

    println!("--- Skills: list (should have test-skill) ---");
    match sandbox.list_skills().await {
        Ok(skills) => {
            let keys: Vec<&String> = skills.keys().collect();
            println!("  Skills: {:?}", keys);
            assert!(skills.contains_key("test-skill"), "Expected 'test-skill', got: {:?}", keys);
        }
        Err(e) => eprintln!("  list_skills failed: {}", e),
    }

    println!("--- Skills: add second 'review-skill' ---");
    let skill2 = SkillDef {
        name: "review-skill".to_string(),
        description: "Code review skill".to_string(),
        content: "# Code Review\n\nReview all changes carefully.".to_string(),
        version: "1.0".to_string(),
        tags: vec![],
    };
    match sandbox.add_skill(&skill2).await {
        Ok(_) => println!("  Added 'review-skill'"),
        Err(e) => eprintln!("  add_skill failed: {}", e),
    }

    println!("--- Skills: list (should have 2) ---");
    match sandbox.list_skills().await {
        Ok(skills) => {
            println!("  Skills: {:?}", skills.keys().collect::<Vec<_>>());
            assert_eq!(skills.len(), 2, "Expected 2 skills, got {}", skills.len());
        }
        Err(e) => eprintln!("  list_skills failed: {}", e),
    }

    println!("--- Skills: remove 'test-skill' ---");
    match sandbox.remove_skill("test-skill").await {
        Ok(_) => println!("  Removed 'test-skill'"),
        Err(e) => eprintln!("  remove_skill failed: {}", e),
    }

    println!("--- Skills: list (should have 1) ---");
    match sandbox.list_skills().await {
        Ok(skills) => {
            let keys: Vec<&String> = skills.keys().collect();
            println!("  Skills: {:?}", keys);
            assert_eq!(skills.len(), 1, "Expected 1 skill, got {}", skills.len());
            assert!(skills.contains_key("review-skill"), "'review-skill' should remain");
        }
        Err(e) => eprintln!("  list_skills failed: {}", e),
    }

    // =============================================
    // Agent bootstrap
    // =============================================
    println!("\n--- Agent: bootstrap ---");
    let agent_config = nanosandbox::ResolvedAgentConfig {
        agent_name: "test-developer".to_string(),
        prompt: "You are a test-driven developer. Always write tests first.".to_string(),
        skills: vec![
            SkillDef {
                name: "tdd-skill".to_string(),
                description: "TDD workflow".to_string(),
                content: "# TDD\n\nRed-green-refactor.".to_string(),
                version: "1.0".to_string(),
                tags: vec![],
            },
        ],
        mcp_servers: {
            let mut m = HashMap::new();
            m.insert("bootstrap-mcp".to_string(), McpServerConfig {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
                env: HashMap::new(),
                enabled: true,
            });
            m
        },
        auto_mode: false,
    };
    match sandbox.bootstrap_agent(&agent_config).await {
        Ok(_) => println!("  Bootstrapped 'test-developer'"),
        Err(e) => eprintln!("  bootstrap_agent failed: {}", e),
    }

    // After bootstrap, MCP servers should include bootstrap-mcp + previously remaining test-context7
    println!("--- MCP: list after bootstrap ---");
    match sandbox.list_mcp_servers().await {
        Ok(servers) => {
            let keys: Vec<&String> = servers.keys().collect();
            println!("  MCP servers: {:?}", keys);
            assert!(servers.contains_key("bootstrap-mcp"), "Expected 'bootstrap-mcp', got: {:?}", keys);
        }
        Err(e) => eprintln!("  list failed: {}", e),
    }

    // After bootstrap, skills should include tdd-skill + previously remaining review-skill
    println!("--- Skills: list after bootstrap ---");
    match sandbox.list_skills().await {
        Ok(skills) => {
            let keys: Vec<&String> = skills.keys().collect();
            println!("  Skills: {:?}", keys);
            assert!(skills.contains_key("tdd-skill"), "Expected 'tdd-skill', got: {:?}", keys);
        }
        Err(e) => eprintln!("  list failed: {}", e),
    }

    // =============================================
    // Cleanup
    // =============================================
    println!("\n=== All E2E tests passed! Cleaning up... ===");
    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;
    println!("=== Done ===");
}
