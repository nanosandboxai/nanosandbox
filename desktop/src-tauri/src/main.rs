#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use command_core::{Command, ParseResult};
use pane_core::grid_dimensions;
use pane_core::PanelRegistry;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::prelude::*;

static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

fn desktop_logs_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nanosandbox")
        .join("desktop-logs")
}

fn desktop_env_store_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nanosandbox")
        .join("desktop")
        .join("env.json")
}

fn desktop_recent_projects_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nanosandbox")
        .join("desktop")
        .join("recent_projects.json")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedEnvStore {
    #[serde(default)]
    sandboxes: HashMap<String, HashMap<String, String>>,
}

fn load_persisted_env_store() -> PersistedEnvStore {
    let path = desktop_env_store_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(value) => value,
        Err(_) => return PersistedEnvStore::default(),
    };
    serde_json::from_str::<PersistedEnvStore>(&content).unwrap_or_default()
}

fn save_persisted_env_store(store: &PersistedEnvStore) -> Result<(), String> {
    let path = desktop_env_store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create env store directory: {}", e))?;
    }
    let body = serde_json::to_string_pretty(store)
        .map_err(|e| format!("Failed to serialize env store: {}", e))?;
    std::fs::write(&path, body).map_err(|e| format!("Failed to write env store: {}", e))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecentProjectRecord {
    path: String,
    last_opened_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RecentProjectsStore {
    #[serde(default)]
    projects: Vec<RecentProjectRecord>,
}

fn load_recent_projects_store() -> RecentProjectsStore {
    let path = desktop_recent_projects_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(value) => value,
        Err(_) => return RecentProjectsStore::default(),
    };
    serde_json::from_str::<RecentProjectsStore>(&content).unwrap_or_default()
}

fn save_recent_projects_store(store: &RecentProjectsStore) -> Result<(), String> {
    let path = desktop_recent_projects_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create recent projects directory: {}", e))?;
    }
    let body = serde_json::to_string_pretty(store)
        .map_err(|e| format!("Failed to serialize recent projects: {}", e))?;
    std::fs::write(&path, body).map_err(|e| format!("Failed to write recent projects: {}", e))
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn humanize_age(now_ms: u64, then_ms: u64) -> String {
    let delta = now_ms.saturating_sub(then_ms) / 1000;
    if delta < 60 {
        format!("{}s ago", delta)
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3_600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}

fn project_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("project")
        .to_string()
}

fn desktop_log_file_hint() -> String {
    let dir = LOG_DIR.get().cloned().unwrap_or_else(desktop_logs_dir);
    format!("{}/desktop.log.YYYY-MM-DD", dir.display())
}

fn init_logging() {
    let log_dir = desktop_logs_dir();
    let mut file_layer_registered = false;

    if std::fs::create_dir_all(&log_dir).is_ok() {
        let appender = tracing_appender::rolling::daily(&log_dir, "desktop.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        let _ = LOG_GUARD.set(guard);
        let _ = LOG_DIR.set(log_dir.clone());

        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "info,nanosb_desktop=debug,sandbox=info,terminal_core=info,command_core=info",
            )
        });

        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_target(true)
            .with_thread_names(true);
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_target(true);

        if tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(stderr_layer)
            .try_init()
            .is_ok()
        {
            file_layer_registered = true;
        }
    }

    if !file_layer_registered {
        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "info,nanosb_desktop=debug,sandbox=info,terminal_core=info,command_core=info",
            )
        });
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_writer(io::stderr))
            .try_init();
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        eprintln!("[panic] {}", panic_info);
        tracing::error!("backend panic: {}", panic_info);
        if let Some(app) = APP_HANDLE.get() {
            enqueue_popup(
                app,
                "error",
                "Backend panic",
                &format!("See {}", desktop_log_file_hint()),
            );
        }
        previous(panic_info);
    }));
}

fn spawn_logged<F>(name: &'static str, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        tracing::debug!(task = name, "task start");
        future.await;
        tracing::debug!(task = name, "task finish");
    });
}

#[derive(Clone, Serialize)]
struct ApiError {
    code: String,
    message: String,
    detail: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ApiResult<T>
where
    T: Serialize,
{
    Ok { ok: bool, data: T },
    Err { ok: bool, error: ApiError },
}

fn ok<T>(data: T) -> ApiResult<T>
where
    T: Serialize,
{
    ApiResult::Ok { ok: true, data }
}

fn err<T>(code: &str, message: &str, detail: Option<String>) -> ApiResult<T>
where
    T: Serialize,
{
    ApiResult::Err {
        ok: false,
        error: ApiError {
            code: code.to_string(),
            message: message.to_string(),
            detail,
        },
    }
}

#[derive(Clone, Serialize)]
struct ThemeSnapshot {
    name: String,
    mode: String,
    tokens: std::collections::HashMap<String, String>,
    xterm: std::collections::HashMap<String, String>,
}

#[derive(Clone, Serialize)]
struct PaneSummary {
    pane_id: usize,
    agent_name: String,
    display_name: Option<String>,
    sandbox_id_short: String,
    mode: String,
    status: String,
    visible: bool,
    focused: bool,
    reconnecting: bool,
    loading_message: Option<String>,
    loading_error: Option<String>,
    elapsed_ms: Option<u64>,
}

#[derive(Clone, Serialize)]
struct LayoutSnapshot {
    focused_pane: usize,
    hidden_panels: Vec<usize>,
    zoomed_pane: Option<usize>,
    rows: usize,
    cols: usize,
}

#[derive(Clone, Serialize)]
struct AppBootstrap {
    version: String,
    theme: ThemeSnapshot,
    layout: LayoutSnapshot,
    panes: Vec<PaneSummary>,
    command_history_size: usize,
    workspace: WorkspaceContext,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct WorkspaceContext {
    project_path: Option<String>,
    session_id: Option<String>,
}

#[derive(Clone, Serialize)]
struct ProjectEntry {
    id: String,
    name: String,
    path: String,
    last_opened: String,
}

#[derive(Clone, Serialize)]
struct SessionEntry {
    id: String,
    updated: String,
    panels: usize,
    summary: String,
}

#[derive(Serialize)]
struct InputSubmitResult {
    kind: String,
    command_name: Option<String>,
    message: Option<String>,
}

#[derive(Clone, Serialize)]
struct PopupAction {
    id: String,
    label: String,
    primary: bool,
}

#[derive(Deserialize)]
struct TerminalWriteFrame {
    pane_id: usize,
    seq: u64,
    encoding: String,
    data: String,
}

#[derive(Serialize)]
struct TerminalWriteAck {
    accepted: bool,
    seq: u64,
}

#[derive(Serialize)]
struct TerminalResizeAck {
    pane_id: usize,
    cols: u16,
    rows: u16,
}

#[derive(Serialize)]
struct BasicAck {
    handled: bool,
}

#[derive(Serialize)]
struct LogsPathAck {
    path: String,
}

#[derive(Serialize)]
struct UploadStartAck {
    upload_id: String,
}

#[derive(Clone, Serialize)]
struct EditorEntry {
    name: String,
    binary: String,
    is_tui: bool,
    available: bool,
}

#[derive(Clone, Serialize)]
struct EventEnvelope<T>
where
    T: Serialize + Clone,
{
    version: u8,
    ts_ms: u64,
    event_id: String,
    payload: T,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type")]
enum PaneEvent {
    #[serde(rename = "pane_snapshot")]
    PaneSnapshot {
        panes: Vec<PaneSummary>,
        layout: LayoutSnapshot,
    },
    #[serde(rename = "focus_changed")]
    FocusChanged { pane_id: usize },
    #[serde(rename = "zoom_changed")]
    ZoomChanged { zoomed_pane: Option<usize> },
}

#[derive(Clone, Serialize)]
#[serde(tag = "type")]
enum TerminalEvent {
    #[serde(rename = "terminal_data")]
    TerminalData {
        pane_id: usize,
        seq: u64,
        encoding: String,
        data: String,
    },
    #[serde(rename = "terminal_connected")]
    TerminalConnected { pane_id: usize, cols: u16, rows: u16 },
    #[serde(rename = "terminal_disconnected")]
    TerminalDisconnected { pane_id: usize, error: Option<String> },
}

#[derive(Clone, Serialize)]
#[serde(tag = "type")]
enum StatusEvent {
    #[serde(rename = "status_update")]
    StatusUpdate {
        running_count: usize,
        loading_count: usize,
        disconnected_count: usize,
        focused_label: String,
    },
}

#[derive(Clone, Serialize)]
#[serde(tag = "type")]
enum UploadEvent {
    #[serde(rename = "upload_started")]
    UploadStarted {
        upload_id: String,
        pane_id: usize,
        filename: String,
    },
    #[serde(rename = "upload_complete")]
    UploadComplete {
        upload_id: String,
        pane_id: usize,
        filename: String,
        remote_path: String,
        size: u64,
    },
    #[serde(rename = "upload_failed")]
    UploadFailed {
        upload_id: String,
        pane_id: usize,
        error: String,
    },
}

#[derive(Clone, Serialize)]
#[serde(tag = "type")]
enum UiEvent {
    #[serde(rename = "panel_toggle")]
    PanelToggle { target: String, open: bool },
    #[serde(rename = "clear_history")]
    ClearHistory,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type")]
enum PopupEvent {
    #[serde(rename = "popup_enqueue")]
    PopupEnqueue {
        popup_id: String,
        kind: String,
        title: String,
        body: String,
        actions: Vec<PopupAction>,
        ttl_ms: Option<u64>,
    },
}

struct DesktopState {
    panes: Mutex<Vec<PaneSummary>>,
    panel_registry: Mutex<PanelRegistry>,
    theme_name: Mutex<String>,
    terminal_seq: Mutex<HashMap<usize, u64>>,
    terminal_handles: Mutex<HashMap<usize, terminal_core::SshTerminalHandle>>,
    sandbox_handles: Mutex<HashMap<usize, Arc<TokioMutex<sandbox::Sandbox>>>>,
    ssh_connection_info: Mutex<HashMap<usize, SshConnectionInfo>>,
    pane_workdirs: Mutex<HashMap<usize, PathBuf>>,
    pane_sandbox_identity: Mutex<HashMap<usize, String>>,
    pane_gitsync_override: Mutex<HashMap<usize, bool>>,
    registry: Mutex<sandbox::AgentsRegistryClient>,
    persisted_env: Mutex<PersistedEnvStore>,
    recent_projects: Mutex<RecentProjectsStore>,
    workspace_context: Mutex<WorkspaceContext>,
    url_buffers: Mutex<HashMap<usize, Vec<u8>>>,
    opened_auth_keys: Mutex<HashSet<String>>,
    zoomed_pane: Mutex<Option<usize>>,
}

#[derive(Clone)]
struct SshConnectionInfo {
    host: String,
    port: u16,
    key_path: PathBuf,
    agent_name: String,
    env: HashMap<String, String>,
    permissions: sandbox::Permissions,
    auto_mode: bool,
    prompt: Option<String>,
    model: Option<String>,
}

fn build_layout(panes: &[PaneSummary], registry: &PanelRegistry, zoomed_pane: Option<usize>) -> LayoutSnapshot {
    if zoomed_pane.is_some() {
        return LayoutSnapshot {
            focused_pane: registry.focused_panel,
            hidden_panels: registry.hidden_panels.iter().copied().collect(),
            zoomed_pane,
            rows: 1,
            cols: 1,
        };
    }

    let visible_count = panes
        .iter()
        .filter(|pane| !registry.hidden_panels.contains(&pane.pane_id))
        .count();
    let (rows, cols) = grid_dimensions(visible_count);

    LayoutSnapshot {
        focused_pane: registry.focused_panel,
        hidden_panels: registry.hidden_panels.iter().copied().collect(),
        zoomed_pane,
        rows,
        cols,
    }
}

fn apply_registry_to_panes(panes: &mut [PaneSummary], registry: &PanelRegistry) {
    for pane in panes.iter_mut() {
        pane.focused = pane.pane_id == registry.focused_panel;
        pane.visible = !registry.hidden_panels.contains(&pane.pane_id);
    }
}

fn emit_pane_snapshot(app: &tauri::AppHandle, state: &State<'_, DesktopState>) -> Result<(), ApiError> {
    let mut panes = state.panes.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire pane state".to_string(),
        detail: None,
    })?;

    let registry = state.panel_registry.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire panel registry".to_string(),
        detail: None,
    })?;

    let zoomed = *state.zoomed_pane.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire zoom state".to_string(),
        detail: None,
    })?;

    apply_registry_to_panes(&mut panes, &registry);
    let layout = build_layout(&panes, &registry, zoomed);

    emit_event(
        app,
        "app://pane",
        PaneEvent::PaneSnapshot {
            panes: panes.clone(),
            layout,
        },
    );
    emit_status_update(app, &panes, registry.focused_panel);
    Ok(())
}

