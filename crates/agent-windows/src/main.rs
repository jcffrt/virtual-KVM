//! RawKVM agent for Windows. Every HID device of the hub becomes a virtual
//! HID device of the RawKVM VHF driver with the same report descriptor, so
//! Windows sees the Logitech receiver as if it were plugged in (ADR 0006).
//! Runs as a service under LocalSystem.

use agent_core::AgentConfig;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rawkvm_agent_windows::{config_path, data_dir};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run in the foreground, logging to the console (default).
    Run,
    /// Entry point used by the service control manager.
    Service,
    /// Create the Root\RawKvmVhf device if needed and install the driver
    /// from its (test-signed) INF. Run as Administrator.
    DriverInstall {
        #[arg(long)]
        inf: std::path::PathBuf,
    },
    /// Save hub address and pairing token in the config and exit.
    Configure {
        #[arg(long)]
        hub: Option<String>,
        #[arg(long)]
        token: Option<String>,
        /// Forget the pinned hub certificate (pair again).
        #[arg(long)]
        unpin: bool,
    },
}

fn init_logging(to_file: bool) -> Result<()> {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if to_file {
        std::fs::create_dir_all(data_dir())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir().join("agent.log"))
            .context("opening agent.log")?;
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .init();
    } else {
        use std::io::IsTerminal;
        tracing_subscriber::fmt()
            .with_ansi(std::io::stdout().is_terminal())
            .with_env_filter(filter)
            .init();
    }
    Ok(())
}

fn configure(hub: Option<String>, token: Option<String>, unpin: bool) -> Result<()> {
    let path = config_path();
    let mut cfg = AgentConfig::load_or_init(&path)?;
    if let Some(hub) = hub {
        cfg.hub = hub.trim().to_string();
    }
    if let Some(token) = token {
        proto::parse_token(&token).context("--token must be 64 hex characters")?;
        cfg.token = token.trim().to_lowercase();
    }
    if unpin {
        cfg.hub_fingerprint.clear();
    }
    cfg.save(&path)?;
    println!("saved {}", path.display());
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command.unwrap_or(Command::Run) {
        Command::Configure { hub, token, unpin } => configure(hub, token, unpin),
        #[cfg(windows)]
        Command::DriverInstall { inf } => {
            if rawkvm_agent_windows::setup::install(&inf)? {
                println!("Windows asks for a reboot to finish the driver installation");
            }
            Ok(())
        }
        #[cfg(not(windows))]
        Command::DriverInstall { .. } => anyhow::bail!("driver-install needs Windows"),
        #[cfg(windows)]
        Command::Service => {
            init_logging(true)?;
            rawkvm_agent_windows::service::dispatch()
        }
        #[cfg(windows)]
        Command::Run => {
            init_logging(false)?;
            rawkvm_agent_windows::run_agent(agent_core::shutdown_signal())
        }
        #[cfg(not(windows))]
        Command::Service | Command::Run => {
            init_logging(false)?;
            tracing::info!("config would be {}", config_path().display());
            anyhow::bail!("this agent needs Windows (RawKVM VHF driver)")
        }
    }
}
