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
pub const PROTO_VERSION: u16 = 1;

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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Screen {
    pub node: Uuid,
    /// Index of the screen within the node (0 = primary).
    pub index: u8,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    /// Logical scale factor (DPI scaling), informational.
    pub scale: f32,
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

/// Progress of a Unifying pairing session (phase 3).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum PairingEvent {
    Started { timeout_s: u8 },
    DeviceFound { name: String },
    Paired { name: String, kind: String },
    Failed { reason: String },
    TimedOut,
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
}

/// Hub status broadcast to widgets (and agents) on change.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HubStatus {
    pub hub_name: String,
    pub active: Option<Uuid>,
    pub nodes: Vec<NodeInfo>,
    pub device_count: u8,
    pub uptime_s: u64,
}

/// Why the hub rejected a node.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
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
    /// widget -> hub -> agents. Full layout.
    Layout {
        screens: Vec<Screen>,
        edges: Vec<EdgeLink>,
    },
    /// agent -> hub. The pointer reached a linked edge.
    SwitchTo {
        target: Uuid,
        enter_edge: Edge,
        pos_ratio: f32,
    },
    /// hub -> agent. This node now receives input. `enter_edge` is the edge of
    /// the destination screen the pointer should appear on.
    Activate {
        enter_edge: Option<Edge>,
        pos_ratio: Option<f32>,
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
                enter_edge: Some(Edge::Left),
                pos_ratio: Some(0.25),
            },
            Control::Deactivate,
            Control::Ping(42),
            Control::Status(HubStatus {
                hub_name: "hub".into(),
                active: None,
                nodes: vec![],
                device_count: 3,
                uptime_s: 10,
            }),
        ];
        for m in msgs {
            let bytes = wire::encode(&m).unwrap();
            let back = wire::decode(&bytes).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn edge_opposite() {
        assert_eq!(Edge::Left.opposite(), Edge::Right);
        assert_eq!(Edge::Top.opposite(), Edge::Bottom);
    }
}
