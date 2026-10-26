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
    /// The receivers whose interfaces are forwarded (`[[receiver]]`, or a
    /// single `[receiver]` table as written before phase 6). The position
    /// is the receiver index of the protocol (`HidDevice::receiver`).
    #[serde(rename = "receiver", deserialize_with = "one_or_many")]
    pub receivers: Vec<ReceiverMatch>,
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

/// Logitech's USB vendor id. HID++ and the DJ mode switch only apply to
/// receivers of this vendor (ADR 0030).
pub const LOGITECH_VID: u16 = 0x046d;

/// Logitech receivers: Unifying (C52B, C532), Nano (C534), Lightspeed
/// (C539, C53F, C547), Bolt (C548).
const LOGITECH_PIDS: [u16; 7] = [0xc52b, 0xc532, 0xc534, 0xc539, 0xc53f, 0xc547, 0xc548];

/// Most receiver blocks a hub takes: device ids are `16 * receiver +
/// interface` (see `devices::assign_id`).
pub const MAX_RECEIVERS: usize = 16;

/// Which hidraw interfaces belong to one receiver (`[[receiver]]`).
///
/// A missing key takes a default that depends on `vid`: for Logitech the
/// known receiver products, `force_normal_mode` and `require_hid_generic`;
/// for any other vendor every product and neither of the two.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(from = "ReceiverSpec")]
pub struct ReceiverMatch {
    /// Shown by the widget and used in `settings.toml`. Defaults to
    /// `receiver-N`, N being the block's index (0 for the first).
    pub name: String,
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

/// A `[[receiver]]` block as written, before the vendor defaults.
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ReceiverSpec {
    name: Option<String>,
    #[serde(rename = "match")]
    mode: Option<MatchMode>,
    vid: Option<u16>,
    pids: Option<Vec<u16>>,
    grab_evdev: Option<bool>,
    force_normal_mode: Option<bool>,
    require_hid_generic: Option<bool>,
}

impl From<ReceiverSpec> for ReceiverMatch {
    fn from(s: ReceiverSpec) -> Self {
        let vid = s.vid.unwrap_or(LOGITECH_VID);
        let logitech = vid == LOGITECH_VID;
        Self {
            // Filled in with the block's index by `HubConfig::parse`.
            name: s.name.unwrap_or_default(),
            mode: s.mode.unwrap_or(MatchMode::Receiver),
            vid,
            pids: s.pids.unwrap_or_else(|| {
                if logitech {
                    LOGITECH_PIDS.to_vec()
                } else {
                    Vec::new()
                }
            }),
            grab_evdev: s.grab_evdev.unwrap_or(true),
            force_normal_mode: s.force_normal_mode.unwrap_or(logitech),
            require_hid_generic: s.require_hid_generic.unwrap_or(logitech),
        }
    }
}

/// `[receiver]` (one table, the form before phase 6) or `[[receiver]]`.
/// Through a `toml::Value` so a typo in a block gives serde's own message.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ReceiverMatch>, D::Error> {
    use serde::de::Error;
    match toml::Value::deserialize(d)? {
        toml::Value::Array(list) => list
            .into_iter()
            .map(|v| ReceiverMatch::deserialize(v).map_err(D::Error::custom))
            .collect(),
        one => Ok(vec![
            ReceiverMatch::deserialize(one).map_err(D::Error::custom)?
        ]),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Switching {
    /// Minimum time between two edge switches.
    pub hysteresis_ms: u64,
    /// Send every non-mouse report a second time after this delay, so that a
    /// single lost datagram cannot leave a key pressed. 0 disables it.
    pub resend_ms: u64,
    /// Emergency hotkey: modifier, modifier, then 1..4 (0 previous, Esc
    /// release keys).
    pub hotkey: bool,
    /// "ctrl", "shift", "alt" or "gui".
    pub hotkey_modifier: proto::HotkeyModifier,
    /// Modifier, modifier, 0: back to the previous agent.
    pub hotkey_previous: bool,
    /// Modifier, modifier, Esc: release every key.
    pub hotkey_release_keys: bool,
}

impl Switching {
    pub fn hotkey(&self) -> proto::Hotkey {
        proto::Hotkey {
            enabled: self.hotkey,
            modifier: self.hotkey_modifier,
            previous: self.hotkey_previous,
            release_keys: self.hotkey_release_keys,
        }
    }
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
            receivers: vec![ReceiverMatch {
                name: "receiver-0".into(),
                ..ReceiverMatch::default()
            }],
            switching: Switching::default(),
            edge: proto::EdgeOptions::default(),
            layout: Vec::new(),
            node_options: Vec::new(),
            hidpp: HidppConfig::default(),
        }
    }
}

