//! rawkvm-netwatch: Wi-Fi watchdog of the hub board (ADR 0029). Pings the
//! default gateway every few seconds and, when the radio stops (the
//! uwe5622 firmware hang of the Orange Pi Zero 2W), restarts the Wi-Fi,
//! then reloads its driver, then reboots, at most --max-reboots times an
//! hour. The decisions are in `rawkvm_hub::netwatch`; this is the I/O.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use clap::Parser;
use rawkvm_hub::netwatch::{self, Action, ClockJump, Event, Observation, Route, Settings, Watch};
use tracing::{error, info, warn};

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RAWKVM_COMMIT"), ")");

#[derive(Parser, Debug)]
#[command(
    version = VERSION,
    about = "RawKVM Wi-Fi watchdog: restarts the radio, then reboots, when the hub board loses the network (ADR 0029)"
)]
struct Args {
    /// Wi-Fi interface to watch. Nothing happens while another interface
    /// (Ethernet) has the default route.
    #[arg(long, default_value = "wlan0")]
    iface: String,

    /// Driver modules to reload at step 2, unloaded in this order and
    /// loaded in the reverse one (comma separated). `none`: skip step 2.
    /// Only modules that are loaded are touched.
    #[arg(long, default_value = "sprdwl_ng", value_delimiter = ',')]
    modules: Vec<String>,

    /// Seconds between two pings of the gateway.
    #[arg(long, default_value_t = 10)]
    interval: u64,

    /// Seconds without action after start (the Wi-Fi coming up at boot).
    #[arg(long, default_value_t = 120)]
    grace: u64,

    /// Seconds of silence from the gateway before the first step.
    #[arg(long, default_value_t = 60)]
    fail_after: u64,

    /// Seconds to wait after each step before the next one.
    #[arg(long, default_value_t = 30)]
    step_wait: u64,

    /// Reboots allowed per hour; past that, only log.
    #[arg(long, default_value_t = 3)]
    max_reboots: usize,

    /// Log the steps without taking them.
    #[arg(long)]
    dry_run: bool,

    /// Print the route, a ping of the gateway and the recent reboots, and
    /// exit. Changes nothing.
    #[arg(long)]
    check: bool,

    /// Where the reboot times are kept.
    #[arg(long, default_value = "/var/lib/rawkvm-netwatch/reboots")]
    state: PathBuf,
}

/// Run a command, waiting at most `timeout`; a command stuck past it
/// (a driver unload blocked in the kernel) is abandoned, so the watchdog
/// can still go on to the next step.
fn run(cmd: &str, args: &[&str], timeout: Duration) -> bool {
    let child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            warn!("{cmd}: {e}");
            return false;
        }
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let mut err = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        let _ = std::io::Read::read_to_string(&mut e, &mut err);
                    }
                    warn!("{cmd} {}: {status}: {}", args.join(" "), err.trim());
                }
                return status.success();
            }
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                error!(
                    "{cmd} {} still running after {} s: abandoned",
                    args.join(" "),
                    timeout.as_secs()
                );
                return false;
            }
            Err(e) => {
                warn!("{cmd}: {e}");
                return false;
            }
        }
    }
}

fn output(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn route(iface: &str) -> Route {
    if !Path::new("/sys/class/net").join(iface).exists() {
        return Route::NoInterface;
    }
    netwatch::parse_route(&output("ip", &["route", "show", "default"]), iface)
}

fn ping(gateway: &str, iface: &str) -> bool {
    run(
        "ping",
        &["-n", "-q", "-c", "1", "-W", "2", "-I", iface, gateway],
        Duration::from_secs(5),
    )
}

/// Read the kernel log from now on and raise `flag` on a hang line.
fn watch_kernel(flag: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut f = match std::fs::File::open("/dev/kmsg") {
            Ok(f) => f,
            Err(e) => {
                warn!("/dev/kmsg: {e}; watching the pings only");
                return;
            }
        };
        let _ = f.seek(SeekFrom::End(0));
        let mut r = BufReader::new(f);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) => std::thread::sleep(Duration::from_secs(1)),
                Ok(_) => {
                    if netwatch::is_hang(&line) {
                        let msg = line.split_once(';').map_or(line.as_str(), |(_, m)| m);
                        warn!("kernel: {}", msg.trim());
                        flag.store(true, Ordering::SeqCst);
                    }
                }
                // EPIPE: records overwritten before we read them; go on.
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    });
}

fn restart_wifi(iface: &str) {
    let nm = output("nmcli", &["-t", "-f", "RUNNING", "general"]).trim() == "running";
    let t = Duration::from_secs(20);
    if nm {
        run("nmcli", &["radio", "wifi", "off"], t);
        std::thread::sleep(Duration::from_secs(3));
        run("nmcli", &["radio", "wifi", "on"], t);
    } else {
        run("ip", &["link", "set", iface, "down"], t);
        std::thread::sleep(Duration::from_secs(1));
        run("ip", &["link", "set", iface, "up"], t);
    }
}

fn loaded(modules: &[String]) -> Vec<String> {
    modules
        .iter()
        .filter(|m| !m.is_empty() && *m != "none" && Path::new("/sys/module").join(m).exists())
        .cloned()
        .collect()
}