fn spawn_terminal_event_bridge(app: tauri::AppHandle, pane_id: usize) -> tokio::sync::mpsc::UnboundedSender<terminal_core::TerminalEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<terminal_core::TerminalEvent>();
    spawn_logged("terminal_event_bridge", async move {
        while let Some(event) = rx.recv().await {
            let state: State<'_, DesktopState> = app.state();
            match event {
                terminal_core::TerminalEvent::TerminalData { data } => {
                    let seq = match next_terminal_seq(&state, pane_id) {
                        Ok(value) => value,
                        Err(_) => continue,
                    };
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                    emit_event(
                        &app,
                        "app://terminal",
                        TerminalEvent::TerminalData {
                            pane_id,
                            seq,
                            encoding: "base64".to_string(),
                            data: encoded,
                        },
                    );
                    let _ = process_auth_urls_from_output(&app, &state, pane_id, &data);
                }
                terminal_core::TerminalEvent::SshDisconnected { error } => {
                    emit_event(
                        &app,
                        "app://terminal",
                        TerminalEvent::TerminalDisconnected { pane_id, error },
                    );
                    if let Ok(mut handles) = state.terminal_handles.lock() {
                        handles.remove(&pane_id);
                    }
                    if let Ok(mut panes) = state.panes.lock() {
                        if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                            pane.status = "disconnected".to_string();
                        }
                    }
                    let _ = emit_pane_snapshot(&app, &state);
                }
            }
        }
    });
    tx
}

async fn connect_pane_ssh(app: tauri::AppHandle, pane_id: usize, mut info: SshConnectionInfo) -> Result<(), String> {
    let mode = if info.auto_mode {
        "headless".to_string()
    } else {
        "terminal".to_string()
    };
    let state: State<'_, DesktopState> = app.state();
    let sandbox_identity = state
        .pane_sandbox_identity
        .lock()
        .ok()
        .and_then(|map| map.get(&pane_id).cloned());
    if let Some(identity) = sandbox_identity {
        if let Ok(store) = state.persisted_env.lock() {
            if let Some(saved) = store.sandboxes.get(&identity) {
                for (key, value) in saved {
                    info.env.insert(key.clone(), value.clone());
                }
            }
        }
    }

    let tx = spawn_terminal_event_bridge(app.clone(), pane_id);
    let handle = terminal_core::connect_ssh(
        info.host,
        info.port,
        info.key_path,
        120,
        32,
        &info.agent_name,
        &info.env,
        Some("/workspace"),
        info.permissions,
        info.auto_mode,
        info.prompt.as_deref(),
        false,
        false,
        None,
        info.model.as_deref(),
        pane_id,
        tx,
    )
    .await
    .map_err(|e| e.to_string())?;

    let state: State<'_, DesktopState> = app.state();
    {
        let mut handles = state.terminal_handles.lock().map_err(|_| "Failed to acquire terminal handles".to_string())?;
        handles.insert(pane_id, handle);
    }
    {
        let mut panes = state.panes.lock().map_err(|_| "Failed to acquire pane state".to_string())?;
        if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
            pane.status = "connected".to_string();
            pane.mode = mode;
            pane.loading_message = None;
            pane.loading_error = None;
        }
    }
    let _ = emit_pane_snapshot(&app, &state);
    Ok(())
}

fn spawn_add_agent_runtime(
    app: tauri::AppHandle,
    pane_id: usize,
    agent: String,
    image: Option<String>,
    tag: Option<String>,
    name: Option<String>,
    project: Option<String>,
    branch: Option<String>,
    run_as_root: bool,
    auto_mode: bool,
    prompt: Option<String>,
    env: HashMap<String, String>,
    model: Option<String>,
) {
    spawn_logged("add_agent_runtime", async move {
        let image_ref = match image {
            Some(img) => img,
            None => match tag {
                Some(t) => format!("{}:{}", agent, t),
                None => agent.clone(),
            },
        };

        let image_name = sandbox::normalize_image(&image_ref);
        let compute = agent
            .parse::<sandbox::AgentType>()
            .ok()
            .map(sandbox::agent_compute_for)
            .unwrap_or(sandbox::AgentComputeDefaults { cpus: 2, memory_mb: 2048 });
        let mut builder = sandbox::SandboxConfig::builder()
            .image(&image_name)
            .cpus(compute.cpus)
            .memory_mb(compute.memory_mb)
            .run_as_root(run_as_root);

        if let Some(n) = name.as_deref() {
            builder = builder.name(n);
        }
        if let Some(path) = project.as_deref() {
            builder = builder.project(PathBuf::from(path), branch.as_deref());
        }

        for (key, value) in &env {
            builder = builder.env(key.clone(), value.clone());
        }

        if let Ok(agent_type) = agent.parse::<sandbox::AgentType>() {
            builder = builder.agent_type(agent_type);
        }

        if let Some(m) = model.as_deref() {
            builder = builder.model(m);
        }

        let config = builder.build();
        let create_result = sandbox::Sandbox::create(config).await;

        let state: State<'_, DesktopState> = app.state();
        match create_result {
            Ok(mut sandbox) => {
                if let Ok(mut panes) = state.panes.lock() {
                    if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                        pane.loading_message = Some("Booting microVM...".to_string());
                    }
                }
                let _ = emit_pane_snapshot(&app, &state);

                if let Err(e) = sandbox.start().await {
                    if let Ok(mut panes) = state.panes.lock() {
                        if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                            pane.status = "error".to_string();
                            pane.loading_error = Some(format!("Failed to start sandbox: {}", e));
                        }
                    }
                    enqueue_popup(&app, "error", "Sandbox start failed", &e.to_string());
                    let _ = emit_pane_snapshot(&app, &state);
                    return;
                }

                let short_id = sandbox.id()[..8.min(sandbox.id().len())].to_string();
                let full_id = sandbox.id().to_string();
                let clone_workdir = sandbox
                    .project_mount()
                    .and_then(|mount| mount.worktree_base.clone());
                let ssh_info = sandbox
                    .ssh_port()
                    .zip(sandbox.ssh_key_path())
                    .map(|(port, key_path)| SshConnectionInfo {
                        host: sandbox.ssh_host().unwrap_or_else(|| "127.0.0.1".to_string()),
                        port,
                        key_path,
                        agent_name: agent.clone(),
                        env: env.clone(),
                        permissions: sandbox::Permissions::Default,
                        auto_mode,
                        prompt: prompt.clone(),
                        model: model.clone(),
                    });

                let sandbox_handle = Arc::new(TokioMutex::new(sandbox));
                if let Ok(mut handles) = state.sandbox_handles.lock() {
                    handles.insert(pane_id, sandbox_handle);
                }
                if let Ok(mut workdirs) = state.pane_workdirs.lock() {
                    if let Some(path) = clone_workdir {
                        workdirs.insert(pane_id, path);
                    } else {
                        workdirs.remove(&pane_id);
                    }
                }
                if let Ok(mut identities) = state.pane_sandbox_identity.lock() {
                    identities.insert(pane_id, full_id);
                }

                if let Ok(mut panes) = state.panes.lock() {
                    if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                        pane.sandbox_id_short = short_id;
                        pane.status = "loading".to_string();
                        pane.loading_message = Some("Connecting SSH...".to_string());
                        pane.mode = "loading".to_string();
                    }
                }
                let _ = emit_pane_snapshot(&app, &state);

                let Some(info) = ssh_info else {
                    if let Ok(mut panes) = state.panes.lock() {
                        if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                            pane.status = "error".to_string();
                            pane.loading_error = Some("Sandbox has no SSH info".to_string());
                        }
                    }
                    let _ = emit_pane_snapshot(&app, &state);
                    return;
                };

                if let Ok(mut map) = state.ssh_connection_info.lock() {
                    map.insert(pane_id, info.clone());
                }

                if let Err(e) = connect_pane_ssh(app.clone(), pane_id, info).await {
                    if let Ok(mut panes) = state.panes.lock() {
                        if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                            pane.status = "error".to_string();
                            pane.loading_error = Some(format!("SSH connect failed: {}", e));
                        }
                    }
                    enqueue_popup(&app, "error", "SSH connect failed", &e);
                    let _ = emit_pane_snapshot(&app, &state);
                }
            }
            Err(e) => {
                if let Ok(mut panes) = state.panes.lock() {
                    if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                        pane.status = "error".to_string();
                        pane.loading_error = Some(format!("Failed to create sandbox: {}", e));
                    }
                }
                enqueue_popup(&app, "error", "Sandbox create failed", &e.to_string());
                let _ = emit_pane_snapshot(&app, &state);
            }
        }
    });
}

fn theme_snapshot(theme_name: &str) -> ThemeSnapshot {
    let mode = if theme_name.contains("light") {
        "light"
    } else {
        "dark"
    };

    ThemeSnapshot {
        name: theme_name.to_string(),
        mode: mode.to_string(),
        tokens: std::collections::HashMap::new(),
        xterm: std::collections::HashMap::new(),
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn emit_event<T>(app: &tauri::AppHandle, channel: &str, payload: T)
where
    T: Serialize + Clone,
{
    let envelope = EventEnvelope {
        version: 1,
        ts_ms: unix_now_ms(),
        event_id: uuid::Uuid::new_v4().to_string(),
        payload,
    };
    let _ = app.emit(channel, envelope);
}

fn emit_status_update(app: &tauri::AppHandle, panes: &[PaneSummary], focused_pane: usize) {
    let running_count = panes.iter().filter(|pane| pane.status == "connected").count();
    let loading_count = panes.iter().filter(|pane| pane.status == "loading").count();
    let disconnected_count = panes
        .iter()
        .filter(|pane| pane.status == "disconnected" || pane.status == "error")
        .count();

    let focused_label = panes
        .iter()
        .find(|pane| pane.pane_id == focused_pane)
        .map(|pane| format!("{} · {}", pane.agent_name, pane.sandbox_id_short))
        .unwrap_or_else(|| "none".to_string());

    emit_event(
        app,
        "app://status",
        StatusEvent::StatusUpdate {
            running_count,
            loading_count,
            disconnected_count,
            focused_label,
        },
    );
}

fn next_terminal_seq(state: &State<'_, DesktopState>, pane_id: usize) -> Result<u64, ApiError> {
    let mut seq = match state.terminal_seq.lock() {
        Ok(guard) => guard,
        Err(_) => {
            return Err(ApiError {
                code: "INTERNAL".to_string(),
                message: "Failed to acquire terminal sequence state".to_string(),
                detail: None,
            })
        }
    };
    let entry = seq.entry(pane_id).or_insert(0);
    *entry += 1;
    Ok(*entry)
}

fn parse_filename_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "upload.bin".to_string())
}

fn required_api_keys(agent: &str) -> Vec<(&'static str, bool)> {
    match agent {
        "claude" => vec![("ANTHROPIC_API_KEY", true)],
        "codex" => vec![("OPENAI_API_KEY", true)],
        "goose" => vec![
            ("OPENAI_API_KEY", false),
            ("ANTHROPIC_API_KEY", false),
        ],
        _ => vec![],
    }
}

fn parse_runtime_env_file(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read env file '{}': {}", path, e))?;

    let mut vars = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            vars.push((key.trim().to_string(), value.trim().to_string()));
        }
    }

    Ok(vars)
}

fn build_add_agent_env(
    agent: &str,
    model: Option<&str>,
    prompt: Option<&str>,
    auto_mode: bool,
    permissions: sandbox::Permissions,
    use_env_keys: &[String],
    env_file: Option<&str>,
) -> Result<HashMap<String, String>, String> {
    let mut env = HashMap::new();

    if let Some(path) = env_file {
        let vars = parse_runtime_env_file(path)?;
        for (key, value) in vars {
            env.insert(key, value);
        }
    }

    for (key, _required) in required_api_keys(agent) {
        if let Ok(value) = std::env::var(key) {
            env.entry(key.to_string()).or_insert(value);
        }
    }

    if agent == "goose" && !env.contains_key("GOOSE_PROVIDER") {
        if env.contains_key("ANTHROPIC_API_KEY") {
            env.insert("GOOSE_PROVIDER".to_string(), "anthropic".to_string());
        } else if env.contains_key("OPENAI_API_KEY") {
            env.insert("GOOSE_PROVIDER".to_string(), "openai".to_string());
        }
    }

    if !use_env_keys.is_empty() {
        let mut missing = Vec::new();
        for key in use_env_keys {
            match std::env::var(key) {
                Ok(value) => {
                    env.insert(key.clone(), value);
                }
                Err(_) => missing.push(key.clone()),
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "runtime env key(s) not found: {}",
                missing.join(", ")
            ));
        }
    }

    let agent_vars = terminal_core::agent_env_vars(agent, permissions, auto_mode, model, prompt);
    for (key, value) in agent_vars {
        env.insert(key, value);
    }

    Ok(env)
}

fn validate_upload_pane(state: &State<'_, DesktopState>, pane_id: usize) -> Result<(), ApiError> {
    let panes = state.panes.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire pane state".to_string(),
        detail: None,
    })?;
    if panes.iter().all(|pane| pane.pane_id != pane_id) {
        return Err(ApiError {
            code: "NOT_FOUND".to_string(),
            message: "Pane not found".to_string(),
            detail: None,
        });
    }
    Ok(())
}

