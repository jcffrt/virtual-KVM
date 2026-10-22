//! What the settings window and the tray show, derived from the widget
//! state in Rust so the TypeScript side only renders it.

use agent_core::widget::WidgetState;
use agent_core::AgentConfig;
use proto::{NodeKind, Os};
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
