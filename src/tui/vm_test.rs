//! Scripted VM end-to-end test for the TUI.
//!
//! Drives the extracted headless event loop through a real supervised sandbox
//! lifecycle: create, connect, send data, reconnect, kill, verify cleanup.
//!
//! ## Run command
//!
//! ```text
//! cargo test -p nanosb-cli tui::vm_test -- --ignored --nocapture
//! ```
//!
//! ## Requirements
//!
//! - libkrun / libkrunfw (installed by `nanosb doctor` or `scripts/install/macos.sh`)
//! - gvproxy (installed by the same scripts)
//! - `HYPERVISOR` entitlement (codesigned binary, or run via `scripts/tui-smoke.sh`)
//! - Network access to pull `alpine:latest` from Docker Hub
//! - A real terminal (the test does not enter alternate screen, but the VM
//!   engine requires the binary to be codesigned for the `HYPERVISOR` entitlement)

#![cfg(test)]

use std::time::Duration;

use tokio::sync::mpsc;

use sandbox::AgentSandboxConfig;

use super::app::{App, PanelMode};
use super::event::AppEvent;
use super::run::{add_agent_from_config, handle_command, handle_event};
use super::commands::Command;

/// Run the full TUI VM end-to-end lifecycle.
///
/// Steps:
/// 1. Create a unique sandbox name.
/// 2. Build a minimal `AgentSandboxConfig` for `alpine:latest`.
/// 3. Construct `App` and event channel.
/// 4. Call `add_agent_from_config` to push the panel and spawn the supervisor.
/// 5. Assert a panel was pushed.
/// 6. Pump events until the panel reaches `PanelMode::Terminal` (120s timeout).
/// 7. Assert `supervisor_name().is_some()` and `mode == PanelMode::Terminal`.
/// 8. Assert console output (`VM_E2E_READY`) reaches the panel as TerminalData.
/// 9. `/reconnect` — assert reconnecting state, then wait for Terminal again.
/// 10. `/kill` — assert panel list is empty.
/// 11. Assert the sandbox is gone via `SupervisorClient`.
/// 12. Best-effort cleanup.
#[tokio::test]
#[ignore]
async fn test_tui_vm_end_to_end() {
    let name = format!("tui-vm-test-{}", std::process::id());

    // Build a minimal config for a plain shell image (no agent CLI).
    // A command is required: with none the guest just holds the VM open and
    // the console produces no output, so there would be no TerminalData to
    // assert. This prints a marker then stays alive for the test window.
    let mut config = AgentSandboxConfig::builder()
        .name(&name)
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(512)
        .timeout_secs(600)
        .build();
    config.sandbox.command = Some("/bin/sh".to_string());
    config.sandbox.command_args = vec![
        "-c".to_string(),
        "echo VM_E2E_READY; sleep 300".to_string(),
    ];

    let mut app = App::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Push the panel and spawn the supervisor in background.
    add_agent_from_config(&mut app, &name, config, &tx);

    // Assert a panel was pushed.
    assert_eq!(app.panels.len(), 1, "expected exactly one panel after add_agent_from_config");

    // ── Phase 1: Wait for Terminal ──────────────────────────────────────
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                let _ = handle_event(&mut app, ev, &tx).await;
            }
            Ok(None) => break,
            Err(_) => {} // timeout tick, re-check
        }
        if app.panels.get(0).map(|p| p.mode.clone()) == Some(PanelMode::Terminal) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("panel never reached Terminal within 120s timeout");
        }
    }

    // Assert panel state after connection.
    let panel = &app.panels[0];
    assert!(
        panel.supervisor_name().is_some(),
        "expected panel to have a Supervisor backend after connection"
    );
    assert_eq!(
        panel.mode,
        PanelMode::Terminal,
        "expected panel mode to be Terminal after SSH connect"
    );

    // ── Phase 2: console output flows ───────────────────────────────────
    // AC6 requires proving console attaching yields TerminalData. The guest
    // command prints VM_E2E_READY at boot; assert it arrives as TerminalData
    // (or lands on the panel's vt100 screen).
    let data_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut saw_console_output = false;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                if matches!(&ev, AppEvent::TerminalData { panel_idx: 0, .. }) {
                    saw_console_output = true;
                }
                let _ = handle_event(&mut app, ev, &tx).await;
            }
            Ok(None) => break,
            Err(_) => {} // timeout tick
        }
        if let Some(ref term) = app.panels[0].terminal {
            if term.screen().contents().contains("VM_E2E_READY") {
                saw_console_output = true;
            }
        }
        if saw_console_output {
            break;
        }
        if tokio::time::Instant::now() > data_deadline {
            break;
        }
    }
    assert!(
        saw_console_output,
        "expected console output (TerminalData / VM_E2E_READY) within 30s"
    );

    // The console has a live write channel for forwarding input to the guest.
    let write_tx = app.panels[0]
        .terminal_handle
        .as_ref()
        .expect("panel should have a console terminal handle after attach")
        .write_tx
        .clone();
    assert!(
        write_tx.send(b"\n".to_vec()).is_ok(),
        "console write channel should accept input"
    );

    // ── Phase 3: /reconnect ─────────────────────────────────────────────
    handle_command(&mut app, Command::Reconnect, &tx).await;
    assert!(
        app.panels[0].reconnecting,
        "expected reconnecting flag after /reconnect"
    );
    assert_eq!(
        app.panels[0].mode,
        PanelMode::Loading,
        "expected Loading mode after /reconnect"
    );

    // Pump events until SshConnected arrives and panel returns to Terminal.
    let reconnect_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                let _ = handle_event(&mut app, ev, &tx).await;
            }
            Ok(None) => break,
            Err(_) => {} // timeout tick
        }
        if app.panels.get(0).map(|p| p.mode.clone()) == Some(PanelMode::Terminal) {
            break;
        }
        if tokio::time::Instant::now() > reconnect_deadline {
            panic!("panel never returned to Terminal after /reconnect (60s timeout)");
        }
    }

    // ── Phase 4: /kill ──────────────────────────────────────────────────
    handle_command(&mut app, Command::Kill { panel: None }, &tx).await;
    assert!(
        app.panels.is_empty(),
        "expected all panels to be removed after /kill"
    );

    // ── Phase 5: Verify sandbox is gone ─────────────────────────────────
    // `nanosb ps` reports a supervisor sandbox as Running while its control
    // socket exists, so `!is_running()` is the same signal: it is gone.
    let client = crate::supervisor::client::SupervisorClient::new(&name);
    let gone_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while client.is_running() {
        if tokio::time::Instant::now() > gone_deadline {
            panic!("sandbox still running 30s after /kill");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        !client.is_running(),
        "supervisor for '{}' should be stopped after /kill",
        name
    );

    // ── Cleanup (best-effort) ───────────────────────────────────────────
    let _ = client.stop(true);
    let sandbox_dir = client.sandbox_dir().to_path_buf();
    if sandbox_dir.exists() {
        let _ = std::fs::remove_dir_all(&sandbox_dir);
    }
}