fn ssh_info_for_pane(state: &State<'_, DesktopState>, pane_id: usize) -> Result<SshConnectionInfo, ApiError> {
    let map = state.ssh_connection_info.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire SSH state".to_string(),
        detail: None,
    })?;

    map.get(&pane_id).cloned().ok_or(ApiError {
        code: "SSH_DISCONNECTED".to_string(),
        message: "No active SSH connection for pane".to_string(),
        detail: None,
    })
}

fn clone_workdir_for_pane(state: &State<'_, DesktopState>, pane_id: usize) -> Result<PathBuf, ApiError> {
    let map = state.pane_workdirs.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire project workspace state".to_string(),
        detail: None,
    })?;
    map.get(&pane_id).cloned().ok_or(ApiError {
        code: "NOT_FOUND".to_string(),
        message: "Focused pane has no project clone directory".to_string(),
        detail: None,
    })
}

fn upsert_recent_project(state: &State<'_, DesktopState>, project_path: &Path) -> Result<(), String> {
    let normalized = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.to_path_buf());
    let path_value = normalized.display().to_string();
    let mut store = state
        .recent_projects
        .lock()
        .map_err(|_| "Failed to acquire recent projects store".to_string())?;
    let ts = current_epoch_ms();
    if let Some(existing) = store.projects.iter_mut().find(|entry| entry.path == path_value) {
        existing.last_opened_ms = ts;
    } else {
        store.projects.push(RecentProjectRecord {
            path: path_value,
            last_opened_ms: ts,
        });
    }
    store.projects.sort_by(|a, b| b.last_opened_ms.cmp(&a.last_opened_ms));
    if store.projects.len() > 50 {
        store.projects.truncate(50);
    }
    save_recent_projects_store(&store)
}

fn build_project_entries(store: &RecentProjectsStore) -> Vec<ProjectEntry> {
    let now = current_epoch_ms();
    store
        .projects
        .iter()
        .map(|entry| {
            let project_path = PathBuf::from(&entry.path);
            ProjectEntry {
                id: entry.path.clone(),
                name: project_label(&project_path),
                path: entry.path.clone(),
                last_opened: humanize_age(now, entry.last_opened_ms),
            }
        })
        .collect()
}

fn emit_upload_failed(app: &tauri::AppHandle, upload_id: &str, pane_id: usize, error: &str) {
    emit_event(
        app,
        "app://upload",
        UploadEvent::UploadFailed {
            upload_id: upload_id.to_string(),
            pane_id,
            error: error.to_string(),
        },
    );
}

fn remote_upload_path(filename: &str) -> String {
    format!("{}/{}", upload_core::UPLOAD_DIR, filename)
}

fn spawn_upload_transfer(
    app: tauri::AppHandle,
    pane_id: usize,
    upload_id: String,
    filename: String,
    bytes: Vec<u8>,
    ssh: SshConnectionInfo,
) {
    let remote_path = remote_upload_path(&filename);
    let local_size = bytes.len() as u64;

    emit_event(
        &app,
        "app://upload",
        UploadEvent::UploadStarted {
            upload_id: upload_id.clone(),
            pane_id,
            filename: filename.clone(),
        },
    );

    tauri::async_runtime::spawn(async move {
        let result = upload_core::ssh_upload(&ssh.host, ssh.port, &ssh.key_path, &bytes, &remote_path).await;
        match result {
            Ok(size) => {
                emit_event(
                    &app,
                    "app://upload",
                    UploadEvent::UploadComplete {
                        upload_id,
                        pane_id,
                        filename: filename.clone(),
                        remote_path,
                        size,
                    },
                );
                enqueue_popup(
                    &app,
                    "success",
                    "Upload complete",
                    &format!("Uploaded {} ({})", filename, upload_core::format_size(local_size)),
                );
            }
            Err(e) => {
                emit_upload_failed(&app, &upload_id, pane_id, &e);
                enqueue_popup(&app, "error", "Upload failed", &e);
            }
        }
    });
}

fn start_file_upload(
    app: &tauri::AppHandle,
    state: &State<'_, DesktopState>,
    pane_id: usize,
    path: &str,
) -> Result<String, ApiError> {
    validate_upload_pane(state, pane_id)?;
    let ssh = ssh_info_for_pane(state, pane_id)?;

    let metadata = std::fs::metadata(path).map_err(|e| ApiError {
        code: "BAD_REQUEST".to_string(),
        message: "Upload file not found".to_string(),
        detail: Some(e.to_string()),
    })?;

    if metadata.len() > upload_core::MAX_UPLOAD_SIZE {
        return Err(ApiError {
            code: "UPLOAD_TOO_LARGE".to_string(),
            message: "Upload exceeds max file size".to_string(),
            detail: Some(format!(
                "Max {} MB",
                upload_core::MAX_UPLOAD_SIZE / (1024 * 1024)
            )),
        });
    }

    let bytes = std::fs::read(path).map_err(|e| ApiError {
        code: "BAD_REQUEST".to_string(),
        message: "Failed to read upload file".to_string(),
        detail: Some(e.to_string()),
    })?;

    let upload_id = uuid::Uuid::new_v4().to_string();
    let filename = parse_filename_from_path(path);
    spawn_upload_transfer(
        app.clone(),
        pane_id,
        upload_id.clone(),
        filename,
        bytes,
        ssh,
    );
    Ok(upload_id)
}

fn start_clipboard_image_upload(
    app: &tauri::AppHandle,
    state: &State<'_, DesktopState>,
    pane_id: usize,
) -> Result<String, ApiError> {
    validate_upload_pane(state, pane_id)?;
    let ssh = ssh_info_for_pane(state, pane_id)?;

    let mut clipboard = arboard::Clipboard::new().map_err(|e| ApiError {
        code: "CLIPBOARD_UNAVAILABLE".to_string(),
        message: "Failed to access clipboard".to_string(),
        detail: Some(e.to_string()),
    })?;
    let image = clipboard.get_image().map_err(|e| ApiError {
        code: "BAD_REQUEST".to_string(),
        message: "Clipboard does not contain an image".to_string(),
        detail: Some(e.to_string()),
    })?;

    let width = u32::try_from(image.width).map_err(|_| ApiError {
        code: "BAD_REQUEST".to_string(),
        message: "Clipboard image width is too large".to_string(),
        detail: None,
    })?;
    let height = u32::try_from(image.height).map_err(|_| ApiError {
        code: "BAD_REQUEST".to_string(),
        message: "Clipboard image height is too large".to_string(),
        detail: None,
    })?;

    let png_bytes = upload_core::encode_rgba_to_png(width, height, image.bytes.as_ref()).map_err(|e| {
        ApiError {
            code: "BAD_REQUEST".to_string(),
            message: "Failed to encode clipboard image".to_string(),
            detail: Some(e),
        }
    })?;

    if png_bytes.len() as u64 > upload_core::MAX_UPLOAD_SIZE {
        return Err(ApiError {
            code: "UPLOAD_TOO_LARGE".to_string(),
            message: "Clipboard image exceeds max upload size".to_string(),
            detail: Some(format!(
                "Image size {}",
                upload_core::format_size(png_bytes.len() as u64)
            )),
        });
    }

    let upload_id = uuid::Uuid::new_v4().to_string();
    let filename = format!("clipboard-{}.png", &upload_id[..8]);
    spawn_upload_transfer(
        app.clone(),
        pane_id,
        upload_id.clone(),
        filename,
        png_bytes,
        ssh,
    );
    Ok(upload_id)
}

fn resolve_pane_target(panes: &[PaneSummary], target: Option<&str>, fallback: usize) -> Option<usize> {
    match target {
        Some(raw) => {
            let t = raw.trim();
            if t.is_empty() {
                return Some(fallback);
            }
            if let Ok(idx) = t.parse::<usize>() {
                if panes.iter().any(|pane| pane.pane_id == idx) {
                    return Some(idx);
                }
            }
            panes
                .iter()
                .find(|pane| {
                    pane.agent_name.eq_ignore_ascii_case(t)
                        || pane
                            .display_name
                            .as_deref()
                            .map(|name| name.eq_ignore_ascii_case(t))
                            .unwrap_or(false)
                })
                .map(|pane| pane.pane_id)
        }
        None => Some(fallback),
    }
}

fn first_visible_panel(panes: &[PaneSummary], registry: &PanelRegistry) -> Option<usize> {
    panes
        .iter()
        .find(|pane| !registry.hidden_panels.contains(&pane.pane_id))
        .map(|pane| pane.pane_id)
}

fn ensure_valid_focus(registry: &mut PanelRegistry, panes: &[PaneSummary]) {
    let current_exists_and_visible = panes
        .iter()
        .any(|pane| pane.pane_id == registry.focused_panel && !registry.hidden_panels.contains(&pane.pane_id));
    if current_exists_and_visible {
        return;
    }
    if let Some(next) = first_visible_panel(panes, registry) {
        registry.focused_panel = next;
        return;
    }
    if let Some(first) = panes.first() {
        registry.focused_panel = first.pane_id;
    }
}

fn pane_close_by_id(
    app: &tauri::AppHandle,
    state: &State<'_, DesktopState>,
    pane_id: usize,
) -> Result<(), ApiError> {
    let mut panes = state.panes.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire pane state".to_string(),
        detail: None,
    })?;
    let mut registry = state.panel_registry.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire panel registry".to_string(),
        detail: None,
    })?;

    if panes.iter().all(|pane| pane.pane_id != pane_id) {
        return Err(ApiError {
            code: "NOT_FOUND".to_string(),
            message: "Pane not found".to_string(),
            detail: None,
        });
    }

    let previous_focused = registry.focused_panel;
    registry.hidden_panels.insert(pane_id);
    ensure_valid_focus(&mut registry, &panes);

    let (zoomed, zoom_changed) = {
        let mut zoom = state.zoomed_pane.lock().map_err(|_| ApiError {
            code: "INTERNAL".to_string(),
            message: "Failed to acquire zoom state".to_string(),
            detail: None,
        })?;
        let changed = *zoom == Some(pane_id);
        if changed {
            *zoom = None;
        }
        (*zoom, changed)
    };

    apply_registry_to_panes(&mut panes, &registry);
    let layout = build_layout(&panes, &registry, zoomed);
    emit_event(
        app,
        "app://pane",
        PaneEvent::PaneSnapshot {
            panes: panes.clone(),
            layout,
        },
    );
    if previous_focused != registry.focused_panel {
        emit_event(
            app,
            "app://pane",
            PaneEvent::FocusChanged {
                pane_id: registry.focused_panel,
            },
        );
    }
    if zoom_changed {
        emit_event(
            app,
            "app://pane",
            PaneEvent::ZoomChanged { zoomed_pane: None },
        );
    }
    emit_status_update(app, &panes, registry.focused_panel);
    Ok(())
}

fn pane_open_by_id(
    app: &tauri::AppHandle,
    state: &State<'_, DesktopState>,
    pane_id: usize,
) -> Result<(), ApiError> {
    let mut panes = state.panes.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire pane state".to_string(),
        detail: None,
    })?;
    let mut registry = state.panel_registry.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire panel registry".to_string(),
        detail: None,
    })?;

    if panes.iter().all(|pane| pane.pane_id != pane_id) {
        return Err(ApiError {
            code: "NOT_FOUND".to_string(),
            message: "Pane not found".to_string(),
            detail: None,
        });
    }

    let previous_focused = registry.focused_panel;
    registry.hidden_panels.remove(&pane_id);
    registry.focused_panel = pane_id;

    let zoomed = *state.zoomed_pane.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire zoom state".to_string(),
        detail: None,
    })?;

    apply_registry_to_panes(&mut panes, &registry);
    let layout = build_layout(&panes, &registry, zoomed);
    emit_event(
        app,
        "app://pane",
        PaneEvent::PaneSnapshot {
            panes: panes.clone(),
            layout,
        },
    );
    if previous_focused != pane_id {
        emit_event(app, "app://pane", PaneEvent::FocusChanged { pane_id });
    }
    emit_status_update(app, &panes, pane_id);
    Ok(())
}

