//! RawKVM edge helper for macOS.
//!
//! The macOS agent is a root LaunchDaemon outside the graphical session,
//! so it cannot see or move the pointer. This helper runs as a per-user
//! LaunchAgent, connects to the hub as `EdgeHelper { for_node: <agent> }`,
//! reports the displays, polls the pointer at about 120 Hz and asks the hub
//! to switch when it touches a linked edge. It needs no Input Monitoring or
//! Accessibility permission. See docs/adr/0009.

#[cfg(target_os = "macos")]
mod cg;

use std::path::{Path, PathBuf};

use agent_core::AgentConfig;
use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use uuid::Uuid;

/// The macOS agent's config (root-only); read by `--from-agent` as root.
const AGENT_SYSTEM_CONFIG: &str = "/Library/Application Support/rawkvm/agent.toml";

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RAWKVM_COMMIT"), ")");

#[derive(Parser, Debug)]
#[command(version = VERSION, about)]
struct Args {
    /// Helper configuration. Default:
    /// ~/Library/Application Support/rawkvm/edge.toml.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Copy hub, token, pinned certificate and `for_node` (its node_id) from
    /// the agent's config into the helper config and exit. The installer
    /// runs this with sudo, since the agent config is root-only.
    #[arg(long, value_name = "AGENT_TOML", num_args = 0..=1, default_missing_value = AGENT_SYSTEM_CONFIG)]
    from_agent: Option<PathBuf>,

    /// node_id of this Mac's agent, saved to the config.
    #[arg(long)]
    for_node: Option<Uuid>,

    /// Hub address (`host` or `host:port`), saved to the config.
    #[arg(long)]
    hub: Option<String>,

    /// Pairing token printed by the hub, saved to the config.
    #[arg(long)]
    token: Option<String>,

    /// Forget the pinned hub certificate.
    #[arg(long)]
    unpin: bool,

    /// Save the options above to the config and exit.
    #[arg(long)]
    configure_only: bool,

    /// Print the displays and then the pointer position (5 per second)
    /// until Ctrl+C. Does not connect to the hub.
    #[arg(long)]
    show: bool,

    /// Print the state of each piece, one line each, and exit: 0 if none
    /// failed. Changes nothing.
    #[arg(long)]
    check: bool,
}

/// `--check`: version, config, `for_node` and the hub, from the config and
/// the LaunchAgent's log.
fn check(path: &Path) -> bool {
    use agent_core::setup::{self, Level, Report};
    let mut r = Report::default();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    r.line(Level::Info, "version", &format!("{VERSION} at {exe}"));
    let (level, detail) = setup::config_line(path, "setup.sh");
    r.line(level, "config", &detail);
    // Read only: load_or_init would create the file.
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|t| AgentConfig::parse(&t).ok())
        .and_then(|c| c.for_node)
    {
        Some(id) => r.line(Level::Ok, "for node", &id.to_string()),
        None => r.line(Level::Fail, "for node", "not set: setup.sh"),
    }
    let log = dirs::home_dir()
        .unwrap_or_default()
        .join("Library/Logs/rawkvm-edge.log");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let run = setup::current_run(&text, "rawkvm_edge_macos: starting");
    let (level, detail) = setup::hub_line(&run, &log);
    r.line(level, "hub", &detail);
    r.ok()
}

fn config_path(args: &Args) -> Result<PathBuf> {
    match &args.config {
        Some(p) => Ok(p.clone()),
        None => Ok(AgentConfig::default_path()?.with_file_name("edge.toml")),
    }
}

