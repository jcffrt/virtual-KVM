//! What the settings window and the tray show, derived from the widget
//! state in Rust so the TypeScript side only renders it.

use agent_core::widget::WidgetState;
use agent_core::AgentConfig;
use proto::descriptor::DescriptorInfo;
use proto::{EdgeOptions, HubSettings, NodeKind, Os, PairingEvent};
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
    /// The hub said it is powering off (why); until it is back.
    pub hub_off: Option<String>,
    /// Windows: power the hub off when this PC shuts down.
    pub power_off_with_pc: bool,
    pub active: Option<Uuid>,
    pub nodes: Vec<NodeView>,
    pub devices: Vec<DeviceView>,
    /// Devices paired with the receiver; `None` while the hub has not read
    /// them (old hub, receiver without HID++).
    pub receiver_devices: Option<Vec<PairedView>>,
    pub pairing: PairingView,
    /// Hysteresis and hotkey as the hub applies them (None until it says).
    pub settings: Option<HubSettings>,
    /// "macos" or "windows": wording in the front end ("Este Mac").
    pub platform: &'static str,
    /// "Ctrl, Ctrl" (the hotkey's double tap) while the hotkey is on; the
    /// agent's index follows it.
    pub hotkey: Option<String>,
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

/// One device paired with the receiver.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairedView {
    pub slot: u8,
    pub name: String,
    /// "teclado", "ratón", "trackball", ...
    pub kind: String,
    /// "408a".
    pub wpid: String,
    pub online: bool,
    /// The hub sets its OS layout on every switch.
    pub os_profiles: bool,
    /// Its current layout: "Windows", "macOS", "Linux".
    pub profile: Option<&'static str>,
    pub battery: Option<BatteryView>,
    pub pointer_speed: Option<PointerSpeedView>,
    /// F1..F12 send their special function without Fn.
    pub fn_swap: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BatteryView {
    pub percent: Option<u8>,
    /// "cargando", "cargada", ... (None while discharging).
    pub charge: Option<&'static str>,
    /// 10 % or less and not charging.
    pub low: bool,
    /// What the Battery column shows: "50 %", "50 %, cargando", or just
    /// "cargando" when the device reports no level (it does while charging).
    pub text: String,
}

/// Raw HID++ value (256 = 1x) and its range.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PointerSpeedView {
    pub value: u16,
    pub min: u16,
    pub max: u16,
}

fn battery_view(b: proto::Battery) -> BatteryView {
    use proto::Charge;
    let charge = match b.charge {
        Charge::Discharging => None,
        Charge::Charging | Charge::SlowCharging => Some("cargando"),
        Charge::AlmostFull => Some("casi cargada"),
        Charge::Full => Some("cargada"),
        Charge::Error => Some("error de carga"),
    };
    let text = match (b.percent, charge) {
        (Some(p), Some(c)) => format!("{p} %, {c}"),
        (Some(p), None) => format!("{p} %"),
        (None, Some(c)) => c.to_string(),
        (None, None) => "?".to_string(),
    };
    BatteryView {
        percent: b.percent,
        charge,
        low: b.charge == Charge::Discharging && b.percent.is_some_and(|p| p <= 10),
        text,
    }
}

/// The pairing session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairingView {
    /// Open now; until this Unix time in milliseconds.
    pub until_ms: Option<u64>,
    /// What happened last, in Spanish.
    pub message: Option<String>,
    /// The last event was a success.
    pub ok: bool,
}

fn device_kind(kind: &str) -> String {
    match kind {
        "keyboard" => "teclado".into(),
        "mouse" => "ratón".into(),
        "trackball" => "trackball".into(),
        "touchpad" => "touchpad".into(),
        "numpad" => "teclado numérico".into(),
        "presenter" => "presentador".into(),
        "remote" | "remote control" => "mando".into(),
        other => other.into(),
    }
}

pub fn paired(st: &WidgetState) -> Option<Vec<PairedView>> {
    st.paired_devices.as_ref().map(|list| {
        list.iter()
            .map(|d| PairedView {
                slot: d.slot,
                name: d.name.clone(),
                kind: device_kind(&d.kind),
                wpid: format!("{:04x}", d.wireless_pid),
                online: d.online,
                os_profiles: d.os_profiles,
                profile: d.os_profile.map(os_name),
                battery: d.battery.map(battery_view),
                pointer_speed: d.pointer_speed.map(|p| PointerSpeedView {
                    value: p.value,
                    min: p.min,
                    max: p.max,
                }),
                fn_swap: d.fn_swap,
            })
            .collect()
    })
}

