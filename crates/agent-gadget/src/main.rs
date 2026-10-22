use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_core::{AgentConfig, AgentOptions};
use anyhow::{Context, Result};
use clap::Parser;
use rawkvm_agent_gadget::configfs::{self, Options};
use rawkvm_agent_gadget::sink::{GadgetSink, Paths};
use tracing::{info, warn};

/// Config of the system service.
const SYSTEM_CONFIG: &str = "/etc/rawkvm/agent.toml";

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    #[arg(short, long, default_value = SYSTEM_CONFIG)]
    config: PathBuf,

    /// Hub address (`host` or `host:port`), saved to the config.
    #[arg(long)]
    hub: Option<String>,

    /// Pairing token printed by the hub, saved to the config.
    #[arg(long)]
    token: Option<String>,

    /// Forget the pinned hub certificate (pair again).
    #[arg(long)]
    unpin: bool,

    /// USB device controller to bind (default: the first in /sys/class/udc).
    #[arg(long)]
    udc: Option<String>,

    /// Seconds to wait at startup for the USB device controller to appear
    /// (the dwc2 / musb driver may probe after the service starts).
    #[arg(long, default_value_t = 60, value_name = "SECS")]
    udc_wait: u64,

    /// Do not present the HID++ interface. Use it when the PC runs Linux:
    /// hid-logitech-dj would switch our "receiver" to DJ mode and silence
    /// the keyboard and mouse interfaces (ADR 0007).
    #[arg(long)]
    no_hidpp: bool,

    /// USB serial number to present (default: derived from the node id).
    #[arg(long)]
    serial: Option<String>,

    /// Save --hub / --token / --unpin to the config and exit.
    #[arg(long)]
    configure_only: bool,
}

fn main() -> Result<()> {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_ansi(std::io::stdout().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    if !cfg!(target_os = "linux") {
        anyhow::bail!("the USB gadget agent needs Linux (ConfigFS)");
    }

    let mut cfg = AgentConfig::load_or_init(&args.config)?;
    let mut changed = false;
    if let Some(hub) = args.hub {
        cfg.hub = hub.trim().to_string();
        changed = true;
    }
    if let Some(token) = args.token {
        proto::parse_token(&token).context("--token must be 64 hex characters")?;
        cfg.token = token.trim().to_lowercase();
        changed = true;
    }
    if args.unpin {
        cfg.hub_fingerprint.clear();
        changed = true;
    }
    if changed || args.configure_only {
        cfg.save(&args.config)?;
        info!("saved {}", args.config.display());
    }
    if args.configure_only {
        return Ok(());
    }
    info!(node = %cfg.node_id, name = %cfg.node_name, "starting");

    let serial = args
        .serial
        .unwrap_or_else(|| cfg.node_id.simple().to_string()[..12].to_uppercase());
    let paths = Paths {
        udc: args.udc,
        ..Default::default()
    };
    wait_for_udc(&paths, Duration::from_secs(args.udc_wait))?;
    let mut sink = GadgetSink::new(
        paths,
        Options {
            expose_hidpp: !args.no_hidpp,
            serial,
        },
    );
    // A previous run may have died with the gadget still bound.
    sink.teardown()?;

    let opts = AgentOptions {
        config_path: args.config,
        os: proto::Os::Linux,
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                r = agent_core::run(opts, &mut sink) => r,
                _ = agent_core::shutdown_signal() => Ok(()),
            }
        });
    sink.shutdown();
    result
}

/// Wait until the chosen (or any) UDC exists in `/sys/class/udc`.
fn wait_for_udc(paths: &Paths, timeout: Duration) -> Result<()> {
    let present = |p: &Path| match &paths.udc {
        Some(name) => p.join(name).exists(),
        None => configfs::find_udc(p).is_some(),
    };
    let start = Instant::now();
    let mut warned = false;
    while !present(&paths.udc_class) {
        if start.elapsed() >= timeout {
            anyhow::bail!(
                "no USB device controller in {} after {} s (dtoverlay=dwc2 / OTG port in peripheral mode?)",
                paths.udc_class.display(),
                timeout.as_secs()
            );
        }
        if !warned {
            warn!(
                "waiting for a USB device controller in {}",
                paths.udc_class.display()
            );
            warned = true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if warned {
        info!(
            "USB device controller present after {:.1} s",
            start.elapsed().as_secs_f32()
        );
    }
    Ok(())
}
