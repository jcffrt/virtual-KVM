//! Hub configuration (`/etc/rawkvm/hub.toml`).
//!
//! The file is created on first start with a fresh hub id and pairing token.
//! Missing keys take their defaults, so a hand-written file only needs the
//! values it wants to override.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use proto::TOKEN_LEN;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_CONFIG: &str = "/etc/rawkvm/hub.toml";
pub const DEFAULT_STATE_DIR: &str = "/var/lib/rawkvm";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HubConfig {
    /// Human readable name, shown by agents and the widget.
    pub name: String,
    pub hub_id: Uuid,
    /// Pairing token, 64 hex characters. Agents must present it in `Hello`.
    pub token: String,
    pub listen: SocketAddr,
    /// Where the TLS certificate and key are kept.
    pub state_dir: PathBuf,
    /// Advertise the hub with mDNS / DNS-SD.
    pub mdns: bool,
    pub receiver: ReceiverMatch,
    pub switching: Switching,
    /// Edge switching tuning (`push_ms`, `corner`, `dead_zone_px`).
    pub edge: proto::EdgeOptions,
    /// Edge links between agents, by agent index (`[[layout]]`, see
    /// `layout.rs`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layout: Vec<crate::layout::LinkSpec>,
    /// Per-agent options, by agent index (`[[node_options]]`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub node_options: Vec<NodeOptions>,
    /// HID++ with the receiver's devices (`[hidpp]`).
    pub hidpp: HidppConfig,
}

/// `[hidpp]`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HidppConfig {
    /// Set the keyboards' OS layout (MULTIPLATFORM) to the OS of the agent
    /// that becomes active (ADR 0014).
    pub os_profile: bool,
}

impl Default for HidppConfig {
    fn default() -> Self {
        Self { os_profile: true }
    }
}

/// Keyboard layout profile forced for one agent (`os_profile` in
/// `[[node_options]]`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OsProfile {
    Windows,
    Macos,
    Linux,
    /// Leave the keyboards as they are when this agent becomes active.
    None,
}

/// Options for one agent, by its index (stable, see `nodes.toml`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NodeOptions {
    pub index: u8,
    /// Swap Alt and GUI in the keyboard reports sent to this agent
    /// (ADR 0011). A deviation from raw forwarding, off by default.
    #[serde(default)]
    pub swap_alt_gui: bool,
    /// Keyboard profile for this agent instead of the one of its OS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_profile: Option<OsProfile>,
}

/// Which hidraw nodes are forwarded. See ADR 0003.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MatchMode {
    /// The receiver's own USB interfaces, with `hid-generic` (no DJ).
    Receiver,
    /// The per-device HID children created by `hid-logitech-dj`.
    DjChildren,
}

/// Which hidraw interfaces belong to the receiver.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReceiverMatch {
    #[serde(rename = "match")]
    pub mode: MatchMode,
    /// Vendor of the receiver.
    pub vid: u16,
    /// Accepted receiver product ids. Empty accepts any product of `vid`.
    /// In `dj-children` mode this is matched against the parent receiver.
    pub pids: Vec<u16>,
    /// Take an exclusive `EVIOCGRAB` on the receiver's evdev nodes so the
    /// hub's own console does not consume the keystrokes.
    pub grab_evdev: bool,
    /// In `receiver` mode, send the DJ "switch" command with an empty
    /// bitfield whenever the receiver is opened, so the paired devices
    /// report on interfaces 0/1 instead of multiplexed DJ reports.
    pub force_normal_mode: bool,
    /// In `receiver` mode, skip interfaces bound to any driver other than
    /// `hid-generic`: with `hid-logitech-dj` bound the reports are not the
    /// receiver's raw ones and the DJ-off command does not help.
    pub require_hid_generic: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Switching {
    /// Minimum time between two edge switches.
    pub hysteresis_ms: u64,
    /// Send every non-mouse report a second time after this delay, so that a
    /// single lost datagram cannot leave a key pressed. 0 disables it.
    pub resend_ms: u64,
    /// Emergency hotkey: Ctrl, Ctrl, then 1..4.
    pub hotkey: bool,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            name: default_name(),
            hub_id: Uuid::nil(),
            token: String::new(),
            listen: SocketAddr::from(([0, 0, 0, 0], proto::DEFAULT_PORT)),
            state_dir: PathBuf::from(DEFAULT_STATE_DIR),
            mdns: true,
            receiver: ReceiverMatch::default(),
            switching: Switching::default(),
            edge: proto::EdgeOptions::default(),
            layout: Vec::new(),
            node_options: Vec::new(),
            hidpp: HidppConfig::default(),
        }
    }
}

impl Default for ReceiverMatch {
    fn default() -> Self {
        Self {
            mode: MatchMode::Receiver,
            vid: 0x046d,
            // Unifying (C52B, C532), Nano (C534), Lightspeed (C539, C53F,
            // C547), Bolt (C548).
            pids: vec![0xc52b, 0xc532, 0xc534, 0xc539, 0xc53f, 0xc547, 0xc548],
            grab_evdev: true,
            force_normal_mode: true,
            require_hid_generic: true,
        }
    }
}

impl Default for Switching {
    fn default() -> Self {
        Self {
            hysteresis_ms: 150,
            resend_ms: 15,
            hotkey: true,
        }
    }
}

