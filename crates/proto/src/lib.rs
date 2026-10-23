//! RawKVM wire protocol.
//!
//! Two channels run over one QUIC connection per node:
//!
//! * **Control stream** (one bidirectional stream opened by the node): length
//!   prefixed [`Control`] messages serialized with `postcard`. See [`wire`].
//! * **QUIC datagrams** (unreliable, unordered): raw HID input reports. See
//!   [`datagram`].
//!
//! Everything here is platform independent and has no I/O dependencies apart
//! from the optional `tokio` framing helpers.

pub mod datagram;
pub mod descriptor;
pub mod wire;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Protocol version carried in [`Control::Hello`]. Bump on any incompatible
/// change to the messages or to the datagram header.
///
/// The handshake must stay decodable across versions so that a mismatch
/// ends in a clear `Reject { VersionMismatch }`: never reorder or change the
/// fields of `Hello` or the variants before `Reject`, and only append new
/// [`NodeKind`] variants.
///
/// * 1: phase 0/1.
/// * 2: `NodeKind::EdgeHelper`; `screen` in `SwitchTo` and `Activate`;
///   `options` in `Layout`.
/// * 3: optional [`Entry`] in `SwitchTo` / `Activate`; `canvas` in
///   `Layout`; `version` in `HubStatus`, `screens` in `NodeInfo`;
///   `Settings`; `Screen::scale` is native units per logical unit.
/// * 4: agent -> hub output report datagrams (`FLAG_OUTPUT`);
///   `ReceiverDevices`, `Unpair`.
pub const PROTO_VERSION: u16 = 4;

/// DNS-SD service type the hub advertises on the LAN.
pub const SERVICE_TYPE: &str = "_rawkvm._udp.local.";

/// Default UDP port of the hub's QUIC listener.
pub const DEFAULT_PORT: u16 = 4433;

/// ALPN identifier negotiated inside TLS.
pub const ALPN: &[u8] = b"rawkvm/1";

/// TLS server name the hub certificate is issued for. Agents do not verify
/// it (they pin the certificate fingerprint instead) but send it as SNI.
pub const CERT_SERVER_NAME: &str = "rawkvm-hub";

/// Length of the pairing token shared between the hub and its nodes.
pub const TOKEN_LEN: usize = 32;

/// Operating system of a node. Used for diagnostics and for the widget UI.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
    Other,
}

/// Role of a node. Agents own a virtual HID device; widgets only observe and
/// configure.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Agent,
    Widget,
    /// Watches the pointer and switches at screen edges for the machine of
    /// agent `for_node`, which cannot do it itself (a USB gadget, a root
    /// daemon outside the graphical session). Its `Screens` count as that
    /// node's, it may send `SwitchTo` while that node is active, and it gets
    /// that node's `Activate` / `Deactivate`.
    EdgeHelper {
        for_node: Uuid,
    },
}

/// A HID interface exposed by the receiver, described exactly as the kernel
/// reports it so that every agent can recreate it byte for byte.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HidDevice {
    /// Stable index while the hub process lives. Also the first byte of every
    /// datagram.
    pub id: u8,
    pub name: String,
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    /// USB interface number of the receiver (keyboard / mouse / HID++).
    pub interface: u8,
    /// Raw report descriptor, as returned by `HIDIOCGRDESC`.
    pub report_descriptor: Vec<u8>,
    /// True if the descriptor declares report IDs; reports then carry the ID
    /// as their first byte.
    pub uses_report_ids: bool,
}

/// Screen edge, in the coordinate system of the node that owns the screen.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The edge the pointer enters through on the destination screen.
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
}

/// One monitor of a node, positioned inside that node's own virtual desktop.
///
/// `x`, `y`, `w`, `h` are always in the units in which that node's edge
/// helper reads and places the pointer (macOS: points; Windows: physical
/// pixels, per-monitor DPI aware), so the helper never converts. `scale`
/// is native units per logical unit and only serves to draw the widget's
/// canvas (the widget divides): 1.0 on macOS, DPI / 96 on Windows.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Screen {
    pub node: Uuid,
    /// Index of the screen within the node (0 = primary).
    pub index: u8,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    /// Native units per logical unit (canvas drawing only).
    pub scale: f32,
}

/// Where the pointer appears on the node being activated: `pos_ratio`
/// (0.0..=1.0, top / left end first) along `edge` of `screen`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub screen: u8,
    pub edge: Edge,
    pub pos_ratio: f32,
}

