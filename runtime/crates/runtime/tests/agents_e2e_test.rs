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
    let registry =
        std::env::var("NANOSB_E2E_REGISTRY").unwrap_or_else(|_| DEFAULT_REGISTRY.to_string());
    let tag = std::env::var("NANOSB_E2E_TAG").unwrap_or_else(|_| IMAGE_TAG.to_string());
    // The image repository name differs from the in-guest binary for Cursor:
    // the image is `cursor`, but the binary it ships is `cursor-agent`.
    let repo = match agent_command {
        "cursor-agent" => "cursor",
        other => other,
    };
    format!("{}/{}:{}", registry, repo, tag)
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Check if runtime prerequisites are met
async fn runtime_available() -> bool {
    use runtime::runtime::validate_runtime_prerequisites;
    validate_runtime_prerequisites().await.is_ok()
}

/// Point the runtime at a real `nanosb` binary for the VM boot subprocess.
///
/// The runtime boots each VM by spawning `<exe> internal-boot-vm` (see
/// `boot_persistent_vm_next`). When the test binary is the current exe, libtest
/// treats `internal-boot-vm` as a test filter and runs zero tests, so no VM ever
/// boots and the exec socket never appears. The product path spawns the real
/// `nanosb` binary; do the same here by setting `NANOSB_BINARY_PATH` (honored by
/// the runtime) to the workspace's built `nanosb` before any sandbox starts.
fn ensure_boot_binary() {
    use std::sync::OnceLock;
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        if std::env::var_os("NANOSB_BINARY_PATH").is_some() {
            return;
        }
        // CARGO_MANIFEST_DIR = <workspace>/runtime/crates/runtime
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent());
        if let Some(root) = root {
            for profile in ["release", "debug"] {
                let candidate = root.join("target").join(profile).join("nanosb");
                if candidate.exists() {
                    std::env::set_var("NANOSB_BINARY_PATH", &candidate);
                    return;
                }
            }
        }
    });
}

