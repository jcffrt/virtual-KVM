//! RawKVM agent for Linux. Every HID device of the hub becomes a `/dev/uhid`
//! device with the same report descriptor; the kernel and libinput then see
//! the Logitech receiver as if it were plugged in locally.

mod edge;
mod setup;
mod uhid;

use std::collections::HashMap;
use std::path::PathBuf;

use agent_core::{AgentConfig, AgentOptions, HidSink, OutputReceiver, OutputSender};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use proto::{HidDevice, NodeKind, Os};
use tracing::info;

/// Crate version and git commit of this build.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RAWKVM_COMMIT"), ")");

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Bus {
    /// BUS_VIRTUAL: only hid-generic binds (default, see docs/adr/0002).
    Virtual,
    /// BUS_USB: vendor drivers such as hid-logitech-dj may bind.
    Usb,
}

#[derive(Parser, Debug)]
#[command(version = VERSION, about)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Configuration file (default: ~/.config/rawkvm/agent.toml).
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Hub address (`host` or `host:port`), saved to the config. Without
    /// one the hub is discovered with mDNS.
    #[arg(long)]
    hub: Option<String>,

    /// Pairing token printed by the hub, saved to the config. It stays in
    /// the shell history: prefer --token-file or the --pair prompt.
    #[arg(long)]
    token: Option<String>,

    /// File holding the pairing token, saved to the config.
    #[arg(long, value_name = "FILE")]
    token_file: Option<PathBuf>,

    /// Pair and exit: forget the pinned certificate, ask for the token if
    /// the config has none, connect once and pin the hub.
    #[arg(long)]
    pair: bool,

    /// Show the state of the installation (read only) and exit.
    #[arg(long, conflicts_with = "pair")]
    check: bool,

    /// Forget the pinned hub certificate (pair again).
    #[arg(long)]
    unpin: bool,

    /// Bus type of the virtual devices.
    #[arg(long, value_enum, default_value_t = Bus::Virtual)]
    bus: Bus,

    #[arg(long, default_value = "/dev/uhid")]
    uhid: PathBuf,

    /// Do not run the edge helper (screen-edge switching through the GNOME
    /// extension rawkvm@olitec.cc, docs/gnome-plan.md).
    #[arg(long)]
    no_edge: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// After `apt install`: pair, enable the user service and the GNOME
    /// extension, and check. Run as your own user. Safe to repeat.
    Setup {
        /// Hub address; without it, the hubs found by mDNS are offered.
        #[arg(long)]
        hub: Option<String>,
        /// File holding the pairing token (else it is asked, hidden).
        #[arg(long, value_name = "FILE")]
        token_file: Option<PathBuf>,
        /// Pair again even if already paired.
        #[arg(long)]
        pair: bool,
        /// Leave the GNOME extension alone.
        #[arg(long)]
        no_extension: bool,
    },
}

struct UhidSink {
    path: PathBuf,
    bus: u16,
    devices: HashMap<u8, uhid::UhidDevice>,
    outputs: (OutputSender, Option<OutputReceiver>),
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
            Some((dev.id, self.outputs.0.clone())),
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

    fn take_outputs(&mut self) -> Option<OutputReceiver> {
        self.outputs.1.take()
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
    agent_core::supervise::log_panics();
    let args = Args::parse();
    if !cfg!(target_os = "linux") {
        anyhow::bail!("this agent needs Linux (/dev/uhid)");
    }

    let config_path = match args.config {
        Some(p) => p,
        None => AgentConfig::default_path()?,
    };
    let runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
    };
    if args.check || args.cmd.is_some() {
        setup::use_session_bus();
    }
    if let Some(Cmd::Setup {
        hub,
        token_file,
        pair,
        no_extension,
    }) = args.cmd
    {
        let a = setup::SetupArgs {
            hub,
            token_file,
            pair,
            no_extension,
        };
        return runtime()?.block_on(setup::setup(&config_path, a));
    }
    if args.check {
        let ok = runtime()?.block_on(setup::check(&config_path))?;
        std::process::exit(if ok { 0 } else { 1 });
    }
    let cfg = agent_core::pairing::PairArgs {
        hub: args.hub,
        token: args.token,
        token_file: args.token_file,
        unpin: args.unpin,
        pair: args.pair,
    }
    .apply(&config_path)?;
    if args.pair {
        let opts = AgentOptions {
            config_path,
            os: Os::Linux,
        };
        let addr = runtime()?.block_on(agent_core::pairing::pair(&opts, NodeKind::Agent))?;
        println!("paired with the hub at {addr}; start the service:");
        println!("  systemctl --user enable --now {}", setup::UNIT);
        return Ok(());
    }
    info!(node = %cfg.node_id, name = %cfg.node_name, "starting");

    let mut sink = UhidSink {
        path: args.uhid,
        bus: match args.bus {
            Bus::Virtual => uhid::BUS_VIRTUAL,
            Bus::Usb => uhid::BUS_USB,
        },
        devices: HashMap::new(),
        outputs: {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            (tx, Some(rx))
        },
    };
    let edge_config = config_path.clone();
    let opts = AgentOptions {
        config_path,
        os: Os::Linux,
    };
    let no_edge = args.no_edge;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            // The edge helper never ends the agent: on a fatal error of its
            // own it logs and stops, and the agent keeps the devices.
            let edge = async {
                if no_edge {
                    return std::future::pending().await;
                }
                if let Err(e) = edge::run(&edge_config).await {
                    tracing::error!("edge helper stopped: {e:#}");
                }
                std::future::pending::<()>().await
            };
            tokio::select! {
                r = agent_core::run(opts, &mut sink) => r,
                _ = edge => Ok(()),
                _ = agent_core::shutdown_signal() => Ok(()),
            }
        })
}