impl Default for HubSettings {
    fn default() -> Self {
        Self {
            hysteresis_ms: 150,
            hotkey: true,
        }
    }
}

/// Switching settings a widget can change at run time; the hub keeps them
/// in `<state_dir>/settings.toml`, which overrides `[switching]` of
/// `hub.toml`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubSettings {
    /// Minimum time between two edge switches.
    pub hysteresis_ms: u32,
    /// Emergency hotkey Ctrl, Ctrl, 1..4.
    pub hotkey: bool,
}

/// Where the widget's monitor canvas puts one node's block of screens, in
/// logical units (points). Rigid block: the node's screens keep their own
/// arrangement, multiplied by the node's logical factor.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Placement {
    pub node: Uuid,
    pub x: f64,
    pub y: f64,
    /// Manual correction of the node's scale (native units per logical
    /// unit) when the reported one is wrong. `None`: use the reported one.
    pub scale: Option<f32>,
}

/// Link between an edge range of one screen and an edge range of another.
/// Ranges are expressed as `pos_ratio` intervals (0.0..=1.0 along the edge).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EdgeLink {
    pub from_node: Uuid,
    pub from_screen: u8,
    pub from_edge: Edge,
    pub from_range: (f32, f32),
    pub to_node: Uuid,
    pub to_screen: u8,
    pub to_edge: Edge,
    pub to_range: (f32, f32),
}

/// Edge switching tuning, set in the hub config and sent with `Layout`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct EdgeOptions {
    /// The pointer must stay on the edge this long before switching.
    /// 0 switches on contact.
    pub push_ms: u16,
    /// Percentage of each end of an edge that never switches (window close
    /// buttons, hot corners).
    pub corner_pct: u8,
    /// Distance from the edge at which the pointer enters, and that it must
    /// leave behind before that edge can switch back.
    pub dead_zone_px: u16,
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self {
            push_ms: 0,
            corner_pct: 3,
            dead_zone_px: 2,
        }
    }
}

/// Progress of a Unifying pairing session (phase 3).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum PairingEvent {
    Started { timeout_s: u8 },
    DeviceFound { name: String },
    Paired { name: String, kind: String },
    Failed { reason: String },
    TimedOut,
}

/// A device paired with the hub's receiver (phase 3).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PairedDevice {
    /// Receiver slot, 1..=6.
    pub slot: u8,
    pub name: String,
    /// "keyboard", "mouse", "trackball", ... as the receiver reports it.
    pub kind: String,
    pub wireless_pid: u16,
    /// The device supports the HID++ feature HOSTS_INFO (0x1815).
    pub hosts_info: bool,
    /// Keyboard layout profile the hub last set, if any.
    pub os_profile: Option<Os>,
}

/// Summary of a connected node as seen by the hub.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct NodeInfo {
    pub node_id: Uuid,
    pub node_name: String,
    pub os: Os,
    pub kind: NodeKind,
    pub assigned_index: u8,
    pub rtt_ms: Option<f32>,
    /// Screens the hub knows for this node (reported by it or its edge
    /// helper). 0 for an agent: no edge helper yet.
    pub screens: u8,
}

/// Hub status broadcast to widgets (and agents) on change.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HubStatus {
    pub hub_name: String,
    pub active: Option<Uuid>,
    pub nodes: Vec<NodeInfo>,
    pub device_count: u8,
    pub uptime_s: u64,
    /// Hub build, e.g. "0.1.0 (d69e767)".
    pub version: String,
}

/// Why the hub rejected a node.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    BadToken,
    VersionMismatch,
    TooManyNodes,
}

