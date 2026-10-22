//! RawKVM hub: reads raw HID reports from the Logitech receiver and forwards
//! them, uninterpreted, to the active agent over QUIC.

use rawkvm_hub::{config, hidrec, layout, nodes, router, server};
#[cfg(target_os = "linux")]
use rawkvm_hub::{devices, hidraw};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};

use config::HubConfig;
use layout::LayoutFile;
use nodes::NodeStore;
use router::{Options, Router, RESTORE_GRACE};

/// Crate version and git commit of this build.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RAWKVM_COMMIT"), ")");

#[derive(Parser, Debug)]
#[command(version = VERSION, about)]
struct Args {
    /// Configuration file. Created with a fresh token on first start.
    #[arg(short, long, default_value = config::DEFAULT_CONFIG)]
    config: PathBuf,

    /// List every hidraw node with its descriptor and exit.
    #[arg(long)]
    list: bool,

    /// With --list: save the descriptors of the forwarded nodes as
    /// `<dir>/<phys>.bin`.
    #[arg(long, requires = "list", value_name = "DIR")]
    save: Option<PathBuf>,

    /// Print every report of the forwarded nodes as hex instead of serving.
    /// This prints keystrokes: diagnostics only.
    #[arg(long, conflicts_with = "list")]
    dump: bool,

    /// With --dump: also write the reports to a .hidrec file.
    #[arg(long, requires = "dump", value_name = "FILE")]
    record: Option<PathBuf>,

    /// Serve the devices and reports of a .hidrec file instead of the real
    /// receiver (works on any OS).
    #[arg(long, value_name = "FILE", conflicts_with_all = ["list", "dump"])]
    replay: Option<PathBuf>,

    /// Print the pairing token and exit.
    #[arg(long)]
    show_token: bool,

    /// List the agent indices kept in <state_dir>/nodes.toml and exit.
    #[arg(long)]
    list_nodes: bool,

    /// Release the index of a node (by UUID or by index) and exit. Stop the
    /// hub first, or it writes its own copy back.
    #[arg(long, value_name = "UUID|INDEX")]
    forget_node: Option<String>,

    /// Print the layout in effect at start-up (<state_dir>/layout.toml, or
    /// the [[layout]] seed of the config) and exit.
    #[arg(long)]
    dump_layout: bool,

    /// Validate a layout file (the format --dump-layout prints) and install
    /// it as <state_dir>/layout.toml, then exit. A running hub applies it
    /// on `systemctl reload rawkvm-hub` (SIGHUP).
    #[arg(long, value_name = "FILE")]
    load_layout: Option<PathBuf>,
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

    if args.list {
        return list(&args);
    }
    if args.dump {
        return dump(&args);
    }
    if args.list_nodes || args.forget_node.is_some() {
        return manage_nodes(&args);
    }
    if args.dump_layout || args.load_layout.is_some() {
        return manage_layout(&args);
    }
    if args.show_token {
        let (cfg, _) = HubConfig::load_or_init(&args.config)?;
        println!("{}", cfg.token);
        return Ok(());
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(args))
}