fn pane_kill_by_id(
    app: &tauri::AppHandle,
    state: &State<'_, DesktopState>,
    pane_id: usize,
) -> Result<(), ApiError> {
    let mut panes = state.panes.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire pane state".to_string(),
        detail: None,
    })?;
    let mut registry = state.panel_registry.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire panel registry".to_string(),
        detail: None,
    })?;

    if panes.iter().all(|pane| pane.pane_id != pane_id) {
        return Err(ApiError {
            code: "NOT_FOUND".to_string(),
            message: "Pane not found".to_string(),
            detail: None,
        });
    }

    let previous_focused = registry.focused_panel;
    panes.retain(|pane| pane.pane_id != pane_id);
    registry.hidden_panels.remove(&pane_id);
    ensure_valid_focus(&mut registry, &panes);

    let (zoomed, zoom_changed) = {
        let mut zoom = state.zoomed_pane.lock().map_err(|_| ApiError {
            code: "INTERNAL".to_string(),
            message: "Failed to acquire zoom state".to_string(),
            detail: None,
        })?;
        let changed = *zoom == Some(pane_id);
        if changed {
            *zoom = None;
        }
        (*zoom, changed)
    };

    if let Ok(mut handles) = state.terminal_handles.lock() {
        handles.remove(&pane_id);
    }
    if let Ok(mut info) = state.ssh_connection_info.lock() {
        info.remove(&pane_id);
    }
    if let Ok(mut seq) = state.terminal_seq.lock() {
        seq.remove(&pane_id);
    }
    if let Ok(mut buffers) = state.url_buffers.lock() {
        buffers.remove(&pane_id);
    }
    if let Ok(mut workdirs) = state.pane_workdirs.lock() {
        workdirs.remove(&pane_id);
    }
    if let Ok(mut identities) = state.pane_sandbox_identity.lock() {
        identities.remove(&pane_id);
    }
    if let Ok(mut sync) = state.pane_gitsync_override.lock() {
        sync.remove(&pane_id);
    }

    let sandbox = {
        let mut sandboxes = state.sandbox_handles.lock().map_err(|_| ApiError {
            code: "INTERNAL".to_string(),
            message: "Failed to acquire sandbox state".to_string(),
            detail: None,
        })?;
        sandboxes.remove(&pane_id)
    };

    apply_registry_to_panes(&mut panes, &registry);
    let layout = build_layout(&panes, &registry, zoomed);
    emit_event(
        app,
        "app://pane",
        PaneEvent::PaneSnapshot {
            panes: panes.clone(),
            layout,
        },
    );
    if zoom_changed {
        emit_event(
            app,
            "app://pane",
            PaneEvent::ZoomChanged { zoomed_pane: None },
        );
    }

    let has_focus = panes
        .iter()
        .any(|pane| pane.pane_id == registry.focused_panel);
    if previous_focused != registry.focused_panel && has_focus {
        emit_event(
            app,
            "app://pane",
            PaneEvent::FocusChanged {
                pane_id: registry.focused_panel,
            },
        );
    }
    emit_status_update(app, &panes, registry.focused_panel);

    drop(registry);
    drop(panes);

    if let Some(sandbox) = sandbox {
        spawn_logged("pane_kill_stop_sandbox", async move {
            let mut sb = sandbox.lock().await;
            let _ = sb.stop().await;
        });
    }

    Ok(())
}

fn enqueue_popup(app: &tauri::AppHandle, kind: &str, title: &str, body: &str) {
    emit_event(
        app,
        "app://popup",
        PopupEvent::PopupEnqueue {
            popup_id: uuid::Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            actions: vec![PopupAction {
                id: "dismiss".to_string(),
                label: "Dismiss".to_string(),
                primary: true,
            }],
            ttl_ms: Some(5000),
        },
    );
}

fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

fn detected_editors() -> Vec<EditorEntry> {
    sandbox::settings::KNOWN_TOOLS
        .iter()
        .map(|tool| EditorEntry {
            name: tool.name.to_string(),
            binary: tool.binary.to_string(),
            is_tui: tool.is_tui,
            available: sandbox::settings::is_tool_available(tool.binary),
        })
        .collect()
}

fn try_spawn_gui_editor(binary: &str, clone_path: &Path) -> bool {
    std::process::Command::new(binary)
        .arg(clone_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

fn open_editor_path(tool: Option<&str>, clone_path: &Path) -> Result<String, String> {
    let pref = tool.unwrap_or("auto");
    if pref == "auto" {
        for entry in detected_editors() {
            if entry.available && !entry.is_tui && try_spawn_gui_editor(&entry.binary, clone_path) {
                return Ok(format!("Opened in {}.", entry.name));
            }
        }
        return Err("No supported GUI editor found. Install VS Code, Cursor, GitKraken, or Fork.".to_string());
    }

    if let Some(info) = sandbox::settings::KNOWN_TOOLS
        .iter()
        .find(|item| item.name == pref || item.binary == pref)
    {
        if info.is_tui {
            return Err(format!(
                "'{}' is terminal-only. Use a GUI editor like vscode or cursor in desktop mode.",
                info.name
            ));
        }
        if sandbox::settings::is_tool_available(info.binary) && try_spawn_gui_editor(info.binary, clone_path) {
            return Ok(format!("Opened in {}.", info.name));
        }

        #[cfg(target_os = "macos")]
        if let Some(app_name) = info.macos_app {
            let ok = std::process::Command::new("open")
                .args(["-a", app_name])
                .arg(clone_path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Ok(format!("Opened in {}.", app_name));
            }
        }

        return Err(format!("Tool '{}' is not available on PATH.", info.name));
    }

    if try_spawn_gui_editor(pref, clone_path) {
        return Ok(format!("Opened in {}.", pref));
    }

    Err(format!("No tool '{}' found.", pref))
}

fn load_agents_registry() -> sandbox::AgentsRegistryClient {
    if let Ok(path) = std::env::var("NANOSB_REGISTRY_PATH") {
        let p = std::path::Path::new(&path);
        if p.join("index.json").exists() {
            if let Ok(client) = sandbox::AgentsRegistryClient::from_path(p) {
                return client;
            }
        }
    }

    if let Some(home) = dirs::home_dir() {
        let p = home.join(".nanosandbox").join("agents-registry");
        if p.join("index.json").exists() {
            if let Ok(client) = sandbox::AgentsRegistryClient::from_path(&p) {
                return client;
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let p = cwd.join("../agents-registry");
        if p.join("index.json").exists() {
            if let Ok(client) = sandbox::AgentsRegistryClient::from_path(&p) {
                return client;
            }
        }
    }

    sandbox::AgentsRegistryClient::online_only()
}

fn focused_pane_id(state: &State<'_, DesktopState>) -> Result<usize, ApiError> {
    let registry = state.panel_registry.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire panel registry".to_string(),
        detail: None,
    })?;
    Ok(registry.focused_panel)
}

fn sandbox_handle_for_pane(
    state: &State<'_, DesktopState>,
    pane_id: usize,
) -> Result<Arc<TokioMutex<sandbox::Sandbox>>, ApiError> {
    let sandboxes = state.sandbox_handles.lock().map_err(|_| ApiError {
        code: "INTERNAL".to_string(),
        message: "Failed to acquire sandbox state".to_string(),
        detail: None,
    })?;
    sandboxes.get(&pane_id).cloned().ok_or(ApiError {
        code: "NOT_FOUND".to_string(),
        message: "No sandbox attached to focused pane".to_string(),
        detail: None,
    })
}

fn resolve_target_panes_for_mcp(
    state: &State<'_, DesktopState>,
    target: Option<&str>,
) -> Result<Vec<usize>, ApiError> {
    if let Some(raw) = target {
        if raw.eq_ignore_ascii_case("all") {
            let panes = state.panes.lock().map_err(|_| ApiError {
                code: "INTERNAL".to_string(),
                message: "Failed to acquire pane state".to_string(),
                detail: None,
            })?;
            return Ok(panes.iter().map(|pane| pane.pane_id).collect());
        }

        let panes = state.panes.lock().map_err(|_| ApiError {
            code: "INTERNAL".to_string(),
            message: "Failed to acquire pane state".to_string(),
            detail: None,
        })?;

        if let Ok(id) = raw.parse::<usize>() {
            if panes.iter().any(|pane| pane.pane_id == id) {
                return Ok(vec![id]);
            }
        }

        if let Some(match_pane) = panes.iter().find(|pane| {
            pane.agent_name.eq_ignore_ascii_case(raw)
                || pane
                    .display_name
                    .as_deref()
                    .map(|name| name.eq_ignore_ascii_case(raw))
                    .unwrap_or(false)
        }) {
            return Ok(vec![match_pane.pane_id]);
        }

        return Err(ApiError {
            code: "NOT_FOUND".to_string(),
            message: format!("Target '{}' not found", raw),
            detail: None,
        });
    }

    Ok(vec![focused_pane_id(state)?])
}

fn process_auth_urls_from_output(
    _app: &tauri::AppHandle,
    state: &State<'_, DesktopState>,
    pane_id: usize,
    output: &[u8],
) -> Result<(), ApiError> {
    let previous_buffer = {
        let mut buffers = state.url_buffers.lock().map_err(|_| ApiError {
            code: "INTERNAL".to_string(),
            message: "Failed to acquire URL buffer state".to_string(),
            detail: None,
        })?;
        buffers.remove(&pane_id).unwrap_or_default()
    };

    let (urls, new_buffer) = terminal_core::extract_urls(output, &previous_buffer);
    {
        let mut buffers = state.url_buffers.lock().map_err(|_| ApiError {
            code: "INTERNAL".to_string(),
            message: "Failed to update URL buffer state".to_string(),
            detail: None,
        })?;
        buffers.insert(pane_id, new_buffer);
    }

    for url in urls {
        if !terminal_core::is_auth_url(&url) {
            continue;
        }
        let key = terminal_core::url_dedup_key(&url);
        let should_open = {
            let mut opened = state.opened_auth_keys.lock().map_err(|_| ApiError {
                code: "INTERNAL".to_string(),
                message: "Failed to acquire auth URL state".to_string(),
                detail: None,
            })?;
            opened.insert(key)
        };

        if !should_open {
            continue;
        }

        open_url_in_browser(&url);
    }

    Ok(())
}

#[tauri::command]
fn app_bootstrap(app: tauri::AppHandle, state: State<'_, DesktopState>) -> ApiResult<AppBootstrap> {
    let mut panes = match state.panes.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
    };
    let registry = match state.panel_registry.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
    };
    let theme_name = match state.theme_name.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return err("INTERNAL", "Failed to acquire theme state", None),
    };
    let zoomed_pane = match state.zoomed_pane.lock() {
        Ok(guard) => *guard,
        Err(_) => return err("INTERNAL", "Failed to acquire zoom state", None),
    };
    let workspace = match state.workspace_context.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return err("INTERNAL", "Failed to acquire workspace state", None),
    };

    apply_registry_to_panes(&mut panes, &registry);

    let layout = build_layout(&panes, &registry, zoomed_pane);
    emit_event(
        &app,
        "app://pane",
        PaneEvent::PaneSnapshot {
            panes: panes.clone(),
            layout: layout.clone(),
        },
    );
    emit_status_update(&app, &panes, registry.focused_panel);

    ok(AppBootstrap {
        version: env!("CARGO_PKG_VERSION").to_string(),
        theme: theme_snapshot(&theme_name),
        layout,
        panes: panes.clone(),
        command_history_size: 0,
        workspace,
    })
}

#[tauri::command]
fn pane_focus(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
) -> ApiResult<PaneSummary> {
    let mut panes = match state.panes.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
    };

    let mut registry = match state.panel_registry.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
    };

    if panes.iter().all(|pane| pane.pane_id != pane_id) {
        return err("NOT_FOUND", "Pane not found", None);
    }

    if registry.hidden_panels.contains(&pane_id) {
        return err("BAD_REQUEST", "Cannot focus hidden pane", None);
    }

    registry.focused_panel = pane_id;
    apply_registry_to_panes(&mut panes, &registry);

    let zoomed_pane = match state.zoomed_pane.lock() {
        Ok(guard) => *guard,
        Err(_) => return err("INTERNAL", "Failed to acquire zoom state", None),
    };

    let layout = build_layout(&panes, &registry, zoomed_pane);
    emit_event(
        &app,
        "app://pane",
        PaneEvent::PaneSnapshot {
            panes: panes.clone(),
            layout,
        },
    );
    emit_event(&app, "app://pane", PaneEvent::FocusChanged { pane_id });
    emit_status_update(&app, &panes, pane_id);

    match panes.iter().find(|pane| pane.pane_id == pane_id) {
        Some(pane) => ok(pane.clone()),
        None => err("NOT_FOUND", "Pane not found", None),
    }
}

#[tauri::command]
fn pane_zoom_toggle(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: Option<usize>,
) -> ApiResult<LayoutSnapshot> {
    let panes = match state.panes.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
    };

    let registry = match state.panel_registry.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
    };

    let focused_pane = registry.focused_panel;

    let target = pane_id.unwrap_or(focused_pane);
    if panes.iter().all(|pane| pane.pane_id != target) {
        return err("NOT_FOUND", "Pane not found", None);
    }

    let zoomed_pane = {
        let mut zoom = match state.zoomed_pane.lock() {
            Ok(guard) => guard,
            Err(_) => return err("INTERNAL", "Failed to acquire zoom state", None),
        };
        if *zoom == Some(target) {
            *zoom = None;
        } else {
            *zoom = Some(target);
        }
        *zoom
    };

    let layout = build_layout(&panes, &registry, zoomed_pane);
    emit_event(
        &app,
        "app://pane",
        PaneEvent::ZoomChanged { zoomed_pane },
    );
    emit_event(
        &app,
        "app://pane",
        PaneEvent::PaneSnapshot {
            panes: panes.clone(),
            layout: layout.clone(),
        },
    );

    ok(layout)
}