/// Interactive TTY end-to-end test.
///
/// Boots a supervised sandbox with `interactive: true`. The supervisor injects
/// the guest exec agent and bridges a vsock socket; the panel attaches through
/// that PTY (`connect_exec_pty`). Runs a shell that reports whether stdin is a
/// TTY, then drives the extracted event loop and asserts:
///   - the guest sees a TTY (`TTY_CHECK:yes`), and
///   - input sent through the panel's write channel round-trips as output.
///
/// Run: `NANOSB_BINARY_PATH="$PWD/target/debug/nanosb" \
///   cargo test -p nanosb-cli tui::vm_test::test_interactive_tty_roundtrip -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn test_interactive_tty_roundtrip() {
    let name = format!("tui-interactive-test-{}", std::process::id());
    // Default to a bare image; override with a real agent image (e.g. a local
    // registry Claude image) to prove the same path works there:
    //   NANOSB_TEST_INTERACTIVE_IMAGE=localhost:5050/claude:latest
    let image = std::env::var("NANOSB_TEST_INTERACTIVE_IMAGE")
        .unwrap_or_else(|_| "alpine:latest".to_string());

    let mut config = AgentSandboxConfig::builder()
        .name(&name)
        .image(&image)
        .cpus(1)
        .memory_mb(512)
        .timeout_secs(600)
        .build();
    config.interactive = true;
    config.sandbox.command = Some("/bin/sh".to_string());
    config.sandbox.command_args = vec![
        "-c".to_string(),
        "if [ -t 0 ]; then echo TTY_CHECK:yes; else echo TTY_CHECK:no; fi; \
         while read line; do echo ECHO:$line; done"
            .to_string(),
    ];

    let mut app = App::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    add_agent_from_config(&mut app, &name, config, &tx);
    assert_eq!(app.panels.len(), 1, "expected one panel");
    assert!(app.panels[0].interactive, "panel should be interactive");

    // Pump until the panel reaches Terminal.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                let _ = handle_event(&mut app, ev, &tx).await;
            }
            Ok(None) => break,
            Err(_) => {}
        }
        if app.panels.first().map(|p| p.mode.clone()) == Some(PanelMode::Terminal) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("panel never reached Terminal");
        }
    }

    // Collect console output and assert the TTY check + echo round-trip.
    let write_tx = app.panels[0]
        .terminal_handle
        .as_ref()
        .expect("console handle")
        .write_tx
        .clone();
    let _ = write_tx.send(b"hello-tty\n".to_vec());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut saw_tty_yes = false;
    let mut saw_echo = false;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                let _ = handle_event(&mut app, ev, &tx).await;
            }
            Ok(None) => break,
            Err(_) => {}
        }
        if let Some(ref term) = app.panels.first().and_then(|p| p.terminal.as_ref()) {
            let text = term.screen().contents();
            if text.contains("TTY_CHECK:yes") {
                saw_tty_yes = true;
            }
            if text.contains("ECHO:hello-tty") {
                saw_echo = true;
            }
        }
        if saw_tty_yes && saw_echo {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            break;
        }
    }

    let client = crate::supervisor::client::SupervisorClient::new(&name);
    let _ = client.stop(true);
    let sandbox_dir = client.sandbox_dir().to_path_buf();
    if sandbox_dir.exists() {
        let _ = std::fs::remove_dir_all(&sandbox_dir);
    }

    assert!(saw_tty_yes, "guest stdin was not a TTY (expected TTY_CHECK:yes)");
    assert!(saw_echo, "input did not round-trip (expected ECHO:hello-tty)");
}
