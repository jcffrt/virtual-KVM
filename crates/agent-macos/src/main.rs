//! RawKVM agent for macOS.
//!
//! macOS does not let a user-space process create arbitrary HID devices, so
//! the agent drives the virtual keyboard and pointing device of
//! Karabiner-DriverKit-VirtualHIDDevice through its root-only daemon socket.
//! Reports are decoded with the receiver's own descriptors and their usages
//! copied 1:1 (see `mapper`). The agent therefore runs as root, as a
//! LaunchDaemon. See docs/adr/0005.

mod karabiner;
mod leds;
mod mapper;
mod vhid;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_core::{AgentConfig, AgentOptions, HidSink, OutputReceiver};
use anyhow::Result;
use clap::Parser;
use mapper::{Mapper, Output};
use proto::{Edge, HidDevice, NodeKind, Os};
use tracing::{debug, info, warn};
use vhid::{KeyboardParams, VirtualHid};

/// Config used when running as root (the LaunchDaemon).
const SYSTEM_CONFIG: &str = "/Library/Application Support/rawkvm/agent.toml";
/// Where the LaunchDaemon writes its log (cc.olitec.rawkvm.agent.plist).
const SYSTEM_LOG: &str = "/var/log/rawkvm-agent.log";

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RAWKVM_COMMIT"), ")");

#[derive(Parser, Debug)]
#[command(version = VERSION, about)]
struct Args {
    /// Configuration file. Default: /Library/Application Support/rawkvm/
    /// agent.toml as root, ~/Library/Application Support/rawkvm/agent.toml
    /// otherwise.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Hub address (`host` or `host:port`), saved to the config.
    #[arg(long)]
    hub: Option<String>,

    /// Pairing token, saved to the config. Stays in the shell history:
    /// prefer --token-file or the --pair prompt.
    #[arg(long, hide = true)]
    token: Option<String>,

    /// File holding the pairing token, saved to the config.
    #[arg(long, value_name = "FILE")]
    token_file: Option<PathBuf>,

    /// Connect once, pin the hub certificate and exit (asks for the token,
    /// hidden, if there is none). Stop the service first; setup.sh does.
    #[arg(long)]
    pair: bool,

    /// Print the state of each piece, one line each, and exit: 0 if none
    /// failed. Changes nothing. Run with sudo to read the config.
    #[arg(long, conflicts_with = "pair")]
    check: bool,

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
    /// A post failed: Karabiner may hold a state the mapper no longer has
    /// (a lost release). The whole state is posted again at the next chance.
    resync: bool,
    /// Last keyboard report from the hub, for the held-key watchdog.
    last_keyboard_report: Instant,
    held_warned: bool,
    /// Where the Mac's keyboard LEDs go (see `leds`).
    led_target: leds::Target,
    outputs: Option<OutputReceiver>,
}

/// A key held this long with no new keyboard report is logged (it may be
/// held on purpose; nothing is released automatically).
const HELD_WARN: Duration = Duration::from_secs(10);

impl KarabinerSink {
    /// Post the mapper's outputs; after a failure, post the whole state
    /// instead until it goes through.
    fn deliver(&mut self, outputs: Vec<Output>) {
        let outputs = if self.resync {
            self.mapper.full_state()
        } else {
            outputs
        };
        self.resync = !self.post(outputs);
    }

    /// Returns false if a post failed (the rest is skipped).
    fn post(&mut self, outputs: Vec<Output>) -> bool {
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
                    warn!("posting to Karabiner: {e:#}; the full state will be posted again");
                    self.last_error_log = Some(Instant::now());
                }
                return false;
            }
        }
        true
    }
}

impl HidSink for KarabinerSink {
    fn create(&mut self, dev: &HidDevice) -> Result<()> {
        self.mapper.add_device(dev);
        self.devices.push(dev.clone());
        // Every keyboard of the list gets the LEDs. Rebuilt from the list
        // being created; kept across a disconnection (agent-core drops
        // reports for devices it no longer has).
        *self.led_target.lock().unwrap_or_else(|e| e.into_inner()) =
            self.devices.iter().filter_map(leds::led_report).collect();
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
        if self.mapper.is_keyboard_report(dev_id, report) {
            self.last_keyboard_report = Instant::now();
            self.held_warned = false;
        }
        let out = self.mapper.on_report(dev_id, report);
        self.deliver(out);
        Ok(())
    }

    fn tick(&mut self, now: Instant) {
        if self.resync {
            self.deliver(Vec::new());
        }
        if self.mapper.keyboard_held() && !self.held_warned {
            let idle = now.duration_since(self.last_keyboard_report);
            if idle >= HELD_WARN {
                // No key name: the log must not record what is typed.
                warn!(
                    secs = idle.as_secs(),
                    "a key or modifier has been held with no new keyboard report from the hub; \
                     if it is stuck, use «Liberar teclas» in the widget or press that key once"
                );
                self.held_warned = true;
            }
        }
    }

