//! Nanosandbox CLI
//!
//! A command-line interface for managing VM-based sandboxes.

pub use nanosb_cli::supervisor;

mod cli {
    use clap::{Parser, Subcommand, ValueEnum};
    use colored::Colorize;
    use indicatif::{ProgressBar, ProgressStyle};
    use sandbox::{normalize_image, ImageManager, SandboxRegistry, SandboxStatus};
    use std::time::Duration;
    use tabled::{Table, Tabled};
    use tracing::{error, warn};

    use crate::supervisor::client::SupervisorClient;

    /// Output format for commands
    #[derive(Debug, Clone, Copy, ValueEnum, Default)]
    pub enum OutputFormat {
        #[default]
        Text,
        Json,
    }

    #[derive(Parser)]
    #[command(name = "nanosb")]
    #[command(about = "Nanosandbox - VM-based sandbox management", long_about = None)]
    #[command(version)]
    pub struct Cli {
        #[command(subcommand)]
        pub command: Option<Commands>,

        /// Output format (text, json)
        #[arg(long, default_value = "text", global = true)]
        pub format: OutputFormat,

        /// Verbose output
        #[arg(short, long, global = true)]
        pub verbose: bool,

        /// Project directory to mount into sandboxes
        #[arg(long, global = true)]
        pub project: Option<String>,

        /// Path to sandbox.yml config file or directory containing one.
        /// Can be specified multiple times to load from multiple configs.
        #[arg(long = "config", global = true)]
        pub configs: Vec<String>,

        /// Start only the named sandbox from the config file (instead of all).
        #[arg(long, global = true)]
        pub sandbox: Option<String>,

        /// Override CPU cores for all sandboxes from config
        #[arg(long, global = true)]
        pub cpus: Option<u32>,

        /// Override memory (MB) for all sandboxes from config
        #[arg(long, global = true)]
        pub memory: Option<u32>,

        /// Override timeout (seconds) for all sandboxes from config
        #[arg(long, global = true)]
        pub timeout: Option<u32>,

        /// Agent permission level: default, accept-edits, allow-all
        #[arg(long, global = true)]
        pub permissions: Option<String>,

        /// Environment variables (KEY=VALUE) injected into all sandboxes
        #[arg(short = 'e', long = "env", global = true)]
        pub env: Vec<String>,

        /// Read environment variables from a file (one KEY=VALUE per line)
        #[arg(long = "env-file", global = true)]
        pub env_file: Vec<String>,