pub fn pairing(st: &WidgetState) -> PairingView {
    let (message, ok) = match &st.pairing_last {
        None => (None, false),
        Some(PairingEvent::Started { .. }) => (
            Some("Enciende el dispositivo nuevo (o apágalo y enciéndelo)…".into()),
            false,
        ),
        Some(PairingEvent::DeviceFound { name }) => (Some(format!("Encontrado: {name}…")), false),
        Some(PairingEvent::Paired { name, kind }) => (
            Some(format!("Emparejado: {name} ({}).", device_kind(kind))),
            true,
        ),
        Some(PairingEvent::Failed { reason }) => (Some(format!("Error: {reason}")), false),
        Some(PairingEvent::TimedOut) => (Some("No apareció ningún dispositivo.".into()), false),
    };
    PairingView {
        until_ms: st.pairing_until_ms,
        message,
        ok,
    }
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
        hub_off: st.hub_off.clone(),
        power_off_with_pc: cfg.power_off_hub_with_pc,
        active,
        nodes,
        devices: devices(st),
        receiver_devices: paired(st),
        pairing: pairing(st),
        settings: st.settings,
        platform: if cfg!(windows) { "windows" } else { "macos" },
        hotkey: st.settings.and_then(|s| hotkey_prefix(&s.hotkey)),
        options: st.options,
        custom_layout: !st.canvas.is_empty(),
    }
}

/// The part of the view the tray menu depends on; the menu is rebuilt only
/// when this changes (not on every RTT update).
/// The double tap of the hotkey, as the user reads it: "Ctrl, Ctrl".
pub fn hotkey_prefix(h: &proto::Hotkey) -> Option<String> {
    use proto::HotkeyModifier as M;
    let key = match h.modifier {
        M::Ctrl => "Ctrl",
        M::Shift => "Mayús",
        M::Alt => "Alt",
        M::Gui => "Cmd/Win",
    };
    h.enabled.then(|| format!("{key}, {key}"))
}

/// What the hotkey does besides the agents' digits, for the tray.
pub fn hotkey_extras(h: &proto::Hotkey) -> Option<String> {
    let mut extras = Vec::new();
    if h.previous {
        extras.push("0 el anterior");
    }
    if h.release_keys {
        extras.push("Esc libera teclas");
    }
    let prefix = hotkey_prefix(h)?;
    Some(if extras.is_empty() {
        format!("Atajo: {prefix}, número")
    } else {
        format!("Atajo: {prefix}, número ({})", extras.join(", "))
    })
}

pub fn tray_key(v: &View) -> String {
    let mut k = format!(
        "{}|{}|{:?}|{:?}|{:?}",
        v.connected,
        v.hub_off.is_some(),
        v.hub_name,
        v.active,
        v.settings.map(|s| s.hotkey)
    );
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
    fn paired_devices_and_pairing() {
        let st = WidgetState {
            paired_devices: Some(vec![proto::PairedDevice {
                slot: 3,
                name: "MX Keys".into(),
                kind: "keyboard".into(),
                wireless_pid: 0x408a,
                online: true,
                os_profiles: true,
                os_profile: Some(Os::Windows),
                battery: Some(proto::Battery {
                    percent: Some(5),
                    next_percent: None,
                    charge: proto::Charge::Discharging,
                }),
                pointer_speed: None,
                fn_swap: Some(true),
            }]),
            pairing_last: Some(PairingEvent::Paired {
                name: "K230".into(),
                kind: "keyboard".into(),
            }),
            ..Default::default()
        };
        let p = paired(&st).unwrap();
        assert_eq!(
            (p[0].kind.as_str(), p[0].wpid.as_str(), p[0].profile),
            ("teclado", "408a", Some("Windows"))
        );
        let b = p[0].battery.as_ref().unwrap();
        assert!(b.low && b.charge.is_none());
        assert_eq!(b.text, "5 %");
        assert_eq!(p[0].fn_swap, Some(true));
        let v = pairing(&st);
        assert!(v.ok && v.until_ms.is_none());
        assert_eq!(v.message.as_deref(), Some("Emparejado: K230 (teclado)."));
        assert!(paired(&WidgetState::default()).is_none());
    }

    #[test]
    fn battery_text() {
        use proto::{Battery, Charge};
        let text = |percent, charge| {
            battery_view(Battery {
                percent,
                next_percent: None,
                charge,
            })
            .text
        };
        // Charging devices report level 0 (unknown): no percentage.
        assert_eq!(text(None, Charge::Charging), "cargando");
        assert_eq!(text(Some(50), Charge::Charging), "50 %, cargando");
        assert_eq!(text(Some(50), Charge::Discharging), "50 %");
        assert_eq!(text(None, Charge::Discharging), "?");
    }

    #[test]
    fn hotkey_labels() {
        let mut h = proto::Hotkey::default();
        assert_eq!(hotkey_prefix(&h).as_deref(), Some("Ctrl, Ctrl"));
        assert_eq!(
            hotkey_extras(&h).as_deref(),
            Some("Atajo: Ctrl, Ctrl, número (0 el anterior, Esc libera teclas)")
        );
        h.modifier = proto::HotkeyModifier::Shift;
        h.previous = false;
        h.release_keys = false;
        assert_eq!(
            hotkey_extras(&h).as_deref(),
            Some("Atajo: Mayús, Mayús, número")
        );
        h.enabled = false;
        assert_eq!(hotkey_prefix(&h), None);
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