async fn serve(args: Args) -> Result<()> {
    // First line of every start: which build is running (after a deploy or
    // a power cut, a stale or corrupt binary shows up here or not at all).
    info!(
        version = VERSION,
        pid = std::process::id(),
        config = %args.config.display(),
        "rawkvm-hub starting"
    );
    let (cfg, created) = HubConfig::load_or_init(&args.config)?;
    if created {
        warn!(
            "new pairing token (enter it once in each agent): {}",
            cfg.token
        );
    }
    info!(
        "hub {:?} ({}), config {}",
        cfg.name,
        cfg.hub_id,
        args.config.display()
    );
    let identity = server::load_or_create_identity(&cfg.state_dir)?;
    let store = NodeStore::load(&NodeStore::path_in(&cfg.state_dir))?;
    let router = Arc::new(
        Router::new(
            cfg.hub_id,
            cfg.name.clone(),
            Options {
                hysteresis: Duration::from_millis(cfg.switching.hysteresis_ms),
                resend: (cfg.switching.resend_ms > 0)
                    .then(|| Duration::from_millis(cfg.switching.resend_ms)),
                hotkey: cfg.switching.hotkey,
                layout: cfg.layout.clone(),
                edge: cfg.edge,
            },
        )
        .with_store(store, RESTORE_GRACE)
        .with_layout_file(LayoutFile::path_in(&cfg.state_dir))?,
    );
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let router = router.clone();
        let mut hup = signal(SignalKind::hangup())?;
        tokio::spawn(async move {
            while hup.recv().await.is_some() {
                if let Err(e) = router.reload_layout() {
                    warn!("{e:#}");
                }
            }
        });
    }
    {
        let router = router.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESTORE_GRACE).await;
            router.end_restore();
        });
    }

    match &args.replay {
        Some(path) => {
            let rec = hidrec::load(path)?;
            info!(
                "replaying {} ({} devices, {} reports)",
                path.display(),
                rec.devices.len(),
                rec.reports.len()
            );
            hidrec::spawn_replay(router.clone(), rec);
        }
        None => start_devices(&cfg, router.clone())?,
    }

    let _mdns = if cfg.mdns {
        match server::advertise(&cfg, &identity.fingerprint) {
            Ok(d) => Some(d),
            Err(e) => {
                warn!("mDNS disabled: {e:#}");
                None
            }
        }
    } else {
        None
    };

    tokio::select! {
        r = server::serve(&cfg, &identity, router) => r,
        _ = tokio::signal::ctrl_c() => {
            info!("shutting down");
            Ok(())
        }
    }
}

fn manage_nodes(args: &Args) -> Result<()> {
    let cfg = HubConfig::load_or_default(&args.config)?;
    let path = NodeStore::path_in(&cfg.state_dir);
    let mut store = NodeStore::load(&path)?;
    if let Some(key) = &args.forget_node {
        let id = store
            .find(key)
            .ok_or_else(|| anyhow::anyhow!("no node {key:?} in {}", path.display()))?;
        let e = store.forget(&id).expect("found above");
        store.try_save(&path)?;
        println!("released index {} of {} ({:?})", e.index, e.id, e.name);
        return Ok(());
    }
    let last = store.last_active();
    if store.entries().next().is_none() {
        println!("no node in {}", path.display());
    }
    for e in store.entries() {
        let mark = if Some(e.id) == last {
            "  (last active)"
        } else {
            ""
        };
        println!("{}  {}  {}{mark}", e.index, e.id, e.name);
    }
    Ok(())
}