#[tauri::command]
fn input_submit(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
    text: String,
) -> ApiResult<InputSubmitResult> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ok(InputSubmitResult {
            kind: "empty".to_string(),
            command_name: None,
            message: None,
        });
    }

    let parse_result = match std::panic::catch_unwind(|| command_core::parse_command_verbose(trimmed)) {
        Ok(result) => result,
        Err(_) => {
            enqueue_popup(
                &app,
                "error",
                "Command parser failed",
                "Command parser panicked while processing input.",
            );
            return err("INTERNAL", "Command parser panicked", None);
        }
    };

    match parse_result {
        ParseResult::NotACommand => {
            let content = format!("{text}\r\n");
            let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
            let seq = match next_terminal_seq(&state, pane_id) {
                Ok(value) => value,
                Err(error) => return err(&error.code, &error.message, error.detail),
            };
            emit_event(
                &app,
                "app://terminal",
                TerminalEvent::TerminalData {
                    pane_id,
                    seq,
                    encoding: "base64".to_string(),
                    data: encoded,
                },
            );

            let panes = match state.panes.lock() {
                Ok(guard) => guard,
                Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
            };
            let registry = match state.panel_registry.lock() {
                Ok(guard) => guard,
                Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
            };
            emit_status_update(&app, &panes, registry.focused_panel);

            ok(InputSubmitResult {
                kind: "message".to_string(),
                command_name: None,
                message: Some(format!("Queued for pane {}", pane_id)),
            })
        }
        ParseResult::Ok(command) => {
            let command_name = trimmed.split_whitespace().next().map(str::to_string);

            match command {
                Command::AddAgent {
                    agent,
                    image,
                    tag,
                    project,
                    branch,
                    name,
                    auto_mode,
                    prompt,
                    model,
                    use_env,
                    env_file,
                    run_as_root,
                } => {
                    let workspace = match state.workspace_context.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return err("INTERNAL", "Failed to acquire workspace state", None),
                    };
                    let effective_project = project.or_else(|| workspace.project_path.clone());
                    if let Some(path) = effective_project.as_deref() {
                        let project_path = PathBuf::from(path);
                        let _ = upsert_recent_project(&state, &project_path);
                    }

                    let permissions = sandbox::Permissions::Default;
                    let env = match build_add_agent_env(
                        &agent,
                        model.as_deref(),
                        prompt.as_deref(),
                        auto_mode,
                        permissions,
                        &use_env,
                        env_file.as_deref(),
                    ) {
                        Ok(vars) => vars,
                        Err(e) => return err("BAD_REQUEST", "Invalid /add environment", Some(e)),
                    };

                    let pane_id = {
                        let panes = match state.panes.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                        };
                        panes.iter().map(|pane| pane.pane_id).max().map(|v| v + 1).unwrap_or(0)
                    };

                    {
                        let mut panes = match state.panes.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                        };
                        let mut registry = match state.panel_registry.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                        };

                        registry.focused_panel = pane_id;
                        registry.hidden_panels.remove(&pane_id);

                        panes.push(PaneSummary {
                            pane_id,
                            agent_name: agent.clone(),
                            display_name: name.clone(),
                            sandbox_id_short: "pending".to_string(),
                            mode: "loading".to_string(),
                            status: "loading".to_string(),
                            visible: true,
                            focused: true,
                            reconnecting: false,
                            loading_message: Some("Creating sandbox...".to_string()),
                            loading_error: None,
                            elapsed_ms: None,
                        });
                        apply_registry_to_panes(&mut panes, &registry);
                    }

                    let _ = emit_pane_snapshot(&app, &state);
                    spawn_add_agent_runtime(
                        app.clone(),
                        pane_id,
                        agent,
                        image,
                        tag,
                        name,
                        effective_project,
                        branch,
                        run_as_root,
                        auto_mode,
                        prompt,
                        env,
                        model,
                    );
                }
                Command::Help => {
                    let help = concat!(
                        "Available commands:\n",
                        "  /add <agent> [--tag <version>] [--model <model>] [--auto-mode -p <prompt>] [--run-as-root] [--image <img>] [--project <path>] [--branch <name>] [--name <name>] [--env-file <path>] [--use-env <KEY>]...\n",
                        "                                Add a new agent panel\n",
                        "  /sandboxes                    Toggle sandbox sidebar\n",
                        "  /focus <n>                    Focus panel n (0-indexed)\n",
                        "  /close [n|name]               Hide panel (sandbox keeps running)\n",
                        "  /open [n|name]                Show a hidden panel\n",
                        "  /kill [n|name]                Kill sandbox & remove panel\n",
                        "  /copy                         Copy panel content to clipboard\n",
                        "  /upload <path>                Upload host file to sandbox\n",
                        "  /paste-image                  Paste clipboard image to sandbox\n",
                        "  /zoom                         Toggle panel zoom (Ctrl+F)\n",
                        "  /theme [name]                 Switch colour theme\n",
                        "  /env [KEY=VALUE]              Set/list panel env vars\n",
                        "  /reconnect                    Reconnect SSH terminal\n",
                        "  /branches                     List nanosb branches in project\n",
                        "  /mcp                          Toggle MCP sidebar\n",
                        "  /mcp list                     List MCP servers\n",
                        "  /mcp add <name> <cmd> [args]  Add MCP server\n",
                        "  /mcp remove <name>            Remove MCP server\n",
                        "  /mcp enable <name>            Enable MCP server\n",
                        "  /mcp disable <name>           Disable MCP server\n",
                        "  /skills [list]                List active skills\n",
                        "  /skills add <name>            Add skill from registry\n",
                        "  /skills remove <name>         Remove a skill\n",
                        "  /skills show <name>           Show skill details\n",
                        "  /agent                        Show current agent definition\n",
                        "  /agent set <name>             Set agent from registry\n",
                        "  /agent list                   List available agents\n",
                        "  /agent show <name>            Show agent details\n",
                        "  /gitsync [on|off|now]         Sync sandbox commits to local repo\n",
                        "  /edit [tool]                  Open clone in external tool\n",
                        "  /clearhistory                 Clear command history\n",
                        "  /quit                         Suspend session and exit\n",
                        "  /destroy                      Full cleanup and exit\n",
                    );
                    enqueue_popup(&app, "info", "Help", help);
                }
                Command::Quit => {
                    enqueue_popup(&app, "info", "Quit", "Closing desktop app...");
                    app.exit(0);
                }
                Command::Destroy => {
                    let pane_ids = {
                        let panes = match state.panes.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                        };
                        panes.iter().map(|pane| pane.pane_id).collect::<Vec<_>>()
                    };
                    for id in pane_ids {
                        if let Err(api_error) = pane_kill_by_id(&app, &state, id) {
                            return err(&api_error.code, &api_error.message, api_error.detail);
                        }
                    }
                    enqueue_popup(&app, "success", "Destroy complete", "All panes were destroyed.");
                }
                Command::ClearHistory => {
                    emit_event(&app, "app://ui", UiEvent::ClearHistory);
                    enqueue_popup(&app, "success", "History cleared", "Desktop command history cleared.");
                }
                Command::Focus { panel } => {
                    let mut panes = match state.panes.lock() {
                        Ok(guard) => guard,
                        Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                    };
                    let mut registry = match state.panel_registry.lock() {
                        Ok(guard) => guard,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };

                    if panes.iter().all(|pane| pane.pane_id != panel) {
                        return err("NOT_FOUND", "Pane not found", None);
                    }
                    if registry.hidden_panels.contains(&panel) {
                        return err("BAD_REQUEST", "Cannot focus hidden pane", None);
                    }

                    registry.focused_panel = panel;
                    apply_registry_to_panes(&mut panes, &registry);
                    let zoomed = match state.zoomed_pane.lock() {
                        Ok(guard) => *guard,
                        Err(_) => return err("INTERNAL", "Failed to acquire zoom state", None),
                    };
                    let layout = build_layout(&panes, &registry, zoomed);
                    emit_event(
                        &app,
                        "app://pane",
                        PaneEvent::PaneSnapshot {
                            panes: panes.clone(),
                            layout,
                        },
                    );
                    emit_event(&app, "app://pane", PaneEvent::FocusChanged { pane_id: panel });
                }
                Command::Zoom => {
                    let panes = match state.panes.lock() {
                        Ok(guard) => guard,
                        Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                    };
                    let registry = match state.panel_registry.lock() {
                        Ok(guard) => guard,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };
                    let focused = registry.focused_panel;

                    let zoomed = {
                        let mut zoom = match state.zoomed_pane.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire zoom state", None),
                        };
                        if *zoom == Some(focused) {
                            *zoom = None;
                        } else {
                            *zoom = Some(focused);
                        }
                        *zoom
                    };

                    let layout = build_layout(&panes, &registry, zoomed);
                    emit_event(&app, "app://pane", PaneEvent::ZoomChanged { zoomed_pane: zoomed });
                    emit_event(
                        &app,
                        "app://pane",
                        PaneEvent::PaneSnapshot {
                            panes: panes.clone(),
                            layout,
                        },
                    );
                }
                Command::Close { target } => {
                    let target_id = {
                        let panes = match state.panes.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                        };
                        let registry = match state.panel_registry.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                        };
                        resolve_pane_target(&panes, target.as_deref(), registry.focused_panel)
                    };

                    let target_id = match target_id {
                        Some(id) => id,
                        None => return err("NOT_FOUND", "Pane not found", None),
                    };

                    if let Err(api_error) = pane_close_by_id(&app, &state, target_id) {
                        return err(&api_error.code, &api_error.message, api_error.detail);
                    }
                }
                Command::Open { target } => {
                    let target_id = {
                        let panes = match state.panes.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                        };
                        let registry = match state.panel_registry.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                        };
                        if let Some(t) = target.as_deref() {
                            resolve_pane_target(&panes, Some(t), registry.focused_panel)
                        } else {
                            registry.hidden_panels.iter().next().copied()
                        }
                    };

                    let pane_id = match target_id {
                        Some(id) => id,
                        None => return err("NOT_FOUND", "No hidden pane to open", None),
                    };

                    if let Err(api_error) = pane_open_by_id(&app, &state, pane_id) {
                        return err(&api_error.code, &api_error.message, api_error.detail);
                    }
                }
                Command::Upload { path } => {
                    let pane_id = match state.panel_registry.lock() {
                        Ok(guard) => guard.focused_panel,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };
                    if let Err(upload_error) = start_file_upload(&app, &state, pane_id, &path) {
                        let upload_id = uuid::Uuid::new_v4().to_string();
                        emit_upload_failed(&app, &upload_id, pane_id, &upload_error.message);
                        enqueue_popup(&app, "error", "Upload failed", &upload_error.message);
                        return err(
                            &upload_error.code,
                            &upload_error.message,
                            upload_error.detail,
                        );
                    }
                }
                Command::PasteImage => {
                    let pane_id = match state.panel_registry.lock() {
                        Ok(guard) => guard.focused_panel,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };
                    if let Err(upload_error) = start_clipboard_image_upload(&app, &state, pane_id) {
                        let upload_id = uuid::Uuid::new_v4().to_string();
                        emit_upload_failed(&app, &upload_id, pane_id, &upload_error.message);
                        enqueue_popup(&app, "error", "Upload failed", &upload_error.message);
                        return err(
                            &upload_error.code,
                            &upload_error.message,
                            upload_error.detail,
                        );
                    }
                }
                Command::Reconnect => {
                    let pane_id = match state.panel_registry.lock() {
                        Ok(guard) => guard.focused_panel,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };
                    let info = {
                        let map = match state.ssh_connection_info.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire SSH state", None),
                        };
                        map.get(&pane_id).cloned()
                    };

                    let Some(info) = info else {
                        return err("NOT_FOUND", "No SSH connection info for pane", None);
                    };

                    {
                        let mut panes = match state.panes.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                        };
                        if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                            pane.status = "loading".to_string();
                            pane.loading_message = Some("Reconnecting SSH...".to_string());
                            pane.reconnecting = true;
                        }
                    }
                    let _ = emit_pane_snapshot(&app, &state);

                    let app_clone = app.clone();
                    spawn_logged("pane_reconnect", async move {
                        let _ = connect_pane_ssh(app_clone, pane_id, info).await;
                    });
                }
                Command::Kill { panel } => {
                    let target = {
                        let panes = match state.panes.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
                        };
                        let registry = match state.panel_registry.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                        };
                        resolve_pane_target(&panes, panel.as_deref(), registry.focused_panel)
                    };

                    let target = match target {
                        Some(id) => id,
                        None => return err("NOT_FOUND", "Pane not found", None),
                    };

                    if let Err(api_error) = pane_kill_by_id(&app, &state, target) {
                        return err(&api_error.code, &api_error.message, api_error.detail);
                    }
                }
                Command::Theme { name } => {
                    if let Some(theme_name) = name {
                        let mut current = match state.theme_name.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire theme state", None),
                        };
                        *current = theme_name.clone();
                        enqueue_popup(
                            &app,
                            "success",
                            "Theme updated",
                            &format!("Theme set to {}", theme_name),
                        );
                    } else {
                        enqueue_popup(
                            &app,
                            "info",
                            "Available themes",
                            "nanosandbox, nanosandbox-light, dracula, catppuccin, tokyo-night, nord",
                        );
                    }
                }
                Command::Env { assignment } => {
                    let focused = match state.panel_registry.lock() {
                        Ok(guard) => guard.focused_panel,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };
                    if let Some((key, value)) = assignment {
                        let identity = state
                            .pane_sandbox_identity
                            .lock()
                            .ok()
                            .and_then(|map| map.get(&focused).cloned());
                        let mut info = match state.ssh_connection_info.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire SSH state", None),
                        };
                        if let Some(conn) = info.get_mut(&focused) {
                            conn.env.insert(key.clone(), value.clone());
                            if let Some(id) = identity {
                                let mut store = match state.persisted_env.lock() {
                                    Ok(guard) => guard,
                                    Err(_) => return err("INTERNAL", "Failed to acquire env persistence state", None),
                                };
                                store
                                    .sandboxes
                                    .entry(id)
                                    .or_insert_with(HashMap::new)
                                    .insert(key.clone(), value.clone());
                                if let Err(e) = save_persisted_env_store(&store) {
                                    enqueue_popup(&app, "warning", "Env", &e);
                                }
                            }
                            enqueue_popup(&app, "success", "Env updated", &format!("{} set for pane {}", key, focused));
                        } else {
                            if let Some(id) = identity {
                                let mut store = match state.persisted_env.lock() {
                                    Ok(guard) => guard,
                                    Err(_) => return err("INTERNAL", "Failed to acquire env persistence state", None),
                                };
                                store
                                    .sandboxes
                                    .entry(id)
                                    .or_insert_with(HashMap::new)
                                    .insert(key.clone(), value.clone());
                                if let Err(e) = save_persisted_env_store(&store) {
                                    enqueue_popup(&app, "warning", "Env", &e);
                                }
                                enqueue_popup(
                                    &app,
                                    "success",
                                    "Env updated",
                                    &format!("{} saved for next reconnect", key),
                                );
                            } else {
                            enqueue_popup(
                                &app,
                                "warning",
                                "Env unavailable",
                                "Focused pane has no SSH runtime env yet.",
                            );
                            }
                        }
                    } else {
                        let identity = state
                            .pane_sandbox_identity
                            .lock()
                            .ok()
                            .and_then(|map| map.get(&focused).cloned());
                        let info = match state.ssh_connection_info.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire SSH state", None),
                        };
                        let body = match info.get(&focused) {
                            Some(conn) if !conn.env.is_empty() => conn
                                .env
                                .iter()
                                .map(|(k, _)| k.clone())
                                .collect::<Vec<_>>()
                                .join(", "),
                            Some(_) => "No env vars set for focused pane.".to_string(),
                            None => {
                                if let Some(id) = identity {
                                    if let Ok(store) = state.persisted_env.lock() {
                                        if let Some(saved) = store.sandboxes.get(&id) {
                                            if !saved.is_empty() {
                                                let mut keys: Vec<String> = saved.keys().cloned().collect();
                                                keys.sort();
                                                format!("Persisted env keys: {}", keys.join(", "))
                                            } else {
                                                "No env vars set for focused pane.".to_string()
                                            }
                                        } else {
                                            "Focused pane has no SSH runtime env yet.".to_string()
                                        }
                                    } else {
                                        "Focused pane has no SSH runtime env yet.".to_string()
                                    }
                                } else {
                                    "Focused pane has no SSH runtime env yet.".to_string()
                                }
                            }
                        };
                        enqueue_popup(&app, "info", "Environment", &body);
                    }
                }
                Command::Sandboxes => {
                    emit_event(
                        &app,
                        "app://ui",
                        UiEvent::PanelToggle {
                            target: "sandboxes".to_string(),
                            open: true,
                        },
                    );
                }
                Command::Copy => {
                    // Desktop frontend handles /copy by reading focused terminal buffer and writing clipboard.
                }
                Command::Branches => {
                    let focused = match state.panel_registry.lock() {
                        Ok(guard) => guard.focused_panel,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };
                    let clone_path = match clone_workdir_for_pane(&state, focused) {
                        Ok(path) => path,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Branches", &e.message);
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(e.message),
                            });
                        }
                    };
                    let output = std::process::Command::new("git")
                        .args(["branch", "--list", "nanosb/*"])
                        .current_dir(&clone_path)
                        .output();
                    match output {
                        Ok(out) => {
                            let branches = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            let body = if branches.is_empty() {
                                "No nanosb branches found.".to_string()
                            } else {
                                format!("Nanosb branches:\n{}", branches)
                            };
                            enqueue_popup(&app, "info", "Branches", &body);
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Branches", &format!("Failed to list branches: {}", e));
                        }
                    }
                }
                Command::GitSync { action } => {
                    let focused = match state.panel_registry.lock() {
                        Ok(guard) => guard.focused_panel,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };
                    match action.as_deref() {
                        None => {
                            let enabled = match state.pane_gitsync_override.lock() {
                                Ok(guard) => *guard.get(&focused).unwrap_or(&false),
                                Err(_) => return err("INTERNAL", "Failed to acquire gitsync state", None),
                            };
                            let status = if enabled { "ON (unsafe)" } else { "OFF (safe)" };
                            let branch_info = match sandbox_handle_for_pane(&state, focused) {
                                Ok(handle) => {
                                    let sb = handle.blocking_lock();
                                    match sb.project_mount() {
                                        Some(pm) if !pm.created_branches.is_empty() => pm
                                            .created_branches
                                            .first()
                                            .map(|(_, branch)| format!("Branch: {}", branch))
                                            .unwrap_or_else(|| "No source branch created yet".to_string()),
                                        Some(_) => "No source branch created yet".to_string(),
                                        None => "No project mount for this panel".to_string(),
                                    }
                                }
                                Err(_) => "No sandbox attached to focused pane".to_string(),
                            };
                            enqueue_popup(
                                &app,
                                "info",
                                "Git sync",
                                &format!("Git sync: {}\n{}", status, branch_info),
                            );
                        }
                        Some("on") => {
                            if let Ok(mut guard) = state.pane_gitsync_override.lock() {
                                guard.insert(focused, true);
                            }
                            let mut branch_error: Option<String> = None;
                            if let Ok(handle) = sandbox_handle_for_pane(&state, focused) {
                                let mut sb = handle.blocking_lock();
                                if let Some(pm) = sb.project_mount_mut() {
                                    if pm.created_branches.is_empty() {
                                        if let Err(e) = pm.create_source_branch_and_fetch() {
                                            branch_error = Some(e);
                                        }
                                    }
                                }
                            }
                            if let Some(e) = branch_error {
                                enqueue_popup(
                                    &app,
                                    "warning",
                                    "Git sync",
                                    &format!("Auto-sync enabled, but failed to create source branch: {}", e),
                                );
                            } else {
                                enqueue_popup(&app, "warning", "Git sync", "Auto-sync ENABLED for focused panel.");
                            }
                        }
                        Some("off") => {
                            if let Ok(mut guard) = state.pane_gitsync_override.lock() {
                                guard.insert(focused, false);
                            }
                            enqueue_popup(&app, "success", "Git sync", "Auto-sync DISABLED for focused panel.");
                        }
                        Some("now") => {
                            let handle = match sandbox_handle_for_pane(&state, focused) {
                                Ok(value) => value,
                                Err(e) => {
                                    enqueue_popup(&app, "warning", "Git sync", &e.message);
                                    return ok(InputSubmitResult {
                                        kind: "command".to_string(),
                                        command_name,
                                        message: Some(e.message),
                                    });
                                }
                            };
                            let mut sb = handle.blocking_lock();
                            let Some(pm) = sb.project_mount_mut() else {
                                enqueue_popup(&app, "warning", "Git sync", "No project mount for this panel.");
                                return ok(InputSubmitResult {
                                    kind: "command".to_string(),
                                    command_name,
                                    message: Some("No project mount for this panel".to_string()),
                                });
                            };
                            match pm.create_source_branch_and_fetch() {
                                Ok(()) => {
                                    let branch = pm
                                        .created_branches
                                        .first()
                                        .map(|(_, b)| b.clone())
                                        .unwrap_or_else(|| "unknown".to_string());
                                    enqueue_popup(
                                        &app,
                                        "success",
                                        "Git sync",
                                        &format!("Synced to branch '{}'.", branch),
                                    );
                                }
                                Err(e) => {
                                    enqueue_popup(&app, "warning", "Git sync", &format!("Sync failed: {}", e));
                                }
                            }
                        }
                        Some(other) => {
                            enqueue_popup(
                                &app,
                                "warning",
                                "Git sync",
                                &format!("Unsupported gitsync action '{}'.", other),
                            );
                        }
                    }
                }
                Command::Edit { tool } => {
                    let focused = match state.panel_registry.lock() {
                        Ok(guard) => guard.focused_panel,
                        Err(_) => return err("INTERNAL", "Failed to acquire panel registry", None),
                    };

                    let clone_path = {
                        let map = match state.pane_workdirs.lock() {
                            Ok(guard) => guard,
                            Err(_) => return err("INTERNAL", "Failed to acquire project workspace state", None),
                        };
                        map.get(&focused).cloned()
                    };

                    let Some(clone_path) = clone_path else {
                        enqueue_popup(
                            &app,
                            "warning",
                            "No project clone",
                            "Focused panel has no project clone directory to open.",
                        );
                        return ok(InputSubmitResult {
                            kind: "command".to_string(),
                            command_name,
                            message: Some("Focused panel has no project clone directory".to_string()),
                        });
                    };

                    match open_editor_path(tool.as_deref(), &clone_path) {
                        Ok(message) => {
                            enqueue_popup(&app, "success", "Editor", &message);
                        }
                        Err(message) => {
                            enqueue_popup(&app, "warning", "Editor", &message);
                        }
                    }
                }
                Command::McpToggle => {
                    emit_event(
                        &app,
                        "app://ui",
                        UiEvent::PanelToggle {
                            target: "sandboxes".to_string(),
                            open: true,
                        },
                    );
                }
                Command::McpList => {
                    let pane_id = match focused_pane_id(&state) {
                        Ok(id) => id,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                        Ok(handle) => handle,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "MCP", &e.message);
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(e.message),
                            });
                        }
                    };
                    let sb = sandbox.blocking_lock();
                    let result = sb.gateway().and_then(|gw| gw.http_get("/api/v1/mcp/servers"));
                    match result {
                        Ok((status, body)) if status < 400 => {
                            let parsed: Result<HashMap<String, sandbox::McpServerConfig>, _> =
                                serde_json::from_str(&body);
                            match parsed {
                                Ok(servers) => {
                                    if servers.is_empty() {
                                        enqueue_popup(&app, "info", "MCP", "No MCP servers configured.");
                                    } else {
                                        let mut lines = vec!["MCP Servers:".to_string()];
                                        let mut sorted: Vec<_> = servers.into_iter().collect();
                                        sorted.sort_by(|a, b| a.0.cmp(&b.0));
                                        for (name, cfg) in sorted {
                                            let status = if cfg.enabled { "enabled" } else { "disabled" };
                                            lines.push(format!(
                                                "- {} [{}] {} {}",
                                                name,
                                                status,
                                                cfg.command,
                                                cfg.args.join(" ")
                                            ));
                                        }
                                        enqueue_popup(&app, "info", "MCP", &lines.join("\n"));
                                    }
                                }
                                Err(e) => {
                                    enqueue_popup(
                                        &app,
                                        "warning",
                                        "MCP",
                                        &format!("Failed to parse MCP server list: {}", e),
                                    );
                                }
                            }
                        }
                        Ok((status, body)) => {
                            enqueue_popup(
                                &app,
                                "warning",
                                "MCP",
                                &format!("Failed to list MCP servers ({}): {}", status, body),
                            );
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "MCP", &format!("Failed to list MCP servers: {}", e));
                        }
                    }
                }
                Command::McpAdd {
                    name,
                    command,
                    args,
                    target,
                } => {
                    let targets = match resolve_target_panes_for_mcp(&state, target.as_deref()) {
                        Ok(values) => values,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let body = serde_json::json!({
                        "name": name,
                        "command": command,
                        "args": args,
                        "env": serde_json::Map::<String, serde_json::Value>::new(),
                        "enabled": true,
                    })
                    .to_string();
                    let mut ok_count = 0usize;
                    let mut errors = Vec::new();
                    for pane_id in targets {
                        let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                            Ok(handle) => handle,
                            Err(e) => {
                                errors.push(format!("pane {}: {}", pane_id, e.message));
                                continue;
                            }
                        };
                        let sb = sandbox.blocking_lock();
                        match sb
                            .gateway()
                            .and_then(|gw| gw.http_post("/api/v1/mcp/servers", &body))
                        {
                            Ok((status, _)) if status < 400 => ok_count += 1,
                            Ok((status, resp)) => {
                                errors.push(format!("pane {}: ({}) {}", pane_id, status, resp))
                            }
                            Err(e) => errors.push(format!("pane {}: {}", pane_id, e)),
                        }
                    }
                    if errors.is_empty() {
                        enqueue_popup(
                            &app,
                            "success",
                            "MCP",
                            &format!("MCP server '{}' added in {} pane(s).", name, ok_count),
                        );
                    } else {
                        enqueue_popup(
                            &app,
                            "warning",
                            "MCP",
                            &format!(
                                "MCP server '{}' add completed with errors.\n{}",
                                name,
                                errors.join("\n")
                            ),
                        );
                    }
                }
                Command::McpRemove { name, target } => {
                    let targets = match resolve_target_panes_for_mcp(&state, target.as_deref()) {
                        Ok(values) => values,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let path = format!("/api/v1/mcp/servers/{}", name);
                    let mut ok_count = 0usize;
                    let mut errors = Vec::new();
                    for pane_id in targets {
                        let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                            Ok(handle) => handle,
                            Err(e) => {
                                errors.push(format!("pane {}: {}", pane_id, e.message));
                                continue;
                            }
                        };
                        let sb = sandbox.blocking_lock();
                        match sb.gateway().and_then(|gw| gw.http_delete(&path)) {
                            Ok((status, _)) if status < 400 => ok_count += 1,
                            Ok((status, resp)) => {
                                errors.push(format!("pane {}: ({}) {}", pane_id, status, resp))
                            }
                            Err(e) => errors.push(format!("pane {}: {}", pane_id, e)),
                        }
                    }
                    if errors.is_empty() {
                        enqueue_popup(
                            &app,
                            "success",
                            "MCP",
                            &format!("MCP server '{}' removed from {} pane(s).", name, ok_count),
                        );
                    } else {
                        enqueue_popup(
                            &app,
                            "warning",
                            "MCP",
                            &format!(
                                "MCP server '{}' remove completed with errors.\n{}",
                                name,
                                errors.join("\n")
                            ),
                        );
                    }
                }
                Command::McpEnable { name, target } => {
                    let targets = match resolve_target_panes_for_mcp(&state, target.as_deref()) {
                        Ok(values) => values,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let path = format!("/api/v1/mcp/servers/{}/enable", name);
                    let mut ok_count = 0usize;
                    let mut errors = Vec::new();
                    for pane_id in targets {
                        let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                            Ok(handle) => handle,
                            Err(e) => {
                                errors.push(format!("pane {}: {}", pane_id, e.message));
                                continue;
                            }
                        };
                        let sb = sandbox.blocking_lock();
                        match sb.gateway().and_then(|gw| gw.http_post(&path, "{}")) {
                            Ok((status, _)) if status < 400 => ok_count += 1,
                            Ok((status, resp)) => {
                                errors.push(format!("pane {}: ({}) {}", pane_id, status, resp))
                            }
                            Err(e) => errors.push(format!("pane {}: {}", pane_id, e)),
                        }
                    }
                    if errors.is_empty() {
                        enqueue_popup(
                            &app,
                            "success",
                            "MCP",
                            &format!("MCP server '{}' enabled in {} pane(s).", name, ok_count),
                        );
                    } else {
                        enqueue_popup(
                            &app,
                            "warning",
                            "MCP",
                            &format!(
                                "MCP server '{}' enable completed with errors.\n{}",
                                name,
                                errors.join("\n")
                            ),
                        );
                    }
                }
                Command::McpDisable { name, target } => {
                    let targets = match resolve_target_panes_for_mcp(&state, target.as_deref()) {
                        Ok(values) => values,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let path = format!("/api/v1/mcp/servers/{}/disable", name);
                    let mut ok_count = 0usize;
                    let mut errors = Vec::new();
                    for pane_id in targets {
                        let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                            Ok(handle) => handle,
                            Err(e) => {
                                errors.push(format!("pane {}: {}", pane_id, e.message));
                                continue;
                            }
                        };
                        let sb = sandbox.blocking_lock();
                        match sb.gateway().and_then(|gw| gw.http_post(&path, "{}")) {
                            Ok((status, _)) if status < 400 => ok_count += 1,
                            Ok((status, resp)) => {
                                errors.push(format!("pane {}: ({}) {}", pane_id, status, resp))
                            }
                            Err(e) => errors.push(format!("pane {}: {}", pane_id, e)),
                        }
                    }
                    if errors.is_empty() {
                        enqueue_popup(
                            &app,
                            "success",
                            "MCP",
                            &format!("MCP server '{}' disabled in {} pane(s).", name, ok_count),
                        );
                    } else {
                        enqueue_popup(
                            &app,
                            "warning",
                            "MCP",
                            &format!(
                                "MCP server '{}' disable completed with errors.\n{}",
                                name,
                                errors.join("\n")
                            ),
                        );
                    }
                }
                Command::SkillsList => {
                    let pane_id = match focused_pane_id(&state) {
                        Ok(id) => id,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                        Ok(handle) => handle,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skills", &e.message);
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(e.message),
                            });
                        }
                    };
                    let sb = sandbox.blocking_lock();
                    let result = sb.gateway().and_then(|gw| gw.http_get("/api/v1/skills"));
                    match result {
                        Ok((status, body)) if status < 400 => {
                            let parsed: Result<HashMap<String, sandbox::SkillDef>, _> = serde_json::from_str(&body);
                            match parsed {
                                Ok(skills) => {
                                    if skills.is_empty() {
                                        enqueue_popup(
                                            &app,
                                            "info",
                                            "Skills",
                                            "No skills configured. Use /skills add <name>.",
                                        );
                                    } else {
                                        let mut names: Vec<String> = skills.keys().cloned().collect();
                                        names.sort();
                                        enqueue_popup(&app, "info", "Skills", &format!("Skills:\n{}", names.join("\n")));
                                    }
                                }
                                Err(e) => {
                                    enqueue_popup(&app, "warning", "Skills", &format!("Failed to parse skills: {}", e));
                                }
                            }
                        }
                        Ok((status, body)) => {
                            enqueue_popup(
                                &app,
                                "warning",
                                "Skills",
                                &format!("Failed to list skills ({}): {}", status, body),
                            );
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skills", &format!("Failed to list skills: {}", e));
                        }
                    }
                }
                Command::SkillsAdd { name } => {
                    let pane_id = match focused_pane_id(&state) {
                        Ok(id) => id,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                        Ok(handle) => handle,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skills", &e.message);
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(e.message),
                            });
                        }
                    };
                    let registry = match state.registry.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return err("INTERNAL", "Failed to acquire registry state", None),
                    };
                    let skill = match registry.resolve_skill(&name) {
                        Ok(value) => value,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skills", &format!("Failed to resolve skill '{}': {}", name, e));
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(format!("Failed to resolve skill '{}'", name)),
                            });
                        }
                    };
                    let body = match serde_json::to_string(&skill) {
                        Ok(text) => text,
                        Err(e) => return err("INTERNAL", "Failed to serialize skill payload", Some(e.to_string())),
                    };
                    let sb = sandbox.blocking_lock();
                    match sb.gateway().and_then(|gw| gw.http_post("/api/v1/skills", &body)) {
                        Ok((status, _)) if status < 400 => {
                            let _ = sb
                                .gateway()
                                .and_then(|gw| gw.http_post("/api/v1/agent/restart", r#"{"reason":"skills_update"}"#));
                            enqueue_popup(&app, "success", "Skills", &format!("Skill '{}' added.", name));
                        }
                        Ok((status, resp)) => {
                            enqueue_popup(
                                &app,
                                "warning",
                                "Skills",
                                &format!("Failed to add skill '{}' ({}): {}", name, status, resp),
                            );
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skills", &format!("Failed to add skill '{}': {}", name, e));
                        }
                    }
                }
                Command::SkillsRemove { name } => {
                    let pane_id = match focused_pane_id(&state) {
                        Ok(id) => id,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                        Ok(handle) => handle,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skills", &e.message);
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(e.message),
                            });
                        }
                    };
                    let path = format!("/api/v1/skills/{}", name);
                    let sb = sandbox.blocking_lock();
                    match sb.gateway().and_then(|gw| gw.http_delete(&path)) {
                        Ok((status, _)) if status < 400 => {
                            let _ = sb
                                .gateway()
                                .and_then(|gw| gw.http_post("/api/v1/agent/restart", r#"{"reason":"skills_update"}"#));
                            enqueue_popup(&app, "success", "Skills", &format!("Skill '{}' removed.", name));
                        }
                        Ok((status, resp)) => {
                            enqueue_popup(
                                &app,
                                "warning",
                                "Skills",
                                &format!("Failed to remove skill '{}' ({}): {}", name, status, resp),
                            );
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skills", &format!("Failed to remove skill '{}': {}", name, e));
                        }
                    }
                }
                Command::SkillsShow { name } => {
                    let registry = match state.registry.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return err("INTERNAL", "Failed to acquire registry state", None),
                    };
                    match registry.resolve_skill(&name) {
                        Ok(skill) => {
                            let mut lines = vec![format!("Skill: {}", skill.name)];
                            if !skill.description.is_empty() {
                                lines.push(format!("Description: {}", skill.description));
                            }
                            if !skill.version.is_empty() {
                                lines.push(format!("Version: {}", skill.version));
                            }
                            if !skill.tags.is_empty() {
                                lines.push(format!("Tags: {}", skill.tags.join(", ")));
                            }
                            lines.push(String::new());
                            lines.push(skill.content);
                            enqueue_popup(&app, "info", "Skill", &lines.join("\n"));
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Skill", &format!("Skill '{}' not found: {}", name, e));
                        }
                    }
                }
                Command::AgentShow => {
                    let pane_id = match focused_pane_id(&state) {
                        Ok(id) => id,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                        Ok(handle) => handle,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Agent", &e.message);
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(e.message),
                            });
                        }
                    };
                    let sb = sandbox.blocking_lock();
                    let cfg = sb.config();
                    let mut lines = vec![
                        "Agent definition commands:".to_string(),
                        "  /agent set <name>".to_string(),
                        "  /agent list".to_string(),
                        "  /agent show <name>".to_string(),
                    ];
                    if let Some(resolved) = &cfg.resolved_agent {
                        let summary = if resolved.agent_name.is_empty() {
                            format!(
                                "Current config: {} skills, {} MCPs",
                                resolved.skills.len(),
                                resolved.mcp_servers.len()
                            )
                        } else {
                            format!(
                                "Current agent: {} ({} skills, {} MCPs)",
                                resolved.agent_name,
                                resolved.skills.len(),
                                resolved.mcp_servers.len()
                            )
                        };
                        lines.insert(0, summary);
                    }
                    enqueue_popup(&app, "info", "Agent", &lines.join("\n"));
                }
                Command::AgentSet { name } => {
                    let pane_id = match focused_pane_id(&state) {
                        Ok(id) => id,
                        Err(e) => return err(&e.code, &e.message, e.detail),
                    };
                    let sandbox = match sandbox_handle_for_pane(&state, pane_id) {
                        Ok(handle) => handle,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Agent", &e.message);
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(e.message),
                            });
                        }
                    };
                    let registry = match state.registry.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return err("INTERNAL", "Failed to acquire registry state", None),
                    };
                    let mut resolved = match registry.resolve_full(&name, &[]) {
                        Ok(value) => value,
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Agent", &format!("Failed to resolve agent '{}': {}", name, e));
                            return ok(InputSubmitResult {
                                kind: "command".to_string(),
                                command_name,
                                message: Some(format!("Failed to resolve agent '{}'", name)),
                            });
                        }
                    };
                    let sb = sandbox.blocking_lock();
                    resolved.auto_mode = sb.config().auto_mode;
                    resolved.permissions = sb.config().permissions;
                    resolved.agent_type = sb.config().agent_type;
                    resolved.claude_settings = sb.config().claude_settings.clone();
                    let body = match serde_json::to_string(&resolved) {
                        Ok(text) => text,
                        Err(e) => return err("INTERNAL", "Failed to serialize agent config", Some(e.to_string())),
                    };
                    match sb
                        .gateway()
                        .and_then(|gw| gw.http_post("/api/v1/agent/bootstrap", &body))
                    {
                        Ok((status, _)) if status < 400 => {
                            enqueue_popup(
                                &app,
                                "success",
                                "Agent",
                                &format!(
                                    "Agent '{}' configured ({} skills, {} MCPs).",
                                    name,
                                    resolved.skills.len(),
                                    resolved.mcp_servers.len()
                                ),
                            );
                        }
                        Ok((status, resp)) => {
                            enqueue_popup(
                                &app,
                                "warning",
                                "Agent",
                                &format!("Failed to set agent '{}' ({}): {}", name, status, resp),
                            );
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Agent", &format!("Failed to set agent '{}': {}", name, e));
                        }
                    }
                }
                Command::AgentList => {
                    let registry = match state.registry.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return err("INTERNAL", "Failed to acquire registry state", None),
                    };
                    let agents = registry.list_agents();
                    if agents.is_empty() {
                        enqueue_popup(&app, "info", "Agents", "No agents in registry.");
                    } else {
                        let mut lines = vec!["Available agents:".to_string()];
                        for entry in agents {
                            let desc = if entry.description.trim().is_empty() {
                                String::new()
                            } else {
                                format!(" - {}", entry.description.trim())
                            };
                            lines.push(format!("- {}{}", entry.name, desc));
                        }
                        enqueue_popup(&app, "info", "Agents", &lines.join("\n"));
                    }
                }
                Command::AgentInfo { name } => {
                    let registry = match state.registry.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return err("INTERNAL", "Failed to acquire registry state", None),
                    };
                    match registry.resolve_agent(&name) {
                        Ok(agent) => {
                            let mut lines = vec![format!("Agent: {}", agent.name)];
                            if !agent.description.trim().is_empty() {
                                lines.push(format!("Description: {}", agent.description.trim()));
                            }
                            if !agent.tags.is_empty() {
                                lines.push(format!("Tags: {}", agent.tags.join(", ")));
                            }
                            if !agent.skills.is_empty() {
                                lines.push(format!("Skills: {}", agent.skills.join(", ")));
                            }
                            if !agent.mcps.is_empty() {
                                let names: Vec<String> = agent.mcps.iter().map(|m| m.name.clone()).collect();
                                lines.push(format!("MCPs: {}", names.join(", ")));
                            }
                            lines.push(String::new());
                            lines.push(agent.prompt);
                            enqueue_popup(&app, "info", "Agent", &lines.join("\n"));
                        }
                        Err(e) => {
                            enqueue_popup(&app, "warning", "Agent", &format!("Agent '{}' not found: {}", name, e));
                        }
                    }
                }
            }

            ok(InputSubmitResult {
                kind: "command".to_string(),
                command_name,
                message: None,
            })
        }
        ParseResult::Err(message) => {
            enqueue_popup(&app, "error", "Command error", &message);
            ok(InputSubmitResult {
                kind: "command_error".to_string(),
                command_name: None,
                message: Some(message),
            })
        }
    }
}