    fn destroy_all(&mut self) {
        // Keep the Karabiner devices (they are ours for the whole run); just
        // release everything they hold.
        if self.mapper.device_count() > 0 {
            let out = self.mapper.clear();
            self.resync = false;
            self.deliver(out);
        }
        self.devices.clear();
    }

    fn activate(&mut self, enter_edge: Option<Edge>, pos_ratio: Option<f32>) {
        // Pointer placement needs the user-session helper (phase 2).
        debug!(?enter_edge, ?pos_ratio, "activate");
    }

    fn deactivate(&mut self, keep: &[u8]) {
        // With devices to keep (an exclusive receiver, ADR 0030) the core's
        // per-device release reports already went through the mapper; a
        // global release would drop the kept keys too.
        if !keep.is_empty() {
            return;
        }
        let out = self.mapper.release_all();
        self.resync = false;
        self.deliver(out);
    }

    fn take_outputs(&mut self) -> Option<OutputReceiver> {
        self.outputs.take()
    }
}

/// `--check`: version, config, hub, Karabiner and the LED permission,
/// from the config and the service's log (setup.sh shows these lines).
fn check(config: &Path, log: &Path) -> bool {
    use agent_core::setup::{self, Level, Report};
    let mut r = Report::default();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    r.line(Level::Info, "version", &format!("{VERSION} at {exe}"));
    match std::fs::File::open(config) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => r.line(
            Level::Warn,
            "config",
            &format!("{}: run with sudo to read it", config.display()),
        ),
        _ => {
            let (level, detail) = setup::config_line(config, "setup.sh");
            r.line(level, "config", &detail);
        }
    }
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let run = setup::current_run(&text, "rawkvm_agent_macos: starting");
    let (level, detail) = setup::hub_line(&run, log);
    r.line(level, "hub", &detail);
    let karabiner = setup::last_of(
        &run,
        &[
            ("kind=KeyboardReady value=true", Level::Ok),
            ("not activated", Level::Fail),
            ("version mismatch", Level::Fail),
            ("Karabiner-VirtualHIDDevice-Daemon running", Level::Fail),
            ("disconnected from the Karabiner daemon", Level::Warn),
            ("kind=KeyboardReady value=false", Level::Warn),
        ],
    )
    .unwrap_or((
        Level::Warn,
        format!("no Karabiner lines in {}", log.display()),
    ));
    r.line(karabiner.0, "karabiner", &karabiner.1);
    // Without Input Monitoring only the Caps Lock LED is missing.
    let leds = setup::last_of(
        &run,
        &[
            ("watching the keyboard LEDs", Level::Ok),
            ("keyboard LEDs not watched", Level::Warn),
        ],
    )
    .unwrap_or((Level::Warn, "no LED lines yet".into()));
    r.line(leds.0, "leds", &leds.1);
    r.ok()
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
    agent_core::supervise::log_panics();
    let args = Args::parse();

    let config_path = match args.config {
        Some(p) => p,
        // --check looks at what the service uses.
        None if is_root() || args.check => PathBuf::from(SYSTEM_CONFIG),
        None => AgentConfig::default_path()?,
    };
    let runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
    };
    if args.check {
        std::process::exit(if check(&config_path, Path::new(SYSTEM_LOG)) {
            0
        } else {
            1
        });
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
            os: Os::MacOs,
        };
        let addr = runtime()?.block_on(agent_core::pairing::pair(&opts, NodeKind::Agent))?;
        println!("paired with the hub at {addr}");
        return Ok(());
    }
    info!(version = VERSION, node = %cfg.node_id, name = %cfg.node_name, config = %config_path.display(), "starting");

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

    let led_target = leds::Target::default();
    let outputs = if args.dry_run {
        None
    } else {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        leds::spawn(tx, led_target.clone());
        Some(rx)
    };
    let mut sink = KarabinerSink {
        vhid: vhid.clone(),
        mapper: Mapper::new(),
        devices: Vec::new(),
        country_code: args.country_code,
        last_error_log: None,
        resync: false,
        last_keyboard_report: Instant::now(),
        held_warned: false,
        led_target,
        outputs,
    };
    let opts = AgentOptions {
        config_path,
        os: Os::MacOs,
    };
    let result = runtime()?.block_on(async {
        tokio::select! {
            r = agent_core::run(opts, &mut sink) => r,
            _ = agent_core::shutdown_signal() => Ok(()),
        }
    });
    sink.destroy_all();
    if let Some(v) = vhid {
        v.terminate();
    }
    result
}
