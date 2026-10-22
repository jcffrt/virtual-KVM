//! RawKVM agent for Linux. Every HID device of the hub becomes a `/dev/uhid`
//! device with the same report descriptor; the kernel and libinput then see
//! the Logitech receiver as if it were plugged in locally.

mod uhid;

use std::collections::HashMap;
use std::path::PathBuf;

use agent_core::{AgentConfig, AgentOptions, HidSink};
use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use proto::{HidDevice, Os};
use tracing::info;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Bus {
    /// BUS_VIRTUAL: only hid-generic binds (default, see docs/adr/0002).
    Virtual,
    /// BUS_USB: vendor drivers such as hid-logitech-dj may bind.
    Usb,
}

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Configuration file (default: ~/.config/rawkvm/agent.toml).
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Hub address (`host` or `host:port`), saved to the config. Without
    /// one the hub is discovered with mDNS.
    #[arg(long)]
    hub: Option<String>,

    /// Pairing token printed by the hub on its first start, saved to the
    /// config.
    #[arg(long)]
    token: Option<String>,

    /// Forget the pinned hub certificate (pair again).
    #[arg(long)]
    unpin: bool,

    /// Bus type of the virtual devices.
    #[arg(long, value_enum, default_value_t = Bus::Virtual)]
    bus: Bus,

    #[arg(long, default_value = "/dev/uhid")]
    uhid: PathBuf,
}

struct UhidSink {
    path: PathBuf,
    bus: u16,
    devices: HashMap<u8, uhid::UhidDevice>,
}

impl HidSink for UhidSink {
    fn create(&mut self, dev: &HidDevice) -> Result<()> {
        let phys = format!("rawkvm/input{}", dev.interface);
        let d = uhid::UhidDevice::create(
            &self.path,
            &uhid::CreateParams {
                name: &dev.name,
                phys: &phys,
                uniq: dev.serial.as_deref().unwrap_or(""),
                bus: self.bus,
                vendor: dev.vid.into(),
                product: dev.pid.into(),
                version: 0,
                descriptor: &dev.report_descriptor,
            },
        )?;
        self.devices.insert(dev.id, d);
        Ok(())
    }

    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()> {
        if let Some(d) = self.devices.get(&dev_id) {
            d.input(report).context("UHID_INPUT2")?;
        }
        Ok(())
    }

    fn destroy_all(&mut self) {
        if !self.devices.is_empty() {
            info!("removing {} virtual devices", self.devices.len());
        }
        self.devices.clear();
    }
}

fn main() -> Result<()> {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        // No colour codes in journald.
        .with_ansi(std::io::stdout().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    if !cfg!(target_os = "linux") {
        anyhow::bail!("this agent needs Linux (/dev/uhid)");
    }

    let config_path = match args.config {
        Some(p) => p,
        None => AgentConfig::default_path()?,
    };
    let mut cfg = AgentConfig::load_or_init(&config_path)?;
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
    if changed {
        cfg.save(&config_path)?;
        info!("saved {}", config_path.display());
    }
    info!(node = %cfg.node_id, name = %cfg.node_name, "starting");

    let mut sink = UhidSink {
        path: args.uhid,
        bus: match args.bus {
            Bus::Virtual => uhid::BUS_VIRTUAL,
            Bus::Usb => uhid::BUS_USB,
        },
        devices: HashMap::new(),
    };
    let opts = AgentOptions {
        config_path,
        os: Os::Linux,
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                r = agent_core::run(opts, &mut sink) => r,
                _ = agent_core::shutdown_signal() => Ok(()),
            }
        })
}
