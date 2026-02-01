//! Nanosandbox CLI

#[cfg(feature = "cli")]
use clap::{Parser, Subcommand};

#[cfg(feature = "cli")]
#[derive(Parser)]
#[command(name = "nano")]
#[command(about = "Nanosandbox - VM-based sandbox management")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[cfg(feature = "cli")]
#[derive(Subcommand)]
enum Commands {
    /// Pull an image from a registry
    Pull {
        /// Image reference (e.g., ghcr.io/devdone-labs/dd-agents:latest)
        image: String,
    },

    /// List cached images
    Images,

    /// Run a command in a new sandbox
    Run {
        /// Image to use
        image: String,

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

    /// List running sandboxes
    Ps,

    /// Stop a running sandbox
    Stop {
        /// Sandbox ID or name
        sandbox: String,
    },

    /// Remove a sandbox
    Rm {
        /// Sandbox ID or name
        sandbox: String,

        /// Force removal
        #[arg(short, long)]
        force: bool,
    },
}

#[cfg(feature = "cli")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Pull { image } => {
            println!("Pulling image: {}", image);
            // TODO: Implement
        }
        Commands::Images => {
            println!("Listing images...");
            // TODO: Implement
        }
        Commands::Run { image, command } => {
            println!("Running {} with {:?}", image, command);
            // TODO: Implement
        }
        Commands::Exec { sandbox, command } => {
            println!("Executing in {}: {:?}", sandbox, command);
            // TODO: Implement
        }
        Commands::Ps => {
            println!("Listing sandboxes...");
            // TODO: Implement
        }
        Commands::Stop { sandbox } => {
            println!("Stopping sandbox: {}", sandbox);
            // TODO: Implement
        }
        Commands::Rm { sandbox, force } => {
            println!("Removing sandbox: {} (force={})", sandbox, force);
            // TODO: Implement
        }
    }

    Ok(())
}

#[cfg(not(feature = "cli"))]
fn main() {
    eprintln!("CLI feature not enabled. Build with: cargo build --features cli");
    std::process::exit(1);
}
