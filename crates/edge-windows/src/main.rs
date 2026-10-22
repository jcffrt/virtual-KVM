//! RawKVM edge helper for Windows.
//!
//! The Windows PC receives the keyboard and mouse from the USB gadget and
//! runs nothing else of RawKVM, so it cannot tell the hub when the pointer
//! reaches a screen edge. This helper does that, for the gadget's node:
//! it connects to the hub as `EdgeHelper { for_node: <gadget> }`, reports
//! the monitors, polls `GetCursorPos` at about 120 Hz and asks the hub to
//! switch at linked edges; when the input comes back it places the pointer
//! with `SetCursorPos`. No driver, no hook, no administrator rights. It
//! starts at logon from `HKCU\...\Run` (deploy/windows/install-edge.ps1).
//! See docs/adr/0009 and docs/windows-borde.md.

// No console window when started at logon; CLI commands attach to the
// parent console instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod win;

use std::path::{Path, PathBuf};

use agent_core::AgentConfig;
use anyhow::{Context, Result};
use clap::Parser;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Helper configuration. Default: %APPDATA%\rawkvm\edge.toml.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// node_id of the agent whose machine this is: the USB gadget's
    /// (`sudo rawkvm-hub --list-nodes` on the hub), saved to the config.
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

    /// Print the monitors and then the pointer position (5 per second)
    /// until Ctrl+C. Does not connect to the hub.
    #[arg(long)]
    show: bool,

    /// Log file. Default: %LOCALAPPDATA%\rawkvm\edge.log.
    #[arg(long)]
    log: Option<PathBuf>,
}

fn config_path(args: &Args) -> Result<PathBuf> {
    match &args.config {
        Some(p) => Ok(p.clone()),
        None => Ok(AgentConfig::default_path()?.with_file_name("edge.toml")),
    }
}

fn log_path(args: &Args) -> PathBuf {
    args.log.clone().unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("rawkvm")
            .join("edge.log")
    })
}

/// Apply --for-node / --hub / --token / --unpin. Returns whether anything
/// changed.
fn configure(args: &Args, path: &Path) -> Result<bool> {
    let mut cfg = AgentConfig::load_or_init(path)?;
    let mut changed = false;
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
        if !cfg.node_name.ends_with("(edge)") {
            cfg.node_name = format!("{} (edge)", cfg.node_name);
        }
        cfg.save(path)?;
        println!("saved {} (for_node = {:?})", path.display(), cfg.for_node);
    }
    Ok(changed)
}

/// Log to stdout for CLI commands, to a file (truncated past 4 MiB) when
/// running as the logon helper.
fn init_logging(to_file: Option<&Path>) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let file = to_file.and_then(|p| {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let big = std::fs::metadata(p)
            .map(|m| m.len() > 4 << 20)
            .unwrap_or(false);
        std::fs::OpenOptions::new()
            .create(true)
            .append(!big)
            .write(true)
            .truncate(big)
            .open(p)
            .ok()
    });
    match file {
        Some(f) => tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        None => tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(filter)
            .init(),
    }
}

fn main() -> Result<()> {
    #[cfg(windows)]
    win::attach_console();
    let args = Args::parse();
    let path = config_path(&args)?;
    let cli = args.configure_only || args.show || configure_requested(&args);
    let log = log_path(&args);
    init_logging((!cli).then_some(log.as_path()));
    configure(&args, &path)?;
    if args.configure_only || (cli && !args.show) {
        return Ok(());
    }
    platform_main(&args, path)
}

fn configure_requested(args: &Args) -> bool {
    args.for_node.is_some() || args.hub.is_some() || args.token.is_some() || args.unpin
}

#[cfg(not(windows))]
fn platform_main(_args: &Args, _path: PathBuf) -> Result<()> {
    anyhow::bail!("rawkvm-edge (Windows) only runs on Windows")
}

#[cfg(windows)]
fn platform_main(args: &Args, path: PathBuf) -> Result<()> {
    if !win::dpi_aware() {
        tracing::warn!("could not become per-monitor DPI aware; coordinates may be scaled");
    }
    if args.show {
        return show();
    }
    let cfg = AgentConfig::load_or_init(&path)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        node = %cfg.node_id,
        for_node = ?cfg.for_node,
        config = %path.display(),
        "rawkvm-edge starting"
    );
    let opts = agent_core::AgentOptions {
        config_path: path,
        os: proto::Os::Windows,
    };
    let mut platform = WinScreens;
    let r = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                r = agent_core::helper::run_helper(opts, &mut platform) => r,
                _ = tokio::signal::ctrl_c() => Ok(()),
            }
        });
    if let Err(e) = &r {
        tracing::error!("{e:#}");
    }
    r
}

#[cfg(windows)]
struct WinScreens;

#[cfg(windows)]
impl agent_core::helper::EdgePlatform for WinScreens {
    fn screens(&mut self) -> Vec<proto::Screen> {
        win::screens()
    }
    fn pointer(&mut self) -> Option<agent_core::edge::Point> {
        win::pointer()
    }
    fn warp(&mut self, to: agent_core::edge::Point) {
        if !win::set_pointer(to) {
            tracing::warn!(
                ?to,
                "SetCursorPos failed (secure desktop or elevated window?)"
            );
        }
    }
}

#[cfg(windows)]
fn show() -> Result<()> {
    let screens = win::screens();
    println!("monitors (index, origin, size in pixels, scale):");
    for s in &screens {
        println!(
            "  {}  ({}, {})  {}x{}  x{:.2}{}",
            s.index,
            s.x,
            s.y,
            s.w,
            s.h,
            s.scale,
            if s.index == 0 { "  primary" } else { "" }
        );
    }
    println!("pointer (Ctrl+C to stop):");
    let mut last = None;
    loop {
        let p = win::pointer();
        if p != last {
            match p {
                Some(p) => {
                    let on = screens.iter().find(|s| {
                        p.x >= s.x && p.x < s.x + s.w as i32 && p.y >= s.y && p.y < s.y + s.h as i32
                    });
                    let mut edges = Vec::new();
                    if let Some(s) = on {
                        if p.x == s.x {
                            edges.push("left");
                        }
                        if p.x == s.x + s.w as i32 - 1 {
                            edges.push("right");
                        }
                        if p.y == s.y {
                            edges.push("top");
                        }
                        if p.y == s.y + s.h as i32 - 1 {
                            edges.push("bottom");
                        }
                    }
                    println!(
                        "  ({}, {})  monitor {}  {}",
                        p.x,
                        p.y,
                        on.map_or("-".to_string(), |s| s.index.to_string()),
                        edges.join("+")
                    );
                }
                None => println!("  pointer unavailable (secure desktop?)"),
            }
            last = p;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
