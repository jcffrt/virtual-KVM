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

    /// List the devices paired with the receiver over HID++ (slot, name,
    /// kind, wireless PID, HID++ version, HOSTS_INFO) and exit. Stop the
    /// hub service first.
    #[arg(long)]
    hidpp_list: bool,

    /// Call HID++ 2.0 function FUNCTION (0..15) of feature FEATURE (hex,
    /// e.g. 1815) on receiver slot SLOT with parameter bytes PARAMS (hex)
    /// and print the reply, then exit. For probing: a function may change
    /// the device. Stop the hub service first.
    #[arg(long, num_args = 3.., value_names = ["SLOT", "FEATURE", "FUNCTION", "PARAMS"])]
    hidpp_call: Option<Vec<String>>,

    /// Set the keyboard layout profile of receiver slot SLOT to OS
    /// (windows, macos, linux) on the host it is connected to now, print
    /// the state before and after, and exit. Stop the hub service first.
    #[arg(long, num_args = 2, value_names = ["SLOT", "OS"])]
    hidpp_set_os: Option<Vec<String>>,

    /// With --hidpp-set-os: the feature to use: auto (MULTIPLATFORM 0x4531,
    /// else HOSTS_INFO 0x1815, else DUALPLATFORM 0x4530), multiplatform,
    /// hostsinfo or dualplatform.
    #[arg(long, default_value = "auto", requires = "hidpp_set_os")]
    hidpp_via: String,

    /// With --hidpp-*: print every HID++ report sent (>) and read (<), to
    /// record fixtures.
    #[arg(long)]
    hidpp_trace: bool,
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
    if args.hidpp_list || args.hidpp_call.is_some() || args.hidpp_set_os.is_some() {
        return hidpp_cli(&args);
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
    for o in cfg.node_options.iter().filter(|o| o.swap_alt_gui) {
        warn!(
            index = o.index,
            "Alt and GUI are swapped in the keyboard reports sent to this agent (ADR 0011)"
        );
        if cfg.hidpp.os_profile {
            warn!(
                index = o.index,
                "swap_alt_gui and [hidpp] os_profile are both on: the keyboard layout already \
                 follows the agent (ADR 0014), so Alt and GUI end up crossed; remove swap_alt_gui"
            );
        }
    }
    let router = Arc::new(
        Router::new(
            cfg.hub_id,
            cfg.name.clone(),
            Options {
                hysteresis: Duration::from_millis(cfg.switching.hysteresis_ms),
                resend: (cfg.switching.resend_ms > 0)
                    .then(|| Duration::from_millis(cfg.switching.resend_ms)),
                hotkey: cfg.switching.hotkey(),
                layout: cfg.layout.clone(),
                edge: cfg.edge,
                version: VERSION.to_string(),
                node_options: cfg.node_options.clone(),
                os_profile: cfg.hidpp.os_profile,
            },
        )
        .with_store(store, RESTORE_GRACE)
        .with_layout_file(LayoutFile::path_in(&cfg.state_dir))?
        .with_settings_file(rawkvm_hub::settings::path_in(&cfg.state_dir))?,
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

/// Open the receiver's HID++ interface for the `--hidpp-*` commands.
#[cfg(target_os = "linux")]
fn open_hidpp(args: &Args) -> Result<Arc<rawkvm_hub::hidpp::Hidpp>> {
    use proto::descriptor::DescriptorInfo;
    use rawkvm_hub::hidpp::Hidpp;
    use rawkvm_hub::receiver;
    let cfg = HubConfig::load_or_default(&args.config)?;
    let iface = hidraw::enumerate()?
        .into_iter()
        .filter(|i| devices::is_forwarded(i, &cfg.receiver))
        .find(|i| receiver::is_dj_interface(&DescriptorInfo::parse(&i.descriptor)))
        .ok_or_else(|| anyhow::anyhow!("no HID++ interface of the receiver found; try --list"))?;
    eprintln!(
        "HID++ on {} ({}); the hub service should be stopped",
        iface.hidraw.display(),
        iface.phys
    );
    let file = hidraw::open(&iface.hidraw)?;
    let writer = devices::HidrawWriter::spawn(file.try_clone()?, iface.hidraw.clone(), true)?;
    let mut h = Hidpp::new(Arc::new(writer));
    if args.hidpp_trace {
        let t0 = std::time::Instant::now();
        h = h.with_trace(move |dir, r| {
            println!(
                "{:>9.3} {dir} {}",
                t0.elapsed().as_secs_f64() * 1000.0,
                r.iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        });
    }
    let h = Arc::new(h);
    let reader = h.clone();
    std::thread::spawn(move || {
        let err = hidraw::read_loop(file, |r| {
            reader.on_report(r);
        });
        eprintln!("HID++ interface closed: {err}");
    });
    Ok(h)
}

#[cfg(target_os = "linux")]
fn hidpp_cli(args: &Args) -> Result<()> {
    use rawkvm_hub::hidpp::{OsKind, Request, Via};
    let h = open_hidpp(args)?;
    if let Some(call) = &args.hidpp_call {
        let byte = |s: &str| {
            u8::from_str_radix(s.trim_start_matches("0x"), 16)
                .map_err(|_| anyhow::anyhow!("{s:?} is not a hex byte"))
        };
        let slot: u8 = call[0].parse()?;
        let feature = u16::from_str_radix(call[1].trim_start_matches("0x"), 16)
            .map_err(|_| anyhow::anyhow!("{:?} is not a hex feature id", call[1]))?;
        let function: u8 = call[2].parse()?;
        anyhow::ensure!(function < 16, "FUNCTION is 0..15");
        let params = call[3..]
            .iter()
            .map(|p| byte(p))
            .collect::<Result<Vec<_>>>()?;
        let index = h
            .feature_index(slot, feature)?
            .ok_or_else(|| anyhow::anyhow!("slot {slot} has no feature {feature:04x}"))?;
        let reply = h.request(&Request::feature(slot, index, function, &params))?;
        println!(
            "slot {slot} feature {feature:04x} (index {index}) function {function}: {}",
            hex::encode(&reply)
        );
        return Ok(());
    }
    if let Some(set) = &args.hidpp_set_os {
        let slot: u8 = set[0].parse()?;
        let os = OsKind::parse(&set[1])
            .ok_or_else(|| anyhow::anyhow!("OS is windows, macos or linux, not {:?}", set[1]))?;
        let via = Via::parse(&args.hidpp_via).ok_or_else(|| {
            anyhow::anyhow!("--hidpp-via is auto, multiplatform, hostsinfo or dualplatform")
        })?;
        let before = h
            .survey(slot)?
            .ok_or_else(|| anyhow::anyhow!("slot {slot} is empty"))?;
        println!("before: {}", describe_slot(&h, &before));
        let done = h.set_os(&before, os, via)?;
        println!("written: {done}");
        std::thread::sleep(Duration::from_millis(300));
        match h.survey(slot)? {
            Some(after) => println!("after:  {}", describe_slot(&h, &after)),
            None => println!("after:  slot {slot} is empty"),
        }
        println!(
            "check now whether Alt and Win/Cmd changed places on the PC the keyboard is \
             connected to; if not, it may only apply after the keyboard reconnects \
             (switch it off and on)"
        );
        return Ok(());
    }
    for slot in 1..=6u8 {
        match h.survey(slot) {
            Ok(Some(s)) => println!("{}", describe_slot(&h, &s)),
            Ok(None) => println!("slot {slot}: empty"),
            Err(e) => println!("slot {slot}: {e}"),
        }
    }
    Ok(())
}

/// One slot for `--hidpp-list` / `--hidpp-set-os`.
#[cfg(target_os = "linux")]
fn describe_slot(h: &rawkvm_hub::hidpp::Hidpp, s: &rawkvm_hub::hidpp::SlotInfo) -> String {
    use rawkvm_hub::hidpp::{hosts_os_name, multiplatform_os_names, HOSTS_CAN_SET_OS};
    use std::fmt::Write;
    let p = &s.pairing;
    let mut o = format!(
        "slot {}: {:?} {} wpid {:04x}",
        s.slot,
        p.name.as_deref().unwrap_or("?"),
        p.kind_name(),
        p.wireless_pid
    );
    if let Some(serial) = p.serial {
        let _ = write!(o, " serial {serial:08x}");
    }
    match s.protocol {
        Some((major, minor)) => {
            let _ = write!(o, " HID++ {major}.{minor}");
        }
        None => o.push_str(" not reachable"),
    }
    if let (Some(i), Some(hi)) = (s.hosts_info, s.hosts) {
        let _ = write!(
            o,
            "\n    HOSTS_INFO 1815 (index {i}): {} hosts, current {}, flags {:#04x}{}",
            hi.num_hosts,
            hi.current_host,
            hi.flags,
            if hi.flags & HOSTS_CAN_SET_OS != 0 {
                " (can set OS)"
            } else {
                ""
            }
        );
        if let Some(os) = s.host_os {
            let _ = write!(
                o,
                "\n      current host OS: {} ({}) {}.{}.{}",
                os.os_type,
                hosts_os_name(os.os_type),
                os.major,
                os.minor,
                os.patch
            );
        }
        for host in 0..hi.num_hosts {
            match h.host_info(s.slot, i, host) {
                Ok(r) => {
                    let _ = write!(o, "\n      host {host}: getHostInfo {}", hex::encode(&r));
                }
                Err(e) => {
                    let _ = write!(o, "\n      host {host}: {e}");
                }
            }
        }
    } else if let Some(i) = s.hosts_info {
        let _ = write!(o, "\n    HOSTS_INFO 1815 (index {i})");
    }
    if let Some((i, m)) = s.multiplatform {
        let _ = write!(
            o,
            "\n    MULTIPLATFORM 4531 (index {i}): flags {:#04x}{}, current platform {}",
            m.flags,
            if m.settable() { " (settable)" } else { "" },
            m.current
        );
        for d in &s.platforms {
            let _ = write!(
                o,
                "\n      platform {}: {}",
                d.platform,
                multiplatform_os_names(d.os_flags).join(", ")
            );
        }
    }
    if let Some((i, v)) = s.dualplatform {
        let _ = write!(
            o,
            "\n    DUALPLATFORM 4530 (index {i}): {v} ({})",
            if v == 0 {
                "iOS, macOS"
            } else {
                "Android, Windows"
            }
        );
    }
    if s.protocol.is_some_and(|(m, _)| m >= 2)
        && s.hosts_info.is_none()
        && s.multiplatform.is_none()
        && s.dualplatform.is_none()
    {
        o.push_str("\n    no HOSTS_INFO, MULTIPLATFORM or DUALPLATFORM");
    }
    if let Some((i, b)) = s.battery {
        let pct = |p: Option<u8>| p.map_or("?".to_string(), |p| format!("{p} %"));
        let _ = write!(
            o,
            "\n    BATTERY_STATUS 1000 (index {i}): {}, next {}, {}",
            pct(b.level),
            pct(b.next_level),
            b.status_name()
        );
    }
    if let Some((i, v)) = s.pointer_speed {
        let _ = write!(
            o,
            "\n    POINTER_SPEED 2205 (index {i}): {v} ({:.2}x)",
            v as f32 / 256.0
        );
    }
    if let Some((i, on)) = s.fn_swap {
        let _ = write!(
            o,
            "\n    FN_SWAP 40a3 (index {i}): {}",
            if on {
                "F-keys send their special function"
            } else {
                "F-keys send F1..F12"
            }
        );
    }
    if let Some(e) = &s.error {
        let _ = write!(o, "\n    (a request failed: {e})");
    }
    o
}

#[cfg(not(target_os = "linux"))]
fn hidpp_cli(_args: &Args) -> Result<()> {
    anyhow::bail!("--hidpp-* needs Linux (hidraw)")
}

#[cfg(not(target_os = "linux"))]
fn list(_args: &Args) -> Result<()> {
    anyhow::bail!("--list needs Linux (hidraw)")
}

#[cfg(not(target_os = "linux"))]
fn dump(_args: &Args) -> Result<()> {
    anyhow::bail!("--dump needs Linux (hidraw)")
}