#[tauri::command]
fn command_autocomplete(partial: String) -> ApiResult<Vec<String>> {
    ok(command_core::autocomplete(&partial))
}

#[tauri::command]
fn projects_list(state: State<'_, DesktopState>) -> ApiResult<Vec<ProjectEntry>> {
    let mut store = match state.recent_projects.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire recent projects store", None),
    };

    store.projects.retain(|entry| PathBuf::from(&entry.path).exists());
    if let Err(save_error) = save_recent_projects_store(&store) {
        tracing::warn!("failed to persist filtered recent projects: {}", save_error);
    }

    ok(build_project_entries(&store))
}

#[tauri::command]
fn project_add_recent(state: State<'_, DesktopState>, path: String) -> ApiResult<ProjectEntry> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return err("BAD_REQUEST", "Project path cannot be empty", None);
    }
    let pb = PathBuf::from(trimmed);
    if !pb.exists() {
        return err("BAD_REQUEST", "Project path does not exist", None);
    }
    if !pb.is_dir() {
        return err("BAD_REQUEST", "Project path must be a directory", None);
    }

    if let Err(save_error) = upsert_recent_project(&state, &pb) {
        return err("INTERNAL", "Failed to persist recent project", Some(save_error));
    }

    let store = match state.recent_projects.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire recent projects store", None),
    };
    let path_display = pb.canonicalize().unwrap_or(pb).display().to_string();
    let now = current_epoch_ms();
    let match_entry = store.projects.iter().find(|entry| entry.path == path_display);
    ok(ProjectEntry {
        id: path_display.clone(),
        name: project_label(Path::new(&path_display)),
        path: path_display,
        last_opened: match match_entry {
            Some(value) => humanize_age(now, value.last_opened_ms),
            None => "0s ago".to_string(),
        },
    })
}

