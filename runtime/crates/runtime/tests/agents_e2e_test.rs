//! End-to-end tests for agents from agents-registry running in Nanosandbox
//!
//! These tests verify that all AI coding agents from the agents-registry
//! can be executed within sandboxes managed by Nanosandbox.
//!
//! Disabled in CI — enable with: cargo test --features integration-tests
#![cfg(feature = "integration-tests")]
//!
//! ## Platform Support
//!
//! - **Linux/macOS**: Uses `ghcr.io/nanosandboxai/nanosandbox/<agent>:latest`

//!
//! ## Agents Tested
//!
//! | Agent       | Command        | Description                      |
//! |-------------|----------------|----------------------------------|
//! | Claude Code | `claude`       | Anthropic's AI coding assistant  |
//! | Goose       | `goose`        | AI developer agent by Block      |
//! | Codex       | `codex`        | OpenAI's coding assistant        |
//! | Cursor CLI  | `cursor-agent` | Cursor's AI agent                |
//! | OpenCode     | `opencode`     | Open-source AI coding agent      |
//! | Aider       | `aider`        | AI pair programming in terminal  |
//! | Gemini CLI  | `gemini`       | Google's AI coding assistant     |
//! | Crush       | `crush`        | AI coding agent                  |
//!
//! ## Running Tests
//!
//! ```bash
//! # Run all agent tests
//! cargo test --test agents_e2e_test
//!
//! # Run with output
//! cargo test --test agents_e2e_test -- --nocapture
//!
//! # Run ignored tests (requires full runtime setup)
//! cargo test --test agents_e2e_test -- --ignored
//! ```

use runtime::config::SandboxConfig;
use runtime::Sandbox;

// =============================================================================
// Agent Definitions
// =============================================================================

/// Agent definition: (name, command, args for version check)
struct AgentDef {
    name: &'static str,
    command: &'static str,
    version_args: &'static [&'static str],
}

/// All agents available in agents-registry
const AGENTS: &[AgentDef] = &[
    AgentDef {
        name: "Claude Code",
        command: "claude",
        version_args: &["--version"],
    },
    AgentDef {
        name: "Goose",
        command: "goose",
        version_args: &["--version"],
    },
    AgentDef {
        name: "Codex",
        command: "codex",
        version_args: &["--version"],
    },
    AgentDef {
        name: "Cursor CLI",
        command: "cursor-agent",
        version_args: &["--version"],
    },
    AgentDef {
        name: "OpenCode",
        command: "opencode",
        version_args: &["--version"],
    },
    AgentDef {
        name: "Aider",
        command: "aider",
        version_args: &["--version"],
    },
    AgentDef {
        name: "Gemini CLI",
        command: "gemini",
        version_args: &["--version"],
    },
    AgentDef {
        name: "Crush",
        command: "crush",
        version_args: &["--version"],
    },
];

// =============================================================================
// Platform-specific Image Configuration
// =============================================================================

/// Default image tag for testing (RC builds for pre-release validation)
const IMAGE_TAG: &str = "rc1";

/// Default registry prefix for agent images
const DEFAULT_REGISTRY: &str = "ghcr.io/nanosandboxai/nanosandbox";

/// Build the image reference for a given agent.
///
/// The registry prefix and tag are overridable via the `NANOSB_E2E_REGISTRY`
/// and `NANOSB_E2E_TAG` environment variables (e.g. to test locally-built
/// images pushed to a local registry).
fn agent_image(agent_command: &str) -> String {
    let registry = std::env::var("NANOSB_E2E_REGISTRY")
        .unwrap_or_else(|_| DEFAULT_REGISTRY.to_string());
    let tag = std::env::var("NANOSB_E2E_TAG").unwrap_or_else(|_| IMAGE_TAG.to_string());
    format!("{}/{}:{}", registry, agent_command, tag)
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Check if runtime prerequisites are met
async fn runtime_available() -> bool {
    use runtime::runtime::validate_runtime_prerequisites;
    validate_runtime_prerequisites().await.is_ok()
}

/// Stage the guest exec agent into a persistent host dir (once per process).
fn stage_exec_agent_dir() -> std::path::PathBuf {
    use std::sync::OnceLock;
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nanosb-e2e-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = std::env::var("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".nanosandbox/guest-bin/exec-agent"))
            .unwrap_or_default();
        let dst = dir.join("exec-agent");
        if !dst.exists() {
            std::fs::copy(&src, &dst).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755));
            }
        }
        dir
    })
    .clone()
}