fn reload_modules(modules: &[String]) {
    let t = Duration::from_secs(30);
    for m in modules {
        run("modprobe", &["-r", m], t);
    }
    for m in modules.iter().rev() {
        run("modprobe", &[m], t);
    }
}

fn recent_reboots(args: &Args) -> Vec<i64> {
    let state = std::fs::read_to_string(&args.state).unwrap_or_default();
    netwatch::recent_reboots(
        &state,
        netwatch::unix(SystemTime::now()),
        Duration::from_secs(3600),
    )
}

fn reboot(args: &Args) {
    let mut times = recent_reboots(args);
    times.push(netwatch::unix(SystemTime::now()));
    let text: String = times.iter().map(|t| format!("{t}\n")).collect();
    if let Some(dir) = args.state.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&args.state, text) {
        warn!("{}: {e}", args.state.display());
    }
    // Keep this run's journal: it says why.
    run("journalctl", &["--flush"], Duration::from_secs(10));
    run("sync", &[], Duration::from_secs(20));
    run("systemctl", &["reboot"], Duration::from_secs(30));
}

fn check(args: &Args) -> bool {
    let r = route(&args.iface);
    println!("route    {r:?}");
    let ok = match &r {
        Route::Wifi { gateway: Some(gw) } => {
            let ok = ping(gw, &args.iface);
            println!(
                "ping     {gw}: {}",
                if ok { "answers" } else { "NO ANSWER" }
            );
            ok
        }
        _ => true,
    };
    let mods = loaded(&args.modules);
    println!(
        "modules  {} (loaded: {})",
        args.modules.join(","),
        if mods.is_empty() {
            "none".into()
        } else {
            mods.join(",")
        }
    );
    println!(
        "reboots  {} in the last hour (limit {})",
        recent_reboots(args).len(),
        args.max_reboots
    );
    ok
}

fn mono_secs(start: Instant) -> i64 {
    start.elapsed().as_secs() as i64
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time() // journald adds it
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    if args.check {
        std::process::exit(if check(&args) { 0 } else { 1 });
    }
    let modules = loaded(&args.modules);
    let settings = Settings {
        grace: Duration::from_secs(args.grace),
        fail_after: Duration::from_secs(args.fail_after),
        step_wait: Duration::from_secs(args.step_wait),
        reload_modules: !modules.is_empty(),
        max_reboots: args.max_reboots,
        reboot_window: Duration::from_secs(3600),
    };
    info!(
        version = VERSION,
        iface = %args.iface,
        modules = %modules.join(","),
        dry_run = args.dry_run,
        "rawkvm-netwatch starting"
    );
    let hang = Arc::new(AtomicBool::new(false));
    watch_kernel(hang.clone());
    let start = Instant::now();
    let mut clock = ClockJump::new(netwatch::unix(SystemTime::now()), 0);
    let mut watch = Watch::new(settings);
    let mut last_route = None;
    loop {
        if let Some(j) = clock.check(netwatch::unix(SystemTime::now()), mono_secs(start)) {
            info!(
                "the system clock jumped by {j:+} s (NTP?): earlier log times are off by that much"
            );
        }
        let r = route(&args.iface);
        if last_route.as_ref() != Some(&r) {
            match &r {
                Route::Wifi { gateway } => info!(gateway = ?gateway, "watching {}", args.iface),
                Route::Other { dev } => {
                    info!("{dev} has the default route: not watching {}", args.iface)
                }
                Route::Missing => warn!("no default route"),
                Route::NoInterface => warn!("no interface {}", args.iface),
            }
            last_route = Some(r.clone());
        }
        let ping_ok = match &r {
            Route::Wifi { gateway: Some(gw) } => ping(gw, &args.iface),
            _ => false,
        };
        let obs = Observation {
            at: start.elapsed(),
            route: r,
            ping_ok,
            hang: hang.swap(false, Ordering::SeqCst),
        };
        match watch.observe(&obs, recent_reboots(&args).len()) {
            Some(Event::Act { action, why }) => {
                let dry = if args.dry_run {
                    " (dry run: not done)"
                } else {
                    ""
                };
                match action {
                    Action::RestartWifi => warn!("{why}: restarting the Wi-Fi{dry}"),
                    Action::ReloadModules => {
                        warn!("{why}: reloading {}{dry}", modules.join(","))
                    }
                    Action::Reboot => warn!("{why}: rebooting{dry}"),
                    Action::RebootLimited => error!(
                        "{why}: a reboot is due, but {} were made in the last hour: \
                         not rebooting; starting over",
                        args.max_reboots
                    ),
                }
                if !args.dry_run {
                    match action {
                        Action::RestartWifi => restart_wifi(&args.iface),
                        Action::ReloadModules => reload_modules(&modules),
                        Action::Reboot => reboot(&args),
                        Action::RebootLimited => {}
                    }
                }
            }
            Some(Event::Recovered { down_for, after }) => info!(
                "network back after {} s down{}",
                down_for.as_secs(),
                after.map_or(String::new(), |a| format!(" (last step: {a:?})"))
            ),
            None => {}
        }
        std::thread::sleep(Duration::from_secs(args.interval));
    }
}
