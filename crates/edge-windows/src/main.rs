//! RawKVM edge helper for Windows.
//!
//! The Windows PC receives the keyboard and mouse from the USB gadget and
//! runs nothing else of RawKVM, so it cannot tell the hub when the pointer
//! reaches a screen edge. This helper does that, for the gadget's node:
//! it connects to the hub as `EdgeHelper { for_node: <gadget> }`, reports
//! the monitors, polls `GetCursorPos` at about 120 Hz and asks the hub to
//! switch at linked edges; when the input comes back it places the pointer
//! with `SetCursorPos`. No driver, no hook, no administrator rights. It
//! starts at logon from `HKCU\...\Run` (deploy/windows/setup.ps1, which
//! also runs `--pair`). See docs/adr/0009, docs/adr/0024 and
//! docs/windows-instalacion.md.

// No console window when started at logon; CLI commands attach to the
// parent console instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod win;

use std::path::{Path, PathBuf};

use agent_core::pairing::PairArgs;
use agent_core::setup::{Level, Report};
use agent_core::{AgentConfig, AgentOptions};
use anyhow::{bail, Result};
use clap::Parser;
use proto::{HubStatus, NodeInfo, NodeKind, Os};
use uuid::Uuid;

/// Crate version and git commit of this build.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RAWKVM_COMMIT"), ")");

#[derive(Parser, Debug)]
#[command(version = VERSION, about)]
struct Args {
    /// Helper configuration. Default: %APPDATA%\rawkvm\edge.toml.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// node_id of the agent whose machine this is: the USB gadget's
    /// (`sudo rawkvm-hub --list-nodes` on the hub), saved to the config.
    /// `--pair` finds it when the hub has a single Windows agent.
    #[arg(long)]
    for_node: Option<Uuid>,

    /// Hub address (`host` or `host:port`), saved to the config.
    #[arg(long)]
    hub: Option<String>,

    /// Pairing token printed by the hub, saved to the config. It stays in
    /// the shell history: prefer --token-file.
    #[arg(long, hide = true)]
    token: Option<String>,

    /// File holding the pairing token (`sudo rawkvm-hub --show-token`).
    #[arg(long, value_name = "FILE")]
    token_file: Option<PathBuf>,

    /// Forget the pinned hub certificate.
    #[arg(long)]
    unpin: bool,

    /// Pair and exit: connect once, pin the hub certificate, and find the
    /// agent of this PC (the USB gadget) unless --for-node names it. Needs
    /// a token: --token-file, or one already saved.
    #[arg(long)]
    pair: bool,

    /// Print the state of the configuration and of the hub connection
    /// (from the log), one line per piece, and exit. Changes nothing.
    #[arg(long)]
    check: bool,

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

