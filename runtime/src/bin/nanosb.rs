//! Nanosandbox CLI
//!
//! A command-line interface for managing VM-based sandboxes.

#[cfg(feature = "cli")]
mod cli {
    use clap::{Parser, Subcommand, ValueEnum};
    use colored::Colorize;
    use indicatif::{ProgressBar, ProgressStyle};
    use nanosandbox::{
        ImageManager, Sandbox, SandboxConfig, SandboxRegistry, SandboxStatus,
    };
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
        pub command: Commands,

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

            /// Command to run
            #[arg(trailing_var_arg = true)]
            command: Vec<String>,
        },

        /// Execute a command in a running sandbox
        Exec {
            /// Sandbox ID or name
            sandbox: String,

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

        if cli.verbose {
            tracing_subscriber::fmt()
                .with_env_filter("nanosandbox=debug")
                .init();
        }

        match cli.command {
            Commands::Pull { image } => cmd_pull(&image, cli.format, cli.verbose).await,
            Commands::Images => cmd_images(cli.format).await,
            Commands::Run {
                image,
                name,
                cpus,
                memory,
                command,
            } => cmd_run(&image, name, cpus, memory, &command, cli.format, cli.verbose).await,
            Commands::Exec { sandbox, command } => {
                cmd_exec(&sandbox, &command, cli.format, cli.verbose).await
            }
            Commands::Ps { all } => cmd_ps(all, cli.format).await,
            Commands::Stop { sandbox } => cmd_stop(&sandbox, cli.verbose).await,
            Commands::Rm { sandbox, force } => cmd_rm(&sandbox, force, cli.verbose).await,
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

    /// Run a command in a new sandbox
    async fn cmd_run(
        image: &str,
        name: Option<String>,
        cpus: u32,
        memory: u32,
        command: &[String],
        format: OutputFormat,
        verbose: bool,
    ) -> anyhow::Result<()> {
        let sandbox_name = name.unwrap_or_else(|| format!("sandbox-{}", &uuid::Uuid::new_v4().to_string()[..8]));

        if verbose {
            eprintln!("Creating sandbox '{}' with image '{}'", sandbox_name, image);
        }

        let config = SandboxConfig::builder()
            .name(&sandbox_name)
            .image(image)
            .cpus(cpus)
            .memory_mb(memory)
            .build();

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
                    println!(
                        "{} Sandbox {} started",
                        "✓".green(),
                        sandbox.id().bold()
                    );
                    println!("Run commands with: nanosb exec {} <command>", &sandbox.id()[..12]);
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
        }

        Ok(())
    }

    /// Execute a command in a running sandbox
    async fn cmd_exec(
        sandbox_id: &str,
        command: &[String],
        format: OutputFormat,
        verbose: bool,
    ) -> anyhow::Result<()> {
        if command.is_empty() {
            anyhow::bail!("No command specified. Usage: nanosb exec <sandbox> <command>");
        }

        let registry = SandboxRegistry::new()?;
        
        // Find sandbox by ID or name prefix
        let sandbox_info = registry.list()?.into_iter().find(|s| {
            s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id)
        });

        let sandbox_info = sandbox_info.ok_or_else(|| {
            anyhow::anyhow!("Sandbox not found: {}", sandbox_id)
        })?;

        if sandbox_info.status != SandboxStatus::Running {
            anyhow::bail!(
                "Sandbox {} is not running (status: {:?})",
                sandbox_id,
                sandbox_info.status
            );
        }

        if verbose {
            eprintln!("Found sandbox: {} ({})", sandbox_info.name, sandbox_info.id);
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
                println!("Sandbox {} exists but exec requires runtime integration.", sandbox_id);
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
        let sandbox_info = registry.list()?.into_iter().find(|s| {
            s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id)
        });

        let sandbox_info = sandbox_info.ok_or_else(|| {
            anyhow::anyhow!("Sandbox not found: {}", sandbox_id)
        })?;

        if verbose {
            eprintln!("Stopping sandbox: {} ({})", sandbox_info.name, sandbox_info.id);
        }

        // Update status in registry
        registry.update_status(&sandbox_info.id, SandboxStatus::Stopped)?;

        println!("{} Stopped {}", "✓".green(), sandbox_info.id[..12].to_string().bold());
        Ok(())
    }

    /// Remove a sandbox
    async fn cmd_rm(sandbox_id: &str, force: bool, verbose: bool) -> anyhow::Result<()> {
        let registry = SandboxRegistry::new()?;

        // Find sandbox by ID or name prefix
        let sandbox_info = registry.list()?.into_iter().find(|s| {
            s.id.starts_with(sandbox_id) || s.name.starts_with(sandbox_id)
        });

        let sandbox_info = sandbox_info.ok_or_else(|| {
            anyhow::anyhow!("Sandbox not found: {}", sandbox_id)
        })?;

        if sandbox_info.status == SandboxStatus::Running && !force {
            anyhow::bail!(
                "Sandbox {} is running. Use -f to force removal.",
                sandbox_id
            );
        }

        if verbose {
            eprintln!("Removing sandbox: {} ({})", sandbox_info.name, sandbox_info.id);
        }

        // Remove bundle directory if it exists
        if sandbox_info.bundle_path.exists() {
            std::fs::remove_dir_all(&sandbox_info.bundle_path)?;
        }

        // Unregister from registry
        registry.unregister(&sandbox_info.id)?;

        println!("{} Removed {}", "✓".green(), sandbox_info.id[..12].to_string().bold());
        Ok(())
    }
}

#[cfg(feature = "cli")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}

#[cfg(not(feature = "cli"))]
fn main() {
    eprintln!("CLI feature not enabled. Build with: cargo build --features cli");
    std::process::exit(1);
}
