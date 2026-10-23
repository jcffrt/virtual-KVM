//! What the settings window and the tray show, derived from the widget
//! state in Rust so the TypeScript side only renders it.

use agent_core::widget::WidgetState;
use agent_core::AgentConfig;
use proto::descriptor::DescriptorInfo;
use proto::{EdgeOptions, HubSettings, NodeKind, Os};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct View {
    /// A hub and a token are configured.
    pub paired: bool,
    pub hub_addr: String,
    pub fingerprint: String,
    pub connected: bool,
    pub hub_name: Option<String>,
    pub hub_version: Option<String>,
    pub rtt_ms: Option<f64>,
    pub error: Option<String>,
    pub fatal: bool,
    pub active: Option<Uuid>,
    pub nodes: Vec<NodeView>,
    pub devices: Vec<DeviceView>,
    /// Hysteresis and hotkey as the hub applies them (None until it says).
    pub settings: Option<HubSettings>,
    /// Edge options in force (from the hub's layout).
    pub options: EdgeOptions,
    /// The hub keeps a layout set by a widget (not its config seed).
    pub custom_layout: bool,
}

/// One HID interface of the receiver the hub forwards.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceView {
    pub id: u8,
    pub interface: u8,
    pub name: String,
    /// "046d:c52b".
    pub vid_pid: String,
    pub descriptor_len: usize,
    /// What the interface carries, e.g. ["teclado"], ["ratón", "multimedia"].
    pub kinds: Vec<String>,
}

fn kind_name(page: u16, usage: u16) -> String {
    match (page, usage) {
        (0x01, 0x06) => "teclado".into(),
        (0x01, 0x02) => "ratón".into(),
        (0x01, 0x80) => "control del sistema".into(),
        (0x0C, 0x01) => "multimedia".into(),
        (0xFF00, _) => "HID++ (vendor)".into(),
        (p, _) if p >= 0xFF00 => format!("vendor 0x{p:04X}"),
        (p, u) => format!("0x{p:02X}/0x{u:02X}"),
    }
}