    /// Internal: this is the helper itself, started by the copy that
    /// restarts it when it dies (ADR 0027).
    #[arg(long, hide = true)]
    supervised: bool,
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

/// Apply --for-node / --hub / --token / --token-file / --unpin. Returns
/// whether anything changed.
fn configure(args: &Args, path: &Path) -> Result<bool> {
    let before = AgentConfig::load_or_init(path)?;
    PairArgs {
        hub: args.hub.clone(),
        token: args.token.clone(),
        token_file: args.token_file.clone(),
        unpin: args.unpin,
        pair: false,
    }
    .apply(path)?;
    let mut cfg = AgentConfig::load_or_init(path)?;
    if let Some(id) = args.for_node {
        cfg.for_node = Some(id);
    }
    if !cfg.node_name.ends_with("(edge)") {
        cfg.node_name = format!("{} (edge)", cfg.node_name);
    }
    let changed = cfg != before;
    if changed {
        cfg.save(path)?;
        println!("saved {} (for_node = {:?})", path.display(), cfg.for_node);
    }
    Ok(changed)
}

/// `--pair`: connect once (as a widget, to read the hub's node list), pin
/// the certificate, and choose the agent this helper acts for.
async fn pair(args: &Args, path: &Path) -> Result<()> {
    let cfg = AgentConfig::load_or_init(path)?;
    if cfg.token.is_empty() {
        bail!("no pairing token: pass --token-file FILE (setup.ps1 asks for it)");
    }
    // Pair again from scratch (a reinstalled hub has a new certificate).
    PairArgs {
        unpin: true,
        ..Default::default()
    }
    .apply(path)?;
    let opts = AgentOptions {
        config_path: path.to_path_buf(),
        os: Os::Windows,
    };
    let (addr, status) = agent_core::pairing::survey(&opts).await?;
    let mut cfg = AgentConfig::load_or_init(path)?;
    let node = match args.for_node {
        Some(id) => {
            if !status.nodes.iter().any(|n| n.node_id == id) {
                println!("note: {id} is not connected to the hub right now");
            }
            id
        }
        None => choose_agent(&status, cfg.node_id)?.node_id,
    };
    cfg.for_node = Some(node);
    cfg.save(path)?;
    let name = status
        .nodes
        .iter()
        .find(|n| n.node_id == node)
        .map_or("?", |n| n.node_name.as_str());
    println!(
        "paired with hub {} at {addr}; edge helper for {name} ({node})",
        status.hub_name
    );
    Ok(())
}

/// The agent this PC's helper acts for: the hub's only Windows agent (the
/// USB gadget announces the PC's system). With several, the ones that no
/// other edge helper serves; still several or none, the user chooses with
/// --for-node.
fn choose_agent(status: &HubStatus, me: Uuid) -> Result<&NodeInfo> {
    let windows: Vec<&NodeInfo> = status
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Agent && n.os == Os::Windows)
        .collect();
    let served_by_others = |id: Uuid| {
        status.nodes.iter().any(|n| {
            n.node_id != me && matches!(n.kind, NodeKind::EdgeHelper { for_node } if for_node == id)
        })
    };
    let free: Vec<&NodeInfo> = windows
        .iter()
        .copied()
        .filter(|n| !served_by_others(n.node_id))
        .collect();
    match (windows.as_slice(), free.as_slice()) {
        ([one], _) | (_, [one]) => Ok(one),
        ([], _) => bail!(
            "the hub has no Windows agent connected: is the USB gadget plugged into this PC and \
             running? (or name it with --for-node / -ForNode)"
        ),
        _ => {
            let list: Vec<String> = windows
                .iter()
                .map(|n| format!("  {}  {}", n.node_id, n.node_name))
                .collect();
            bail!(
                "several Windows agents on the hub; choose this PC's with --for-node / -ForNode:\n{}",
                list.join("\n")
            )
        }
    }
}

/// `--check`: the config and, from the log, the hub connection.
fn check(path: &Path, log: &Path) -> bool {
    let mut r = Report::default();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    r.line(Level::Info, "version", &format!("{VERSION} at {exe}"));
    let (level, detail) = agent_core::setup::config_line(path, "setup.ps1");
    r.line(level, "config", &detail);
    // Read only: load_or_init would create the file.
    let for_node = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| AgentConfig::parse(&t).ok())
        .and_then(|c| c.for_node);
    match for_node {
        Some(id) => r.line(Level::Ok, "for node", &id.to_string()),
        None => r.line(
            Level::Fail,
            "for node",
            "not set: setup.ps1 (or rawkvm-edge --pair) finds the USB gadget",
        ),
    }
    let text = std::fs::read_to_string(log).unwrap_or_default();
    // The log is appended across runs: only the latest decides.
    let run = agent_core::setup::current_run(&text, SUPERVISOR_START);
    let (level, detail) = agent_core::setup::hub_line(&run, log);
    r.line(level, "hub", &detail);
    if let Some((level, detail)) = agent_core::setup::last_of(&run, &[(DIED, Level::Warn)]) {
        r.line(level, "restarted", &detail);
    }
    r.ok()
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
    let log = log_path(&args);
    if args.check {
        std::process::exit(if check(&path, &log) { 0 } else { 1 });
    }
    let cli = args.configure_only || args.show || args.pair || configure_requested(&args);
    init_logging((!cli).then_some(log.as_path()));
    agent_core::supervise::log_panics();
    if !cli && !args.supervised {
        return supervise();
    }
    configure(&args, &path)?;
    if args.pair {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(pair(&args, &path));
    }
    if args.configure_only || (cli && !args.show) {
        return Ok(());
    }
    platform_main(&args, path)
}