/// Control plane messages. The direction of each variant is documented.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Control {
    /// node -> hub. First message on the control stream.
    Hello {
        proto_ver: u16,
        node_id: Uuid,
        node_name: String,
        os: Os,
        kind: NodeKind,
        token: [u8; TOKEN_LEN],
    },
    /// hub -> node. Reply to `Hello`.
    HelloAck {
        hub_id: Uuid,
        hub_name: String,
        /// 1..=4 for agents (used by the emergency hotkey), 0 for widgets.
        assigned_index: u8,
    },
    /// hub -> node. Reply to `Hello` when the node is not accepted. The hub
    /// closes the connection afterwards.
    Reject {
        reason: RejectReason,
    },
    /// hub -> nodes. Sent after `HelloAck` and whenever the device set changes.
    DeviceList {
        devices: Vec<HidDevice>,
    },
    /// agent -> hub. The agent's monitors, sent after `HelloAck` and on change.
    Screens {
        screens: Vec<Screen>,
    },
    /// hub -> nodes (from the hub config or a widget); widget -> hub.
    /// Every known screen and every edge link, both directions listed, and
    /// the widget canvas (empty while the hub uses its config seed).
    Layout {
        screens: Vec<Screen>,
        edges: Vec<EdgeLink>,
        options: EdgeOptions,
        canvas: Vec<Placement>,
    },
    /// agent, edge helper or widget -> hub. Make `target` active. With an
    /// `entry` (an edge was reached; `pos_ratio` already mapped through the
    /// link's ranges) the target's helper places the pointer there;
    /// without one (widget menu) it stays where it is.
    SwitchTo {
        target: Uuid,
        entry: Option<Entry>,
    },
    /// hub -> agent and its edge helpers. This node now receives input.
    /// With an entry the helper places the pointer; without one (hotkey,
    /// fallback, widget menu) it leaves it alone.
    Activate {
        entry: Option<Entry>,
    },
    /// hub -> agent. This node no longer receives input. A "release all"
    /// datagram for every device precedes this message.
    Deactivate,
    /// widget -> hub.
    PairingStart {
        receiver: String,
        timeout_s: u8,
    },
    /// hub -> widget.
    PairingEvent(PairingEvent),
    /// widget -> hub.
    PairingStop,
    /// hub -> nodes.
    Status(HubStatus),
    /// Either direction. The payload is echoed back in `Pong`.
    Ping(u64),
    Pong(u64),
    /// hub -> widgets (on connect and on change); widget -> hub (change).
    Settings(HubSettings),
    /// hub -> widgets (on connect and on change). Devices paired with the
    /// receiver, read over HID++.
    ReceiverDevices {
        devices: Vec<PairedDevice>,
    },
    /// widget -> hub. Unpair the device in receiver slot `slot`.
    Unpair {
        slot: u8,
    },
}

impl Control {
    /// Message name without payload, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Control::Hello { .. } => "Hello",
            Control::HelloAck { .. } => "HelloAck",
            Control::Reject { .. } => "Reject",
            Control::DeviceList { .. } => "DeviceList",
            Control::Screens { .. } => "Screens",
            Control::Layout { .. } => "Layout",
            Control::SwitchTo { .. } => "SwitchTo",
            Control::Activate { .. } => "Activate",
            Control::Deactivate => "Deactivate",
            Control::PairingStart { .. } => "PairingStart",
            Control::PairingEvent(_) => "PairingEvent",
            Control::PairingStop => "PairingStop",
            Control::Status(_) => "Status",
            Control::Ping(_) => "Ping",
            Control::Pong(_) => "Pong",
            Control::Settings(_) => "Settings",
            Control::ReceiverDevices { .. } => "ReceiverDevices",
            Control::Unpair { .. } => "Unpair",
        }
    }
}

/// Parse a 64 hex character token.
pub fn parse_token(hex_str: &str) -> Option<[u8; TOKEN_LEN]> {
    let s = hex_str.trim();
    if s.len() != TOKEN_LEN * 2 {
        return None;
    }
    let mut out = [0u8; TOKEN_LEN];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

/// Hex encode a token (lowercase).
pub fn token_hex(token: &[u8; TOKEN_LEN]) -> String {
    hex::encode(token)
}

/// SHA-256 fingerprint of a DER certificate, as 64 lowercase hex
/// characters. Published in the mDNS TXT record `fp` and pinned by agents.
pub fn cert_fingerprint(der: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(der))
}