/// Stage the guest exec agent into a persistent host dir (once per process).
fn stage_exec_agent_dir() -> std::path::PathBuf {
    use std::sync::OnceLock;
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nanosb-e2e-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Mirror `nanosb_cli::deploy::guest_bin_dir` resolution.
        let src = std::env::var("NANOSB_GUEST_BIN_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|_| {
                std::env::var("NANOSB_HOME").map(|h| std::path::PathBuf::from(h).join("guest-bin"))
            })
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".nanosandbox/guest-bin"))
                    .unwrap_or_default()
            })
            .join("exec-agent");
        let dst = dir.join("exec-agent");
        if !dst.exists() {
            std::fs::copy(&src, &dst).unwrap_or_else(|e| {
                panic!(
                    "failed to stage exec-agent from {}: {} (build it with \
                     `cd guest/crates/exec-agent && RUSTFLAGS=\"-C linker=rust-lld\" \
                     cargo build --target aarch64-unknown-linux-musl --release`)",
                    src.display(),
                    e
                )
            });
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

const MOUNT_MARKER: &str = "nanosb-mount-marker";

/// Stage a host dir whose marker file proves a virtiofs mount landed in the guest.
fn stage_mount_dir(kind: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nanosb-e2e-mount-{}", kind));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MOUNT_MARKER), kind).unwrap();
    dir
}

/// Create a sandbox configuration for agent testing, with the host↔guest exec
/// channel wired up so commands can be run inside the guest.
fn create_agent_sandbox_config(sandbox_name: &str, image: &str) -> SandboxConfig {
    ensure_boot_binary();

    let mut config = SandboxConfig::builder()
        .name(sandbox_name)
        .image(image)
        .cpus(2)
        .memory_mb(1024)
        .runtime_mode(runtime::config::RuntimeMode::Next)
        .build();
    // Mirror the product: the TUI runs agents as the non-root guest user, so an
    // image whose CLI lands under /root (0700) must still be exec'able here.
    config.user = Some("1000".to_string());

    let agent_dir = stage_exec_agent_dir();
    config.extra_mounts.push(runtime::config::ExtraMount {
        tag: "agent".to_string(),
        host_path: agent_dir.to_string_lossy().to_string(),
        target: "/agent".to_string(),
        readonly: false,
    });
    // Mirror the product's layout (project at the guest HOME, agent state nested
    // inside). Two or more extra mounts is the path that regressed.
    config.extra_mounts.push(runtime::config::ExtraMount {
        tag: "nanosb-0".to_string(),
        host_path: stage_mount_dir("project").to_string_lossy().to_string(),
        target: "/home/developer".to_string(),
        readonly: false,
    });
    config.extra_mounts.push(runtime::config::ExtraMount {
        tag: "nanosb-1".to_string(),
        host_path: stage_mount_dir("state").to_string_lossy().to_string(),
        target: "/home/developer/.config/opencode".to_string(),
        readonly: false,
    });
    let exec_sock = std::env::temp_dir().join(format!("nanosb-e2e-{}.sock", sandbox_name));
    let _ = std::fs::remove_file(&exec_sock);
    config.vsock_socket = Some(exec_sock.to_string_lossy().to_string());
    config.vsock_port = Some(1024);
    // Mirror the product's no-workload path: the exec agent is PID 1 and serves
    // the vsock exec channel directly.
    config.command = Some("/agent/exec-agent".to_string());
    config.command_args = vec!["1024".to_string()];
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

/// Run an agent command in the sandbox. PATH is the image's own ENV (adopted by
/// the runtime), so CLIs installed outside the stock PATH (opencode/aider under
/// `/root/...`) resolve. No PATH override here on purpose — that is the contract.
fn exec_agent_command(
    sandbox: &Sandbox,
    command: &str,
    args: &[&str],
) -> runtime::exec::ExecResultT<runtime::exec::ExecResult> {
    exec_client(sandbox).exec(command, args)
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
    let mut config = create_agent_sandbox_config(sandbox_name, &agent_image(command));
    // This test specifically exercises the non-root guest user path.
    config.user = Some("1000".to_string());

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
// Extra Virtiofs Mount Test
// =============================================================================

/// Boot with the product's multi-mount set and prove every mount landed in the
/// guest. Regression guard: the parser used to mount only the first extra mount
/// and silently skip the rest.
#[tokio::test]
#[ignore]
async fn test_agent_extra_mounts_land() {
    println!("\n========================================");
    println!("  Extra Virtiofs Mounts Test");
    println!("========================================");

    if !runtime_available().await {
        println!("[SKIP] Runtime prerequisites not met");
        return;
    }

    let sandbox_name = "extra-mounts";
    let config = create_agent_sandbox_config(sandbox_name, &agent_image("claude"));
    let mut sandbox = Sandbox::create(config)
        .await
        .expect("sandbox create failed");
    sandbox.start().await.expect("sandbox start failed");

    let client = exec_client(&sandbox);
    let project = client
        .exec("cat", &["/home/developer/nanosb-mount-marker"])
        .expect("failed to exec cat on project marker");
    let state = client
        .exec("cat", &["/home/developer/.config/opencode/nanosb-mount-marker"])
        .expect("failed to exec cat on state marker");

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;

    assert_eq!(
        project.exit_code, 0,
        "project mount missing in guest: {}",
        project.stderr
    );
    assert_eq!(project.stdout.trim(), "project", "wrong project marker");
    assert_eq!(
        state.exit_code, 0,
        "nested state mount missing in guest: {}",
        state.stderr
    );
    assert_eq!(state.stdout.trim(), "state", "wrong state marker");
    println!("[PASS] all extra mounts landed in the guest");
}

/// Start an agent interactively over a PTY (as the TUI panel does) and assert it
/// comes up and emits output without an early non-zero exit. Interactive TUIs
/// keep running, so we cannot assert on exit code; set NANOSB_E2E_AGENT to pick
/// the agent (defaults to claude).
#[tokio::test]
#[ignore]
async fn test_agent_interactive_tty_smoke() {
    let agent = std::env::var("NANOSB_E2E_AGENT").unwrap_or_else(|_| "claude".to_string());

    if !runtime_available().await {
        println!("[SKIP] Runtime prerequisites not met");
        return;
    }

    let sandbox_name = format!("interactive-{}", agent);
    let config = create_agent_sandbox_config(&sandbox_name, &agent_image(&agent));
    let mut sandbox = Sandbox::create(config)
        .await
        .expect("sandbox create failed");
    sandbox.start().await.expect("sandbox start failed");

    let opts = runtime::exec::ExecOptions::new().tty(true).size(120, 40);
    let mut handle = exec_client(&sandbox)
        .start(&agent, &[], opts)
        .expect("exec start failed");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut bytes = 0usize;
    let mut early_exit = None;
    while std::time::Instant::now() < deadline {
        match handle.next_event(Some(std::time::Duration::from_secs(2))) {
            Ok(Some(runtime::exec::ExecEvent::Output(chunk))) => bytes += chunk.data.len(),
            Ok(Some(runtime::exec::ExecEvent::Exit { code })) => {
                early_exit = Some(code);
                break;
            }
            Ok(Some(runtime::exec::ExecEvent::Error { message })) => {
                early_exit = Some(-1);
                println!("[TTY] {} exec error: {}", agent, message);
            }
            _ => {}
        }
    }

    let _ = sandbox.stop().await;
    let _ = sandbox.destroy().await;

    println!(
        "[TTY] {} output_bytes={} early_exit={:?}",
        agent, bytes, early_exit
    );
    assert!(bytes > 0, "{} produced no PTY output", agent);
    assert!(
        early_exit.is_none() || early_exit == Some(0),
        "{} exited non-zero over PTY: {:?}",
        agent,
        early_exit
    );
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