/// First line of a run in the log, and the line of each restart.
const SUPERVISOR_START: &str = "rawkvm-edge supervisor starting";
const DIED: &str = "rawkvm-edge died after";

/// Run the helper as a child process (the same command line plus
/// `--supervised`) and start it again whenever it dies, logging how it
/// ended: a windowless process has no stderr, and without the helper the
/// user cannot leave this PC by the screen edge (ADR 0027).
fn supervise() -> Result<()> {
    #[cfg(windows)]
    if let Err(e) = win::kill_children_with_me() {
        tracing::warn!("job object: {e}; a helper may outlive this process");
    }
    let exe = std::env::current_exe()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    tracing::info!(version = VERSION, "{SUPERVISOR_START}");
    agent_core::supervise::supervise("rawkvm-edge", Default::default(), || {
        std::process::Command::new(&exe)
            .args(&args)
            .arg("--supervised")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    })?;
    Ok(())
}

fn configure_requested(args: &Args) -> bool {
    args.for_node.is_some()
        || args.hub.is_some()
        || args.token.is_some()
        || args.token_file.is_some()
        || args.unpin
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
        version = VERSION,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, os: Os, kind: NodeKind) -> NodeInfo {
        NodeInfo {
            node_id: Uuid::new_v4(),
            node_name: name.into(),
            os,
            kind,
            assigned_index: 1,
            rtt_ms: None,
            screens: 0,
            unexposed: Vec::new(),
            bridge: None,
        }
    }

    fn status(nodes: Vec<NodeInfo>) -> HubStatus {
        HubStatus {
            hub_name: "orangepi3".into(),
            active: None,
            nodes,
            device_count: 0,
            uptime_s: 0,
            version: String::new(),
            held: Vec::new(),
        }
    }

    #[test]
    fn the_only_windows_agent() {
        let me = Uuid::new_v4();
        let gadget = node("orangepi3", Os::Windows, NodeKind::Agent);
        let id = gadget.node_id;
        let s = status(vec![
            node("mac", Os::MacOs, NodeKind::Agent),
            node("ubuntu", Os::Linux, NodeKind::Agent),
            gadget,
            node("mac", Os::MacOs, NodeKind::Widget),
        ]);
        assert_eq!(choose_agent(&s, me).unwrap().node_id, id);
        assert!(choose_agent(&status(vec![]), me).is_err());
    }

    #[test]
    fn several_windows_agents() {
        let me = Uuid::new_v4();
        let a = node("gadget-a", Os::Windows, NodeKind::Agent);
        let b = node("gadget-b", Os::Windows, NodeKind::Agent);
        let (ida, idb) = (a.node_id, b.node_id);
        let e = choose_agent(&status(vec![a.clone(), b.clone()]), me).unwrap_err();
        assert!(format!("{e}").contains("gadget-a") && format!("{e}").contains(&idb.to_string()));
        // Another PC's helper serves b: a is this PC's.
        let other = node(
            "other (edge)",
            Os::Windows,
            NodeKind::EdgeHelper { for_node: idb },
        );
        let s = status(vec![a.clone(), b.clone(), other]);
        assert_eq!(choose_agent(&s, me).unwrap().node_id, ida);
        // Our own (old) helper does not count.
        let mut mine = node(
            "gamer (edge)",
            Os::Windows,
            NodeKind::EdgeHelper { for_node: ida },
        );
        mine.node_id = me;
        assert!(choose_agent(&status(vec![a, b, mine]), me).is_err());
    }
}
