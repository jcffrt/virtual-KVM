//! Agent configuration (`~/.config/rawkvm/agent.toml` on Linux; the OS
//! config directory elsewhere).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use proto::TOKEN_LEN;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// Persistent identity of this machine. Generated on first start.
    pub node_id: Uuid,
    pub node_name: String,
    /// `host:port` of the hub. Empty: discover it with mDNS.
    pub hub: String,
    /// Pairing token shown by the hub on its first start (64 hex chars).
    pub token: String,
    /// SHA-256 of the hub certificate, pinned on the first successful
    /// connection (trust on first use). Empty: not paired yet.
    pub hub_fingerprint: String,
    /// Edge helpers only: `node_id` of the agent this helper acts for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub for_node: Option<Uuid>,
    /// Widget on Windows only: power the hub off when this PC shuts down
    /// or restarts (the PC that powers the hub's board, ADR 0025).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub power_off_hub_with_pc: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            node_id: Uuid::nil(),
            node_name: gethostname::gethostname()
                .into_string()
                .ok()
                .and_then(|s| s.split('.').next().map(str::to_string))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "rawkvm-agent".into()),
            hub: String::new(),
            token: String::new(),
            hub_fingerprint: String::new(),
            for_node: None,
            power_off_hub_with_pc: false,
        }
    }
}

/// What to do with the fingerprint a hub presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// No fingerprint pinned yet: accept and pin it.
    FirstUse,
    /// Matches the pinned one.
    Known,
    /// Differs from the pinned one: refuse.
    Mismatch { pinned: String },
}

impl AgentConfig {
    pub fn default_path() -> Result<PathBuf> {
        // `$XDG_CONFIG_HOME` (~/.config) on Linux, Application Support on
        // macOS, %APPDATA% on Windows.
        Ok(dirs::config_dir()
            .context("no config directory for this user")?
            .join("rawkvm")
            .join("agent.toml"))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(text)?;
        if !cfg.hub_fingerprint.is_empty() && !is_fingerprint(&cfg.hub_fingerprint) {
            bail!("`hub_fingerprint` must be 64 hex characters");
        }
        Ok(cfg)
    }

