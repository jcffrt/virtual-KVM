//! `rawkvm-agent setup` and `rawkvm-agent --check` (ADR 0023).
//!
//! The .deb installs everything that needs root. `setup` is the second
//! step, run by the user in their session: move away from the manual
//! install in the home directory, pair with the hub, enable the user
//! service and the GNOME extension, then check. `--check` only reads.

use std::io::{BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agent_core::pairing::{self, PairArgs};
use agent_core::{AgentConfig, AgentOptions};
use anyhow::{bail, Context, Result};
use proto::{NodeKind, Os};

pub const UNIT: &str = "rawkvm-agent.service";
pub const EXTENSION: &str = "rawkvm@olitec.cc";
const SYSTEM_EXTENSION_DIR: &str = "/usr/share/gnome-shell/extensions/rawkvm@olitec.cc";

/// Options of `rawkvm-agent setup`.
#[derive(Debug, Default)]
pub struct SetupArgs {
    pub hub: Option<String>,
    pub token_file: Option<PathBuf>,
    /// Pair again even if the config is already paired.
    pub pair: bool,
    pub no_extension: bool,
}

/// Files of the manual install (deploy/linux/install.sh, before 0.5.0).
struct Home(PathBuf);

impl Home {
    fn current() -> Result<Self> {
        Ok(Self(
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is not set")?,
        ))
    }
    fn unit(&self) -> PathBuf {
        self.0.join(".config/systemd/user").join(UNIT)
    }
    fn binary(&self) -> PathBuf {
        self.0.join(".local/bin/rawkvm-agent")
    }
    fn extension(&self) -> PathBuf {
        self.0
            .join(".local/share/gnome-shell/extensions")
            .join(EXTENSION)
    }
}

pub async fn setup(config_path: &Path, args: SetupArgs) -> Result<()> {
    // SAFETY: getuid has no preconditions.
    if unsafe { libc::getuid() } == 0 {
        bail!("run `rawkvm-agent setup` as your own user, not with sudo (the service and the extension are per user)");
    }
    let home = Home::current()?;
    step("Previous manual install");
    migrate(&home)?;

    step("Pairing");
    let cfg = AgentConfig::load_or_init(config_path)?;
    let paired = !cfg.token.is_empty() && !cfg.hub_fingerprint.is_empty();
    if paired && !args.pair && args.hub.is_none() && args.token_file.is_none() {
        say(&format!(
            "already paired with {} (rawkvm-agent setup --pair to pair again)",
            if cfg.hub.is_empty() {
                "the hub found by mDNS"
            } else {
                &cfg.hub
            }
        ));
    } else {
        // The running service would hold the same node id.
        let _ = systemctl(&["stop", UNIT]);
        let hub = match args.hub {
            Some(h) => h,
            None => choose_hub(&cfg).await?,
        };
        let token = match &args.token_file {
            Some(_) => None,
            None => ask_token(&cfg)?,
        };
        PairArgs {
            hub: Some(hub),
            token,
            token_file: args.token_file,
            pair: true,
            ..Default::default()
        }
        .apply(config_path)?;
        let opts = AgentOptions {
            config_path: config_path.to_path_buf(),
            os: Os::Linux,
        };
        let addr = pairing::pair(&opts, NodeKind::Agent).await?;
        say(&format!("paired with the hub at {addr}"));
    }

    step("Service");
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    systemctl(&["restart", UNIT])?;
    say("systemctl --user enable + restart rawkvm-agent: done");

    step("GNOME extension");
    if args.no_extension {
        say("skipped (--no-extension)");
    } else {
        enable_extension()?;
    }

    // Give the service a moment to connect before checking.
    tokio::time::sleep(Duration::from_secs(3)).await;
    step("Check");
    check(config_path).await?;
    Ok(())
}

/// Remove what deploy/linux/install.sh left in the home directory: its user
/// unit (it shadows the packaged one and runs ~/.local/bin/rawkvm-agent),
/// that binary, and the copy of the extension (it shadows the packaged
/// one). The config and the pairing stay.
fn migrate(home: &Home) -> Result<()> {
    let mut any = false;
    let unit = home.unit();
    if unit.exists() {
        let text = std::fs::read_to_string(&unit).unwrap_or_default();
        if text.contains("rawkvm-agent") {
            let _ = systemctl(&["disable", "--now", UNIT]);
            std::fs::remove_file(&unit).with_context(|| format!("removing {}", unit.display()))?;
            say(&format!("removed {}", unit.display()));
            any = true;
        } else {
            say(&format!(
                "{} is not ours: left alone (it overrides the packaged unit)",
                unit.display()
            ));
        }
    }
    let bin = home.binary();
    if bin.exists() {
        let running = std::env::current_exe().ok();
        if running.as_deref() == Some(bin.as_path()) {
            say(&format!(
                "running {}: run /usr/bin/rawkvm-agent setup to leave it",
                bin.display()
            ));
        } else {
            std::fs::remove_file(&bin).with_context(|| format!("removing {}", bin.display()))?;
            say(&format!("removed {}", bin.display()));
            any = true;
        }
    }
    let ext = home.extension();
    if ext.join("metadata.json").exists() && Path::new(SYSTEM_EXTENSION_DIR).exists() {
        std::fs::remove_dir_all(&ext).with_context(|| format!("removing {}", ext.display()))?;
        say(&format!(
            "removed {} (the packaged copy takes over after the next login)",
            ext.display()
        ));
        any = true;
    }
    if any {
        let _ = systemctl(&["daemon-reload"]);
    } else {
        say("nothing to move");
    }
    Ok(())
}

/// Offer the hubs found with mDNS, or ask for an address.
async fn choose_hub(cfg: &AgentConfig) -> Result<String> {
    say("looking for hubs on the LAN (mDNS, 3 s)...");
    let hubs = agent_core::discovery::discover(Duration::from_secs(3))
        .await
        .unwrap_or_default();
    let found: Vec<(String, SocketAddr)> = hubs
        .iter()
        .filter_map(|h| h.addrs.first().map(|a| (h.name.clone(), *a)))
        .collect();
    let interactive = std::io::stdin().is_terminal();
    if !interactive {
        return match (found.as_slice(), cfg.hub.as_str()) {
            ([(_, a)], _) => Ok(a.ip().to_string()),
            ([], "") | ([_, _, ..], "") => {
                bail!("{} hubs found: pass --hub ADDRESS", found.len())
            }
            (_, saved) => Ok(saved.to_string()),
        };
    }
    for (i, (name, addr)) in found.iter().enumerate() {
        println!("  {}) {name} at {}", i + 1, addr.ip());
    }
    let default = found
        .first()
        .map(|(_, a)| a.ip().to_string())
        .or_else(|| (!cfg.hub.is_empty()).then(|| cfg.hub.clone()));
    let question = match (&default, found.len()) {
        (Some(d), 0) => format!("Hub address [{d}]: "),
        (Some(d), 1) => format!("Hub: 1, or an address [{d}]: "),
        (Some(d), n) => format!("Hub: 1-{n}, or an address [{d}]: "),
        (None, _) => "Hub address (IP or name): ".to_string(),
    };
    loop {
        let answer = read_line(&question)?;
        if let Some(hub) = pick_hub(&answer, &found, default.as_deref()) {
            return Ok(hub);
        }
        eprintln!("Type a number from the list or an address.");
    }
}

/// The hub chosen by `answer`: a number from `found`, an address, or the
/// default for an empty answer.
fn pick_hub(answer: &str, found: &[(String, SocketAddr)], default: Option<&str>) -> Option<String> {
    let a = answer.trim();
    if a.is_empty() {
        return default.map(str::to_string);
    }
    if let Ok(n) = a.parse::<usize>() {
        return found
            .get(n.checked_sub(1)?)
            .map(|(_, addr)| addr.ip().to_string());
    }
    (!a.contains(char::is_whitespace)).then(|| a.to_string())
}

/// The token, asked without echo; Enter keeps the saved one.
fn ask_token(cfg: &AgentConfig) -> Result<Option<String>> {
    if !std::io::stdin().is_terminal() {
        if cfg.token.is_empty() {
            bail!("no pairing token: pass --token-file FILE (or run in a terminal to type it)");
        }
        return Ok(None);
    }
    if cfg.token.is_empty() {
        return pairing::prompt_token().map(Some);
    }
    loop {
        let text = pairing::prompt_hidden("Pairing token (Enter keeps the saved one): ")?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        let t = text.trim().to_lowercase();
        if proto::parse_token(&t).is_some() {
            return Ok(Some(t));
        }
        eprintln!("That is not 64 hex characters; try again.");
    }
}

fn enable_extension() -> Result<()> {
    if which("gnome-shell").is_none() {
        say("no GNOME Shell: screen-edge switching is off on this machine");
        return Ok(());
    }
    if !Path::new(SYSTEM_EXTENSION_DIR)
        .join("metadata.json")
        .exists()
    {
        say(&format!(
            "{SYSTEM_EXTENSION_DIR} is missing: reinstall the rawkvm-agent package"
        ));
        return Ok(());
    }
    if let Some(v) = shell_major() {
        let meta = std::fs::read_to_string(Path::new(SYSTEM_EXTENSION_DIR).join("metadata.json"))
            .unwrap_or_default();
        if !meta.contains(&format!("\"{v}\"")) {
            say(&format!(
                "WARNING: GNOME Shell {v} is not in the extension's shell-version: it would show as OUT OF DATE"
            ));
        }
    }
    // The setting holds after the next login even when the running shell
    // does not know the extension yet (Wayland does not rescan).
    let enabled = gsettings_list("enabled-extensions");
    if !enabled.iter().any(|e| e == EXTENSION) {
        let mut list = enabled;
        list.push(EXTENSION.to_string());
        gsettings_set_list("enabled-extensions", &list)?;
    }
    let disabled = gsettings_list("disabled-extensions");
    if disabled.iter().any(|e| e == EXTENSION) {
        let list: Vec<String> = disabled.into_iter().filter(|e| e != EXTENSION).collect();
        gsettings_set_list("disabled-extensions", &list)?;
    }
    let _ = Command::new("gnome-extensions")
        .args(["enable", EXTENSION])
        .output();
    say("enabled; if the check says it is not running, log out and back in once");
    Ok(())
}

// ---------------------------------------------------------------- check

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "ok  ",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

/// Print the state of every piece, read only, in a form to paste in a
/// chat. True if nothing failed.
pub async fn check(config_path: &Path) -> Result<bool> {
    let mut worst = Level::Ok;
    let mut line = |level: Level, what: &str, detail: &str| {
        worst = worst.max(level);
        println!("{}  {what:<10} {detail}", level.tag());
    };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    line(
        Level::Info,
        "version",
        &format!("{} at {exe}", crate::VERSION),
    );

    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uhid")
    {
        Ok(_) => line(Level::Ok, "uhid", "/dev/uhid read/write"),
        Err(e) => line(
            Level::Fail,
            "uhid",
            &format!("/dev/uhid: {e} (package rule: the desktop session's user, or group input)"),
        ),
    }

    match std::fs::read_to_string(config_path) {
        Err(_) => line(
            Level::Fail,
            "config",
            &format!("{} missing: rawkvm-agent setup", config_path.display()),
        ),
        Ok(text) => match AgentConfig::parse(&text) {
            Err(e) => line(
                Level::Fail,
                "config",
                &format!("{}: {e:#}", config_path.display()),
            ),
            Ok(cfg) => {
                let hub = if cfg.hub.is_empty() {
                    "mDNS"
                } else {
                    cfg.hub.as_str()
                };
                let token = if cfg.token.is_empty() {
                    "no token"
                } else {
                    "token set"
                };
                let pin = if cfg.hub_fingerprint.is_empty() {
                    "not paired".to_string()
                } else {
                    format!("certificate {}…", &cfg.hub_fingerprint[..12])
                };
                let level = if cfg.token.is_empty() || cfg.hub_fingerprint.is_empty() {
                    Level::Fail
                } else {
                    Level::Ok
                };
                line(
                    level,
                    "config",
                    &format!("{}: hub {hub}, {token}, {pin}", config_path.display()),
                );
            }
        },
    }

    let enabled = systemctl_out(&["is-enabled", UNIT]);
    let active = systemctl_out(&["is-active", UNIT]);
    let level = if enabled == "enabled" && active == "active" {
        Level::Ok
    } else {
        Level::Fail
    };
    line(level, "service", &format!("{UNIT}: {enabled}, {active}"));

    let journal = Command::new("journalctl")
        .args([
            "--user",
            "-u",
            UNIT,
            "-b",
            "-n",
            "300",
            "-o",
            "cat",
            "--no-pager",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let (level, detail) = hub_state(journal.lines());
    line(level, "hub", &detail);

    let home = Home::current()?;
    if which("gnome-shell").is_some() {
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "?".into());
        let shell = shell_major().map_or("?".into(), |v| v.to_string());
        let user_copy = home.extension().join("metadata.json").exists();
        let where_ = if user_copy {
            "copy in ~/.local (shadows the package)"
        } else {
            "packaged"
        };
        let running = extension_running().await;
        let state = Command::new("gnome-extensions")
            .args(["info", EXTENSION])
            .output()
            .map(|o| extension_state(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or(None);
        let (level, detail) = match (running, state.as_deref()) {
            (Some(true), _) => (Level::Ok, "running".to_string()),
            (_, Some(s)) if s != "ACTIVE" && s != "ENABLED" => (
                Level::Warn,
                format!("state {s} (log out and back in once after setup)"),
            ),
            (Some(false), _) => (
                Level::Warn,
                "not running: log out and back in once (Wayland loads new extensions at login)"
                    .to_string(),
            ),
            (None, _) => (
                Level::Info,
                "no D-Bus session here (run --check in the desktop session)".to_string(),
            ),
        };
        line(
            level,
            "extension",
            &format!("{detail}; {where_}; GNOME Shell {shell}, {session}"),
        );
    } else {
        line(
            Level::Info,
            "extension",
            "no GNOME Shell: no screen-edge switching",
        );
    }

    let leftovers: Vec<String> = [home.unit(), home.binary()]
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .collect();
    if !leftovers.is_empty() {
        line(
            Level::Warn,
            "old files",
            &format!(
                "{} (rawkvm-agent setup moves them away)",
                leftovers.join(", ")
            ),
        );
    }
    Ok(worst < Level::Fail)
}

/// The hub connection from the agent's log: the last of "connected", a
/// reconnect, or an error decides.
fn hub_state<'a>(lines: impl Iterator<Item = &'a str>) -> (Level, String) {
    let mut last: Option<(Level, String)> = None;
    for l in lines {
        let l = l.trim();
        let verdict = if l.contains(" connected addr=") || l.ends_with(" connected") {
            Some(Level::Ok)
        } else if l.contains("ERROR") || l.contains("rejected") {
            Some(Level::Fail)
        } else if l.contains("reconnecting in") || l.contains("WARN") && l.contains("hub") {
            Some(Level::Warn)
        } else {
            None
        };
        if let Some(v) = verdict {
            last = Some((v, l.to_string()));
        }
    }
    last.unwrap_or((
        Level::Warn,
        "no connection lines in this boot's log (journalctl --user -u rawkvm-agent)".into(),
    ))
}

/// `State: ACTIVE` from `gnome-extensions info`.
fn extension_state(info: &str) -> Option<String> {
    info.lines()
        .find_map(|l| l.trim().strip_prefix("State:"))
        .map(|s| s.trim().to_string())
}

/// Whether the extension's D-Bus name is on the session bus; None without
/// a session bus (an SSH login without the desktop).
async fn extension_running() -> Option<bool> {
    let conn = zbus::Connection::session().await.ok()?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await.ok()?;
    let name = zbus::names::BusName::try_from(crate::edge::BUS_NAME).ok()?;
    dbus.name_has_owner(name).await.ok()
}

// ---------------------------------------------------------------- helpers

fn step(name: &str) {
    println!("\n== {name}");
}

fn say(msg: &str) {
    println!("   {msg}");
}

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(s)
}

fn systemctl(args: &[&str]) -> Result<()> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("running systemctl --user")?;
    if !out.status.success() {
        bail!(
            "systemctl --user {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn systemctl_out(args: &[&str]) -> String {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(cmd))
        .find(|p| p.is_file())
}

fn shell_major() -> Option<u32> {
    let out = Command::new("gnome-shell").arg("--version").output().ok()?;
    parse_shell_major(&String::from_utf8_lossy(&out.stdout))
}

/// `GNOME Shell 46.0` -> 46.
fn parse_shell_major(s: &str) -> Option<u32> {
    s.trim()
        .strip_prefix("GNOME Shell ")?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn gsettings_list(key: &str) -> Vec<String> {
    Command::new("gsettings")
        .args(["get", "org.gnome.shell", key])
        .output()
        .map(|o| parse_gvariant_strings(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn gsettings_set_list(key: &str, list: &[String]) -> Result<()> {
    let value = format!(
        "[{}]",
        list.iter()
            .map(|e| format!("'{}'", e.replace('\'', "")))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let ok = Command::new("gsettings")
        .args(["set", "org.gnome.shell", key, &value])
        .status()
        .context("running gsettings")?
        .success();
    if !ok {
        bail!("gsettings set org.gnome.shell {key} failed");
    }
    Ok(())
}

/// `['a@b', 'c@d']` or `@as []` -> the strings.
fn parse_gvariant_strings(s: &str) -> Vec<String> {
    s.trim()
        .trim_start_matches("@as")
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|e| e.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvariant_lists() {
        assert!(parse_gvariant_strings("@as []\n").is_empty());
        assert_eq!(
            parse_gvariant_strings("['ding@rastersoft.com', 'rawkvm@olitec.cc']\n"),
            ["ding@rastersoft.com", "rawkvm@olitec.cc"]
        );
    }

    #[test]
    fn shell_versions() {
        assert_eq!(parse_shell_major("GNOME Shell 46.0\n"), Some(46));
        assert_eq!(parse_shell_major("GNOME Shell 42.9"), Some(42));
        assert_eq!(parse_shell_major("nope"), None);
    }

    #[test]
    fn extension_states() {
        let info = "rawkvm@olitec.cc\n  Name: RawKVM edge bridge\n  State: OUT OF DATE\n";
        assert_eq!(extension_state(info).as_deref(), Some("OUT OF DATE"));
        assert_eq!(extension_state(""), None);
    }

    #[test]
    fn hub_from_the_log() {
        let log = "\
INFO agent_core: hub certificate pinned fingerprint=ab
INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepi3 index=3
WARN agent_core: hub closed the session
INFO agent_core: reconnecting in 0.5 s
INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepi3 index=3";
        let (level, detail) = hub_state(log.lines());
        assert_eq!(level, Level::Ok);
        assert!(detail.contains("index=3"));
        let (level, _) = hub_state("ERROR agent_core: the hub rejected the pairing token".lines());
        assert_eq!(level, Level::Fail);
        assert_eq!(hub_state("".lines()).0, Level::Warn);
    }

    #[test]
    fn hub_answers() {
        let found = vec![
            (
                "orangepi3".to_string(),
                "192.168.1.28:4433".parse().unwrap(),
            ),
            ("other".to_string(), "192.168.1.30:4433".parse().unwrap()),
        ];
        assert_eq!(
            pick_hub("", &found, Some("192.168.1.28")).as_deref(),
            Some("192.168.1.28")
        );
        assert_eq!(pick_hub("2", &found, None).as_deref(), Some("192.168.1.30"));
        assert_eq!(pick_hub("3", &found, None), None);
        assert_eq!(pick_hub("0", &found, None), None);
        assert_eq!(
            pick_hub(" pi.lan ", &found, None).as_deref(),
            Some("pi.lan")
        );
        assert_eq!(pick_hub("", &[], None), None);
    }
}
