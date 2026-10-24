//! `rawkvm-agent setup` and `rawkvm-agent --check` (ADR 0023).
//!
//! The .deb installs everything that needs root. `setup` is the second
//! step, run by the user in their session: move away from the manual
//! install in the home directory, pair with the hub, enable the user
//! service and the GNOME extension, then check. `--check` only reads.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agent_core::pairing::{self, PairArgs};
use agent_core::setup::{self, ask_token, choose_hub, say, step, Level, Report};
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

/// Over SSH the session bus address may be unset; the desktop session's
/// bus is at $XDG_RUNTIME_DIR/bus (gsettings, gnome-extensions, zbus).
pub fn use_session_bus() {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return;
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let bus = Path::new(&dir).join("bus");
        if bus.exists() {
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus.display()),
            );
        }
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

/// Print the state of every piece, read only, in a form to paste in a
/// chat. True if nothing failed.
pub async fn check(config_path: &Path) -> Result<bool> {
    let mut r = Report::default();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    r.line(
        Level::Info,
        "version",
        &format!("{} at {exe}", crate::VERSION),
    );

    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uhid")
    {
        Ok(_) => r.line(Level::Ok, "uhid", "/dev/uhid read/write"),
        Err(e) => r.line(
            Level::Fail,
            "uhid",
            &format!("/dev/uhid: {e} (package rule: the desktop session's user, or group input)"),
        ),
    }

    let (level, detail) = setup::config_line(config_path, "rawkvm-agent setup");
    r.line(level, "config", &detail);

    let enabled = systemctl_out(&["is-enabled", UNIT]);
    let active = systemctl_out(&["is-active", UNIT]);
    let level = if enabled == "enabled" && active == "active" {
        Level::Ok
    } else {
        Level::Fail
    };
    r.line(level, "service", &format!("{UNIT}: {enabled}, {active}"));

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
    let (level, detail) = setup::hub_state(journal.lines()).unwrap_or((
        Level::Warn,
        "no connection lines in this boot's log (journalctl --user -u rawkvm-agent)".into(),
    ));
    r.line(level, "hub", &detail);

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
        // LC_ALL=C: "State:", not the translated label.
        let state = Command::new("gnome-extensions")
            .env("LC_ALL", "C")
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
        r.line(
            level,
            "extension",
            &format!("{detail}; {where_}; GNOME Shell {shell}, {session}"),
        );
    } else {
        r.line(
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
        r.line(
            Level::Warn,
            "old files",
            &format!(
                "{} (rawkvm-agent setup moves them away)",
                leftovers.join(", ")
            ),
        );
    }
    Ok(r.ok())
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
}