    /// Load the config, creating it (with a new node id) if needed.
    pub fn load_or_init(path: &Path) -> Result<Self> {
        let mut cfg = match fs::read_to_string(path) {
            Ok(text) => {
                Self::parse(&text).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if cfg.node_id.is_nil() {
            cfg.node_id = Uuid::new_v4();
            cfg.save(path)?;
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = format!(
            "# RawKVM agent configuration.\n# Remove `hub_fingerprint` to pair with a different hub.\n\n{}",
            toml::to_string_pretty(self)?
        );
        // Atomic and durable: the gadget's board may lose power at any time.
        let tmp = path.with_extension("tmp");
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        #[cfg(unix)]
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::File::open(dir).and_then(|d| d.sync_all())?;
        }
        Ok(())
    }

    pub fn token_bytes(&self) -> Result<[u8; TOKEN_LEN]> {
        if self.token.is_empty() {
            bail!("no pairing token configured (run with --token <hex> once)");
        }
        proto::parse_token(&self.token).context("the pairing token must be 64 hex characters")
    }

    pub fn trust(&self, presented: &str) -> Trust {
        if self.hub_fingerprint.is_empty() {
            Trust::FirstUse
        } else if self.hub_fingerprint.eq_ignore_ascii_case(presented) {
            Trust::Known
        } else {
            Trust::Mismatch {
                pinned: self.hub_fingerprint.clone(),
            }
        }
    }
}

/// Take the pairing (hub, token, pinned certificate) of another program's
/// config on this machine: the Windows widget uses the edge helper's, so
/// that setup.ps1 pairs once (ADR 0024). Only a complete pairing is taken,
/// and only when `target` has none or `source` was written after it (a
/// pairing made later in the widget itself is kept). Complete means a
/// token and a pinned certificate; the hub address is copied as it is, and
/// empty means find the hub with mDNS. Returns whether `target` changed.
pub fn adopt_pairing(target: &Path, source: &Path) -> Result<bool> {
    let Ok(text) = fs::read_to_string(source) else {
        return Ok(false);
    };
    let src = AgentConfig::parse(&text).with_context(|| format!("parsing {}", source.display()))?;
    if src.token.is_empty() || src.hub_fingerprint.is_empty() {
        return Ok(false);
    }
    let mut cfg = AgentConfig::load_or_init(target)?;
    let same = cfg.hub == src.hub
        && cfg.token == src.token
        && cfg
            .hub_fingerprint
            .eq_ignore_ascii_case(&src.hub_fingerprint);
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    let newer = match (modified(source), modified(target)) {
        (Some(s), Some(t)) => s > t,
        _ => true,
    };
    let unpaired = cfg.token.is_empty() || cfg.hub_fingerprint.is_empty();
    if same || !(unpaired || newer) {
        return Ok(false);
    }
    cfg.hub = src.hub;
    cfg.token = src.token;
    cfg.hub_fingerprint = src.hub_fingerprint;
    cfg.save(target)?;
    Ok(true)
}

pub fn is_fingerprint(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn trust_on_first_use() {
        let mut cfg = AgentConfig::default();
        assert_eq!(cfg.trust(FP), Trust::FirstUse);
        cfg.hub_fingerprint = FP.to_uppercase();
        assert_eq!(cfg.trust(FP), Trust::Known);
        let other = FP.replace('b', "c");
        assert!(matches!(cfg.trust(&other), Trust::Mismatch { .. }));
    }

    #[test]
    fn parse_validates() {
        assert!(AgentConfig::parse("hub_fingerprint = \"abc\"\n").is_err());
        assert!(AgentConfig::parse("bogus = 1\n").is_err());
        let cfg = AgentConfig::parse("hub = \"10.0.0.2:4433\"\n").unwrap();
        assert_eq!(cfg.hub, "10.0.0.2:4433");
        assert!(cfg.token_bytes().is_err());
    }

    #[test]
    fn init_persists_node_id() {
        let dir = std::env::temp_dir().join(format!("rawkvm-agent-test-{}", Uuid::new_v4()));
        let path = dir.join("agent.toml");
        let a = AgentConfig::load_or_init(&path).unwrap();
        assert!(!a.node_id.is_nil());
        let mut b = AgentConfig::load_or_init(&path).unwrap();
        assert_eq!(a.node_id, b.node_id);
        b.token = "11".repeat(32);
        b.hub_fingerprint = FP.into();
        b.save(&path).unwrap();
        let c = AgentConfig::load_or_init(&path).unwrap();
        assert_eq!(c, b);
        assert_eq!(c.token_bytes().unwrap(), [0x11; 32]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adopts_a_newer_complete_pairing() {
        let dir = std::env::temp_dir().join(format!("rawkvm-adopt-{}", Uuid::new_v4()));
        let (widget, edge) = (dir.join("widget.toml"), dir.join("edge.toml"));
        let tick = || std::thread::sleep(std::time::Duration::from_millis(30));
        // No edge config, or an incomplete one: nothing.
        assert!(!adopt_pairing(&widget, &edge).unwrap());
        let mut e = AgentConfig {
            node_id: Uuid::new_v4(),
            hub: "192.168.1.28".into(),
            token: "11".repeat(32),
            ..Default::default()
        };
        e.save(&edge).unwrap();
        assert!(!adopt_pairing(&widget, &edge).unwrap());

        // An unpaired widget takes it, and keeps its own node id.
        e.hub_fingerprint = FP.into();
        e.save(&edge).unwrap();
        let w0 = AgentConfig::load_or_init(&widget).unwrap();
        assert!(adopt_pairing(&widget, &edge).unwrap());
        let w = AgentConfig::load_or_init(&widget).unwrap();
        assert_eq!(
            (w.node_id, w.hub.as_str(), w.hub_fingerprint.as_str()),
            (w0.node_id, "192.168.1.28", FP)
        );
        assert!(!adopt_pairing(&widget, &edge).unwrap(), "already the same");

        // Paired later in the widget itself: kept.
        tick();
        let mut own = w.clone();
        own.hub = "192.168.1.99".into();
        own.save(&widget).unwrap();
        assert!(!adopt_pairing(&widget, &edge).unwrap());

        // The edge paired again afterwards (setup.ps1 -Pair): taken.
        tick();
        e.token = "22".repeat(32);
        e.save(&edge).unwrap();
        assert!(adopt_pairing(&widget, &edge).unwrap());
        assert_eq!(
            AgentConfig::load_or_init(&widget).unwrap().token,
            "22".repeat(32)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adopts_a_pairing_made_with_mdns() {
        // setup.sh with Enter at the hub address: hub = "" (mDNS).
        let dir = std::env::temp_dir().join(format!("rawkvm-adopt-{}", Uuid::new_v4()));
        let (widget, edge) = (dir.join("widget.toml"), dir.join("edge.toml"));
        AgentConfig {
            node_id: Uuid::new_v4(),
            token: "11".repeat(32),
            hub_fingerprint: FP.into(),
            ..Default::default()
        }
        .save(&edge)
        .unwrap();
        assert!(adopt_pairing(&widget, &edge).unwrap());
        let w = AgentConfig::load_or_init(&widget).unwrap();
        assert_eq!(
            (w.hub.as_str(), w.token.as_str(), w.hub_fingerprint.as_str()),
            ("", "11".repeat(32).as_str(), FP)
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
