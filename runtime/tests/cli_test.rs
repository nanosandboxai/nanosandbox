//! CLI Integration Tests
//!
//! Tests for the nanosb CLI tool.

use assert_cmd::Command;
use predicates::prelude::*;

/// Get a command for the nanosb binary
fn nanosb() -> Command {
    Command::cargo_bin("nanosb").unwrap()
}

// ============================================================================
// Help and Version Tests
// ============================================================================

#[test]
fn test_help() {
    nanosb()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Nanosandbox"))
        .stdout(predicate::str::contains("pull"))
        .stdout(predicate::str::contains("images"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("exec"))
        .stdout(predicate::str::contains("ps"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("rm"));
}

#[test]
fn test_version() {
    nanosb()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("nanosb"));
}

// ============================================================================
// Command-specific Help Tests
// ============================================================================

#[test]
fn test_pull_help() {
    nanosb()
        .args(["pull", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pull an image"))
        .stdout(predicate::str::contains("IMAGE"));
}

#[test]
fn test_images_help() {
    nanosb()
        .args(["images", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List cached images"));
}

#[test]
fn test_run_help() {
    nanosb()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Run a command"))
        .stdout(predicate::str::contains("--cpus"))
        .stdout(predicate::str::contains("--memory"));
}

#[test]
fn test_exec_help() {
    nanosb()
        .args(["exec", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Execute a command"));
}

#[test]
fn test_ps_help() {
    nanosb()
        .args(["ps", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List sandboxes"))
        .stdout(predicate::str::contains("--all"));
}

#[test]
fn test_stop_help() {
    nanosb()
        .args(["stop", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stop a running sandbox"));
}

#[test]
fn test_rm_help() {
    nanosb()
        .args(["rm", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Remove a sandbox"))
        .stdout(predicate::str::contains("--force"));
}

// ============================================================================
// Global Options Tests
// ============================================================================

#[test]
fn test_format_option() {
    // Test that --format is recognized
    nanosb()
        .args(["--format", "json", "images"])
        .assert()
        .success();
}

#[test]
fn test_verbose_option() {
    // Test that -v is recognized
    nanosb().args(["-v", "images"]).assert().success();
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_unknown_command() {
    nanosb()
        .arg("unknown")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn test_missing_image_for_pull() {
    nanosb()
        .arg("pull")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_missing_sandbox_for_stop() {
    nanosb()
        .arg("stop")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_missing_sandbox_for_rm() {
    nanosb()
        .arg("rm")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_missing_sandbox_for_exec() {
    nanosb()
        .arg("exec")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_missing_image_for_run() {
    nanosb()
        .arg("run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// ============================================================================
// Images Command Tests
// ============================================================================

#[test]
fn test_images_empty() {
    // This test might have cached images, so we just check it runs
    nanosb().arg("images").assert().success();
}

#[test]
fn test_images_json_format() {
    nanosb()
        .args(["--format", "json", "images"])
        .assert()
        .success();
}

// ============================================================================
// Ps Command Tests
// ============================================================================

#[test]
fn test_ps_empty() {
    nanosb().arg("ps").assert().success();
}

#[test]
fn test_ps_all() {
    nanosb().args(["ps", "-a"]).assert().success();
}

#[test]
fn test_ps_json_format() {
    nanosb().args(["--format", "json", "ps"]).assert().success();
}

// ============================================================================
// Stop/Rm with Non-existent Sandbox Tests
// ============================================================================

#[test]
fn test_stop_nonexistent() {
    nanosb()
        .args(["stop", "nonexistent-sandbox-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_rm_nonexistent() {
    nanosb()
        .args(["rm", "nonexistent-sandbox-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

// ============================================================================
// Network-dependent Tests
// ============================================================================

/// Test pulling an image (requires network)
#[test]
fn test_pull_alpine() {
    nanosb()
        .args(["pull", "alpine:3.19"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pulled"));
}

/// Test pull with JSON output (requires network)
#[test]
fn test_pull_json() {
    nanosb()
        .args(["--format", "json", "pull", "alpine:3.19"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"image\""))
        .stdout(predicate::str::contains("\"layers\""));
}