fn default_name() -> String {
    // `pi.lan` -> `pi`: a dot would split the DNS-SD instance name.
    gethostname::gethostname()
        .into_string()
        .ok()
        .and_then(|s| s.split('.').next().map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "rawkvm-hub".into())
}

impl ReceiverMatch {
    pub fn matches(&self, vid: u16, pid: u16) -> bool {
        vid == self.vid && (self.pids.is_empty() || self.pids.contains(&pid))
    }
}

impl HubConfig {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn token_bytes(&self) -> Result<[u8; TOKEN_LEN]> {
        proto::parse_token(&self.token)
            .context("`token` in the hub config must be 64 hex characters")
    }

    /// Fill in a missing hub id and token. Returns true if anything changed.
    pub fn complete(&mut self) -> bool {
        let mut changed = false;
        if self.hub_id.is_nil() {
            self.hub_id = Uuid::new_v4();
            changed = true;
        }
        if self.token.is_empty() {
            let mut t = [0u8; TOKEN_LEN];
            rand::thread_rng().fill_bytes(&mut t);
            self.token = proto::token_hex(&t);
            changed = true;
        }
        changed
    }

    /// Read the config if it exists, defaults otherwise. Never writes. Used
    /// by the diagnostic modes.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Read the config, generating and persisting the hub id and token if
    /// they are missing. Returns the config and whether a new token was
    /// generated (so the caller can show it).
    pub fn load_or_init(path: &Path) -> Result<(Self, bool)> {
        let mut cfg = Self::load_or_default(path)?;
        let had_token = !cfg.token.is_empty();
        if cfg.complete() {
            cfg.save(path)?;
        }
        cfg.token_bytes()?;
        Ok((cfg, !had_token))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self)?;
        let text = format!(
            "# RawKVM hub configuration. Generated on first start; edit freely.\n\
             # `token` must be entered in every agent once (pairing).\n\n{body}"
        );
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write_private(path, text.as_bytes())
    }
}

/// Write a file readable only by its owner, atomically and durably: a
/// temporary file is written and fsynced, renamed over `path`, and the
/// directory is fsynced. The hub's board may lose power at any moment
/// (an OTG port fed by the PC), and the SD card keeps writes in a cache
/// for a while; without this a power cut can leave an empty or old file.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(data)?;
    f.sync_all()
        .with_context(|| format!("syncing {}", tmp.display()))?;
    drop(f);
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .with_context(|| format!("syncing {}", dir.display()))?;
    }
    Ok(())
}

pub fn ensure_dir(dir: &Path) -> Result<()> {
    if dir.exists() && !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_takes_defaults() {
        let cfg = HubConfig::parse("name = \"pi\"\n[switching]\nhysteresis_ms = 300\n").unwrap();
        assert_eq!(cfg.name, "pi");
        assert_eq!(cfg.switching.hysteresis_ms, 300);
        assert_eq!(cfg.switching.resend_ms, 15);
        assert_eq!(cfg.listen.port(), proto::DEFAULT_PORT);
        assert!(cfg.receiver.matches(0x046d, 0xc52b));
        assert!(!cfg.receiver.matches(0x046d, 0x1234));
        assert!(!cfg.receiver.matches(0x1234, 0xc52b));
    }

    #[test]
    fn node_options() {
        let c = HubConfig::parse("[[node_options]]\nindex = 1\nswap_alt_gui = true\n").unwrap();
        assert_eq!(
            c.node_options,
            [NodeOptions {
                index: 1,
                swap_alt_gui: true,
                os_profile: None,
            }]
        );
        let c = HubConfig::parse(
            "[hidpp]\nos_profile = false\n[[node_options]]\nindex = 3\nos_profile = \"windows\"\n",
        )
        .unwrap();
        assert!(!c.hidpp.os_profile);
        assert_eq!(c.node_options[0].os_profile, Some(OsProfile::Windows));
        assert!(HubConfig::default().hidpp.os_profile);
        assert!(HubConfig::parse("[[node_options]]\nindex = 1\nos_profile = \"dos\"\n").is_err());
        assert!(HubConfig::parse("[[node_options]]\nindex = 1\nswap = true\n").is_err());
        assert!(HubConfig::default().node_options.is_empty());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(HubConfig::parse("nmae = \"typo\"\n").is_err());
    }

    #[test]
    fn match_mode() {
        assert_eq!(HubConfig::default().receiver.mode, MatchMode::Receiver);
        let cfg = HubConfig::parse("[receiver]\nmatch = \"dj-children\"\n").unwrap();
        assert_eq!(cfg.receiver.mode, MatchMode::DjChildren);
        assert!(HubConfig::parse("[receiver]\nmatch = \"other\"\n").is_err());
    }

    #[test]
    fn init_generates_once_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("rawkvm-hub-test-{}", Uuid::new_v4()));
        let path = dir.join("hub.toml");
        let (a, created) = HubConfig::load_or_init(&path).unwrap();
        assert!(created);
        assert!(!a.hub_id.is_nil());
        assert_eq!(a.token.len(), 64);
        let (b, created) = HubConfig::load_or_init(&path).unwrap();
        assert!(!created);
        assert_eq!(a, b);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