/// Create a sandbox configuration for agent testing, with the host↔guest exec
/// channel wired up so commands can be run inside the guest.
fn create_agent_sandbox_config(sandbox_name: &str, image: &str) -> SandboxConfig {
    let mut config = SandboxConfig::builder()
        .name(sandbox_name)
        .image(image)
        .cpus(2)
        .memory_mb(1024)
        .runtime_mode(runtime::config::RuntimeMode::Next)
        .user("1000")
        .build();

    let agent_dir = stage_exec_agent_dir();
    config.extra_mounts.push(runtime::config::ExtraMount {
        tag: "agent".to_string(),
        host_path: agent_dir.to_string_lossy().to_string(),
        target: "/agent".to_string(),
        readonly: false,
    });
    let exec_sock = std::env::temp_dir().join(format!("nanosb-e2e-{}.sock", sandbox_name));
    let _ = std::fs::remove_file(&exec_sock);
    config.vsock_socket = Some(exec_sock.to_string_lossy().to_string());
    config.vsock_port = Some(1024);
    config.exec_agent = Some("/agent/exec-agent".to_string());
    config
}

/// Create an exec client for a started sandbox's vsock exec channel, waiting
/// for the host socket to appear (the VM boots in a child process).
fn exec_client(sandbox: &Sandbox) -> runtime::exec::ExecClient {
    let sock = sandbox.config().vsock_socket.clone().unwrap_or_default();
    let path = std::path::PathBuf::from(&sock);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !path.exists() {
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    runtime::exec::ExecClient::new(sock)
}

/// Run a command in the sandbox with a PATH that includes the agent binary
/// dirs. opencode/aider install under `/root/...`, which is not on the exec
/// agent's default PATH.
fn exec_agent_command(
    sandbox: &Sandbox,
    command: &str,
    args: &[&str],
) -> runtime::exec::ExecResultT<runtime::exec::ExecResult> {
    let opts = runtime::exec::ExecOptions::new().env(
        "PATH",
        "/root/.opencode/bin:/root/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    );
    exec_client(sandbox).exec_with(command, args, opts)
}

// =============================================================================
// Individual Agent Tests (require runtime + image)
// =============================================================================

/// Test Claude Code agent version command
#[tokio::test]
#[ignore]
async fn test_agent_claude_version() {
    let result = test_agent_version("Claude Code", "claude", &["--version"]).await;
    assert!(
        result.is_ok(),
        "Claude agent test failed: {:?}",
        result.err()
    );
}

/// Test Goose agent version command
#[tokio::test]
#[ignore]
async fn test_agent_goose_version() {
    let result = test_agent_version("Goose", "goose", &["--version"]).await;
    assert!(
        result.is_ok(),
        "Goose agent test failed: {:?}",
        result.err()
    );
}

/// Test Codex agent version command
#[tokio::test]
#[ignore]
async fn test_agent_codex_version() {
    let result = test_agent_version("Codex", "codex", &["--version"]).await;
    assert!(
        result.is_ok(),
        "Codex agent test failed: {:?}",
        result.err()
    );
}

/// Test Cursor CLI agent version command
#[tokio::test]
#[ignore]
async fn test_agent_cursor_version() {
    let result = test_agent_version("Cursor CLI", "cursor-agent", &["--version"]).await;
    assert!(
        result.is_ok(),
        "Cursor agent test failed: {:?}",
        result.err()
    );
}

/// Result type for agent tests
#[derive(Debug)]
#[allow(dead_code)]
enum AgentTestResult {
    Success,
    Skipped(String),
    Failed(String),
}

#[allow(dead_code)]
impl AgentTestResult {
    fn is_ok(&self) -> bool {
        matches!(self, AgentTestResult::Success | AgentTestResult::Skipped(_))
    }

    fn err(&self) -> Option<&str> {
        match self {
            AgentTestResult::Failed(msg) => Some(msg),
            _ => None,
        }
    }
}

/// Generic agent version test helper
///
/// Returns:
/// - `Ok(Success)` if the agent executed successfully
/// - `Ok(Skipped)` if prerequisites are not met (no runtime)
/// - `Err(Failed)` if the image pull or execution failed
async fn test_agent_version(
    agent_name: &str,
    command: &str,
    args: &[&str],
) -> Result<AgentTestResult, AgentTestResult> {
    println!("\n========================================");
    println!("  Testing {} Agent", agent_name);
    println!("========================================");
    println!("Image: {}", agent_image(command));
    println!("Command: {} {}", command, args.join(" "));

    // Check runtime availability - skip if not met (this is expected in some environments)
    if !runtime_available().await {
        let msg = "Runtime prerequisites not met. Install libkrun (macOS/Linux)".to_string();
        println!("[SKIP] {}", msg);
        return Ok(AgentTestResult::Skipped(msg));
    }
    println!("[OK] Runtime available");

    // Create sandbox
    let sandbox_name = format!("agent-test-{}", command.replace('-', "_"));
    let config = create_agent_sandbox_config(&sandbox_name, &agent_image(command));

    println!("\n[Step 1] Creating sandbox...");
    let sandbox_result = Sandbox::create(config).await;

    match sandbox_result {
        Ok(mut sandbox) => {
            println!("[OK] Sandbox created: {}", sandbox.id());

            // Start sandbox
            println!("\n[Step 2] Starting sandbox...");
            match sandbox.start().await {
                Ok(_) => {
                    println!("[OK] Sandbox started");

                    // Execute version command
                    println!("\n[Step 3] Executing: {} {}", command, args.join(" "));
                    let test_result = match exec_agent_command(&sandbox, command, args) {
                        Ok(result) => {
                            println!("\n[OUTPUT]");
                            println!("Exit code: {}", result.exit_code);
                            if !result.stdout.is_empty() {
                                println!("Stdout:\n{}", result.stdout);
                            }
                            if !result.stderr.is_empty() {
                                println!("Stderr:\n{}", result.stderr);
                            }

                            // Verify success
                            if result.exit_code == 0 {
                                println!("\n[PASS] {} --version executed successfully", command);
                                Ok(AgentTestResult::Success)
                            } else {
                                let msg = format!(
                                    "{} returned non-zero exit code: {}",
                                    command, result.exit_code
                                );
                                println!("\n[FAIL] {}", msg);
                                Err(AgentTestResult::Failed(msg))
                            }
                        }
                        Err(e) => {
                            let msg = format!("Exec failed: {}", e);
                            println!("[FAIL] {}", msg);
                            Err(AgentTestResult::Failed(msg))
                        }
                    };

                    // Stop sandbox
                    println!("\n[Step 4] Stopping sandbox...");
                    let _ = sandbox.stop().await;

                    // Cleanup
                    println!("\n[Step 5] Destroying sandbox...");
                    let _ = sandbox.destroy().await;
                    println!("[OK] Cleanup complete");

                    test_result
                }
                Err(e) => {
                    let msg = format!("Failed to start sandbox: {}", e);
                    println!("[FAIL] {}", msg);
                    let _ = sandbox.destroy().await;
                    Err(AgentTestResult::Failed(msg))
                }
            }
        }
        Err(e) => {
            // Image pull failure is a FAILURE, not a skip
            // Runtime was available but image couldn't be pulled
            let msg = format!(
                "Sandbox creation failed (image pull error): {}\n\
                 Ensure the agent image is available:\n\
                 docker pull {}",
                e,
                agent_image(command)
            );
            println!("[FAIL] {}", msg);
            Err(AgentTestResult::Failed(msg))
        }
    }
}

// =============================================================================
// Comprehensive Agent Tests (requires full setup)
// =============================================================================

/// Test all agents in sequence - requires runtime and image to be available
///
/// This test verifies all agents can execute their version commands successfully.
///
/// This test will FAIL if:
/// - Runtime is available but image cannot be pulled
/// - Any agent returns non-zero exit code
///
/// Run with: cargo test --test agents_e2e_test test_all_agents -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn test_all_agents_version() {
    println!("\n========================================");
    println!("  Agents Registry Comprehensive Test");
    println!("========================================");
    println!("Image pattern: {} (e.g. {})", agent_image("<agent>"), agent_image("claude"));
    println!("Testing {} agents\n", AGENTS.len());

    // Check runtime - skip entire test if not met
    if !runtime_available().await {
        println!("[SKIP] Runtime prerequisites not met");
        println!("[INFO] This test requires libkrun (macOS/Linux)");
        return;
    }
    println!("[OK] Runtime available");

    let mut passed = 0;
    let mut failed = 0;
    let mut image_pull_failed = false;

    for agent in AGENTS {
        println!("\n--- Testing {} ---", agent.name);

        let sandbox_name = format!("all-agents-{}", agent.command.replace('-', "_"));
        let config = create_agent_sandbox_config(&sandbox_name, &agent_image(agent.command));

        match Sandbox::create(config).await {
            Ok(mut sandbox) => {
                if let Err(e) = sandbox.start().await {
                    println!("[FAIL] Start failed: {}", e);
                    failed += 1;
                    let _ = sandbox.destroy().await;
                    continue;
                }

                match exec_agent_command(&sandbox, agent.command, agent.version_args) {
                    Ok(result) => {
                        if result.exit_code == 0 {
                            println!(
                                "[PASS] {}: {}",
                                agent.name,
                                result.stdout.lines().next().unwrap_or("OK")
                            );
                            passed += 1;
                        } else {
                            println!("[FAIL] {}: exit code {}", agent.name, result.exit_code);
                            if !result.stderr.is_empty() {
                                println!(
                                    "       stderr: {}",
                                    result.stderr.lines().next().unwrap_or("")
                                );
                            }
                            failed += 1;
                        }
                    }
                    Err(e) => {
                        println!("[FAIL] {}: {}", agent.name, e);
                        failed += 1;
                    }
                }

                let _ = sandbox.stop().await;
                let _ = sandbox.destroy().await;
            }
            Err(e) => {
                // Image pull failure is a FAILURE when runtime is available
                println!("[FAIL] {}: sandbox creation failed: {}", agent.name, e);
                failed += 1;
                image_pull_failed = true;
            }
        }
    }

    println!("\n========================================");
    println!("  Test Summary");
    println!("========================================");
    println!("Total:   {}", AGENTS.len());
    println!("Passed:  {}", passed);
    println!("Failed:  {}", failed);
    println!("========================================\n");

    // Fail the test if any agent failed
    if image_pull_failed {
        panic!(
            "Image pull failed. Ensure agent images are available at ghcr.io/nanosandboxai/nanosandbox/"
        );
    }

    assert_eq!(
        failed,
        0,
        "{} out of {} agent tests failed",
        failed,
        AGENTS.len()
    );
}

// =============================================================================
// Non-Root Guest User Test
// =============================================================================

/// Test that the guest user is NON-ROOT inside a pure agent image.
///
/// Pure images must not run as root: `id -u` must exit 0 and print a uid != 0.
#[tokio::test]
#[ignore]
async fn test_agent_guest_user_is_non_root() {
    println!("\n========================================");
    println!("  Guest User Non-Root Test (pure image)");
    println!("========================================");

    if !runtime_available().await {
        println!("[SKIP] Runtime prerequisites not met");
        return;
    }
    println!("[OK] Runtime available");

    let command = "claude";
    let sandbox_name = "nonroot-check";
    let config = create_agent_sandbox_config(sandbox_name, &agent_image(command));

    println!("\n[Step 1] Creating sandbox...");
    let mut sandbox = match Sandbox::create(config).await {
        Ok(s) => s,
        Err(e) => panic!("Sandbox creation failed: {}", e),
    };
    println!("[OK] Sandbox created: {}", sandbox.id());

    println!("\n[Step 2] Starting sandbox...");
    sandbox.start().await.expect("sandbox start failed");
    println!("[OK] Sandbox started");

    println!("\n[Step 3] Executing: id -u");
    let result = exec_client(&sandbox)
        .exec("id", &["-u"])
        .expect("exec id -u failed");
    println!("Exit code: {}", result.exit_code);
    println!("Stdout: {}", result.stdout.trim());

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;

    assert_eq!(result.exit_code, 0, "id -u failed");
    let uid = result.stdout.trim();
    assert_ne!(uid, "0", "guest user is root (uid=0) in pure image");
    println!("[PASS] guest user is non-root (uid={})", uid);
}

// =============================================================================
// Image Pull Test
// =============================================================================

/// Test pulling an agent image from agents-registry
#[tokio::test]
async fn test_pull_agents_registry_image() {
    use runtime::image::ImageManager;
    use tempfile::TempDir;

    println!("\n========================================");
    println!("  Agents Registry Image Pull Test");
    println!("========================================");
    let image = agent_image("claude");
    println!("Image: {}", image);

    let temp_dir = TempDir::new().unwrap();
    let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();

    println!("\n[Step 1] Pulling image...");
    println!("[INFO] This may take several minutes on first run");

    match manager.pull(&image).await {
        Ok(pulled) => {
            println!("[OK] Image pulled successfully");
            println!("     Layers: {}", pulled.layers.len());
            println!("     Size: {} bytes", pulled.size);

            // Verify layers are cached
            for (i, layer) in pulled.layers.iter().enumerate() {
                let cached = manager.layer_exists(layer);
                println!(
                    "     Layer {}: {} (cached: {})",
                    i + 1,
                    &layer[..12],
                    cached
                );
            }

            println!("\n[PASS] Agents registry image pull test passed");
        }
        Err(e) => {
            println!("[SKIP] Image pull failed: {}", e);
            println!("[INFO] This may be expected if:");
            println!("       - No network access");
            println!("       - GHCR authentication required");
            println!("       - Image not published yet");
        }
    }

    println!("\n========================================\n");
}

// =============================================================================
// Environment Report
// =============================================================================

/// Print environment information for debugging
#[tokio::test]
async fn test_agents_environment_report() {
    println!("\n========================================");
    println!("  Agent Testing Environment Report");
    println!("========================================\n");

    // Platform
    println!("[Platform]");
    println!("  OS: {}", std::env::consts::OS);
    println!("  Arch: {}", std::env::consts::ARCH);
    println!("  Image pattern: {} (e.g. {})", agent_image("<agent>"), agent_image("claude"));

    // Runtime
    println!("\n[Runtime Status]");
    if runtime_available().await {
        println!("  Status: [AVAILABLE]");
    } else {
        println!("  Status: [NOT AVAILABLE]");
        println!("  Hint: Install runtime prerequisites for your platform");
    }

    // Agents
    println!("\n[Agents to Test]");
    for agent in AGENTS {
        println!(
            "  - {}: {} {}",
            agent.name,
            agent.command,
            agent.version_args.join(" ")
        );
    }

    println!("\n========================================\n");
}