pub fn devices(st: &WidgetState) -> Vec<DeviceView> {
    st.devices
        .iter()
        .map(|d| {
            let info = DescriptorInfo::parse(&d.report_descriptor);
            let mut kinds: Vec<String> = info
                .applications
                .iter()
                .map(|&(p, u)| kind_name(p, u))
                .collect();
            kinds.dedup();
            DeviceView {
                id: d.id,
                interface: d.interface,
                name: d.name.clone(),
                vid_pid: format!("{:04x}:{:04x}", d.vid, d.pid),
                descriptor_len: d.report_descriptor.len(),
                kinds,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeView {
    pub id: Uuid,
    /// 1..=4 for agents (the hotkey digit), 0 otherwise.
    pub index: u8,
    pub name: String,
    pub os: &'static str,
    /// "agent", "helper" or "widget".
    pub role: &'static str,
    /// For an edge helper: the name of the agent it acts for.
    pub helper_of: Option<String>,
    pub screens: u8,
    /// Agents only: some screens are known, so edge switching works.
    pub has_helper: bool,
    pub active: bool,
    pub rtt_ms: Option<f32>,
}

fn os_name(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macOS",
        Os::Windows => "Windows",
        Os::Linux => "Linux",
        Os::Other => "otro",
    }
}

pub fn view(st: &WidgetState, cfg: &AgentConfig) -> View {
    let status = st.status.as_ref();
    let active = status.and_then(|s| s.active);
    let mut nodes: Vec<NodeView> = status
        .map(|s| s.nodes.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|n| {
            let (role, helper_of) = match n.kind {
                NodeKind::Agent => ("agent", None),
                NodeKind::Widget => ("widget", None),
                NodeKind::EdgeHelper { for_node } => (
                    "helper",
                    Some(
                        status
                            .and_then(|s| s.nodes.iter().find(|a| a.node_id == for_node))
                            .map_or_else(|| for_node.to_string(), |a| a.node_name.clone()),
                    ),
                ),
            };
            NodeView {
                id: n.node_id,
                index: n.assigned_index,
                name: n.node_name.clone(),
                os: os_name(n.os),
                role,
                helper_of,
                screens: n.screens,
                has_helper: n.kind == NodeKind::Agent && n.screens > 0,
                active: Some(n.node_id) == active,
                rtt_ms: n.rtt_ms,
            }
        })
        .collect();
    let rank = |r: &str| match r {
        "agent" => 0,
        "helper" => 1,
        _ => 2,
    };
    nodes.sort_by(|a, b| (rank(a.role), a.index, &a.name).cmp(&(rank(b.role), b.index, &b.name)));
    View {
        paired: !cfg.hub.is_empty() && !cfg.token.is_empty(),
        hub_addr: cfg.hub.clone(),
        fingerprint: cfg.hub_fingerprint.clone(),
        connected: st.connected.is_some(),
        hub_name: st
            .connected
            .as_ref()
            .map(|c| c.0.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| status.map(|s| s.hub_name.clone())),
        hub_version: status.map(|s| s.version.clone()).filter(|v| !v.is_empty()),
        rtt_ms: st.rtt_ms,
        error: st.error.clone(),
        fatal: st.fatal,
        active,
        nodes,
        devices: devices(st),
        settings: st.settings,
        options: st.options,
        custom_layout: !st.canvas.is_empty(),
    }
}

/// The part of the view the tray menu depends on; the menu is rebuilt only
/// when this changes (not on every RTT update).
pub fn tray_key(v: &View) -> String {
    let mut k = format!("{}|{:?}|{:?}", v.connected, v.hub_name, v.active);
    for n in v.nodes.iter().filter(|n| n.role == "agent") {
        k.push_str(&format!("|{}:{}:{}", n.index, n.name, n.has_helper));
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{HubStatus, NodeInfo};

    fn info(name: &str, kind: NodeKind, index: u8, screens: u8, os: Os) -> NodeInfo {
        NodeInfo {
            node_id: Uuid::new_v4(),
            node_name: name.into(),
            os,
            kind,
            assigned_index: index,
            rtt_ms: Some(4.0),
            screens,
        }
    }

    #[test]
    fn nodes_roles_helpers_and_order() {
        // The deployment of 2026-10-22.
        let gadget = info("orangepi", NodeKind::Agent, 1, 2, Os::Linux);
        let mac = info("MacBook", NodeKind::Agent, 2, 2, Os::MacOs);
        let ubuntu = info("ubuntu", NodeKind::Agent, 3, 0, Os::Linux);
        let mac_edge = info(
            "MacBook (edge)",
            NodeKind::EdgeHelper {
                for_node: mac.node_id,
            },
            0,
            0,
            Os::MacOs,
        );
        let widget = info("MacBook", NodeKind::Widget, 0, 0, Os::MacOs);
        let st = WidgetState {
            connected: Some(("orangepi".into(), "192.168.1.28:4433".into())),
            status: Some(HubStatus {
                hub_name: "orangepi".into(),
                active: Some(mac.node_id),
                nodes: vec![widget, mac_edge, ubuntu, mac.clone(), gadget],
                device_count: 3,
                uptime_s: 1,
                version: "0.1.0 (0f83e4e)".into(),
            }),
            rtt_ms: Some(5.2),
            ..Default::default()
        };
        let cfg = AgentConfig {
            hub: "192.168.1.28".into(),
            token: "ab".repeat(32),
            ..Default::default()
        };
        let v = view(&st, &cfg);
        assert!(v.paired && v.connected);
        assert_eq!(v.hub_version.as_deref(), Some("0.1.0 (0f83e4e)"));
        let names: Vec<_> = v
            .nodes
            .iter()
            .map(|n| (n.role, n.index, n.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("agent", 1, "orangepi"),
                ("agent", 2, "MacBook"),
                ("agent", 3, "ubuntu"),
                ("helper", 0, "MacBook (edge)"),
                ("widget", 0, "MacBook"),
            ]
        );
        assert!(v.nodes[1].active && v.nodes[1].has_helper);
        assert!(!v.nodes[2].has_helper, "Ubuntu: no screens, drawn grey");
        assert_eq!(v.nodes[3].helper_of.as_deref(), Some("MacBook"));
        assert_eq!(v.active, Some(mac.node_id));
    }

    #[test]
    fn devices_describe_the_receiver_interfaces() {
        use proto::descriptor::tests::{BOOT_KEYBOARD, UNIFYING_IFACE1, UNIFYING_IFACE2};
        let dev = |id: u8, desc: &[u8]| proto::HidDevice {
            id,
            name: "Logitech USB Receiver".into(),
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            interface: id,
            report_descriptor: desc.to_vec(),
            uses_report_ids: false,
        };
        let st = WidgetState {
            devices: vec![
                dev(0, BOOT_KEYBOARD),
                dev(1, UNIFYING_IFACE1),
                dev(2, UNIFYING_IFACE2),
            ],
            ..Default::default()
        };
        let d = devices(&st);
        assert_eq!(d[0].kinds, ["teclado"]);
        assert_eq!(d[0].vid_pid, "046d:c52b");
        assert!(d[1].kinds.contains(&"ratón".to_string()));
        assert!(d[1].kinds.contains(&"multimedia".to_string()));
        assert_eq!(d[2].kinds, ["HID++ (vendor)"]);
    }

    #[test]
    fn unpaired_and_tray_key() {
        let v = view(&WidgetState::default(), &AgentConfig::default());
        assert!(!v.paired && !v.connected && v.nodes.is_empty());
        let mut w = v.clone();
        w.rtt_ms = Some(9.0);
        assert_eq!(tray_key(&v), tray_key(&w), "RTT does not rebuild the menu");
        w.connected = true;
        assert_ne!(tray_key(&v), tray_key(&w));
    }
}