fn manage_layout(args: &Args) -> Result<()> {
    let cfg = HubConfig::load_or_default(&args.config)?;
    let path = LayoutFile::path_in(&cfg.state_dir);
    if let Some(src) = &args.load_layout {
        let text = std::fs::read_to_string(src)?;
        let file: LayoutFile =
            toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", src.display()))?;
        file.save(&path)?;
        println!(
            "installed {} links in {}; apply with `sudo systemctl reload rawkvm-hub`",
            file.link.len(),
            path.display()
        );
        return Ok(());
    }
    match LayoutFile::load(&path)? {
        Some(file) => print!("{}", file.to_text()?),
        None if cfg.layout.is_empty() => println!(
            "# no layout ({} absent, no [[layout]] in the config)",
            path.display()
        ),
        None => {
            println!(
                "# no {}; the hub uses the [[layout]] seed of {}, resolved\n\
                 # against the screens the helpers report (needs a running hub):",
                path.display(),
                args.config.display()
            );
            #[derive(serde::Serialize)]
            struct Seed<'a> {
                layout: &'a [layout::LinkSpec],
            }
            print!(
                "{}",
                toml::to_string_pretty(&Seed {
                    layout: &cfg.layout
                })?
            );
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn start_devices(cfg: &HubConfig, router: Arc<Router>) -> Result<()> {
    info!(mode = ?cfg.receiver.mode, "reading the receiver");
    devices::spawn(router, cfg.receiver.clone());
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn start_devices(_cfg: &HubConfig, _router: Arc<Router>) -> Result<()> {
    anyhow::bail!("reading the receiver needs Linux; use --replay <file.hidrec> on this OS")
}

#[cfg(target_os = "linux")]
fn list(args: &Args) -> Result<()> {
    use proto::descriptor::DescriptorInfo;
    let cfg = HubConfig::load_or_default(&args.config)?;
    let all = hidraw::enumerate()?;
    if all.is_empty() {
        println!("no hidraw nodes");
    }
    println!("match mode: {:?}", cfg.receiver.mode);
    for i in &all {
        let fwd = devices::is_forwarded(i, &cfg.receiver);
        let d = DescriptorInfo::parse(&i.descriptor);
        println!(
            "{} {} bus {:04x} {:04x}:{:04x} {:?}\n    phys {}  interface {}  driver {}  serial {}\n    events {:?}  parent {:?}\n    descriptor {} bytes  report_ids {}  apps {:?}  inputs {:?}  keyboard {:?}\n    {}",
            if fwd { "[forward]" } else { "[ignore] " },
            i.hidraw.display(),
            i.bus,
            i.vid,
            i.pid,
            i.name,
            i.phys,
            i.interface(),
            i.driver.as_deref().unwrap_or("-"),
            i.serial.as_deref().unwrap_or("-"),
            i.event_nodes,
            i.parent_hid,
            i.descriptor.len(),
            d.uses_report_ids,
            d.applications,
            d.inputs.iter().map(|r| (r.report_id, r.len)).collect::<Vec<_>>(),
            d.keyboard,
            hex::encode(&i.descriptor),
        );
        if let (true, Some(dir)) = (fwd, &args.save) {
            config::ensure_dir(dir)?;
            let name: String = i
                .phys
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let path = dir.join(format!("{name}.bin"));
            std::fs::write(&path, &i.descriptor)?;
            println!("    saved {}", path.display());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn dump(args: &Args) -> Result<()> {
    use std::sync::Mutex;
    use std::time::Instant;
    let cfg = HubConfig::load_or_default(&args.config)?;
    let ifaces: Vec<_> = hidraw::enumerate()?
        .into_iter()
        .filter(|i| devices::is_forwarded(i, &cfg.receiver))
        .collect();
    anyhow::ensure!(
        !ifaces.is_empty(),
        "no hidraw node matches mode {:?}; try --list",
        cfg.receiver.mode
    );
    // Same ids as the service would assign (interface number in mode A).
    let mut ids = std::collections::HashMap::new();
    let devs: Vec<_> = ifaces
        .iter()
        .map(|i| {
            let id = devices::assign_id(&mut ids, i, cfg.receiver.mode).unwrap_or(u8::MAX);
            devices::to_hid_device(i, id)
        })
        .collect();
    let writer = match &args.record {
        Some(p) => Some(Arc::new(Mutex::new(hidrec::Writer::create(p, &devs)?))),
        None => None,
    };
    eprintln!("dumping {} nodes; Ctrl+C to stop", ifaces.len());
    let t0 = Instant::now();
    let mut threads = Vec::new();
    for (iface, dev) in ifaces.into_iter().zip(&devs) {
        let id = dev.id;
        let file = hidraw::open(&iface.hidraw)?;
        let grabs: Vec<_> = if cfg.receiver.grab_evdev {
            iface
                .event_nodes
                .iter()
                .filter_map(|p| hidraw::grab(p).ok())
                .collect()
        } else {
            Vec::new()
        };
        eprintln!(
            "  [{id}] {} {} ({})",
            iface.hidraw.display(),
            iface.name,
            iface.phys
        );
        let writer = writer.clone();
        threads.push(std::thread::spawn(move || {
            let _grabs = grabs;
            let err = hidraw::read_loop(file, |r| {
                println!(
                    "{:>10.3} [{id}] {:>2} {}",
                    t0.elapsed().as_secs_f64() * 1000.0,
                    r.len(),
                    hex::encode(r)
                );
                if let Some(w) = &writer {
                    let _ = w.lock().unwrap().report(id, r);
                }
            });
            eprintln!("[{id}] closed: {err}");
        }));
    }
    for t in threads {
        let _ = t.join();
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn list(_args: &Args) -> Result<()> {
    anyhow::bail!("--list needs Linux (hidraw)")
}

#[cfg(not(target_os = "linux"))]
fn dump(_args: &Args) -> Result<()> {
    anyhow::bail!("--dump needs Linux (hidraw)")
}
