//! RawKVM agent for macOS.
//!
//! macOS does not let a user-space process create arbitrary HID devices, so
//! the agent drives the virtual keyboard and pointing device of
//! Karabiner-DriverKit-VirtualHIDDevice through its root-only daemon socket.
//! Reports are decoded with the receiver's own descriptors and their usages
//! copied 1:1 (see `mapper`). The agent therefore runs as root, as a
//! LaunchDaemon. See docs/adr/0005.

mod karabiner;
mod mapper;
mod vhid;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_core::{AgentConfig, AgentOptions, HidSink};
use anyhow::{Context, Result};
use clap::Parser;
use mapper::{Mapper, Output};
use proto::{Edge, HidDevice, Os};
use tracing::{debug, info, warn};
use vhid::{KeyboardParams, VirtualHid};

/// Config used when running as root (the LaunchDaemon).
const SYSTEM_CONFIG: &str = "/Library/Application Support/rawkvm/agent.toml";

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Configuration file. Default: /Library/Application Support/rawkvm/
    /// agent.toml as root, ~/Library/Application Support/rawkvm/agent.toml
    /// otherwise.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Hub address (`host` or `host:port`), saved to the config.
    #[arg(long)]
    hub: Option<String>,

    /// Pairing token printed by the hub on its first start, saved to the
    /// config.
    #[arg(long)]
    token: Option<String>,

    /// Forget the pinned hub certificate (pair again).
    #[arg(long)]
    unpin: bool,

    /// Connect to the hub and log the decoded Karabiner reports instead of
    /// posting them. Needs no root and no Karabiner. Prints keystrokes:
    /// diagnostics only.
    #[arg(long)]
    dry_run: bool,

    /// Karabiner daemon socket.
    #[arg(long, default_value = karabiner::SOCKET_PATH)]
    socket: PathBuf,

    /// HID country code for the virtual keyboard (0 = not supported, which
    /// lets macOS ask for the keyboard type).
    #[arg(long, default_value_t = 0)]
    country_code: u8,
}

struct KarabinerSink {
    /// `None` in dry-run mode.
    vhid: Option<VirtualHid>,
    mapper: Mapper,
    devices: Vec<HidDevice>,
    country_code: u8,
    last_error_log: Option<Instant>,
}

impl KarabinerSink {
    fn post(&mut self, outputs: Vec<Output>) {
        for o in outputs {
            let Some(vhid) = &self.vhid else {
                info!(?o, "dry-run");
                continue;
            };
            let r = match &o {
                Output::Keyboard(k) => vhid.send(karabiner::Request::PostKeyboard, &k.to_bytes()),
                Output::Keys(kind, keys) => vhid.send(kind.request(), &kind.to_bytes(keys)),
                Output::Pointing(p) => vhid.send(karabiner::Request::PostPointing, &p.to_bytes()),
            };
            if let Err(e) = r {
                // Once every 10 s at most; the daemon may be down for a while.
                if self
                    .last_error_log
                    .map_or(true, |t| t.elapsed() > Duration::from_secs(10))
                {
                    warn!("posting to Karabiner: {e:#}");
                    self.last_error_log = Some(Instant::now());
                }
                return;
            }
        }
    }
}

impl HidSink for KarabinerSink {
    fn create(&mut self, dev: &HidDevice) -> Result<()> {
        self.mapper.add_device(dev);
        self.devices.push(dev.clone());
        if let (Some(vhid), Some((vid, pid))) =
            (&self.vhid, Mapper::keyboard_identity(&self.devices))
        {
            vhid.set_keyboard_params(KeyboardParams {
                vendor_id: vid,
                product_id: pid,
                country_code: self.country_code,
            });
        }
        Ok(())
    }

    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()> {
        let out = self.mapper.on_report(dev_id, report);
        self.post(out);
        Ok(())
    }

    fn destroy_all(&mut self) {
        // Keep the Karabiner devices (they are ours for the whole run); just
        // release everything they hold.
        if self.mapper.device_count() > 0 {
            let out = self.mapper.clear();
            self.post(out);
        }
        self.devices.clear();
    }

    fn activate(&mut self, enter_edge: Option<Edge>, pos_ratio: Option<f32>) {
        // Pointer placement needs the user-session helper (phase 2).
        debug!(?enter_edge, ?pos_ratio, "activate");
    }

    fn deactivate(&mut self) {
        let out = self.mapper.release_all();
        self.post(out);
    }
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
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

    let config_path = match args.config {
        Some(p) => p,
        None if is_root() => PathBuf::from(SYSTEM_CONFIG),
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
    info!(node = %cfg.node_id, name = %cfg.node_name, config = %config_path.display(), "starting");

    let vhid = if args.dry_run {
        warn!("dry run: reports are logged, not posted (this prints keystrokes)");
        None
    } else {
        if !is_root() {
            warn!("not running as root: the Karabiner daemon socket is root-only");
        }
        let v = VirtualHid::new(args.socket, KeyboardParams::default());
        if let Err(e) = v.connect() {
            warn!("{e:#}; will keep retrying");
        }
        Some(v)
    };

    let mut sink = KarabinerSink {
        vhid: vhid.clone(),
        mapper: Mapper::new(),
        devices: Vec::new(),
        country_code: args.country_code,
        last_error_log: None,
    };
    let opts = AgentOptions {
        config_path,
        os: Os::MacOs,
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                r = agent_core::run(opts, &mut sink) => r,
                _ = tokio::signal::ctrl_c() => Ok(()),
            }
        });
    sink.destroy_all();
    if let Some(v) = vhid {
        v.terminate();
    }
    result
}
