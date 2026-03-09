//! Nanosandbox CLI
//!
//! A command-line interface for managing VM-based sandboxes.

#[cfg(feature = "cli")]
mod cli {
    use clap::{Parser, Subcommand, ValueEnum};
    use colored::Colorize;
    use indicatif::{ProgressBar, ProgressStyle};
    use nanosandbox::{ImageManager, Sandbox, SandboxConfig, SandboxRegistry, SandboxStatus, Stream};
    use std::io::Write;
    use std::time::Duration;
    use tabled::{Table, Tabled};

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
    }

    #[derive(Subcommand)]
    pub enum Commands {
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

            /// Timeout in seconds (default: 600)
            #[arg(long, default_value = "600")]
            timeout: u32,

            /// Buffer output instead of streaming in real-time
            /// (only useful with --format json)
            #[arg(long)]
            buffered: bool,

            /// Command to run
            #[arg(trailing_var_arg = true)]
            command: Vec<String>,
        },

        /// Execute a command in a running sandbox
        Exec {
            /// Sandbox ID or name
            sandbox: String,

            /// Buffer output instead of streaming in real-time
            #[arg(long)]
            buffered: bool,

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
        Rm {
            /// Sandbox ID or name
            sandbox: String,

            /// Force removal (stop if running)
            #[arg(short, long)]
            force: bool,
        },

        /// Check runtime prerequisites
        Doctor,
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
        #[tabled(rename = "IMAGE")]
        image: String,
        #[tabled(rename = "STATUS")]
        status: String,
        #[tabled(rename = "CREATED")]
        created: String,
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

        if cli.verbose && cli.command.is_some() {
            tracing_subscriber::fmt()
                .with_env_filter("nanosandbox=debug")
                .init();
        }

        match cli.command {
            None => {
                // Default: launch TUI when no subcommand is given
                nanosandbox::tui::run::run_tui().await
            }
            Some(Commands::Pull { image }) => cmd_pull(&image, cli.format, cli.verbose).await,
            Some(Commands::Images) => cmd_images(cli.format).await,
            Some(Commands::Run {
                image,
                name,
                cpus,
                memory,
                env,
                env_file,
                timeout,
                buffered,
                command,
            }) => {
                cmd_run(
                    &image,
                    name,
                    cpus,
                    memory,
                    &env,
                    env_file.as_deref(),
                    timeout,
                    buffered,
                    &command,
                    cli.format,
                    cli.verbose,
                )
                .await
            }
            Some(Commands::Exec {
                sandbox,
                buffered,
                command,
            }) => cmd_exec(&sandbox, buffered, &command, cli.format, cli.verbose).await,
            Some(Commands::Ps { all }) => cmd_ps(all, cli.format).await,
            Some(Commands::Stop { sandbox }) => cmd_stop(&sandbox, cli.verbose).await,
            Some(Commands::Rm { sandbox, force }) => cmd_rm(&sandbox, force, cli.verbose).await,
            Some(Commands::Doctor) => cmd_doctor(cli.format).await,
        }
    }

    /// Pull an image from a registry
    async fn cmd_pull(image: &str, format: OutputFormat, verbose: bool) -> anyhow::Result<()> {
        let pb = create_pull_progress();
        pb.set_message(format!("Pulling {}", image));

        let manager = ImageManager::with_default_cache_and_auth()?;

        if verbose {
            pb.set_message("Connecting to registry...".to_string());
        }

        let pulled = manager.pull(image).await?;
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

    /// Parse environment variables from --env flags and --env-file
    fn parse_env_vars(
        env_args: &[String],
        env_file: Option<&str>,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut vars = Vec::new();

        // Parse --env-file first (if provided)
        if let Some(path) = env_file {
            let content = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("Failed to read env file '{}': {}", path, e))?;
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
                    anyhow::bail!(
                        "Environment variable '{}' not found. Use KEY=VALUE format.",
                        entry
                    );
                }
            }
        }

        Ok(vars)
    }

    /// Run a command in a new sandbox
    ///
    /// By default, output is streamed in real-time (like `docker run`).
    /// Use `--buffered` or `--format json` for buffered output.
    #[allow(clippy::too_many_arguments)]
    async fn cmd_run(
        image: &str,
        name: Option<String>,
        cpus: u32,
        memory: u32,
        env_args: &[String],
        env_file: Option<&str>,
        timeout: u32,
        buffered: bool,
        command: &[String],
        format: OutputFormat,
        verbose: bool,
    ) -> anyhow::Result<()> {
        preflight_check().await?;

        let sandbox_name =
            name.unwrap_or_else(|| format!("sandbox-{}", &uuid::Uuid::new_v4().to_string()[..8]));

        // Parse environment variables
        let env_vars = parse_env_vars(env_args, env_file)?;

        if verbose {
            eprintln!("Creating sandbox '{}' with image '{}'", sandbox_name, image);
            if !env_vars.is_empty() {
                eprintln!(
                    "Environment variables: {}",
                    env_vars
                        .iter()
                        .map(|(k, _)| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }

        let mut builder = SandboxConfig::builder()
            .name(&sandbox_name)
            .image(image)
            .cpus(cpus)
            .memory_mb(memory)
            .timeout_secs(timeout);

        for (key, value) in &env_vars {
            builder = builder.env(key, value);
        }

        let config = builder.build();

        let pb = create_pull_progress();
        pb.set_message("Creating sandbox...");

        let mut sandbox = Sandbox::create(config).await?;

        pb.set_message("Starting sandbox...");
        sandbox.start().await?;
        pb.finish_and_clear();

        if command.is_empty() {
            // No command, just print sandbox info
            match format {
                OutputFormat::Text => {
                    println!("{} Sandbox {} started", "✓".green(), sandbox.id().bold());
                    println!(
                        "Run commands with: nanosb exec {} <command>",
                        &sandbox.id()[..12]
                    );
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "id": sandbox.id(),
                        "status": "running",
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        } else {
            // Execute the command
            let cmd = &command[0];
            let args: Vec<&str> = command[1..].iter().map(|s| s.as_str()).collect();

            if verbose {
                eprintln!("Executing: {} {:?}", cmd, args);
            }

            // Use buffered mode only when explicitly requested or for JSON output
            let use_buffered = buffered || matches!(format, OutputFormat::Json);

            if use_buffered {
                // Buffered execution - output after completion
                let result = sandbox.exec(cmd, &args).await?;

                match format {
                    OutputFormat::Text => {
                        print!("{}", result.stdout);
                        eprint!("{}", result.stderr);
                    }
                    OutputFormat::Json => {
                        let json = serde_json::json!({
                            "exit_code": result.exit_code,
                            "stdout": result.stdout,
                            "stderr": result.stderr,
                            "duration_ms": result.duration_ms,
                        });
                        println!("{}", serde_json::to_string_pretty(&json)?);
                    }
                }

                // Clean up sandbox
                sandbox.destroy().await?;

                if result.exit_code != 0 {
                    std::process::exit(result.exit_code);
                }
            } else {
                // Streaming execution (default) - output in real-time.
                // We use write_all_retry instead of println! because rapid
                // streaming (e.g. LLM token deltas) can fill the terminal
                // buffer, causing EAGAIN (os error 35). println! panics on
                // write errors, so we retry with backoff instead.
                let exit_code = sandbox
                    .exec_stream(cmd, &args, |chunk| {
                        match chunk.stream {
                            Stream::Stdout => {
                                let data = format!("{}\n", chunk.data);
                                let stdout = std::io::stdout();
                                let mut handle = stdout.lock();
                                write_all_retry(&mut handle, data.as_bytes());
                            }
                            Stream::Stderr => {
                                let data = format!("{}\n", chunk.data);
                                let stderr = std::io::stderr();
                                let mut handle = stderr.lock();
                                write_all_retry(&mut handle, data.as_bytes());
                            }
                        }
                    })
                    .await?;

                // Clean up sandbox
                sandbox.destroy().await?;

                if exit_code != 0 {
                    std::process::exit(exit_code);
                }
            }
        }

        Ok(())
    }

    /// Execute a command in a running sandbox
    async fn cmd_exec(
        sandbox_id: &str,
        buffered: bool,
        command: &[String],
        format: OutputFormat,
        verbose: bool,
    ) -> anyhow::Result<()> {
        if command.is_empty() {
            anyhow::bail!("No command specified. Usage: nanosb exec <sandbox> <command>");
        }

        preflight_check().await?;

        let registry = SandboxRegistry::new()?;

        // Find sandbox by ID or name prefix
        let sandbox_info = registry
            .list()?
            .into_iter()
            .find(|s| s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id));

        let sandbox_info =
            sandbox_info.ok_or_else(|| anyhow::anyhow!("Sandbox not found: {}", sandbox_id))?;

        if sandbox_info.status != SandboxStatus::Running {
            anyhow::bail!(
                "Sandbox {} is not running (status: {:?})",
                sandbox_id,
                sandbox_info.status
            );
        }

        if verbose {
            eprintln!("Found sandbox: {} ({})", sandbox_info.name, sandbox_info.id);
            if !buffered {
                eprintln!("Streaming mode enabled (default)");
            }
        }

        // For exec, we need to connect to the running sandbox
        // This requires the runtime to be running, so we inform the user
        eprintln!(
            "{} Note: 'exec' requires an active runtime connection.",
            "!".yellow()
        );
        eprintln!("  For ephemeral execution, use 'nanosb run <image> <command>' instead.");

        // Return the sandbox info for reference
        match format {
            OutputFormat::Text => {
                println!(
                    "Sandbox {} exists but exec requires runtime integration.",
                    sandbox_id
                );
            }
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "id": sandbox_info.id,
                    "name": sandbox_info.name,
                    "status": format!("{:?}", sandbox_info.status),
                    "note": "exec requires runtime integration",
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
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

        match format {
            OutputFormat::Text => {
                if filtered.is_empty() {
                    if all {
                        println!("No sandboxes found.");
                    } else {
                        println!("No running sandboxes. Use 'nanosb ps -a' to show all.");
                    }
                    return Ok(());
                }

                let rows: Vec<SandboxRow> = filtered
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
                            image: s.image.clone(),
                            status: status_str,
                            created: format_duration(duration),
                        }
                    })
                    .collect();

                let table = Table::new(rows).to_string();
                println!("{}", table);
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&filtered)?);
            }
        }

        Ok(())
    }

    /// Stop a running sandbox
    async fn cmd_stop(sandbox_id: &str, verbose: bool) -> anyhow::Result<()> {
        let registry = SandboxRegistry::new()?;

        // Find sandbox by ID or name prefix
        let sandbox_info = registry
            .list()?
            .into_iter()
            .find(|s| s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id));

        let sandbox_info =
            sandbox_info.ok_or_else(|| anyhow::anyhow!("Sandbox not found: {}", sandbox_id))?;

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
        let registry = SandboxRegistry::new()?;

        // Find sandbox by ID or name prefix
        let sandbox_info = registry
            .list()?
            .into_iter()
            .find(|s| s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id));

        let sandbox_info =
            sandbox_info.ok_or_else(|| anyhow::anyhow!("Sandbox not found: {}", sandbox_id))?;

        if sandbox_info.status == SandboxStatus::Running && !force {
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
        use nanosandbox::runtime::validate_runtime_prerequisites_detailed;

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
    fn print_doctor_results(result: &nanosandbox::runtime::ValidationResult) {
        println!();
        println!("Checking runtime prerequisites...");
        println!();

        let mut passed = 0u32;
        let errors = &result.errors;
        let warnings = &result.warnings;

        let checks = get_platform_checks();

        for check in &checks {
            let failed = errors.iter().find(|e| e.check == check.name);
            let warned = warnings.iter().find(|w| w.contains(check.keyword));

            if let Some(err) = failed {
                println!(
                    "  {} {}: {}",
                    "[✗]".red().bold(),
                    check.name,
                    err.message
                );
                if let Some(ref hint) = &err.fix_hint {
                    println!("      {}: {}", "Fix".yellow(), hint);
                }
            } else if let Some(warning) = warned {
                println!(
                    "  {} {}",
                    "[!]".yellow().bold(),
                    warning
                );
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
                    name: "libkrun Library",
                    keyword: "libkrun",
                    ok_message: "/opt/homebrew/lib/libkrun.dylib",
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
                    name: "libkrun Library",
                    keyword: "libkrun",
                    ok_message: "found",
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

        #[cfg(target_os = "windows")]
        {
            vec![
                PlatformCheck {
                    name: "Windows Containers",
                    keyword: "Containers",
                    ok_message: "enabled",
                },
                PlatformCheck {
                    name: "containerd Service",
                    keyword: "containerd",
                    ok_message: "running",
                },
                PlatformCheck {
                    name: "runhcs Shim",
                    keyword: "runhcs",
                    ok_message: "found",
                },
                PlatformCheck {
                    name: "HCS Service",
                    keyword: "HCS",
                    ok_message: "running",
                },
                PlatformCheck {
                    name: "Hyper-V",
                    keyword: "Hyper-V",
                    ok_message: "enabled",
                },
            ]
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            vec![PlatformCheck {
                name: "Platform",
                keyword: "platform",
                ok_message: "supported",
            }]
        }
    }

    fn doctor_results_to_json(
        result: &nanosandbox::runtime::ValidationResult,
    ) -> serde_json::Value {
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

    /// Run preflight validation, showing doctor output on failure.
    async fn preflight_check() -> anyhow::Result<()> {
        use nanosandbox::runtime::validate_runtime_prerequisites_detailed;

        let result = validate_runtime_prerequisites_detailed().await;
        if !result.is_ok() {
            print_doctor_results(&result);

            #[cfg(target_os = "macos")]
            eprintln!("\nRun './scripts/install/macos.sh' to install dependencies.");
            #[cfg(target_os = "linux")]
            eprintln!("\nRun './scripts/install/linux.sh' to install dependencies.");

            anyhow::bail!("Runtime prerequisites not met. Run 'nanosb doctor' for details.");
        }
        Ok(())
    }

    /// Write all bytes to a writer, retrying on EAGAIN/WouldBlock.
    ///
    /// When streaming rapid output (e.g. LLM token deltas via stream-json),
    /// the terminal buffer can fill up and return EAGAIN (os error 35 on macOS).
    /// Unlike `println!()` which panics on write errors, this function retries
    /// with a short sleep to let the terminal drain its buffer.
    fn write_all_retry(w: &mut impl Write, mut buf: &[u8]) {
        while !buf.is_empty() {
            match w.write(buf) {
                Ok(0) => break, // EOF / closed pipe
                Ok(n) => buf = &buf[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Terminal buffer full — back off briefly and retry
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break, // Broken pipe or other fatal error — stop quietly
            }
        }
        let _ = w.flush();
    }
}

#[cfg(feature = "cli")]
fn main() -> anyhow::Result<()> {
    // Handle internal subprocess commands BEFORE starting the tokio runtime.
    //
    // This is critical on macOS: the TUI uses a multi-threaded tokio runtime,
    // and Hypervisor.framework's hv_vm_create() fails when called from a
    // fork()ed child of a multi-threaded process. By spawning the VM boot
    // subprocess via posix_spawn (std::process::Command) and handling it here
    // — before any threads are created — the child runs in a clean,
    // single-threaded process where hv_vm_create() works correctly.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if std::env::args().nth(1).as_deref() == Some("internal-boot-vm") {
        nanosandbox::runtime::handle_boot_vm_subprocess();
        // ^ never returns
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(cli::run())
}

#[cfg(not(feature = "cli"))]
fn main() {
    eprintln!("CLI feature not enabled. Build with: cargo build --features cli");
    std::process::exit(1);
}