        /// Resume the most recent saved session for this project
        #[arg(
            short = 'r',
            long = "resume",
            global = true,
            conflicts_with = "session"
        )]
        pub resume: bool,

        /// Resume a specific session id from history
        #[arg(long, global = true)]
        pub session: Option<String>,
    }

    #[derive(Subcommand)]
    pub enum Commands {
        /// Hidden supervisor subcommand (spawned by the CLI).
        #[command(hide = true)]
        #[clap(name = "__supervise")]
        Supervise {
            /// Sandbox name
            sandbox_name: String,
            /// JSON-serialized SandboxConfig
            config_json: String,
            /// JSON-serialized extra mounts
            #[arg(long, default_value = "[]")]
            extra_mounts_json: String,
            /// Timeout in seconds
            #[arg(long, default_value = "3600")]
            timeout_secs: u64,
        },

        /// View sandbox console logs
        Logs {
            /// Sandbox name or ID
            sandbox: String,
            /// Follow log output (like tail -f)
            #[arg(short, long)]
            follow: bool,
            /// Number of bytes to show from the end
            #[arg(long)]
            tail: Option<usize>,
        },

        /// Attach to a sandbox console
        Attach {
            /// Sandbox name or ID
            sandbox: String,
        },

        /// Apply a sandbox.yml manifest (declarative create/recreate)
        Apply {
            /// Path to sandbox.yml or a directory containing one
            #[arg(short = 'f', long = "file", default_value = "sandbox.yml")]
            file: String,
            /// Delete sandboxes that are no longer in the manifest
            #[arg(long)]
            prune: bool,
            /// Recreate sandboxes even when their config is unchanged
            #[arg(long)]
            force: bool,
            /// Show what would change without applying
            #[arg(long = "dry-run")]
            dry_run: bool,
            /// Apply only the named sandbox
            #[arg(long)]
            sandbox: Option<String>,
        },

        /// Restart a sandbox from its saved config (recreate)
        Restart {
            /// Sandbox name or ID
            sandbox: String,

            /// Restart even if env/secrets cannot be re-resolved (boots keyless)
            #[arg(long)]
            allow_no_env: bool,
        },

        /// Show a sandbox's state, config, and mounts
        Describe {
            /// Sandbox name or ID
            sandbox: String,
        },

        /// Pull an image from a registry
        Pull {
            /// Image reference (e.g., alpine:3.19, ghcr.io/user/image:tag)
            image: String,
        },

        /// List cached images
        Images,

        /// Run a command in a new sandbox
        Run {
            /// Image to use
            image: String,

            /// Name for the sandbox (optional)
            #[arg(long)]
            name: Option<String>,

            /// CPU cores to allocate
            #[arg(long, default_value = "2")]
            cpus: u32,

            /// Memory in MB
            #[arg(long, default_value = "4096")]
            memory: u32,

            /// Environment variables (KEY=VALUE)
            #[arg(short = 'e', long = "env")]
            env: Vec<String>,

            /// Read environment variables from a file
            #[arg(long = "env-file")]
            env_file: Option<String>,

            /// Forward host port to guest. Format: HOST:GUEST or PORT (same on both).
            /// Repeatable. Example: --port 1455 --port 8080:80
            #[arg(short = 'p', long = "port")]
            ports: Vec<String>,

            /// Timeout in seconds (default: 600)
            #[arg(long, default_value = "600")]
            timeout: u32,

            /// Run agent commands as root inside the sandbox VM
            #[arg(long)]
            run_as_root: bool,

            /// Run the command as this guest user (UID, UID:GID, or username)
            #[arg(long)]
            user: Option<String>,

            /// Home directory to set for the guest command (default: /home/<user>)
            #[arg(long)]
            home: Option<String>,

            /// Enable the host↔guest exec channel (vsock) for `nanosb exec`.
            ///
            /// Injects the exec agent into the guest and bridges a host socket
            /// at ~/.nanosandbox/sandboxes/<name>/exec.sock. Off by default.
            #[arg(long = "exec")]
            exec: bool,

            /// Stream the sandbox console to stdout (like tail -f)
            #[arg(short, long)]
            follow: bool,

            /// Command to run
            #[arg(trailing_var_arg = true)]
            command: Vec<String>,
        },

        /// Execute a command in a running sandbox (requires --exec at start)
        Exec {
            /// Sandbox name or ID
            sandbox: String,

            /// Run through `/bin/sh -c` (interpret shell syntax)
            #[arg(long)]
            shell: bool,

            /// Allocate a PTY (interactive programs; implies streaming)
            #[arg(short = 't', long)]
            tty: bool,

            /// Stream output as it is produced
            #[arg(short, long)]
            follow: bool,

            /// Command to run
            #[arg(trailing_var_arg = true)]
            command: Vec<String>,
        },

        /// List sandboxes
        Ps {
            /// Show all sandboxes (including stopped)
            #[arg(short, long)]
            all: bool,
        },

        /// Stop a running sandbox
        Stop {
            /// Sandbox ID or name
            sandbox: String,
        },

        /// Remove a sandbox
        #[command(alias = "delete")]
        Rm {
            /// Sandbox ID or name
            sandbox: String,

            /// Force removal (stop if running)
            #[arg(short, long)]
            force: bool,
        },

        /// Check runtime prerequisites
        Doctor,

        /// Clean up stale project clones and list nanosb branches
        Cleanup {
            /// Project directory (defaults to current directory)
            #[arg(long)]
            project: Option<String>,
        },

        /// List saved nanosb sessions for a project
        Sessions {
            /// Project directory (defaults to current directory)
            #[arg(long)]
            project: Option<String>,
        },

        /// Manage the image and blob cache
        Cache {
            #[command(subcommand)]
            action: CacheAction,
        },

        /// List registered projects
        Projects,

        /// Remove a project from the registry
        ProjectsForget {
            /// Path to the project to forget
            path: String,
        },
    }

    #[derive(Subcommand)]
    pub enum CacheAction {
        /// Remove unused cache data to reclaim disk space
        Prune {
            /// Remove ALL cached data including blobs (full cache reset)
            #[arg(long)]
            all: bool,
        },
    }

    /// Image info for table display
    #[derive(Tabled)]
    struct ImageRow {
        #[tabled(rename = "REPOSITORY")]
        repository: String,
        #[tabled(rename = "TAG")]
        tag: String,
        #[tabled(rename = "SIZE")]
        size: String,
        #[tabled(rename = "PULLED")]
        pulled: String,
    }

    /// Sandbox info for table display
    #[derive(Tabled)]
    struct SandboxRow {
        #[tabled(rename = "ID")]
        id: String,
        #[tabled(rename = "NAME")]
        name: String,
        #[tabled(rename = "IMAGE")]
        image: String,
        #[tabled(rename = "STATUS")]
        status: String,
        #[tabled(rename = "CREATED")]
        created: String,
    }

    /// Supervised (next-mode) sandbox info for table display.
    struct SupervisedSandboxRow {
        name: String,
        image: String,
        status: String,
        created: String,
    }

    /// Saved session info for table display
    #[derive(Tabled)]
    struct SessionRow {
        #[tabled(rename = "ID")]
        id: String,
        #[tabled(rename = "UPDATED_AT")]
        updated_at: String,
        #[tabled(rename = "UPDATED")]
        updated: String,
        #[tabled(rename = "PANELS")]
        panels: usize,
        #[tabled(rename = "SUMMARY")]
        summary: String,
    }

    /// Format bytes to human readable
    fn format_bytes(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes >= GB {
            format!("{:.1} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.1} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.1} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} B", bytes)
        }
    }

    /// Format duration to human readable
    fn format_duration(duration: chrono::Duration) -> String {
        let seconds = duration.num_seconds();
        if seconds < 60 {
            format!("{} seconds ago", seconds)
        } else if seconds < 3600 {
            format!("{} minutes ago", seconds / 60)
        } else if seconds < 86400 {
            format!("{} hours ago", seconds / 3600)
        } else {
            format!("{} days ago", seconds / 86400)
        }
    }

    /// Create a progress bar for image pulling
    fn create_pull_progress() -> ProgressBar {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    }

    /// Run the CLI
    pub async fn run() -> anyhow::Result<()> {
        let cli = Cli::parse();

        // Initialize logging:
        // - File: always writes to ~/.nanosandbox/logs/nanosb.YYYY-MM-DD (daily rotation)
        // - File level: WARN by default, override with NANOSB_LOG=debug|info|trace
        // - Stderr: only when --verbose (always debug level)
        // - Old logs (>7 days) are cleaned up on startup
        // The _log_guard must be held alive for the duration of the program.
        let _log_guard: Option<tracing_appender::non_blocking::WorkerGuard>;

        {
            use tracing_subscriber::layer::SubscriberExt;
            use tracing_subscriber::util::SubscriberInitExt;
            use tracing_subscriber::{fmt, EnvFilter, Layer};

            let logs_dir = logs_dir();
            let file_setup = if let Err(e) = std::fs::create_dir_all(&logs_dir) {
                eprintln!(
                    "Warning: could not create logs directory {}: {}",
                    logs_dir.display(),
                    e
                );
                None
            } else {
                cleanup_old_logs(&logs_dir, 7);
                let file_appender = tracing_appender::rolling::daily(&logs_dir, "nanosb");
                let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
                Some((non_blocking, guard))
            };

            if let Some((file_writer, guard)) = file_setup {
                _log_guard = Some(guard);

                // In TUI mode (cli.command is None) we can't write to stderr —
                // it corrupts the ratatui alternate screen. So default to info
                // there to capture the SSE/exec diagnostics. For command mode,
                // keep warn so we don't spam the file unnecessarily.
                let default_level = if cli.command.is_none() {
                    "info"
                } else {
                    "warn"
                };
                let file_level = std::env::var("NANOSB_LOG")
                    .ok()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| default_level.to_string());
                // Use a global default level so that ALL crates (including
                // nanosb_cli) are captured.  Previous per-crate filters

                let file_filter = file_level.clone();
                let file_layer = fmt::layer()
                    .with_writer(file_writer)
                    .with_ansi(false)
                    .with_filter(EnvFilter::new(&file_filter));

                // Only attach a stderr layer when there's a command AND --verbose.
                // TUI must never write to stderr (it breaks the alternate screen).
                if cli.command.is_some() && cli.verbose {
                    let stderr_layer =
                        fmt::layer()
                            .with_writer(std::io::stderr)
                            .with_filter(EnvFilter::new(
                                "runtime=debug,nanosb_cli=debug,nanosb=debug,sandbox=debug",
                            ));

                    tracing_subscriber::registry()
                        .with(file_layer)
                        .with(stderr_layer)
                        .init();
                } else {
                    tracing_subscriber::registry().with(file_layer).init();
                }
            } else {
                _log_guard = None;
                if cli.command.is_some() && cli.verbose {
                    tracing_subscriber::fmt()
                        .with_env_filter(
                            "runtime=debug,nanosb_cli=debug,nanosb=debug,sandbox=debug",
                        )
                        .init();
                } else {
                    let _ = env_logger::Builder::from_env(
                        env_logger::Env::default().default_filter_or("off"),
                    )
                    .try_init();
                }
            }
        }

        match cli.command {
            None => {
                // Collect config file paths.
                let mut config_paths: Vec<std::path::PathBuf> =
                    cli.configs.iter().map(std::path::PathBuf::from).collect();

                // Auto-detect sandbox.yml in CWD.
                let cwd = std::env::current_dir()?;
                if sandbox::find_sandbox_file(&cwd).is_some() && !config_paths.contains(&cwd) {
                    config_paths.insert(0, cwd.clone());
                }

                // Also auto-detect sandbox.yml in --project path.
                if let Some(ref project) = cli.project {
                    let project_dir = std::path::PathBuf::from(project);
                    if sandbox::find_sandbox_file(&project_dir).is_some()
                        && !config_paths.contains(&project_dir)
                    {
                        config_paths.push(project_dir);
                    }
                }

                // Load and resolve sandbox configs.
                let mut sandbox_configs = if config_paths.is_empty() {
                    Vec::new()
                } else {
                    sandbox::load_sandbox_files(&config_paths)
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                };

                // Build runtime-only env pool for this invocation.
                // Source order: auto .env, then --env-file, then --env.
                let mut runtime_env: Vec<(String, String)> = Vec::new();

                // Auto-load .env from CWD when present.
                if let Ok(cwd) = std::env::current_dir() {
                    let env_path = cwd.join(".env");
                    if env_path.exists() {
                        let auto_env_files = vec![env_path.to_string_lossy().to_string()];
                        let auto_env = parse_env_vars(&[], &auto_env_files)?;
                        runtime_env.extend(auto_env);
                    }
                }

                // Parse --env and --env-file into key-value pairs.
                // These are runtime-only for TUI mode and never persisted.
                let cli_env = parse_env_vars(&cli.env, &cli.env_file)?;
                runtime_env.extend(cli_env.iter().cloned());

                // Parse --permissions flag.
                let cli_permissions = cli
                    .permissions
                    .as_deref()
                    .map(|s| s.parse::<sandbox::Permissions>())
                    .transpose()
                    .map_err(|e| {
                        error!("Failed to parse --permissions flag: {}", e);
                        anyhow::anyhow!("{}", e)
                    })?;

                // Apply CLI flag overrides (merge step 4).
                sandbox::apply_cli_overrides(
                    &mut sandbox_configs,
                    cli.cpus,
                    cli.memory,
                    cli.timeout,
                    cli_permissions,
                    &[],
                );

                let mut runtime_env_pool = std::collections::HashMap::new();
                for (k, v) in runtime_env {
                    runtime_env_pool.insert(k, v);
                }

                // Filter to a single sandbox if --sandbox is specified.
                let sandbox_configs = if let Some(ref name) = cli.sandbox {
                    let filtered: Vec<_> = sandbox_configs
                        .into_iter()
                        .filter(|(key, config)| key == name || config.sandbox.name == *name)
                        .collect();
                    if filtered.is_empty() {
                        error!("Sandbox '{}' not found in config files", name);
                        anyhow::bail!("Sandbox '{}' not found in config files", name);
                    }
                    filtered
                } else {
                    sandbox_configs
                };

                // Always use CWD as project path so non-git directories get session
                // persistence and project mounting (git is initialised in source on first use).
                let project_path = cli
                    .project
                    .map(std::path::PathBuf::from)
                    .or_else(|| std::env::current_dir().ok());

                let session_start = if let Some(session_id) = cli.session {
                    nanosb_cli::tui::run::SessionStartMode::ResumeById(session_id)
                } else if cli.resume {
                    nanosb_cli::tui::run::SessionStartMode::ResumeLatest
                } else {
                    nanosb_cli::tui::run::SessionStartMode::Fresh
                };

                nanosb_cli::tui::run::run_tui(
                    project_path,
                    sandbox_configs,
                    session_start,
                    runtime_env_pool,
                )
                .await
            }
            Some(Commands::Supervise { .. }) => {
                anyhow::bail!(
                    "__supervise must be invoked directly (handled before runtime startup)"
                )
            }
            Some(Commands::Logs {
                sandbox,
                follow,
                tail,
            }) => cmd_logs(&sandbox, follow, tail, cli.verbose).await,
            Some(Commands::Attach { sandbox }) => cmd_attach(&sandbox).await,
            Some(Commands::Apply {
                file,
                prune,
                force,
                dry_run,
                sandbox,
            }) => {
                cmd_apply(
                    &file,
                    prune,
                    force,
                    dry_run,
                    sandbox.as_deref(),
                    cli.format,
                    cli.verbose,
                )
                .await
            }
            Some(Commands::Restart {
                sandbox,
                allow_no_env,
            }) => cmd_restart(&sandbox, allow_no_env, cli.verbose).await,
            Some(Commands::Describe { sandbox }) => cmd_describe(&sandbox).await,
            Some(Commands::Pull { image }) => cmd_pull(&image, cli.format, cli.verbose).await,
            Some(Commands::Images) => cmd_images(cli.format).await,
            Some(Commands::Run {
                image,
                name,
                cpus,
                memory,
                env,
                env_file,
                ports,
                timeout,
                run_as_root,
                user,
                home,
                exec,
                follow,
                command,
            }) => {
                let port_pairs = parse_port_specs(&ports)?;
                let user = user.or_else(|| run_as_root.then(|| "0".to_string()));
                cmd_run(
                    &image,
                    name,
                    cpus,
                    memory,
                    &env,
                    env_file.as_deref(),
                    &port_pairs,
                    timeout,
                    user.as_deref(),
                    home.as_deref(),
                    exec,
                    follow,
                    &command,
                    cli.format,
                    cli.verbose,
                )
                .await
            }
            Some(Commands::Exec {
                sandbox,
                shell,
                tty,
                follow,
                command,
            }) => cmd_exec(&sandbox, shell, tty, follow, &command).await,
            Some(Commands::Ps { all }) => cmd_ps(all, cli.format).await,
            Some(Commands::Stop { sandbox }) => cmd_stop(&sandbox, cli.verbose).await,
            Some(Commands::Rm { sandbox, force }) => cmd_rm(&sandbox, force, cli.verbose).await,
            Some(Commands::Doctor) => cmd_doctor(cli.format).await,
            Some(Commands::Cleanup { project }) => cmd_cleanup(project.as_deref()).await,
            Some(Commands::Sessions { project }) => {
                cmd_sessions(project.as_deref(), cli.format).await
            }
            Some(Commands::Cache { action }) => match action {
                CacheAction::Prune { all } => cmd_cache_prune(all, cli.format).await,
            },
            Some(Commands::Projects) => cmd_projects(cli.format).await,
            Some(Commands::ProjectsForget { path }) => {
                cmd_projects_forget(&path, cli.format).await
            }
        }
    }

    /// Pull an image from a registry
    /// View sandbox console logs (works for running and stopped sandboxes).
    async fn cmd_logs(
        sandbox_name: &str,
        follow: bool,
        tail: Option<usize>,
        verbose: bool,
    ) -> anyhow::Result<()> {
        use std::io::{Read, Seek, SeekFrom, Write};

        let client = SupervisorClient::new(sandbox_name);
        let log_path = client.console_log_path();

        if !log_path.exists() {
            anyhow::bail!(
                "no console log for '{}' (expected {})",
                sandbox_name,
                log_path.display()
            );
        }

        let initial = match (follow, tail) {
            (_, Some(n)) => client.read_log_tail(n),
            (true, None) => client.read_log_tail(64 * 1024),
            (false, None) => client.read_log_file(),
        }
        .map_err(|e| anyhow::anyhow!("{}", e))?;
        print!("{}", initial);
        std::io::stdout().flush().ok();

        if !follow {
            return Ok(());
        }
        if verbose {
            eprintln!("(following {} — Ctrl-C to stop)", log_path.display());
        }

        let mut offset = std::fs::metadata(&log_path)?.len();
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let len = match std::fs::metadata(&log_path) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if len < offset {
                // Rotated or truncated: restart from the beginning of the new file.
                offset = 0;
            }
            if len > offset {
                let mut file = std::fs::File::open(&log_path)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                offset = len;
                print!("{}", String::from_utf8_lossy(&buf));
                std::io::stdout().flush().ok();
            }
        }
    }

    /// Attach to a sandbox console (replay + follow until exit).
    async fn cmd_attach(sandbox_name: &str) -> anyhow::Result<()> {
        use crate::supervisor::client::AttachConnection;
        use std::io::{Read, Write};

        let client = SupervisorClient::new(sandbox_name);
        if !client.is_running() {
            anyhow::bail!(
                "sandbox '{}' is not running (no control socket at {})",
                sandbox_name,
                client.control_socket_path().display()
            );
        }

        let mut conn = AttachConnection::open(&client).map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut writer = conn.writer_clone().map_err(|e| anyhow::anyhow!("{}", e))?;
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let frame =
                            serde_json::json!({ "type": "input", "data": data }).to_string();
                        let mut payload = frame.into_bytes();
                        payload.push(b'\n');
                        if writer.write_all(&payload).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        while let Some(frame) = conn.next_frame().map_err(|e| anyhow::anyhow!("{}", e))? {
            match frame {
                crate::supervisor::AttachFrame::Output { data } => {
                    print!("{}", data);
                    std::io::stdout().flush().ok();
                }
                crate::supervisor::AttachFrame::Exit { code } => {
                    if code != 0 {
                        eprintln!("\n[console exited with code {}]", code);
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    async fn cmd_pull(image: &str, format: OutputFormat, verbose: bool) -> anyhow::Result<()> {
        let image = normalize_image(image);
        let pb = create_pull_progress();
        pb.set_message(format!("Pulling {}", image));

        let manager = ImageManager::with_default_cache_and_auth()?;

        if verbose {
            pb.set_message("Connecting to registry...".to_string());
        }

        let pulled = manager.pull(&image).await?;
        pb.finish_and_clear();

        match format {
            OutputFormat::Text => {
                println!(
                    "{} Pulled {} ({} layers, {})",
                    "✓".green(),
                    image.bold(),
                    pulled.layers.len(),
                    format_bytes(pulled.size)
                );
            }
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "image": image,
                    "layers": pulled.layers.len(),
                    "size": pulled.size,
                    "digest": pulled.config_digest,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
        }

        Ok(())
    }

    /// List cached images
    async fn cmd_images(format: OutputFormat) -> anyhow::Result<()> {
        let manager = ImageManager::with_default_cache()?;
        let images = manager.list().await?;

        match format {
            OutputFormat::Text => {
                if images.is_empty() {
                    println!("No images cached. Use 'nanosb pull <image>' to pull an image.");
                    return Ok(());
                }

                let rows: Vec<ImageRow> = images
                    .iter()
                    .map(|img| {
                        let duration = chrono::Utc::now() - img.pulled_at;
                        ImageRow {
                            repository: img.reference.repository.clone(),
                            tag: img.reference.tag.clone(),
                            size: format_bytes(img.size),
                            pulled: format_duration(duration),
                        }
                    })
                    .collect();

                let table = Table::new(rows).to_string();
                println!("{}", table);
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&images)?);
            }
        }

        Ok(())
    }

    /// Parse environment variables from --env flags and --env-file(s).
    fn parse_env_vars(
        env_args: &[String],
        env_files: &[String],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut vars = Vec::new();

        // Parse --env-file(s) first (later files override earlier ones)
        for path in env_files {
            let content = std::fs::read_to_string(path).map_err(|e| {
                error!("Failed to read env file '{}': {}", path, e);
                anyhow::anyhow!("Failed to read env file '{}': {}", path, e)
            })?;
            for line in content.lines() {
                let line = line.trim();
                // Skip empty lines and comments
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    vars.push((key.trim().to_string(), value.trim().to_string()));
                }
            }
        }

        // Parse --env KEY=VALUE flags (override env-file)
        for entry in env_args {
            if let Some((key, value)) = entry.split_once('=') {
                vars.push((key.to_string(), value.to_string()));
            } else {
                // If just KEY is provided, try to read from host environment
                if let Ok(value) = std::env::var(entry) {
                    vars.push((entry.to_string(), value));
                } else {
                    error!("Environment variable '{}' not found", entry);
                    anyhow::bail!(
                        "Environment variable '{}' not found. Use KEY=VALUE format.",
                        entry
                    );
                }
            }
        }

        Ok(vars)
    }

    fn sha256_hex(data: &str) -> String {
        nanosb_cli::deploy::sha256_hex(data)
    }

    async fn wait_supervisor_stopped(
        client: &crate::supervisor::client::SupervisorClient,
        timeout_secs: u64,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            match client.read_state() {
                None => return,
                Some(state)
                    if matches!(
                        state.state,
                        crate::supervisor::SandboxState::Stopped
                            | crate::supervisor::SandboxState::Error
                    ) =>
                {
                    return;
                }
                _ => {}
            }
            if std::time::Instant::now() > deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    fn supervisor_sandbox_dirs() -> Vec<std::path::PathBuf> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let base = std::path::PathBuf::from(home).join(".nanosandbox/sandboxes");
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.push(path);
                }
            }
        }
        out
    }

    fn spawn_supervisor(
        sandbox_name: &str,
        config_json: &str,
        extra_mounts_json: &str,
        boot_env_json: &str,
        origin_json: &str,
        timeout_secs: u32,
    ) -> anyhow::Result<()> {
        nanosb_cli::deploy::spawn_supervisor(
            sandbox_name,
            config_json,
            extra_mounts_json,
            boot_env_json,
            origin_json,
            timeout_secs,
        )
    }

    async fn wait_supervisor_running(
        client: &crate::supervisor::client::SupervisorClient,
        timeout_secs: u64,
    ) -> anyhow::Result<()> {
        nanosb_cli::deploy::wait_supervisor_running(client, timeout_secs).await
    }

    fn materialize_plan(
        sandbox_dir: &std::path::Path,
        plan: &sandbox::deploy::DeployPlan,
    ) -> anyhow::Result<()> {
        nanosb_cli::deploy::materialize_plan(sandbox_dir, plan)
    }

    fn extra_mounts_from_plan(
        plan: &sandbox::deploy::DeployPlan,
    ) -> Vec<runtime::config::ExtraMount> {
        nanosb_cli::deploy::extra_mounts_from_plan(plan)
    }

    fn deploy_plan_for(
        config: &sandbox::AgentSandboxConfig,
        sandbox_dir: &std::path::Path,
    ) -> (sandbox::deploy::DeployPlan, runtime::config::SandboxConfig) {
        nanosb_cli::deploy::deploy_plan_for(config, sandbox_dir)
    }

    async fn cmd_apply(
        file: &str,
        prune: bool,
        force: bool,
        dry_run: bool,
        only: Option<&str>,
        format: OutputFormat,
        verbose: bool,
    ) -> anyhow::Result<()> {
        use crate::supervisor::client::SupervisorClient;
        use std::collections::HashSet;
        use std::path::PathBuf;

        let path = PathBuf::from(file);
        let path = if path.is_dir() {
            sandbox::find_sandbox_file(&path)
                .ok_or_else(|| anyhow::anyhow!("no sandbox.yml found in {}", path.display()))?
        } else {
            path
        };
        if !path.exists() {
            anyhow::bail!("manifest not found: {}", path.display());
        }

        let configs =
            sandbox::load_sandbox_files(&[path.clone()]).map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut results: Vec<(String, String, String)> = Vec::new();
        let mut any_error = false;

        for (_key, config) in &configs {
            let name = config.sandbox.name.clone();
            if let Some(only) = only {
                if name != only {
                    continue;
                }
            }

            let sandbox_dir = SupervisorClient::new(&name).sandbox_dir().to_path_buf();
            let (plan, rc) = deploy_plan_for(config, &sandbox_dir);
            let config_json_full = serde_json::to_string(&rc)?;
            let desired_hash = sha256_hex(&config_json_full);
            let (config_json, boot_env_json) =
                nanosb_cli::deploy::extract_boot_env(&config_json_full)?;
            let extra_mounts_json = serde_json::to_string(&extra_mounts_from_plan(&plan))?;

            let client = SupervisorClient::new(&name);
            let state = client.read_state();
            let running = client.is_running();

            let action = if state.is_none() {
                "created"
            } else if force {
                "recreated"
            } else if state.as_ref().map(|s| s.config_hash.as_str()) != Some(desired_hash.as_str())
            {
                "recreated"
            } else if running {
                "unchanged"
            } else {
                "started"
            };

            if verbose {
                eprintln!("apply: {} -> {}", name, action);
            }

            if dry_run {
                results.push((name, action.to_string(), "dry-run".to_string()));
                continue;
            }

            let outcome: anyhow::Result<()> = if action == "unchanged" {
                Ok(())
            } else {
                let mut outcome: anyhow::Result<()> = Ok(());
                if running {
                    if let Err(e) = client.stop(true) {
                        outcome = Err(anyhow::anyhow!("{}", e));
                    }
                    wait_supervisor_stopped(&client, 20).await;
                }
                if outcome.is_ok() && action != "started" {
                    outcome = materialize_plan(&sandbox_dir, &plan);
                }
                if outcome.is_ok() {
                    let origin = nanosb_cli::deploy::Origin::manifest(
                        Some(path.to_string_lossy().to_string()),
                        rc.env.keys().cloned().collect(),
                    );
                    outcome = spawn_supervisor(
                        &name,
                        &config_json,
                        &extra_mounts_json,
                        &boot_env_json,
                        &origin.to_json(),
                        rc.timeout_secs,
                    );
                }
                if outcome.is_ok() {
                    outcome = wait_supervisor_running(&client, 60).await;
                }
                outcome
            };

            match outcome {
                Ok(()) => results.push((name, action.to_string(), rc.image.clone())),
                Err(e) => {
                    any_error = true;
                    results.push((name, "error".to_string(), e.to_string()));
                }
            }
        }

        if prune {
            let declared: HashSet<String> = configs
                .iter()
                .map(|(_, c)| c.sandbox.name.clone())
                .collect();
            for dir in supervisor_sandbox_dirs() {
                let Some(name) = dir.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    continue;
                };
                if declared.contains(&name) || !dir.join("config.json").exists() {
                    continue;
                }
                if dry_run {
                    results.push((name, "deleted".to_string(), "dry-run".to_string()));
                    continue;
                }
                let client = SupervisorClient::new(&name);
                if client.is_running() {
                    let _ = client.stop(true);
                    wait_supervisor_stopped(&client, 20).await;
                }
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => results.push((name, "deleted".to_string(), String::new())),
                    Err(e) => {
                        any_error = true;
                        results.push((name, "error".to_string(), e.to_string()));
                    }
                }
            }
        }

        match format {
            OutputFormat::Text => {
                for (name, action, detail) in &results {
                    println!("{:<10} {:<24} {}", action, name, detail);
                }
            }
            OutputFormat::Json => {
                let json: Vec<_> = results
                    .iter()
                    .map(|(name, action, detail)| {
                        serde_json::json!({ "name": name, "action": action, "detail": detail })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
        }

        if any_error {
            anyhow::bail!("one or more sandboxes failed to apply");
        }
        Ok(())
    }

    async fn cmd_restart(
        sandbox_id: &str,
        allow_no_env: bool,
        verbose: bool,
    ) -> anyhow::Result<()> {
        use crate::supervisor::client::SupervisorClient;
        use nanosb_cli::deploy::Origin;

        let client = SupervisorClient::new(sandbox_id);
        let dir = client.sandbox_dir().to_path_buf();
        if !dir.exists() {
            anyhow::bail!("sandbox not found: {}", sandbox_id);
        }
        let deploy_raw = std::fs::read_to_string(dir.join("deploy.json")).map_err(|_| {
            anyhow::anyhow!(
                "sandbox '{}' has no deploy.json (not supervisor-managed)",
                sandbox_id
            )
        })?;
        let deploy: serde_json::Value = serde_json::from_str(&deploy_raw)?;
        let config_json = deploy["config_json"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let extra_mounts_json = deploy["extra_mounts_json"]
            .as_str()
            .unwrap_or("[]")
            .to_string();
        let timeout_secs = serde_json::from_str::<runtime::config::SandboxConfig>(&config_json)
            .map(|c| c.timeout_secs)
            .unwrap_or(600);

        let origin = std::fs::read_to_string(dir.join("origin.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Origin>(&raw).ok())
            .unwrap_or_default();

        let boot_env_json = if allow_no_env {
            match resolve_boot_env(&origin) {
                Ok(Some(env)) => env,
                _ => "{}".to_string(),
            }
        } else {
            match resolve_boot_env(&origin)? {
                Some(env) => env,
                None => anyhow::bail!(
                    "cannot restart '{}': its env/secrets cannot be re-resolved \
                     (source: {}). Pass --allow-no-env to boot without them.",
                    sandbox_id,
                    if origin.source.is_empty() {
                        "unknown (pre-dates origin tracking)"
                    } else {
                        origin.source.as_str()
                    }
                ),
            }
        };

        if verbose {
            eprintln!("Restarting sandbox '{}'", sandbox_id);
        }
        if client.is_running() {
            let _ = client.stop(true);
            wait_supervisor_stopped(&client, 20).await;
        }
        let _ = std::fs::remove_file(dir.join("state.json"));
        let _ = std::fs::remove_file(dir.join("config.json"));
        let _ = std::fs::remove_dir_all(dir.join("logs"));

        spawn_supervisor(
            sandbox_id,
            &config_json,
            &extra_mounts_json,
            &boot_env_json,
            &origin.to_json(),
            timeout_secs,
        )?;
        wait_supervisor_running(&client, 60).await?;
        println!("{} Restarted {}", "✓".green(), sandbox_id.bold());
        Ok(())
    }

    /// Re-resolve a sandbox's boot env from its recorded origin.
    ///
    /// Returns `None` when the origin is unknown/pre-dates tracking. Returns an
    /// error when a key the sandbox needs is no longer available on the host.
    fn resolve_boot_env(
        origin: &nanosb_cli::deploy::Origin,
    ) -> anyhow::Result<Option<String>> {
        if origin.source.is_empty() {
            return Ok(None);
        }

        let mut env: Vec<(String, String)> = Vec::new();

        if origin.source == "cli" {
            for path in &origin.env_files {
                let files = vec![path.clone()];
                env.extend(parse_env_vars(&[], &files)?);
            }
            let mut missing: Vec<String> = Vec::new();
            for key in &origin.env_keys {
                match std::env::var(key) {
                    Ok(val) => {
                        if !env.iter().any(|(k, _)| k == key) {
                            env.push((key.clone(), val));
                        }
                    }
                    Err(_) => {
                        if !env.iter().any(|(k, _)| k == key) {
                            missing.push(key.clone());
                        }
                    }
                }
            }
            if !missing.is_empty() {
                anyhow::bail!(
                    "cannot re-resolve env for restart; not set on host: {}",
                    missing.join(", ")
                );
            }
        }

        let map: std::collections::HashMap<String, String> = env.into_iter().collect();
        Ok(Some(
            serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string()),
        ))
    }

    async fn cmd_describe(sandbox_id: &str) -> anyhow::Result<()> {
        use crate::supervisor::client::SupervisorClient;

        let client = SupervisorClient::new(sandbox_id);
        let dir = client.sandbox_dir().to_path_buf();
        if !dir.exists() {
            anyhow::bail!("sandbox not found: {}", sandbox_id);
        }

        println!("Name:      {}", sandbox_id);
        if let Some(state) = client.read_state() {
            println!("State:     {:?}", state.state);
            println!("PID:       {:?}", state.pid);
            println!("Exit code: {:?}", state.exit_code);
            println!("Started:   {}", state.started_at);
            println!("Config:    {}", state.config_hash);
        }

        if let Ok(raw) = std::fs::read_to_string(dir.join("config.json")) {
            if let Ok(rc) = serde_json::from_str::<runtime::config::SandboxConfig>(&raw) {
                println!("Image:     {}", rc.image);
                println!("CPUs:      {}", rc.cpus);
                println!("Memory:    {} MB", rc.memory_mb);
                println!("Mode:      {:?}", rc.runtime_mode);
                if let Some(cmd) = &rc.command {
                    println!("Command:   {} {}", cmd, rc.command_args.join(" "));
                }
            }
        }

        if let Ok(raw) = std::fs::read_to_string(dir.join("deploy.json")) {
            if let Ok(deploy) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(mounts_json) = deploy["extra_mounts_json"].as_str() {
                    if let Ok(mounts) =
                        serde_json::from_str::<Vec<runtime::config::ExtraMount>>(mounts_json)
                    {
                        for mount in mounts {
                            println!(
                                "Mount:     {} -> {} (ro={})",
                                mount.host_path, mount.target, mount.readonly
                            );
                        }
                    }
                }
            }
        }

        let console_log = dir.join("logs").join("console.log");
        if console_log.exists() {
            println!("Logs:      {}", console_log.display());
        }
        Ok(())
    }

    async fn cmd_run_next(
        sandbox_name: &str,
        image: &str,
        cpus: u32,
        memory: u32,
        env_vars: &[(String, String)],
        env_files: &[String],
        ports: &[(u16, u16)],
        timeout: u32,
        user: Option<&str>,
        home: Option<&str>,
        exec: bool,
        follow: bool,
        command: &[String],
        format: OutputFormat,
        verbose: bool,
    ) -> anyhow::Result<()> {
        use crate::supervisor::client::SupervisorClient;

        let image = normalize_image(image);
        let mut builder = runtime::config::SandboxConfig::builder()
            .name(sandbox_name)
            .image(&image)
            .cpus(cpus)
            .memory_mb(memory)
            .timeout_secs(timeout)
            .runtime_mode(runtime::config::RuntimeMode::Next);
        if let Some(u) = user {
            builder = builder.user(u);
        }
        if let Some(h) = home {
            builder = builder.home(h);
        }
        for (key, value) in env_vars {
            builder = builder.env(key, value);
        }
        let mut config = builder.build();
        for (host, guest) in ports {
            config
                .network
                .port_mappings
                .push(runtime::config::PortMapping::tcp(*host, *guest));
        }
        if !command.is_empty() {
            config.command = Some(command[0].clone());
            config.command_args = command[1..].to_vec();
        }

        // Optional exec channel: stage the guest agent and bridge a host vsock
        // socket so `nanosb exec` can run commands in this sandbox.
        let mut extra_mounts: Vec<runtime::config::ExtraMount> = Vec::new();
        if exec {
            let sandbox_dir = SupervisorClient::new(sandbox_name).sandbox_dir().to_path_buf();
            let agent_dir = sandbox_dir.join("agent");
            let guest_path = nanosb_cli::deploy::stage_exec_agent(&agent_dir)?;
            let exec_sock = sandbox_dir.join("exec.sock");
            let _ = std::fs::remove_file(&exec_sock);
            extra_mounts.push(runtime::config::ExtraMount {
                tag: "agent".to_string(),
                host_path: agent_dir.to_string_lossy().to_string(),
                target: "/agent".to_string(),
                readonly: false,
            });
            config.vsock_socket = Some(exec_sock.to_string_lossy().to_string());
            config.vsock_port = Some(nanosb_cli::deploy::EXEC_VSOCK_PORT);
            // With no user command, PID 1 is the agent itself; otherwise the
            // agent runs alongside and exec is unused for this run.
            if command.is_empty() {
                config.command = Some(guest_path);
                config.command_args =
                    vec![nanosb_cli::deploy::EXEC_VSOCK_PORT.to_string()];
            }
        }
        let extra_mounts_json = serde_json::to_string(&extra_mounts)?;

        let config_json = serde_json::to_string(&config)?;
        let (config_json, boot_env_json) = nanosb_cli::deploy::extract_boot_env(&config_json)?;

        let origin = nanosb_cli::deploy::Origin::cli(
            env_vars.iter().map(|(k, _)| k.clone()).collect(),
            env_files.to_vec(),
        );

        let json_mode = matches!(format, OutputFormat::Json);
        if json_mode && follow {
            eprintln!(
                "warning: --follow is ignored in json mode (output captured in \"output\")"
            );
        }
        spawn_supervisor(
            sandbox_name,
            &config_json,
            &extra_mounts_json,
            &boot_env_json,
            &origin.to_json(),
            timeout,
        )?;
        let client = SupervisorClient::new(sandbox_name);
        wait_supervisor_running(&client, 60).await?;
        if !json_mode {
            println!("{} Sandbox {} started", "✓".green(), sandbox_name.bold());
        }

        let log_path = client.console_log_path();
        let mut offset = 0u64;
        let mut captured = String::new();
        let mut exit_code: i32 = 0;
        loop {
            if let Ok(meta) = std::fs::metadata(&log_path) {
                let len = meta.len();
                if len < offset {
                    offset = 0;
                }
                if len > offset {
                    use std::io::{Read, Seek, SeekFrom, Write};
                    if let Ok(mut f) = std::fs::File::open(&log_path) {
                        let _ = f.seek(SeekFrom::Start(offset));
                        let mut buf = Vec::new();
                        let _ = f.read_to_end(&mut buf);
                        offset = len;
                        let chunk = String::from_utf8_lossy(&buf).to_string();
                        if follow && !json_mode {
                            print!("{}", chunk);
                            std::io::stdout().flush().ok();
                        } else {
                            captured.push_str(&chunk);
                        }
                    }
                }
            }
            if let Some(state) = client.read_state() {
                if matches!(
                    state.state,
                    crate::supervisor::SandboxState::Stopped
                        | crate::supervisor::SandboxState::Error
                ) {
                    if let Some(code) = state.exit_code {
                        exit_code = code;
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if json_mode {
            let json = serde_json::json!({
                "sandbox": sandbox_name,
                "image": image,
                "exit_code": exit_code,
                "output": captured,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        if verbose {
            eprintln!("sandbox '{}' finished", sandbox_name);
        }
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        Ok(())
    }

    /// Parse `--port` specs into (host, guest) pairs. Format: "HOST:GUEST" or "PORT" (same).
    fn parse_port_specs(specs: &[String]) -> anyhow::Result<Vec<(u16, u16)>> {
        let mut out = Vec::with_capacity(specs.len());
        for s in specs {
            let s = s.trim();
            if s.is_empty() {
                continue;
            }
            let pair = if let Some((h, g)) = s.split_once(':') {
                let host: u16 = h
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid host port in '{}'", s))?;
                let guest: u16 = g
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid guest port in '{}'", s))?;
                (host, guest)
            } else {
                let p: u16 = s
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid port '{}'", s))?;
                (p, p)
            };
            out.push(pair);
        }
        Ok(out)
    }

    async fn cmd_run(
        image: &str,
        name: Option<String>,
        cpus: u32,
        memory: u32,
        env_args: &[String],
        env_file: Option<&str>,
        ports: &[(u16, u16)],
        timeout: u32,
        user: Option<&str>,
        home: Option<&str>,
        exec: bool,
        follow: bool,
        command: &[String],
        format: OutputFormat,
        verbose: bool,
    ) -> anyhow::Result<()> {
        let sandbox_name =
            name.unwrap_or_else(|| format!("sandbox-{}", &uuid::Uuid::new_v4().to_string()[..8]));

        let env_files: Vec<String> = env_file.iter().map(|s| s.to_string()).collect();
        let env_vars = parse_env_vars(env_args, &env_files)?;

        cmd_run_next(
            &sandbox_name,
            image,
            cpus,
            memory,
            &env_vars,
            &env_files,
            ports,
            timeout,
            user,
            home,
            exec,
            follow,
            command,
            format,
            verbose,
        )
        .await
    }

    /// Execute a command in a running sandbox over its exec channel.
    async fn cmd_exec(
        sandbox: &str,
        shell: bool,
        tty: bool,
        follow: bool,
        command: &[String],
    ) -> anyhow::Result<()> {
        use crate::supervisor::client::SupervisorClient;

        if command.is_empty() {
            anyhow::bail!("no command specified. Usage: nanosb exec <sandbox> <command>");
        }

        let client = SupervisorClient::new(sandbox);
        if !client.is_running() {
            anyhow::bail!("sandbox '{}' is not running", sandbox);
        }

        let sock = client.sandbox_dir().join("exec.sock");
        if !sock.exists() {
            anyhow::bail!(
                "sandbox '{}' has no exec channel (start it with --exec)",
                sandbox
            );
        }
        let exec = runtime::exec::ExecClient::new(sock);

        let opts = runtime::exec::ExecOptions::new().shell(shell).tty(tty);
        let (program, args): (String, Vec<String>) = if shell {
            (command.join(" "), Vec::new())
        } else {
            (command[0].clone(), command[1..].to_vec())
        };
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        // A PTY implies streaming (interactive).
        if follow || tty {
            use std::io::Write;
            let mut handle = exec
                .start(&program, &arg_refs, opts)
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            // For a PTY, forward the host's stdin to the guest while reading
            // guest output. The handle is not shared, so multiplex manually:
            // poll stdin briefly, then drain any available guest events.
            if tty {
                let mut stdin_buf = [0u8; 1024];
                let mut keyfds = [libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                }];
                loop {
                    let r = unsafe { libc::poll(keyfds.as_mut_ptr(), 1, 50) };
                    if r > 0 && keyfds[0].revents & libc::POLLIN != 0 {
                        let n = unsafe {
                            libc::read(0, stdin_buf.as_mut_ptr() as *mut libc::c_void, stdin_buf.len())
                        };
                        if n > 0 {
                            let s = String::from_utf8_lossy(&stdin_buf[..n as usize]).to_string();
                            let _ = handle.write_stdin(&s);
                        }
                    }
                    // Drain whatever the guest has produced so far.
                    match handle.next_event(Some(std::time::Duration::from_millis(10))) {
                        Ok(Some(runtime::exec::ExecEvent::Output(c))) => {
                            let out = std::io::stdout();
                            let mut h = out.lock();
                            let _ = h.write_all(c.data.as_bytes());
                            let _ = h.flush();
                        }
                        Ok(Some(runtime::exec::ExecEvent::Exit { code })) => {
                            if code != 0 {
                                std::process::exit(code);
                            }
                            return Ok(());
                        }
                        Ok(Some(runtime::exec::ExecEvent::Error { message })) => {
                            anyhow::bail!("{}", message)
                        }
                        Err(_) => {}
                        _ => {}
                    }
                }
            }

            let code = loop {
                match handle.next_event(None) {
                    Ok(Some(runtime::exec::ExecEvent::Output(c))) => {
                        let out = std::io::stdout();
                        let mut h = out.lock();
                        let _ = h.write_all(c.data.as_bytes());
                        let _ = h.flush();
                    }
                    Ok(Some(runtime::exec::ExecEvent::Exit { code })) => break code,
                    Ok(Some(runtime::exec::ExecEvent::Error { message })) => {
                        anyhow::bail!("{}", message)
                    }
                    Ok(Some(runtime::exec::ExecEvent::Started { .. })) => {}
                    Ok(None) => break 0,
                    Err(e) => anyhow::bail!("{}", e),
                }
            };
            if code != 0 {
                std::process::exit(code);
            }
        } else {
            let res = exec
                .exec_with(&program, &arg_refs, opts)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            print!("{}", res.stdout);
            eprint!("{}", res.stderr);
            if res.exit_code != 0 {
                std::process::exit(res.exit_code);
            }
        }
        Ok(())
    }

    /// List sandboxes
    async fn cmd_ps(all: bool, format: OutputFormat) -> anyhow::Result<()> {
        let registry = SandboxRegistry::new()?;
        let sandboxes = registry.list()?;

        let filtered: Vec<_> = if all {
            sandboxes
        } else {
            sandboxes
                .into_iter()
                .filter(|s| s.status == SandboxStatus::Running)
                .collect()
        };

        let mut supervised: Vec<SupervisedSandboxRow> = Vec::new();
        for dir in supervisor_sandbox_dirs() {
            let Some(name) = dir.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            if !dir.join("config.json").exists() {
                continue;
            }
            let client = crate::supervisor::client::SupervisorClient::new(&name);
            let state = client.read_state();
            let running = client.is_running();
            let status = match state.as_ref().map(|s| &s.state) {
                Some(crate::supervisor::SandboxState::Running) => "Running",
                Some(crate::supervisor::SandboxState::Starting) => "Starting",
                Some(crate::supervisor::SandboxState::Stopped) => "Stopped",
                Some(crate::supervisor::SandboxState::Error) => "Error",
                None if running => "Running",
                None => "Stopped",
            }
            .to_string();
            if !all && status != "Running" {
                continue;
            }
            let image = std::fs::read_to_string(dir.join("config.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<runtime::config::SandboxConfig>(&raw).ok())
                .map(|c| c.image)
                .unwrap_or_default();
            let started_at = state
                .as_ref()
                .map(|s| s.started_at.clone())
                .unwrap_or_default();
            let created = chrono::DateTime::parse_from_rfc3339(&started_at)
                .map(|dt| format_duration(chrono::Utc::now() - dt.with_timezone(&chrono::Utc)))
                .unwrap_or_else(|_| "-".to_string());
            supervised.push(SupervisedSandboxRow {
                name,
                image,
                status,
                created,
            });
        }

        if filtered.is_empty() && supervised.is_empty() {
            match format {
                OutputFormat::Text => {
                    if all {
                        println!("No sandboxes found.");
                    } else {
                        println!("No running sandboxes. Use 'nanosb ps -a' to show all.");
                    }
                }
                OutputFormat::Json => {
                    println!("[]");
                }
            }
            return Ok(());
        }

        match format {
            OutputFormat::Text => {
                let mut rows: Vec<SandboxRow> = filtered
                    .iter()
                    .map(|s| {
                        let duration = chrono::Utc::now() - s.created_at;
                        let status_str = match s.status {
                            SandboxStatus::Running => format!("{}", "Running".green()),
                            SandboxStatus::Stopped => format!("{}", "Stopped".yellow()),
                            SandboxStatus::Error => format!("{}", "Error".red()),
                            _ => format!("{:?}", s.status),
                        };
                        SandboxRow {
                            id: s.id[..12].to_string(),
                            name: s.name.clone(),
                            image: s.image.clone(),
                            status: status_str,
                            created: format_duration(duration),
                        }
                    })
                    .collect();

                for row in supervised {
                    let status_str = match row.status.as_str() {
                        "Running" => format!("{}", row.status.green()),
                        "Starting" => format!("{}", row.status.yellow()),
                        "Error" => format!("{}", row.status.red()),
                        _ => format!("{}", row.status.yellow()),
                    };
                    rows.push(SandboxRow {
                        id: "-".to_string(),
                        name: row.name,
                        image: row.image,
                        status: status_str,
                        created: row.created,
                    });
                }

                let table = Table::new(rows).to_string();
                println!("{}", table);
            }
            OutputFormat::Json => {
                let mut json: Vec<serde_json::Value> = Vec::new();
                for s in &filtered {
                    json.push(serde_json::to_value(s)?);
                }
                for row in &supervised {
                    json.push(serde_json::json!({
                        "name": row.name,
                        "image": row.image,
                        "status": row.status,
                        "started": row.created,
                        "mode": "supervised",
                    }));
                }
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
        }

        Ok(())
    }

    /// Stop a running sandbox
    async fn cmd_stop(sandbox_id: &str, verbose: bool) -> anyhow::Result<()> {
        {
            use crate::supervisor::client::SupervisorClient;
            let client = SupervisorClient::new(sandbox_id);
            if client.sandbox_dir().exists() && client.is_running() {
                if verbose {
                    eprintln!("Stopping supervisor sandbox '{}'", sandbox_id);
                }
                client.stop(false).map_err(|e| anyhow::anyhow!("{}", e))?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                loop {
                    if let Some(state) = client.read_state() {
                        if matches!(
                            state.state,
                            crate::supervisor::SandboxState::Stopped
                                | crate::supervisor::SandboxState::Error
                        ) {
                            break;
                        }
                    }
                    if std::time::Instant::now() > deadline {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                println!("{} Stopped {}", "✓".green(), sandbox_id.bold());
                return Ok(());
            }
        }

        let registry = SandboxRegistry::new()?;

        // Find sandbox by ID or name prefix
        let sandbox_info = registry
            .list()?
            .into_iter()
            .find(|s| s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id));

        let sandbox_info = sandbox_info.ok_or_else(|| {
            error!("Sandbox not found for stop: {}", sandbox_id);
            anyhow::anyhow!("Sandbox not found: {}", sandbox_id)
        })?;

        if verbose {
            eprintln!(
                "Stopping sandbox: {} ({})",
                sandbox_info.name, sandbox_info.id
            );
        }

        // Update status in registry
        registry.update_status(&sandbox_info.id, SandboxStatus::Stopped)?;

        println!(
            "{} Stopped {}",
            "✓".green(),
            sandbox_info.id[..12].to_string().bold()
        );
        Ok(())
    }

    /// Remove a sandbox
    async fn cmd_rm(sandbox_id: &str, force: bool, verbose: bool) -> anyhow::Result<()> {
        {
            use crate::supervisor::client::SupervisorClient;
            let client = SupervisorClient::new(sandbox_id);
            if client.sandbox_dir().exists() {
                if client.is_running() {
                    if !force {
                        anyhow::bail!(
                            "Sandbox {} is running. Use -f to force removal.",
                            sandbox_id
                        );
                    }
                    let _ = client.stop(true);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                    loop {
                        if let Some(state) = client.read_state() {
                            if matches!(
                                state.state,
                                crate::supervisor::SandboxState::Stopped
                                    | crate::supervisor::SandboxState::Error
                            ) {
                                break;
                            }
                        }
                        if std::time::Instant::now() > deadline {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }
                std::fs::remove_dir_all(client.sandbox_dir())?;
                println!("{} Removed {}", "✓".green(), sandbox_id.bold());
                return Ok(());
            }
        }

        let registry = SandboxRegistry::new()?;

        // Find sandbox by ID or name prefix
        let sandbox_info = registry
            .list()?
            .into_iter()
            .find(|s| s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id));

        let sandbox_info = sandbox_info.ok_or_else(|| {
            error!("Sandbox not found: {}", sandbox_id);
            anyhow::anyhow!("Sandbox not found: {}", sandbox_id)
        })?;

        if sandbox_info.status == SandboxStatus::Running && !force {
            warn!("Attempted to remove running sandbox {}", sandbox_id);
            anyhow::bail!(
                "Sandbox {} is running. Use -f to force removal.",
                sandbox_id
            );
        }

        if verbose {
            eprintln!(
                "Removing sandbox: {} ({})",
                sandbox_info.name, sandbox_info.id
            );
        }

        // Remove bundle directory if it exists
        if sandbox_info.bundle_path.exists() {
            std::fs::remove_dir_all(&sandbox_info.bundle_path)?;
        }

        // Unregister from registry
        registry.unregister(&sandbox_info.id)?;

        println!(
            "{} Removed {}",
            "✓".green(),
            sandbox_info.id[..12].to_string().bold()
        );
        Ok(())
    }

    /// Check runtime prerequisites and display status
    async fn cmd_doctor(format: OutputFormat) -> anyhow::Result<()> {
        use sandbox::validation::validate_runtime_prerequisites_detailed;

        let result = validate_runtime_prerequisites_detailed().await;

        match format {
            OutputFormat::Text => {
                print_doctor_results(&result);
            }
            OutputFormat::Json => {
                let json = doctor_results_to_json(&result);
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
        }

        if result.is_ok() {
            Ok(())
        } else {
            std::process::exit(1);
        }
    }

    /// Print doctor results as colored checklist
    fn print_doctor_results(result: &sandbox::validation::ValidationResult) {
        println!();
        println!("Checking runtime prerequisites...");
        println!();

        let mut passed = 0u32;
        let errors = &result.errors;
        let warnings = &result.warnings;

        let checks = get_platform_checks();

        let check_names: Vec<&str> = checks.iter().map(|c| c.name).collect();

        for check in &checks {
            let failed = errors.iter().find(|e| e.check == check.name);
            let warned = warnings.iter().find(|w| w.contains(check.keyword));

            if let Some(err) = failed {
                println!("  {} {}: {}", "[✗]".red().bold(), check.name, err.message);
                if let Some(ref hint) = &err.fix_hint {
                    println!("      {}: {}", "Fix".yellow(), hint);
                }
            } else if let Some(warning) = warned {
                println!("  {} {}", "[!]".yellow().bold(), warning);
                passed += 1;
            } else {
                println!(
                    "  {} {}: {}",
                    "[✓]".green().bold(),
                    check.name,
                    check.ok_message
                );
                passed += 1;
            }
        }

        // Show any errors that didn't match a known check name
        for err in errors {
            if !check_names.contains(&err.check.as_str()) {
                println!("  {} {}: {}", "[✗]".red().bold(), err.check, err.message);
                if let Some(ref hint) = &err.fix_hint {
                    println!("      {}: {}", "Fix".yellow(), hint);
                }
            }
        }

        println!();
        println!(
            "{} checks passed, {} errors, {} warnings",
            passed,
            errors.len(),
            warnings.len()
        );
        println!();

        if result.is_ok() {
            println!("{}", "Ready to run sandboxes.".green());
            let logs_dir = logs_dir();
            println!("  Logs: {}", logs_dir.display());
        } else {
            println!("{}", "Cannot run sandboxes. Fix the errors above.".red());
        }
        println!();
    }

    struct PlatformCheck {
        name: &'static str,
        keyword: &'static str,
        ok_message: &'static str,
    }

    fn get_platform_checks() -> Vec<PlatformCheck> {
        #[cfg(target_os = "macos")]
        {
            vec![
                PlatformCheck {
                    name: "Architecture",
                    keyword: "architecture",
                    ok_message: "Apple Silicon (aarch64)",
                },
                PlatformCheck {
                    name: "libkrunfw Kernel Firmware",
                    keyword: "libkrunfw",
                    ok_message: "found (libkrunfw.5.dylib)",
                },
                PlatformCheck {
                    name: "Hypervisor.framework",
                    keyword: "Hypervisor",
                    ok_message: "available",
                },
                PlatformCheck {
                    name: "gvproxy",
                    keyword: "gvproxy",
                    ok_message: "available (full outbound networking)",
                },
            ]
        }

        #[cfg(target_os = "linux")]
        {
            vec![
                PlatformCheck {
                    name: "libkrunfw Kernel Firmware",
                    keyword: "libkrunfw",
                    ok_message: "found (libkrunfw.so.5)",
                },
                PlatformCheck {
                    name: "KVM Device",
                    keyword: "KVM",
                    ok_message: "/dev/kvm accessible",
                },
                PlatformCheck {
                    name: "gvproxy",
                    keyword: "gvproxy",
                    ok_message: "available (full outbound networking)",
                },
            ]
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            vec![PlatformCheck {
                name: "Platform",
                keyword: "platform",
                ok_message: "supported",
            }]
        }
    }

    fn doctor_results_to_json(result: &sandbox::validation::ValidationResult) -> serde_json::Value {
        serde_json::json!({
            "ok": result.is_ok(),
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "errors": result.errors.iter().map(|e| {
                serde_json::json!({
                    "check": e.check,
                    "message": e.message,
                    "fix_hint": e.fix_hint,
                })
            }).collect::<Vec<_>>(),
            "warnings": result.warnings,
        })
    }

    /// Prune the image/blob cache to reclaim disk space.
    async fn cmd_cache_prune(all: bool, format: OutputFormat) -> anyhow::Result<()> {
        let manager = ImageManager::with_default_cache()?;
        let result = manager.prune(all)?;

        match format {
            OutputFormat::Text => {
                if result.orphaned_bundles > 0 {
                    println!(
                        "Orphaned bundles removed: {} ({})",
                        result.orphaned_bundles,
                        format_bytes(result.orphaned_bundles_bytes)
                    );
                }
                if result.decompressed_tars > 0 {
                    println!(
                        "Decompressed tars removed: {} ({})",
                        result.decompressed_tars,
                        format_bytes(result.decompressed_tars_bytes)
                    );
                }
                if result.stale_temps > 0 {
                    println!(
                        "Stale temp files removed: {} ({})",
                        result.stale_temps,
                        format_bytes(result.stale_temps_bytes)
                    );
                }
                if all {
                    if result.blobs > 0 {
                        println!(
                            "Blobs removed: {} ({})",
                            result.blobs,
                            format_bytes(result.blobs_bytes)
                        );
                    }
                    if result.manifests > 0 {
                        println!("Manifests removed: {}", result.manifests);
                    }
                }

                if result.total_bytes > 0 {
                    println!(
                        "\n{} Total reclaimed: {}",
                        "✓".green(),
                        format_bytes(result.total_bytes).bold()
                    );
                } else {
                    println!("Cache is clean — nothing to prune.");
                }
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        }

        Ok(())
    }

    /// Clean up stale project clones and list project branches.
    async fn cmd_cleanup(project: Option<&str>) -> anyhow::Result<()> {
        let project_path = match project {
            Some(p) => std::path::PathBuf::from(p),
            None => std::env::current_dir()?,
        };

        let canonical_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        let clones = sandbox::project::clones_dir(&canonical_path);
        if !clones.exists() {
            println!("No nanosb clones found for {}", project_path.display());
            return Ok(());
        }

        let mut cleaned = 0;
        if let Ok(entries) = std::fs::read_dir(&clones) {
            for entry in entries {
                let entry = entry?;
                if entry.path().is_dir() {
                    println!(
                        "Cleaning up stale clone: {}",
                        entry.file_name().to_string_lossy()
                    );

                    let clone_path = entry.path();

                    // Detect the branch name from the clone
                    let branch_output = std::process::Command::new("git")
                        .args(["rev-parse", "--abbrev-ref", "HEAD"])
                        .current_dir(&clone_path)
                        .output();
                    let branch_name = branch_output
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

                    // Auto-commit any uncommitted changes
                    let status_output = std::process::Command::new("git")
                        .args(["status", "--porcelain"])
                        .current_dir(&clone_path)
                        .output();
                    if let Ok(status_out) = status_output {
                        let status_text = String::from_utf8_lossy(&status_out.stdout);
                        if !status_text.trim().is_empty() {
                            println!("  Auto-committing uncommitted changes...");
                            let _ = std::process::Command::new("git")
                                .args(["add", "-A"])
                                .current_dir(&clone_path)
                                .output();
                            let _ = std::process::Command::new("git")
                                .args(["commit", "-m", "nanosb: auto-save on cleanup"])
                                .current_dir(&clone_path)
                                .env("GIT_AUTHOR_NAME", "nanosandbox")
                                .env("GIT_AUTHOR_EMAIL", "nanosandbox@localhost")
                                .env("GIT_COMMITTER_NAME", "nanosandbox")
                                .env("GIT_COMMITTER_EMAIL", "nanosandbox@localhost")
                                .output();
                        }
                    }

                    // Fetch the branch back to source repo
                    if let Some(ref branch) = branch_name {
                        let refspec = format!("{}:{}", branch, branch);
                        let _ = std::process::Command::new("git")
                            .args(["fetch", &clone_path.to_string_lossy(), &refspec, "--force"])
                            .current_dir(&project_path)
                            .output();
                    }

                    // Remove the clone directory
                    std::fs::remove_dir_all(&clone_path).ok();
                    cleaned += 1;
                }
            }
        }

        // Remove empty clones dir
        if std::fs::read_dir(&clones)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true)
        {
            std::fs::remove_dir_all(&clones).ok();
        }

        // List nanosb branches
        let output = std::process::Command::new("git")
            .args(["branch", "--list", "nanosb/*"])
            .current_dir(&project_path)
            .output();

        if let Ok(out) = output {
            let branches = String::from_utf8_lossy(&out.stdout);
            if !branches.trim().is_empty() {
                println!("\nRemaining nanosb branches:");
                for line in branches.lines() {
                    println!("  {}", line.trim());
                }
                println!("\nTo delete a merged branch: git branch -d <branch-name>");
            }
        }

        println!("\nCleaned up {} clone(s).", cleaned);
        Ok(())
    }

    /// List saved sessions for the current or provided project path.
    async fn cmd_sessions(project: Option<&str>, format: OutputFormat) -> anyhow::Result<()> {
        fn format_age_precise(dt: &chrono::DateTime<chrono::Utc>) -> String {
            let now = chrono::Utc::now();
            let duration = now.signed_duration_since(*dt);
            let secs = duration.num_seconds();

            if secs < 60 {
                format!("{}s ago", secs.max(0))
            } else if secs < 3600 {
                format!("{}m ago", duration.num_minutes())
            } else if secs < 86_400 {
                format!("{}h ago", duration.num_hours())
            } else {
                format!("{}d ago", duration.num_days())
            }
        }

        let project_path = match project {
            Some(p) => std::path::PathBuf::from(p),
            None => std::env::current_dir()?,
        };

        let sessions = sandbox::session::Session::list(&project_path);

        match format {
            OutputFormat::Text => {
                if sessions.is_empty() {
                    println!("No saved sessions for {}", project_path.display());
                    return Ok(());
                }

                let rows: Vec<SessionRow> = sessions
                    .iter()
                    .map(|entry| SessionRow {
                        id: entry.id.clone(),
                        updated_at: entry
                            .session
                            .updated_at
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string(),
                        updated: format_age_precise(&entry.session.updated_at),
                        panels: entry.session.panels.len(),
                        summary: entry.session.summary(),
                    })
                    .collect();

                println!("Project: {}", project_path.display());
                println!("{}", Table::new(rows));
                println!("\nResume latest: nanosb -r");
                println!("Resume specific: nanosb --session <id>");
            }
            OutputFormat::Json => {
                let json_sessions: Vec<_> = sessions
                    .iter()
                    .map(|entry| {
                        serde_json::json!({
                            "id": entry.id,
                            "created_at": entry.session.created_at,
                            "updated_at": entry.session.updated_at,
                            "updated": format_age_precise(&entry.session.updated_at),
                            "panels": entry.session.panels.len(),
                            "summary": entry.session.summary(),
                        })
                    })
                    .collect();

                let out = serde_json::json!({
                    "project": project_path,
                    "sessions": json_sessions,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        }

        Ok(())
    }

    async fn cmd_projects(format: OutputFormat) -> anyhow::Result<()> {
        fn format_age_precise(dt: &chrono::DateTime<chrono::Utc>) -> String {
            let now = chrono::Utc::now();
            let duration = now.signed_duration_since(*dt);
            let secs = duration.num_seconds();
            if secs < 60 {
                format!("{}s ago", secs.max(0))
            } else if secs < 3600 {
                format!("{}m ago", duration.num_minutes())
            } else if secs < 86_400 {
                format!("{}h ago", duration.num_hours())
            } else {
                format!("{}d ago", duration.num_days())
            }
        }

        let registry = sandbox::ProjectRegistry::load();
        let projects = registry.list();

        match format {
            OutputFormat::Text => {
                if projects.is_empty() {
                    println!("No projects registered. Launch nanosb with --project to register one.");
                    return Ok(());
                }

                #[derive(tabled::Tabled)]
                struct ProjectRow {
                    #[tabled(rename = "PATH")]
                    path: String,
                    #[tabled(rename = "NAME")]
                    name: String,
                    #[tabled(rename = "LAST USED")]
                    last_used: String,
                    #[tabled(rename = "AGE")]
                    age: String,
                }

                let rows: Vec<ProjectRow> = projects
                    .iter()
                    .map(|entry| ProjectRow {
                        path: entry.path.clone(),
                        name: entry.display_name.clone(),
                        last_used: entry
                            .last_used
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string(),
                        age: format_age_precise(&entry.last_used),
                    })
                    .collect();

                println!("{}", tabled::Table::new(rows));
            }
            OutputFormat::Json => {
                let json_projects: Vec<_> = projects
                    .iter()
                    .map(|entry| {
                        serde_json::json!({
                            "path": entry.path,
                            "display_name": entry.display_name,
                            "last_used": entry.last_used,
                        })
                    })
                    .collect();
                let out = serde_json::json!({ "projects": json_projects });
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        }

        Ok(())
    }

    async fn cmd_projects_forget(path: &str, format: OutputFormat) -> anyhow::Result<()> {
        let mut registry = sandbox::ProjectRegistry::load();
        let path = std::path::Path::new(path);
        let removed = registry.forget(path);

        match format {
            OutputFormat::Text => {
                if removed {
                    println!("Removed '{}' from project registry.", path.display());
                } else {
                    println!(
                        "Project '{}' was not in the registry.",
                        path.display()
                    );
                }
            }
            OutputFormat::Json => {
                let out = serde_json::json!({
                    "removed": removed,
                    "path": path.to_string_lossy(),
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        }

        Ok(())
    }

    /// Run preflight validation, showing doctor output on failure.
    /// Returns the logs directory: `~/.nanosandbox/logs/` on all platforms.
    fn logs_dir() -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join(".nanosandbox")
            .join("logs")
    }

    /// Remove log files older than `retention_days` from the given directory.
    fn cleanup_old_logs(dir: &std::path::Path, retention_days: u64) {
        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs(retention_days * 24 * 60 * 60);

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let _name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) if n.starts_with("nanosb.") || n.starts_with("vm-") => n,
                _ => continue,
            };

            if let Ok(metadata) = path.metadata() {
                if let Ok(modified) = metadata.modified() {
                    if modified < cutoff {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    // Handle internal subprocess commands BEFORE starting the tokio runtime.
    //
    // This is critical on macOS: the TUI uses a multi-threaded tokio runtime,
    // and Hypervisor.framework's hv_vm_create() fails when called from a
    // fork()ed child of a multi-threaded process. By spawning the VM boot
    // subprocess via posix_spawn (std::process::Command) and handling it here
    // — before any threads are created — the child runs in a clean,
    // single-threaded process where hv_vm_create() works correctly.
    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        sandbox::handle_boot_vm_subprocess();
        // ^ never returns
    }

    if std::env::args().nth(1).as_deref() == Some("__supervise") {
        use clap::Parser;
        let parsed = cli::Cli::parse();
        if let Some(cli::Commands::Supervise {
            sandbox_name,
            config_json,
            extra_mounts_json,
            timeout_secs,
        }) = parsed.command
        {
            crate::supervisor::run_supervisor(crate::supervisor::SuperviseArgs {
                sandbox_name,
                config_json,
                extra_mounts_json,
                timeout_secs,
            });
        }
        anyhow::bail!("__supervise requires sandbox_name and config_json arguments");
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(cli::run())
}