/// Apply --from-agent / --for-node / --hub / --token / --unpin.
fn configure(args: &Args, path: &Path) -> Result<bool> {
    let mut cfg = AgentConfig::load_or_init(path)?;
    let mut changed = false;
    if let Some(agent) = &args.from_agent {
        let text = std::fs::read_to_string(agent)
            .with_context(|| format!("reading {} (run with sudo)", agent.display()))?;
        let a =
            AgentConfig::parse(&text).with_context(|| format!("parsing {}", agent.display()))?;
        anyhow::ensure!(
            !a.node_id.is_nil(),
            "{} has no node_id yet",
            agent.display()
        );
        cfg.for_node = Some(a.node_id);
        cfg.hub = a.hub;
        cfg.token = a.token;
        cfg.hub_fingerprint = a.hub_fingerprint;
        cfg.node_name = format!("{} (edge)", a.node_name);
        changed = true;
    }
    if let Some(id) = args.for_node {
        cfg.for_node = Some(id);
        changed = true;
    }
    if let Some(hub) = &args.hub {
        cfg.hub = hub.trim().to_string();
        changed = true;
    }
    if let Some(token) = &args.token {
        proto::parse_token(token).context("--token must be 64 hex characters")?;
        cfg.token = token.trim().to_lowercase();
        changed = true;
    }
    if args.unpin {
        cfg.hub_fingerprint.clear();
        changed = true;
    }
    if changed {
        cfg.save(path)?;
        info!(for_node = ?cfg.for_node, "saved {}", path.display());
    }
    Ok(changed)
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
    let path = config_path(&args)?;
    if args.check {
        std::process::exit(if check(&path) { 0 } else { 1 });
    }
    configure(&args, &path)?;
    if args.configure_only || args.from_agent.is_some() {
        return Ok(());
    }
    platform_main(&args, path)
}

#[cfg(not(target_os = "macos"))]
fn platform_main(_args: &Args, _path: PathBuf) -> Result<()> {
    anyhow::bail!("rawkvm-edge-macos only runs on macOS")
}

#[cfg(target_os = "macos")]
fn platform_main(args: &Args, path: PathBuf) -> Result<()> {
    if args.show {
        return show();
    }
    let cfg = AgentConfig::load_or_init(&path)?;
    info!(version = VERSION, node = %cfg.node_id, for_node = ?cfg.for_node, config = %path.display(), "starting");
    let mut platform = MacScreens::new();
    let opts = agent_core::AgentOptions {
        config_path: path,
        os: proto::Os::MacOs,
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                r = agent_core::helper::run_helper(opts, &mut platform) => r,
                _ = agent_core::shutdown_signal() => Ok(()),
            }
        })
}

#[cfg(target_os = "macos")]
struct MacScreens {
    warper: cg::Warper,
}

#[cfg(target_os = "macos")]
impl MacScreens {
    fn new() -> Self {
        Self {
            warper: cg::Warper::new(),
        }
    }
}

#[cfg(target_os = "macos")]
impl agent_core::helper::EdgePlatform for MacScreens {
    fn screens(&mut self) -> Vec<proto::Screen> {
        cg::displays().into_iter().map(|d| d.screen).collect()
    }
    fn pointer(&mut self) -> Option<agent_core::edge::Point> {
        cg::pointer()
    }
    fn warp(&mut self, to: agent_core::edge::Point) {
        if let Err(e) = self.warper.warp(to) {
            tracing::warn!(?to, "CGWarpMouseCursorPosition failed: {e}");
        }
    }
}

#[cfg(target_os = "macos")]
fn show() -> Result<()> {
    let displays = cg::displays();
    println!("displays (index, id, origin, size in points, Retina factor):");
    for d in &displays {
        let s = &d.screen;
        println!(
            "  {}  id {:<10} ({}, {})  {}x{}  x{:.1}{}",
            s.index,
            d.id,
            s.x,
            s.y,
            s.w,
            s.h,
            d.backing_scale,
            if d.main { "  main" } else { "" }
        );
    }
    println!("pointer (Ctrl+C to stop):");
    let mut last = None;
    loop {
        let p = cg::pointer();
        if p != last {
            match p {
                Some(p) => {
                    let on = displays.iter().map(|d| &d.screen).find(|s| {
                        p.x >= s.x && p.x < s.x + s.w as i32 && p.y >= s.y && p.y < s.y + s.h as i32
                    });
                    let edges = on
                        .map(|s| {
                            let mut e = Vec::new();
                            if p.x == s.x {
                                e.push("left");
                            }
                            if p.x == s.x + s.w as i32 - 1 {
                                e.push("right");
                            }
                            if p.y == s.y {
                                e.push("top");
                            }
                            if p.y == s.y + s.h as i32 - 1 {
                                e.push("bottom");
                            }
                            e.join("+")
                        })
                        .unwrap_or_default();
                    println!(
                        "  ({}, {})  screen {}  {}",
                        p.x,
                        p.y,
                        on.map_or("-".to_string(), |s| s.index.to_string()),
                        edges
                    );
                }
                None => println!("  pointer unavailable"),
            }
            last = p;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