/// Constant-time token comparison.
pub fn token_eq(a: &[u8; TOKEN_LEN], b: &[u8; TOKEN_LEN]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_roundtrip() {
        let hex_str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let t = parse_token(hex_str).unwrap();
        assert_eq!(t[0], 0x00);
        assert_eq!(t[1], 0x11);
        assert_eq!(t[31], 0xff);
        assert!(token_eq(&t, &t));
        let mut u = t;
        u[5] ^= 1;
        assert!(!token_eq(&t, &u));
        assert!(parse_token("zz").is_none());
        assert!(parse_token(&hex_str[..10]).is_none());
        assert_eq!(token_hex(&t), hex_str);
    }

    #[test]
    fn fingerprint_is_sha256_hex() {
        assert_eq!(
            cert_fingerprint(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn control_roundtrip() {
        let msgs = vec![
            Control::Hello {
                proto_ver: PROTO_VERSION,
                node_id: Uuid::new_v4(),
                node_name: "pc".into(),
                os: Os::Linux,
                kind: NodeKind::Agent,
                token: [7u8; TOKEN_LEN],
            },
            Control::HelloAck {
                hub_id: Uuid::new_v4(),
                hub_name: "hub".into(),
                assigned_index: 1,
            },
            Control::DeviceList {
                devices: vec![HidDevice {
                    id: 0,
                    name: "Logitech USB Receiver".into(),
                    vid: 0x046d,
                    pid: 0xc52b,
                    serial: None,
                    interface: 0,
                    report_descriptor: descriptor::tests::BOOT_KEYBOARD.to_vec(),
                    uses_report_ids: false,
                }],
            },
            Control::Activate {
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 0.25,
                }),
            },
            Control::Activate { entry: None },
            Control::Settings(HubSettings {
                hysteresis_ms: 150,
                hotkey: true,
            }),
            Control::Hello {
                proto_ver: PROTO_VERSION,
                node_id: Uuid::new_v4(),
                node_name: "mac-edge".into(),
                os: Os::MacOs,
                kind: NodeKind::EdgeHelper {
                    for_node: Uuid::new_v4(),
                },
                token: [7u8; TOKEN_LEN],
            },
            Control::SwitchTo {
                target: Uuid::new_v4(),
                entry: Some(Entry {
                    screen: 1,
                    edge: Edge::Left,
                    pos_ratio: 0.5,
                }),
            },
            Control::Layout {
                screens: vec![],
                edges: vec![],
                options: EdgeOptions::default(),
                canvas: vec![Placement {
                    node: Uuid::new_v4(),
                    x: 3190.0,
                    y: 0.0,
                    scale: Some(1.25),
                }],
            },
            Control::Deactivate,
            Control::ReceiverDevices {
                devices: vec![PairedDevice {
                    slot: 1,
                    name: "MX Keys".into(),
                    kind: "keyboard".into(),
                    wireless_pid: 0x408a,
                    hosts_info: true,
                    os_profile: Some(Os::Windows),
                }],
            },
            Control::Unpair { slot: 2 },
            Control::Ping(42),
            Control::Status(HubStatus {
                hub_name: "hub".into(),
                active: None,
                nodes: vec![],
                device_count: 3,
                uptime_s: 10,
                version: "0.1.0 (test)".into(),
            }),
        ];
        for m in msgs {
            let bytes = wire::encode(&m).unwrap();
            let back = wire::decode(&bytes).unwrap();
            assert_eq!(m, back);
        }
    }

    /// A version 1 agent's `Hello`, byte for byte, must still decode so the
    /// hub can answer `Reject { VersionMismatch }`, and that `Reject` must
    /// be what a version 1 agent decodes.
    #[test]
    fn handshake_stays_decodable_across_versions() {
        let id = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        // postcard of v1 Hello { proto_ver: 1, node_id, "pc", Linux, Agent, [7; 32] }.
        let mut v1 = vec![0u8, 1]; // variant 0, varint 1
        v1.push(16);
        v1.extend_from_slice(id.as_bytes());
        v1.extend_from_slice(&[2, b'p', b'c', 2, 0]);
        v1.extend_from_slice(&[7; TOKEN_LEN]);
        let Control::Hello {
            proto_ver,
            node_id,
            kind,
            ..
        } = postcard::from_bytes(&v1).unwrap()
        else {
            panic!("not a Hello");
        };
        assert_eq!((proto_ver, node_id, kind), (1, id, NodeKind::Agent));

        let reject = postcard::to_allocvec(&Control::Reject {
            reason: RejectReason::VersionMismatch,
        })
        .unwrap();
        assert_eq!(reject, [2, 1]); // v1: variant 2 (Reject), reason 1
    }

    #[test]
    fn edge_opposite() {
        assert_eq!(Edge::Left.opposite(), Edge::Right);
        assert_eq!(Edge::Top.opposite(), Edge::Bottom);
    }
}
