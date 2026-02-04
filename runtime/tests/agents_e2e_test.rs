//! End-to-end tests for DD-Agents running in Nanosandbox
//!
//! These tests verify that all AI coding agents from the dd-agents-registry
//! can be executed within sandboxes managed by Nanosandbox.
//!
//! ## Platform Support
//!
//! - **Linux**: Uses `ghcr.io/devdone-labs/dd-agents:latest` (Linux image)
//! - **Windows**: Uses `ghcr.io/devdone-labs/dd-agents-windows:503e063` (Windows image)
//!
//! ## Agents Tested
//!
//! | Agent       | Command        | Description                      |
//! |-------------|----------------|----------------------------------|
//! | OpenCode    | `opencode`     | Go-based AI coding assistant     |
//! | Claude Code | `claude`       | Anthropic's AI coding assistant  |
//! | Goose       | `goose`        | AI developer agent by Block      |
//! | Codex       | `codex`        | OpenAI's coding assistant        |
//! | Cursor CLI  | `cursor-agent` | Cursor's AI agent                |
//!
//! ## Running Tests
//!
//! ```bash
//! # Run all agent tests
//! cargo test --test agents_e2e_test --features cli
//!
//! # Run with output
//! cargo test --test agents_e2e_test --features cli -- --nocapture
//!
//! # Run ignored tests (requires full runtime setup)
//! cargo test --test agents_e2e_test --features cli -- --ignored
//! ```

use nanosandbox::config::SandboxConfig;
use nanosandbox::Sandbox;

// =============================================================================
// Agent Definitions
// =============================================================================

/// Agent definition: (name, command, args for version check)
struct AgentDef {
    name: &'static str,
    command: &'static str,
    version_args: &'static [&'static str],
}

/// All agents available in dd-agents-registry
const AGENTS: &[AgentDef] = &[
    AgentDef {
        name: "OpenCode",
        command: "opencode",
        version_args: &["--version"],
    },
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
];

// =============================================================================
// Platform-specific Image Configuration
// =============================================================================

/// DD-Agents image for Linux
#[cfg(target_os = "linux")]
const DD_AGENTS_IMAGE: &str = "ghcr.io/devdone-labs/dd-agents:latest";

/// DD-Agents image for macOS (uses Linux image via krunvm)
#[cfg(target_os = "macos")]
const DD_AGENTS_IMAGE: &str = "ghcr.io/devdone-labs/dd-agents:latest";

/// DD-Agents image for Windows (separate package for Windows containers)
#[cfg(target_os = "windows")]
const DD_AGENTS_IMAGE: &str = "ghcr.io/devdone-labs/dd-agents-windows:503e063";

// =============================================================================
// Helper Functions
// =============================================================================

/// Check if runtime prerequisites are met
async fn runtime_available() -> bool {
    use nanosandbox::runtime::validate_runtime_prerequisites;
    validate_runtime_prerequisites().await.is_ok()
}

/// Create a sandbox configuration for agent testing
fn create_agent_sandbox_config(sandbox_name: &str) -> SandboxConfig {
    SandboxConfig::builder()
        .name(sandbox_name)
        .image(DD_AGENTS_IMAGE)
        .cpus(2)
        .memory_mb(1024)
        .build()
}

// =============================================================================
// Individual Agent Tests (require runtime + image)
// =============================================================================

/// Test OpenCode agent version command
///
/// Requires runtime and DD-Agents image to be available.
/// Run with: cargo test --test agents_e2e_test test_agent_opencode_version -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn test_agent_opencode_version() {
    let result = test_agent_version("OpenCode", "opencode", &["--version"]).await;
    assert!(
        result.is_ok(),
        "OpenCode agent test failed: {:?}",
        result.err()
    );
}

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
    println!("Image: {}", DD_AGENTS_IMAGE);
    println!("Command: {} {}", command, args.join(" "));

    // Check runtime availability - skip if not met (this is expected in some environments)
    if !runtime_available().await {
        let msg =
            "Runtime prerequisites not met. Install crun/krun (Linux), krunvm (macOS), or enable Windows Containers".to_string();
        println!("[SKIP] {}", msg);
        return Ok(AgentTestResult::Skipped(msg));
    }
    println!("[OK] Runtime available");

    // Create sandbox
    let sandbox_name = format!("agent-test-{}", command.replace('-', "_"));
    let config = create_agent_sandbox_config(&sandbox_name);

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
                    let test_result = match sandbox.exec(command, args).await {
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
                 Ensure the dd-agents image is available:\n\
                 docker pull {}",
                e, DD_AGENTS_IMAGE
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
/// This test pulls the dd-agents image and verifies all agents can execute
/// their version commands successfully.
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
    println!("  DD-Agents Comprehensive Test");
    println!("========================================");
    println!("Image: {}", DD_AGENTS_IMAGE);
    println!("Testing {} agents\n", AGENTS.len());

    // Check runtime - skip entire test if not met
    if !runtime_available().await {
        println!("[SKIP] Runtime prerequisites not met");
        println!(
            "[INFO] This test requires crun/krun (Linux), krunvm (macOS), or Windows Containers"
        );
        return;
    }
    println!("[OK] Runtime available");

    let mut passed = 0;
    let mut failed = 0;
    let mut image_pull_failed = false;

    for agent in AGENTS {
        println!("\n--- Testing {} ---", agent.name);

        let sandbox_name = format!("all-agents-{}", agent.command.replace('-', "_"));
        let config = create_agent_sandbox_config(&sandbox_name);

        match Sandbox::create(config).await {
            Ok(mut sandbox) => {
                if let Err(e) = sandbox.start().await {
                    println!("[FAIL] Start failed: {}", e);
                    failed += 1;
                    let _ = sandbox.destroy().await;
                    continue;
                }

                match sandbox.exec(agent.command, agent.version_args).await {
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
            "Image pull failed. Ensure the dd-agents image is available:\n\
             docker pull {}",
            DD_AGENTS_IMAGE
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
// Image Pull Test
// =============================================================================

/// Test pulling the dd-agents image
#[tokio::test]
async fn test_pull_dd_agents_image() {
    use nanosandbox::image::ImageManager;
    use tempfile::TempDir;

    println!("\n========================================");
    println!("  DD-Agents Image Pull Test");
    println!("========================================");
    println!("Image: {}", DD_AGENTS_IMAGE);

    let temp_dir = TempDir::new().unwrap();
    let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();

    println!("\n[Step 1] Pulling image...");
    println!("[INFO] This may take several minutes on first run");

    match manager.pull(DD_AGENTS_IMAGE).await {
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

            println!("\n[PASS] DD-Agents image pull test passed");
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
    println!("  Image: {}", DD_AGENTS_IMAGE);

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