#[tauri::command]
fn sessions_list(project_path: String) -> ApiResult<Vec<SessionEntry>> {
    let trimmed = project_path.trim();
    if trimmed.is_empty() {
        return ok(Vec::new());
    }
    let project = PathBuf::from(trimmed);
    let sessions = sandbox::session::Session::list(&project);
    let now = chrono::Utc::now();
    let mapped = sessions
        .into_iter()
        .map(|entry| {
            let delta = now.signed_duration_since(entry.session.updated_at).num_seconds().max(0);
            let updated = if delta < 60 {
                format!("{}s ago", delta)
            } else if delta < 3_600 {
                format!("{}m ago", delta / 60)
            } else if delta < 86_400 {
                format!("{}h ago", delta / 3_600)
            } else {
                format!("{}d ago", delta / 86_400)
            };
            SessionEntry {
                id: entry.id,
                updated,
                panels: entry.session.panels.len(),
                summary: entry.session.summary(),
            }
        })
        .collect();
    ok(mapped)
}

#[tauri::command]
fn workspace_init(
    state: State<'_, DesktopState>,
    project_path: String,
    session_id: Option<String>,
) -> ApiResult<WorkspaceContext> {
    let trimmed = project_path.trim();
    if trimmed.is_empty() {
        return err("BAD_REQUEST", "Project path cannot be empty", None);
    }
    let pb = PathBuf::from(trimmed);
    if !pb.exists() || !pb.is_dir() {
        return err("BAD_REQUEST", "Project path must be an existing directory", None);
    }

    let normalized = pb.canonicalize().unwrap_or(pb);
    if let Err(save_error) = upsert_recent_project(&state, &normalized) {
        tracing::warn!("failed to persist workspace project: {}", save_error);
    }

    let context = WorkspaceContext {
        project_path: Some(normalized.display().to_string()),
        session_id: session_id.filter(|value| !value.trim().is_empty()),
    };

    let mut workspace = match state.workspace_context.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire workspace state", None),
    };
    *workspace = context.clone();
    ok(context)
}