/// A Logitech receiver with the defaults (and no name).
impl Default for ReceiverMatch {
    fn default() -> Self {
        ReceiverSpec::default().into()
    }
}

impl Default for Switching {
    fn default() -> Self {
        Self {
            hysteresis_ms: 150,
            resend_ms: 15,
            hotkey: true,
            hotkey_modifier: proto::HotkeyModifier::Ctrl,
            hotkey_previous: true,
            hotkey_release_keys: true,
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
    /// A block for `vid` with that vendor's defaults (ADR 0030) and no
    /// name.
    pub fn for_vendor(vid: u16) -> Self {
        ReceiverSpec {
            vid: Some(vid),
            ..Default::default()
        }
        .into()
    }

    pub fn matches(&self, vid: u16, pid: u16) -> bool {
        vid == self.vid && (self.pids.is_empty() || self.pids.contains(&pid))
    }

    pub fn is_logitech(&self) -> bool {
        self.vid == LOGITECH_VID
    }

    /// `force_normal_mode`, which only means something for Logitech.
    pub fn switches_dj_off(&self) -> bool {
        self.force_normal_mode && self.is_logitech() && self.mode == MatchMode::Receiver
    }

    /// Could an interface match both blocks?
    fn overlaps(&self, other: &ReceiverMatch) -> bool {
        self.vid == other.vid
            && (self.pids.is_empty()
                || other.pids.is_empty()
                || self.pids.iter().any(|p| other.pids.contains(p)))
    }
}

impl HubConfig {
    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg: Self = toml::from_str(text)?;
        for (i, r) in cfg.receivers.iter_mut().enumerate() {
            if r.name.is_empty() {
                r.name = format!("receiver-{i}");
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.receivers.is_empty() {
            bail!("the hub config needs at least one [[receiver]] block");
        }
        if self.receivers.len() > MAX_RECEIVERS {
            bail!(
                "the hub config has {} [[receiver]] blocks; at most {MAX_RECEIVERS}",
                self.receivers.len()
            );
        }
        for (i, a) in self.receivers.iter().enumerate() {
            for (j, b) in self.receivers.iter().enumerate().skip(i + 1) {
                if a.name == b.name {
                    bail!(
                        "[[receiver]] blocks {i} and {j} are both named {:?}",
                        a.name
                    );
                }
                if a.overlaps(b) {
                    bail!(
                        "[[receiver]] {:?} and {:?} overlap (vid {:04x} and the same or no pids): \
                         an interface would belong to both",
                        a.name,
                        b.name,
                        a.vid
                    );
                }
            }
        }
        Ok(())
    }

    /// Things in the config the hub ignores, to log once at start.
    pub fn warnings(&self) -> Vec<String> {
        self.receivers
            .iter()
            .filter(|r| r.force_normal_mode && !r.is_logitech())
            .map(|r| {
                format!(
                    "[[receiver]] {:?}: force_normal_mode only applies to Logitech (vid 046d); ignored",
                    r.name
                )
            })
            .collect()
    }

    /// The receiver the HID++ features (paired devices, pairing, battery,
    /// keyboard layout) work with: the first Logitech one. HID++ for
    /// several receivers is not supported (ADR 0030).
    pub fn hidpp_receiver(&self) -> Option<usize> {
        self.receivers.iter().position(ReceiverMatch::is_logitech)
    }

    /// Receiver names by index, for the router.
    pub fn receiver_names(&self) -> Vec<String> {
        self.receivers.iter().map(|r| r.name.clone()).collect()
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

    /// The commented example shipped in the .deb shows the real defaults.
    #[test]
    fn packaged_example_matches_the_defaults() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/deb/hub/hub.toml.example");
        let text = fs::read_to_string(path).unwrap();
        let uncommented: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| {
                l.starts_with('[') || l.split_once(" = ").is_some_and(|(k, _)| !k.contains(' '))
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let cfg = HubConfig::parse(&uncommented).unwrap();
        let want = HubConfig {
            name: "<hostname>".into(),
            node_options: vec![NodeOptions {
                index: 1,
                swap_alt_gui: false,
                os_profile: Some(OsProfile::Macos),
            }],
            ..Default::default()
        };
        assert_eq!(cfg, want);
    }

    #[test]
    fn partial_file_takes_defaults() {
        let cfg = HubConfig::parse("name = \"pi\"\n[switching]\nhysteresis_ms = 300\n").unwrap();
        assert_eq!(cfg.name, "pi");
        assert_eq!(cfg.switching.hysteresis_ms, 300);
        assert_eq!(cfg.switching.resend_ms, 15);
        assert_eq!(cfg.listen.port(), proto::DEFAULT_PORT);
        assert_eq!(cfg.receivers.len(), 1);
        let r = &cfg.receivers[0];
        assert_eq!(r.name, "receiver-0");
        assert!(r.matches(0x046d, 0xc52b));
        assert!(!r.matches(0x046d, 0x1234));
        assert!(!r.matches(0x1234, 0xc52b));
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
        assert_eq!(HubConfig::default().receivers[0].mode, MatchMode::Receiver);
        let cfg = HubConfig::parse("[receiver]\nmatch = \"dj-children\"\n").unwrap();
        assert_eq!(cfg.receivers[0].mode, MatchMode::DjChildren);
        assert!(HubConfig::parse("[receiver]\nmatch = \"other\"\n").is_err());
        assert!(HubConfig::parse("[receiver]\nvdi = 1\n").is_err());
    }

    /// The `[receiver]` table a hub before phase 6 generated still loads,
    /// as a one-element list with the same values.
    #[test]
    fn single_receiver_table_still_loads() {
        let old = "name = \"rawkvm-hub\"\n\
                   [receiver]\n\
                   match = \"receiver\"\n\
                   vid = 1133\n\
                   pids = [50475, 50482, 50484, 50489, 50495, 50503, 50504]\n\
                   grab_evdev = true\n\
                   force_normal_mode = true\n\
                   require_hid_generic = true\n\
                   [switching]\nhysteresis_ms = 150\n";
        let cfg = HubConfig::parse(old).unwrap();
        assert_eq!(cfg.receivers, HubConfig::default().receivers);
        assert_eq!(cfg.hidpp_receiver(), Some(0));
        assert!(cfg.warnings().is_empty());
        // And saving writes the list form, which loads back the same.
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("[[receiver]]"), "{text}");
        assert_eq!(HubConfig::parse(&text).unwrap(), cfg);
    }

    #[test]
    fn several_receivers_with_vendor_defaults() {
        let cfg = HubConfig::parse(
            "[[receiver]]\nname = \"logi\"\n\
             [[receiver]]\nvid = 0x248a\n\
             [[receiver]]\nvid = 0x045e\npids = [0x07a5]\nforce_normal_mode = true\n",
        )
        .unwrap();
        let [logi, other, ms] = &cfg.receivers[..] else {
            panic!("{:?}", cfg.receivers);
        };
        assert_eq!(logi.name, "logi");
        assert!(logi.switches_dj_off() && logi.require_hid_generic);
        assert_eq!(logi.pids, LOGITECH_PIDS);
        assert_eq!(other.name, "receiver-1");
        assert!(other.pids.is_empty() && other.matches(0x248a, 0x8367));
        assert!(!other.force_normal_mode && !other.require_hid_generic && other.grab_evdev);
        assert_eq!(ms.name, "receiver-2");
        // Set by hand on another vendor: kept, warned about, never used.
        assert!(ms.force_normal_mode && !ms.switches_dj_off());
        assert_eq!(cfg.warnings().len(), 1);
        assert!(cfg.warnings()[0].contains("receiver-2"));
        assert_eq!(cfg.hidpp_receiver(), Some(0));
        assert_eq!(cfg.receiver_names(), ["logi", "receiver-1", "receiver-2"]);

        let no_logitech = HubConfig::parse("[[receiver]]\nvid = 0x248a\n").unwrap();
        assert_eq!(no_logitech.hidpp_receiver(), None);
    }

    #[test]
    fn overlapping_receivers_are_rejected() {
        let err = |text: &str| HubConfig::parse(text).unwrap_err().to_string();
        // Two Logitech blocks with the default pids.
        assert!(err("[[receiver]]\n[[receiver]]\nname = \"b\"\n").contains("overlap"));
        // Any product of a vendor against one of its products.
        assert!(
            err("[[receiver]]\nvid = 1\n[[receiver]]\nvid = 1\npids = [2]\n").contains("overlap")
        );
        assert!(
            err("[[receiver]]\nvid = 1\npids = [2, 3]\n[[receiver]]\nvid = 1\npids = [3]\n")
                .contains("overlap")
        );
        // Disjoint products of one vendor are fine.
        assert!(HubConfig::parse(
            "[[receiver]]\nvid = 1\npids = [2]\n[[receiver]]\nvid = 1\npids = [3]\n"
        )
        .is_ok());
        assert!(
            err("[[receiver]]\nname = \"x\"\n[[receiver]]\nname = \"x\"\nvid = 2\n")
                .contains("both named")
        );
        assert!(err("receiver = []\n").contains("at least one"));
        let many: String = (0..17)
            .map(|i| format!("[[receiver]]\nvid = {}\n", i + 1))
            .collect();
        assert!(err(&many).contains("at most"));
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
