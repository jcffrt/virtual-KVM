//! RawKVM hub: reads raw HID reports from the Logitech receiver and forwards
//! them, uninterpreted, to the active agent over QUIC.

use rawkvm_hub::{config, hidrec, router, server};
#[cfg(target_os = "linux")]
use rawkvm_hub::{devices, hidraw};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};

use config::HubConfig;
use router::{Options, Router};

#[derive(Parser, Debug)]
#[command(version, about)]
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
    let router = Arc::new(Router::new(
        cfg.hub_id,
        cfg.name.clone(),
        Options {
            hysteresis: Duration::from_millis(cfg.switching.hysteresis_ms),
            resend: (cfg.switching.resend_ms > 0)
                .then(|| Duration::from_millis(cfg.switching.resend_ms)),
            hotkey: cfg.switching.hotkey,
        },
    ));

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