#[tauri::command]
fn pane_close(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
) -> ApiResult<BasicAck> {
    if let Err(api_error) = pane_close_by_id(&app, &state, pane_id) {
        return err(&api_error.code, &api_error.message, api_error.detail);
    }
    ok(BasicAck { handled: true })
}

#[tauri::command]
fn pane_open(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
) -> ApiResult<BasicAck> {
    if let Err(api_error) = pane_open_by_id(&app, &state, pane_id) {
        return err(&api_error.code, &api_error.message, api_error.detail);
    }
    ok(BasicAck { handled: true })
}

#[tauri::command]
fn pane_kill(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
) -> ApiResult<BasicAck> {
    if let Err(api_error) = pane_kill_by_id(&app, &state, pane_id) {
        return err(&api_error.code, &api_error.message, api_error.detail);
    }
    ok(BasicAck { handled: true })
}

#[tauri::command]
fn terminal_write(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    frame: TerminalWriteFrame,
) -> ApiResult<TerminalWriteAck> {
    if frame.encoding != "base64" {
        return err("BAD_REQUEST", "Unsupported terminal frame encoding", None);
    }

    let panes = match state.panes.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
    };

    if panes.iter().all(|pane| pane.pane_id != frame.pane_id) {
        return err("NOT_FOUND", "Pane not found", None);
    }

    let decoded = match base64::engine::general_purpose::STANDARD.decode(frame.data.as_bytes()) {
        Ok(value) => value,
        Err(_) => return err("BAD_REQUEST", "Invalid terminal frame payload", None),
    };

    let sent = {
        let handles = match state.terminal_handles.lock() {
            Ok(guard) => guard,
            Err(_) => return err("INTERNAL", "Failed to acquire terminal handles", None),
        };
        if let Some(handle) = handles.get(&frame.pane_id) {
            handle.write_tx.send(decoded).is_ok()
        } else {
            false
        }
    };

    if !sent {
        emit_event(
            &app,
            "app://terminal",
            TerminalEvent::TerminalDisconnected {
                pane_id: frame.pane_id,
                error: Some("No active SSH terminal for pane".to_string()),
            },
        );
        return err("SSH_DISCONNECTED", "No active SSH terminal for pane", None);
    }

    ok(TerminalWriteAck {
        accepted: true,
        seq: frame.seq,
    })
}

#[tauri::command]
fn terminal_resize(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
    cols: u16,
    rows: u16,
) -> ApiResult<TerminalResizeAck> {
    let panes = match state.panes.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire pane state", None),
    };

    if panes.iter().all(|pane| pane.pane_id != pane_id) {
        return err("NOT_FOUND", "Pane not found", None);
    }

    {
        let handles = match state.terminal_handles.lock() {
            Ok(guard) => guard,
            Err(_) => return err("INTERNAL", "Failed to acquire terminal handles", None),
        };
        if let Some(handle) = handles.get(&pane_id) {
            let _ = handle.resize_tx.send((cols, rows));
        }
    }

    emit_event(
        &app,
        "app://terminal",
        TerminalEvent::TerminalConnected { pane_id, cols, rows },
    );

    ok(TerminalResizeAck { pane_id, cols, rows })
}

#[tauri::command]
fn upload_file(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
    path: String,
) -> ApiResult<UploadStartAck> {
    match start_file_upload(&app, &state, pane_id, &path) {
        Ok(upload_id) => ok(UploadStartAck { upload_id }),
        Err(upload_error) => {
            let upload_id = uuid::Uuid::new_v4().to_string();
            emit_upload_failed(&app, &upload_id, pane_id, &upload_error.message);
            enqueue_popup(&app, "error", "Upload failed", &upload_error.message);
            err(
                &upload_error.code,
                &upload_error.message,
                upload_error.detail,
            )
        }
    }
}

#[tauri::command]
fn upload_paste_image(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    pane_id: usize,
) -> ApiResult<UploadStartAck> {
    match start_clipboard_image_upload(&app, &state, pane_id) {
        Ok(upload_id) => ok(UploadStartAck { upload_id }),
        Err(upload_error) => {
            let upload_id = uuid::Uuid::new_v4().to_string();
            emit_upload_failed(&app, &upload_id, pane_id, &upload_error.message);
            enqueue_popup(&app, "error", "Upload failed", &upload_error.message);
            err(
                &upload_error.code,
                &upload_error.message,
                upload_error.detail,
            )
        }
    }
}

#[tauri::command]
fn popup_dismiss(_popup_id: String) -> ApiResult<BasicAck> {
    ok(BasicAck { handled: true })
}

#[tauri::command]
fn popup_action(app: tauri::AppHandle, popup_id: String, action_id: String) -> ApiResult<BasicAck> {
    enqueue_popup(
        &app,
        "info",
        "Popup action",
        &format!("{} -> {}", popup_id, action_id),
    );
    ok(BasicAck { handled: true })
}

#[tauri::command]
fn theme_get(state: State<'_, DesktopState>) -> ApiResult<ThemeSnapshot> {
    let theme_name = match state.theme_name.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return err("INTERNAL", "Failed to acquire theme state", None),
    };

    ok(theme_snapshot(&theme_name))
}

#[tauri::command]
fn theme_set(state: State<'_, DesktopState>, name: String) -> ApiResult<ThemeSnapshot> {
    let mut theme_name = match state.theme_name.lock() {
        Ok(guard) => guard,
        Err(_) => return err("INTERNAL", "Failed to acquire theme state", None),
    };

    *theme_name = name;
    ok(theme_snapshot(&theme_name))
}

#[tauri::command]
fn logs_path() -> ApiResult<LogsPathAck> {
    let path = LOG_DIR
        .get()
        .cloned()
        .unwrap_or_else(desktop_logs_dir)
        .display()
        .to_string();
    ok(LogsPathAck { path })
}

#[tauri::command]
fn editor_list() -> ApiResult<Vec<EditorEntry>> {
    ok(detected_editors())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        sandbox::handle_boot_vm_subprocess();
    }

    init_logging();
    install_panic_hook();

    tauri::Builder::default()
        .setup(|app| {
            let _ = APP_HANDLE.set(app.handle().clone());
            Ok(())
        })
        .manage(DesktopState {
            panes: Mutex::new(Vec::new()),
            panel_registry: Mutex::new(PanelRegistry {
                focused_panel: 0,
                hidden_panels: HashSet::new(),
            }),
            theme_name: Mutex::new("nanosandbox".to_string()),
            terminal_seq: Mutex::new(HashMap::new()),
            terminal_handles: Mutex::new(HashMap::new()),
            sandbox_handles: Mutex::new(HashMap::new()),
            ssh_connection_info: Mutex::new(HashMap::new()),
            pane_workdirs: Mutex::new(HashMap::new()),
            pane_sandbox_identity: Mutex::new(HashMap::new()),
            pane_gitsync_override: Mutex::new(HashMap::new()),
            registry: Mutex::new(load_agents_registry()),
            persisted_env: Mutex::new(load_persisted_env_store()),
            recent_projects: Mutex::new(load_recent_projects_store()),
            workspace_context: Mutex::new(WorkspaceContext::default()),
            url_buffers: Mutex::new(HashMap::new()),
            opened_auth_keys: Mutex::new(HashSet::new()),
            zoomed_pane: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            app_bootstrap,
            projects_list,
            project_add_recent,
            sessions_list,
            workspace_init,
            pane_focus,
            pane_zoom_toggle,
            pane_close,
            pane_open,
            pane_kill,
            input_submit,
            command_autocomplete,
            terminal_write,
            terminal_resize,
            upload_file,
            upload_paste_image,
            popup_dismiss,
            popup_action,
            theme_get,
            theme_set,
            logs_path,
            editor_list
        ])
        .run(tauri::generate_context!())
        .expect("error while running nanosb desktop app");
}
