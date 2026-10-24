//! Pieces shared by the agents' `setup` and `--check` commands: choosing
//! the hub, asking for the token, and the `ok / info / warn / FAIL` lines
//! of `--check` (one per piece, to paste in a chat as they are).

use std::io::{BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::{pairing, AgentConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Level {
    pub fn tag(self) -> &'static str {
        match self {
            Level::Ok => "ok  ",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

/// Prints the lines of `--check` and remembers the worst level.
#[derive(Debug)]
pub struct Report {
    worst: Level,
}

impl Default for Report {
    fn default() -> Self {
        Self { worst: Level::Ok }
    }
}

impl Report {
    pub fn line(&mut self, level: Level, what: &str, detail: &str) {
        self.worst = self.worst.max(level);
        println!("{}  {what:<10} {detail}", level.tag());
    }

    /// True if nothing failed.
    pub fn ok(&self) -> bool {
        self.worst < Level::Fail
    }
}

/// The `config` line: hub, token and pinned certificate of the config at
/// `path` (never the token itself).
pub fn config_line(path: &Path, fix: &str) -> (Level, String) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (Level::Fail, format!("{} missing: {fix}", path.display())),
    };
    let cfg = match AgentConfig::parse(&text) {
        Ok(c) => c,
        Err(e) => return (Level::Fail, format!("{}: {e:#}", path.display())),
    };
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
        format!("certificate {}...", &cfg.hub_fingerprint[..12])
    };
    let level = if cfg.token.is_empty() || cfg.hub_fingerprint.is_empty() {
        Level::Fail
    } else {
        Level::Ok
    };
    (
        level,
        format!("{}: hub {hub}, {token}, {pin}", path.display()),
    )
}

/// The hub connection from an agent's log: the last of "connected", a
/// reconnect, or an error decides. `None` if no line says anything.
pub fn hub_state<'a>(lines: impl Iterator<Item = &'a str>) -> Option<(Level, String)> {
    let mut last = None;
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
    last
}

/// Offer the hubs found with mDNS, or an address; non-interactive, the one
/// hub found or the saved address.
pub async fn choose_hub(cfg: &AgentConfig) -> Result<String> {
    say("looking for hubs on the LAN (mDNS, 3 s)...");
    let hubs = crate::discovery::discover(Duration::from_secs(3))
        .await
        .unwrap_or_default();
    let found: Vec<(String, SocketAddr)> = hubs
        .iter()
        .filter_map(|h| h.addrs.first().map(|a| (h.name.clone(), *a)))
        .collect();
    if !std::io::stdin().is_terminal() {
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
pub fn pick_hub(
    answer: &str,
    found: &[(String, SocketAddr)],
    default: Option<&str>,
) -> Option<String> {
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

/// The token, asked without echo; Enter keeps the saved one (`None`).
pub fn ask_token(cfg: &AgentConfig) -> Result<Option<String>> {
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

/// A step heading of `setup`.
pub fn step(name: &str) {
    println!("\n== {name}");
}

/// A line under a step.
pub fn say(msg: &str) {
    println!("   {msg}");
}

pub fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_from_the_log() {
        let log = "\
INFO agent_core: hub certificate pinned fingerprint=ab
INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepi3 index=3
WARN agent_core: hub closed the session
INFO agent_core: reconnecting in 0.5 s
INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepi3 index=3";
        let (level, detail) = hub_state(log.lines()).unwrap();
        assert_eq!(level, Level::Ok);
        assert!(detail.contains("index=3"));
        let (level, _) =
            hub_state("ERROR agent_core: the hub rejected the pairing token".lines()).unwrap();
        assert_eq!(level, Level::Fail);
        assert!(hub_state("".lines()).is_none());
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

    #[test]
    fn config_lines() {
        let dir = std::env::temp_dir().join(format!("rawkvm-setup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("edge.toml");
        assert_eq!(config_line(&path, "setup").0, Level::Fail);
        AgentConfig {
            hub: "192.168.1.28".into(),
            token: "ab".repeat(32),
            ..Default::default()
        }
        .save(&path)
        .unwrap();
        let (level, detail) = config_line(&path, "setup");
        assert_eq!(level, Level::Fail);
        assert!(detail.ends_with("hub 192.168.1.28, token set, not paired"));
        assert!(!detail.contains(&"ab".repeat(32)));
        AgentConfig {
            token: "ab".repeat(32),
            hub_fingerprint: "cd".repeat(32),
            ..Default::default()
        }
        .save(&path)
        .unwrap();
        let (level, detail) = config_line(&path, "setup");
        assert_eq!(level, Level::Ok);
        assert!(detail.ends_with("hub mDNS, token set, certificate cdcdcdcdcdcd..."));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
