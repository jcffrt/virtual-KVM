//! Routing state of the hub: connected nodes, the active agent, devices.
//!
//! Everything here is synchronous and transport agnostic so it can be unit
//! tested. Reader threads call [`Router::on_report`]; connection tasks call
//! [`Router::register`], [`Router::on_control`] and [`Router::unregister`].
//! Control messages to a node go through its unbounded channel (drained by
//! the connection's writer task); reports go straight to [`Link`].

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bytes::Bytes;
use proto::datagram::{Datagram, FLAG_RELEASE_ALL};
use proto::descriptor::{DescriptorInfo, USAGE_MOUSE, USAGE_PAGE_GENERIC_DESKTOP};
use proto::{
    Control, Edge, EdgeLink, HidDevice, HubStatus, NodeInfo, NodeKind, Os, RejectReason, Screen,
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::hotkey::Hotkey;

/// Agents get indices 1..=MAX_AGENTS (the emergency hotkey digits).
pub const MAX_AGENTS: u8 = 4;

/// Unreliable report channel to one node.
pub trait Link: Send + Sync + 'static {
    fn send_datagram(&self, data: Bytes);
    fn rtt(&self) -> Option<Duration>;
}

#[derive(Debug, Clone)]
pub struct Options {
    pub hysteresis: Duration,
    /// Delay of the redundant copy of non-mouse reports.
    pub resend: Option<Duration>,
    pub hotkey: bool,
}

/// The identity part of `Control::Hello`.
#[derive(Debug, Clone)]
pub struct Hello {
    pub node_id: Uuid,
    pub node_name: String,
    pub os: Os,
    pub kind: NodeKind,
}

/// Returned by [`Router::register`]; pass it back to `unregister`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    pub generation: u64,
    pub index: u8,
}

struct Device {
    dev: HidDevice,
    info: DescriptorInfo,
    release: Vec<Vec<u8>>,
}

impl Device {
    fn new(dev: HidDevice) -> Self {
        let info = DescriptorInfo::parse(&dev.report_descriptor);
        let release = info.release_reports();
        Self { dev, info, release }
    }

    /// Mouse motion is not worth resending: a stale copy is useless.
    fn is_mouse_report(&self, report: &[u8]) -> bool {
        let id = if self.info.uses_report_ids {
            report.first().copied()
        } else {
            None
        };
        self.info.inputs.iter().any(|r| {
            r.report_id == id
                && r.app_usage_page == USAGE_PAGE_GENERIC_DESKTOP
                && r.app_usage == USAGE_MOUSE
        })
    }
}

struct Node {
    hello: Hello,
    index: u8,
    generation: u64,
    link: Arc<dyn Link>,
    ctl: UnboundedSender<Control>,
    seq: [u16; 256],
}

impl Node {
    fn send(&self, msg: Control) {
        // The receiver only disappears when the connection is going away.
        let _ = self.ctl.send(msg);
    }

    fn datagram(&mut self, dev_id: u8, flags: u8, report: &[u8]) -> Option<Bytes> {
        let seq = self.seq[dev_id as usize];
        self.seq[dev_id as usize] = seq.wrapping_add(1);
        Datagram {
            dev_id,
            flags,
            seq,
            report,
        }
        .encode()
        .ok()
    }

    fn is_agent(&self) -> bool {
        self.hello.kind == NodeKind::Agent
    }
}

#[derive(Default)]
struct State {
    devices: BTreeMap<u8, Device>,
    nodes: HashMap<Uuid, Node>,
    /// Agent index last assigned to each node id, kept across reconnections.
    indices: HashMap<Uuid, u8>,
    active: Option<Uuid>,
    /// Agents in the order they were last active (most recent last).
    history: Vec<Uuid>,
    last_switch: Option<Instant>,
    layout: Option<(Vec<Screen>, Vec<EdgeLink>)>,
    screens: BTreeMap<Uuid, Vec<Screen>>,
    hotkey: Hotkey,
    next_generation: u64,
}

pub struct Router {
    hub_id: Uuid,
    hub_name: String,
    opts: Options,
    started: Instant,
    runtime: Option<tokio::runtime::Handle>,
    state: Mutex<State>,
}

impl Router {
    /// Must be called inside a tokio runtime if `opts.resend` is set.
    pub fn new(hub_id: Uuid, hub_name: String, opts: Options) -> Self {
        Self {
            hub_id,
            hub_name,
            runtime: tokio::runtime::Handle::try_current().ok(),
            opts,
            started: Instant::now(),
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn active(&self) -> Option<Uuid> {
        self.lock().active
    }

    pub fn status(&self) -> HubStatus {
        self.build_status(&self.lock())
    }

    // ---- devices -------------------------------------------------------

    pub fn add_device(&self, dev: HidDevice) {
        let mut st = self.lock();
        info!(
            id = dev.id,
            iface = dev.interface,
            name = %dev.name,
            "device {:04x}:{:04x} added ({} byte descriptor)",
            dev.vid,
            dev.pid,
            dev.report_descriptor.len()
        );
        st.devices.insert(dev.id, Device::new(dev));
        self.broadcast_devices(&st);
        self.broadcast_status(&st);
    }

    pub fn remove_device(&self, id: u8) {
        let mut st = self.lock();
        if st.devices.remove(&id).is_some() {
            info!(id, "device removed");
            self.broadcast_devices(&st);
            self.broadcast_status(&st);
        }
    }

    fn device_list(st: &State) -> Vec<HidDevice> {
        st.devices.values().map(|d| d.dev.clone()).collect()
    }

    fn broadcast_devices(&self, st: &State) {
        let devices = Self::device_list(st);
        for n in st.nodes.values() {
            n.send(Control::DeviceList {
                devices: devices.clone(),
            });
        }
    }

    // ---- nodes ---------------------------------------------------------

    /// Admit a node that presented a valid token. Queues `HelloAck` and the
    /// initial state on `ctl`.
    pub fn register(
        &self,
        hello: Hello,
        link: Arc<dyn Link>,
        ctl: UnboundedSender<Control>,
    ) -> Result<Registration, RejectReason> {
        let mut st = self.lock();
        let id = hello.node_id;
        if st.nodes.remove(&id).is_some() {
            info!(node = %id, "node reconnected; replacing the old connection");
        }
        let index = match hello.kind {
            NodeKind::Widget => 0,
            NodeKind::Agent => Self::assign_index(&mut st, id)?,
        };
        st.next_generation += 1;
        let generation = st.next_generation;
        let node = Node {
            hello,
            index,
            generation,
            link,
            ctl,
            seq: [0; 256],
        };
        node.send(Control::HelloAck {
            hub_id: self.hub_id,
            hub_name: self.hub_name.clone(),
            assigned_index: index,
        });
        node.send(Control::DeviceList {
            devices: Self::device_list(&st),
        });
        if let Some((screens, edges)) = &st.layout {
            node.send(Control::Layout {
                screens: screens.clone(),
                edges: edges.clone(),
            });
        }
        if !node.is_agent() {
            for screens in st.screens.values() {
                node.send(Control::Screens {
                    screens: screens.clone(),
                });
            }
        }
        info!(
            node = %id,
            name = %node.hello.node_name,
            os = ?node.hello.os,
            kind = ?node.hello.kind,
            index,
            "node registered"
        );
        let is_agent = node.is_agent();
        st.nodes.insert(id, node);

        if is_agent {
            if st.active == Some(id) {
                // Same machine came back while still active.
                st.nodes[&id].send(Control::Activate {
                    enter_edge: None,
                    pos_ratio: None,
                });
            } else if st.active.is_none() {
                self.switch_locked(&mut st, id, None, None, true);
            }
        }
        self.broadcast_status(&st);
        Ok(Registration { generation, index })
    }

    fn assign_index(st: &mut State, id: Uuid) -> Result<u8, RejectReason> {
        let used: Vec<u8> = st
            .nodes
            .values()
            .filter(|n| n.is_agent())
            .map(|n| n.index)
            .collect();
        let sticky = st.indices.get(&id).copied().filter(|i| !used.contains(i));
        let index = sticky
            .or_else(|| {
                // Prefer an index nobody has held, then any free one.
                let reserved: Vec<u8> = st.indices.values().copied().collect();
                (1..=MAX_AGENTS)
                    .find(|i| !used.contains(i) && !reserved.contains(i))
                    .or_else(|| (1..=MAX_AGENTS).find(|i| !used.contains(i)))
            })
            .ok_or(RejectReason::TooManyNodes)?;
        st.indices.retain(|_, v| *v != index);
        st.indices.insert(id, index);
        Ok(index)
    }

    pub fn unregister(&self, id: Uuid, reg: Registration) {
        let mut st = self.lock();
        match st.nodes.get(&id) {
            Some(n) if n.generation == reg.generation => {}
            _ => return, // already replaced by a newer connection
        }
        st.nodes.remove(&id);
        info!(node = %id, "node disconnected");
        if st.active == Some(id) {
            st.active = None;
            let fallback = st
                .history
                .iter()
                .rev()
                .copied()
                .find(|n| st.nodes.get(n).is_some_and(Node::is_agent))
                .or_else(|| {
                    st.nodes
                        .values()
                        .filter(|n| n.is_agent())
                        .min_by_key(|n| n.index)
                        .map(|n| n.hello.node_id)
                });
            match fallback {
                Some(next) => {
                    info!(node = %next, "active node lost; falling back");
                    self.switch_locked(&mut st, next, None, None, true);
                }
                None => info!("active node lost; no agent left"),
            }
        }
        self.broadcast_status(&st);
    }

    // ---- control -------------------------------------------------------

    pub fn on_control(&self, from: Uuid, msg: Control) {
        let mut st = self.lock();
        let Some(sender) = st.nodes.get(&from) else {
            return;
        };
        let kind = sender.hello.kind;
        match msg {
            Control::Ping(x) => sender.send(Control::Pong(x)),
            Control::Pong(_) => {}
            Control::SwitchTo {
                target,
                enter_edge,
                pos_ratio,
            } => {
                let allowed = kind == NodeKind::Widget || st.active == Some(from);
                if !allowed {
                    debug!(node = %from, "SwitchTo from an inactive agent ignored");
                    return;
                }
                let pos = pos_ratio.clamp(0.0, 1.0);
                self.switch_locked(&mut st, target, Some(enter_edge), Some(pos), false);
            }
            Control::Screens { mut screens } if kind == NodeKind::Agent => {
                for s in &mut screens {
                    s.node = from;
                }
                for w in st.nodes.values().filter(|n| !n.is_agent()) {
                    w.send(Control::Screens {
                        screens: screens.clone(),
                    });
                }
                st.screens.insert(from, screens);
            }
            Control::Layout { screens, edges } if kind == NodeKind::Widget => {
                info!(
                    screens = screens.len(),
                    edges = edges.len(),
                    "layout updated"
                );
                for n in st.nodes.values().filter(|n| n.hello.node_id != from) {
                    n.send(Control::Layout {
                        screens: screens.clone(),
                        edges: edges.clone(),
                    });
                }
                st.layout = Some((screens, edges));
            }
            Control::PairingStart { .. } | Control::PairingStop => {
                sender.send(Control::PairingEvent(proto::PairingEvent::Failed {
                    reason: "pairing is not implemented yet".into(),
                }));
            }
            other => warn!(node = %from, msg = other.name(), "unexpected message"),
        }
    }

    /// Make `target` the active agent. `force` bypasses the hysteresis.
    fn switch_locked(
        &self,
        st: &mut State,
        target: Uuid,
        enter_edge: Option<Edge>,
        pos_ratio: Option<f32>,
        force: bool,
    ) -> bool {
        if st.active == Some(target) {
            return false;
        }
        let now = Instant::now();
        if !force
            && st
                .last_switch
                .is_some_and(|t| now.duration_since(t) < self.opts.hysteresis)
        {
            debug!("switch suppressed by hysteresis");
            return false;
        }
        if !st.nodes.get(&target).is_some_and(Node::is_agent) {
            warn!(node = %target, "switch target is not a connected agent");
            return false;
        }

        if let Some(old) = st.active.take() {
            let State { devices, nodes, .. } = &mut *st;
            if let Some(n) = nodes.get_mut(&old) {
                for d in devices.values() {
                    for r in &d.release {
                        if let Some(b) = n.datagram(d.dev.id, FLAG_RELEASE_ALL, r) {
                            n.link.send_datagram(b);
                        }
                    }
                }
                n.send(Control::Deactivate);
            }
        }
        st.active = Some(target);
        st.history.retain(|n| *n != target);
        st.history.push(target);
        st.last_switch = Some(now);
        let n = &st.nodes[&target];
        info!(node = %target, name = %n.hello.node_name, index = n.index, "switched");
        n.send(Control::Activate {
            enter_edge,
            pos_ratio,
        });
        self.broadcast_status(st);
        true
    }

    // ---- reports -------------------------------------------------------

    /// Forward one raw report read from device `dev_id` to the active agent.
    pub fn on_report(&self, dev_id: u8, report: &[u8]) {
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(dev) = st.devices.get(&dev_id) else {
            return;
        };

        if self.opts.hotkey {
            if let Some(kb) = &dev.info.keyboard {
                if let Some(ks) = kb.decode(dev.info.uses_report_ids, report) {
                    if let Some(index) = st.hotkey.feed(Instant::now(), ks.modifiers, ks.keys) {
                        let target = st
                            .nodes
                            .values()
                            .find(|n| n.is_agent() && n.index == index)
                            .map(|n| n.hello.node_id);
                        match target {
                            Some(t) => {
                                info!(index, "emergency hotkey");
                                self.switch_locked(st, t, None, None, true);
                            }
                            None => info!(index, "emergency hotkey: no agent with that index"),
                        }
                        // The digit is not delivered anywhere.
                        return;
                    }
                }
            }
        }

        let resend = !dev.is_mouse_report(report);
        let Some(node) = st.active.and_then(|id| st.nodes.get_mut(&id)) else {
            return;
        };
        let Some(bytes) = node.datagram(dev_id, 0, report) else {
            warn!(dev_id, len = report.len(), "report too large; dropped");
            return;
        };
        node.link.send_datagram(bytes.clone());
        if resend {
            if let (Some(delay), Some(rt)) = (self.opts.resend, &self.runtime) {
                let link = node.link.clone();
                rt.spawn(async move {
                    tokio::time::sleep(delay).await;
                    link.send_datagram(bytes);
                });
            }
        }
    }

    // ---- status --------------------------------------------------------

    fn build_status(&self, st: &State) -> HubStatus {
        let mut nodes: Vec<NodeInfo> = st
            .nodes
            .values()
            .map(|n| NodeInfo {
                node_id: n.hello.node_id,
                node_name: n.hello.node_name.clone(),
                os: n.hello.os,
                kind: n.hello.kind,
                assigned_index: n.index,
                rtt_ms: n.link.rtt().map(|d| d.as_secs_f32() * 1000.0),
            })
            .collect();
        nodes.sort_by_key(|n| (n.kind == NodeKind::Widget, n.assigned_index));
        HubStatus {
            hub_name: self.hub_name.clone(),
            active: st.active,
            nodes,
            device_count: st.devices.len().min(u8::MAX as usize) as u8,
            uptime_s: self.started.elapsed().as_secs(),
        }
    }

    fn broadcast_status(&self, st: &State) {
        let status = self.build_status(st);
        for n in st.nodes.values() {
            n.send(Control::Status(status.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::tests::{BOOT_KEYBOARD, UNIFYING_IFACE1};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    #[derive(Default)]
    struct FakeLink(Mutex<Vec<Bytes>>);

    impl Link for FakeLink {
        fn send_datagram(&self, data: Bytes) {
            self.0.lock().unwrap().push(data);
        }
        fn rtt(&self) -> Option<Duration> {
            Some(Duration::from_millis(3))
        }
    }

    impl FakeLink {
        fn take(&self) -> Vec<(u8, u8, u16, Vec<u8>)> {
            self.0
                .lock()
                .unwrap()
                .drain(..)
                .map(|b| {
                    let d = Datagram::decode(&b).unwrap();
                    (d.dev_id, d.flags, d.seq, d.report.to_vec())
                })
                .collect()
        }
    }

    struct TestNode {
        id: Uuid,
        link: Arc<FakeLink>,
        rx: UnboundedReceiver<Control>,
        reg: Registration,
    }

    impl TestNode {
        fn msgs(&mut self) -> Vec<Control> {
            let mut v = Vec::new();
            while let Ok(m) = self.rx.try_recv() {
                v.push(m);
            }
            v
        }
        /// Messages other than the ubiquitous Status broadcasts.
        fn events(&mut self) -> Vec<&'static str> {
            self.msgs()
                .iter()
                .map(Control::name)
                .filter(|n| *n != "Status")
                .collect()
        }
    }

    fn router() -> Router {
        Router::new(
            Uuid::new_v4(),
            "hub".into(),
            Options {
                hysteresis: Duration::from_millis(150),
                resend: None,
                hotkey: true,
            },
        )
    }

    fn device(id: u8, desc: &[u8]) -> HidDevice {
        HidDevice {
            id,
            name: "Logitech USB Receiver".into(),
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            interface: id,
            report_descriptor: desc.to_vec(),
            uses_report_ids: DescriptorInfo::parse(desc).uses_report_ids,
        }
    }

    fn join(r: &Router, kind: NodeKind) -> Result<TestNode, RejectReason> {
        join_as(r, Uuid::new_v4(), kind)
    }

    fn join_as(r: &Router, id: Uuid, kind: NodeKind) -> Result<TestNode, RejectReason> {
        let link = Arc::new(FakeLink::default());
        let (tx, rx) = unbounded_channel();
        let reg = r.register(
            Hello {
                node_id: id,
                node_name: "n".into(),
                os: Os::Linux,
                kind,
            },
            link.clone(),
            tx,
        )?;
        Ok(TestNode { id, link, rx, reg })
    }

    fn key_report(mods: u8, key: u8) -> Vec<u8> {
        vec![mods, 0, key, 0, 0, 0, 0, 0]
    }

    #[test]
    fn first_agent_becomes_active() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        let mut a = join(&r, NodeKind::Agent).unwrap();
        assert_eq!(a.reg.index, 1);
        assert_eq!(a.events(), ["HelloAck", "DeviceList", "Activate"]);
        let mut b = join(&r, NodeKind::Agent).unwrap();
        assert_eq!(b.reg.index, 2);
        assert_eq!(b.events(), ["HelloAck", "DeviceList"]);
        assert_eq!(r.active(), Some(a.id));
        let mut w = join(&r, NodeKind::Widget).unwrap();
        assert_eq!(w.reg.index, 0);
        assert_eq!(r.status().nodes.len(), 3);
        assert_eq!(w.events(), ["HelloAck", "DeviceList"]);
    }

    #[test]
    fn reports_reach_only_the_active_agent() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        let a = join(&r, NodeKind::Agent).unwrap();
        let b = join(&r, NodeKind::Agent).unwrap();
        r.on_report(0, &key_report(0, 0x04));
        r.on_report(0, &key_report(0, 0));
        r.on_report(7, &key_report(0, 0x04)); // unknown device
        let got = a.link.take();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (0, 0, 0, key_report(0, 0x04)));
        assert_eq!(got[1].2, 1);
        assert!(b.link.take().is_empty());
    }

    #[test]
    fn switch_releases_keys_and_respects_hysteresis() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        r.add_device(device(1, UNIFYING_IFACE1));
        let mut a = join(&r, NodeKind::Agent).unwrap();
        let mut b = join(&r, NodeKind::Agent).unwrap();
        a.msgs();
        b.msgs();

        // An inactive agent cannot steal the input.
        r.on_control(
            b.id,
            Control::SwitchTo {
                target: b.id,
                enter_edge: Edge::Left,
                pos_ratio: 0.5,
            },
        );
        assert_eq!(r.active(), Some(a.id));

        // Registration activated `a` just now: hysteresis blocks the switch.
        r.on_control(
            a.id,
            Control::SwitchTo {
                target: b.id,
                enter_edge: Edge::Left,
                pos_ratio: 0.5,
            },
        );
        assert_eq!(r.active(), Some(a.id));

        r.lock().last_switch = Some(Instant::now() - Duration::from_secs(1));
        r.on_control(
            a.id,
            Control::SwitchTo {
                target: b.id,
                enter_edge: Edge::Left,
                pos_ratio: 1.7,
            },
        );
        assert_eq!(r.active(), Some(b.id));

        let released = a.link.take();
        assert!(!released.is_empty());
        assert!(released.iter().all(|d| d.1 == FLAG_RELEASE_ALL));
        assert!(released.iter().all(|d| d.3.iter().skip(1).all(|&x| x == 0)));
        assert!(released.iter().any(|d| d.0 == 0));
        assert!(released.iter().any(|d| d.0 == 1));
        assert_eq!(a.events(), ["Deactivate"]);
        let msgs = b.msgs();
        assert!(msgs.contains(&Control::Activate {
            enter_edge: Some(Edge::Left),
            pos_ratio: Some(1.0),
        }));
    }

    #[test]
    fn hotkey_switches_and_swallows_the_digit() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        let a = join(&r, NodeKind::Agent).unwrap();
        let b = join(&r, NodeKind::Agent).unwrap();
        for _ in 0..2 {
            r.on_report(0, &key_report(0x01, 0));
            r.on_report(0, &key_report(0, 0));
        }
        r.on_report(0, &key_report(0, 0x1F)); // '2'
        assert_eq!(r.active(), Some(b.id));
        let to_a = a.link.take();
        // Four Ctrl reports forwarded, then the release-all, but no digit.
        assert_eq!(to_a.len(), 5);
        assert_eq!(to_a[4].1, FLAG_RELEASE_ALL);
        assert!(to_a.iter().all(|d| d.3[2] == 0));
        assert!(b.link.take().is_empty());
        r.on_report(0, &key_report(0, 0));
        assert_eq!(b.link.take().len(), 1);
    }

    #[test]
    fn losing_the_active_agent_falls_back() {
        let r = router();
        let a = join(&r, NodeKind::Agent).unwrap();
        let b = join(&r, NodeKind::Agent).unwrap();
        let mut c = join(&r, NodeKind::Agent).unwrap();
        r.lock().last_switch = None;
        r.on_control(
            a.id,
            Control::SwitchTo {
                target: c.id,
                enter_edge: Edge::Right,
                pos_ratio: 0.1,
            },
        );
        assert_eq!(r.active(), Some(c.id));
        c.msgs();
        r.unregister(c.id, c.reg);
        // `a` was active before `c`; `b` never was.
        assert_eq!(r.active(), Some(a.id));
        r.unregister(a.id, a.reg);
        assert_eq!(r.active(), Some(b.id));
        r.unregister(b.id, b.reg);
        assert_eq!(r.active(), None);
    }

    #[test]
    fn reconnect_keeps_index_and_ignores_stale_unregister() {
        let r = router();
        let id = Uuid::new_v4();
        let first = join_as(&r, id, NodeKind::Agent).unwrap();
        let _other = join(&r, NodeKind::Agent).unwrap();
        let mut second = join_as(&r, id, NodeKind::Agent).unwrap();
        assert_eq!(second.reg.index, first.reg.index);
        assert!(second.events().contains(&"Activate"));
        r.unregister(id, first.reg); // late cleanup of the old connection
        assert_eq!(r.active(), Some(id));
        assert_eq!(r.status().nodes.len(), 2);
    }

    #[test]
    fn fifth_agent_is_rejected() {
        let r = router();
        let nodes: Vec<_> = (0..4).map(|_| join(&r, NodeKind::Agent).unwrap()).collect();
        let idx: Vec<u8> = nodes.iter().map(|n| n.reg.index).collect();
        assert_eq!(idx, [1, 2, 3, 4]);
        assert_eq!(
            join(&r, NodeKind::Agent).err(),
            Some(RejectReason::TooManyNodes)
        );
        assert!(join(&r, NodeKind::Widget).is_ok());
    }

    #[test]
    fn screens_and_layout_are_relayed() {
        let r = router();
        let a = join(&r, NodeKind::Agent).unwrap();
        let mut w = join(&r, NodeKind::Widget).unwrap();
        w.msgs();
        let screen = Screen {
            node: Uuid::nil(),
            index: 0,
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
            scale: 1.0,
        };
        r.on_control(
            a.id,
            Control::Screens {
                screens: vec![screen.clone()],
            },
        );
        let got = w.msgs();
        match &got[0] {
            Control::Screens { screens } => assert_eq!(screens[0].node, a.id),
            m => panic!("unexpected {m:?}"),
        }
        r.on_control(
            w.id,
            Control::Layout {
                screens: vec![screen],
                edges: vec![],
            },
        );
        let mut late = join(&r, NodeKind::Agent).unwrap();
        assert!(late.events().contains(&"Layout"));
    }

    fn real_fixture(name: &str) -> Option<Vec<u8>> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/unifying-c52b")
            .join(name);
        std::fs::read(p).ok()
    }

    /// Hotkey and release-all on the descriptors `hid-logitech-dj`
    /// synthesizes (mode B): keyboard report ID 1 is 7 bytes, no reserved
    /// byte.
    #[test]
    fn real_dj_keyboard_and_trackball() {
        let (Some(k230), Some(ergo)) = (
            real_fixture("0003:046D:400D.0005.bin"),
            real_fixture("0003:046D:406F.0004.bin"),
        ) else {
            eprintln!("fixtures missing; skipped");
            return;
        };
        let r = router();
        r.add_device(device(0, &k230));
        r.add_device(device(1, &ergo));
        let a = join(&r, NodeKind::Agent).unwrap();
        let b = join(&r, NodeKind::Agent).unwrap();

        let kb = |mods: u8, key: u8| vec![0x01, mods, key, 0, 0, 0, 0, 0];
        for _ in 0..2 {
            r.on_report(0, &kb(0x10, 0)); // right Ctrl
            r.on_report(0, &kb(0, 0));
        }
        r.on_report(0, &kb(0, 0x1F)); // '2'
        assert_eq!(r.active(), Some(b.id));

        let to_a = a.link.take();
        assert_eq!(to_a.iter().filter(|d| d.1 == 0).count(), 4);
        let released: Vec<_> = to_a.iter().filter(|d| d.1 == FLAG_RELEASE_ALL).collect();
        // Every standard input report of both devices, none of HID++/DJ.
        let expected = DescriptorInfo::parse(&k230).release_reports().len()
            + DescriptorInfo::parse(&ergo).release_reports().len();
        assert_eq!(released.len(), expected);
        let kb_release = released.iter().find(|d| d.0 == 0 && d.3[0] == 1).unwrap();
        assert_eq!(kb_release.3, vec![1, 0, 0, 0, 0, 0, 0, 0]);
        assert!(released.iter().all(|d| d.3[0] < 0x10));

        // Trackball motion is a mouse report, keyboard is not.
        let ergo_dev = Device::new(device(1, &ergo));
        assert!(ergo_dev.is_mouse_report(&[0x02, 0, 1, 0, 0, 0, 0, 0]));
        assert!(!ergo_dev.is_mouse_report(&[0x03, 0, 0, 0, 0]));
    }

    /// Mode A: the receiver's own interfaces with hid-generic.
    #[test]
    fn real_receiver_interfaces() {
        let (Some(i0), Some(i1), Some(i2)) = (
            real_fixture("0003:046D:C52B.0007.nodj.bin"),
            real_fixture("0003:046D:C52B.0008.nodj.bin"),
            real_fixture("0003:046D:C52B.0009.nodj.bin"),
        ) else {
            eprintln!("fixtures missing; skipped");
            return;
        };
        let r = router();
        r.add_device(device(0, &i0));
        r.add_device(device(1, &i1));
        r.add_device(device(2, &i2));
        let a = join(&r, NodeKind::Agent).unwrap();
        let b = join(&r, NodeKind::Agent).unwrap();

        for _ in 0..2 {
            r.on_report(0, &key_report(0x01, 0));
            r.on_report(0, &key_report(0, 0));
        }
        r.on_report(0, &key_report(0, 0x1F));
        assert_eq!(r.active(), Some(b.id));

        let mut released: Vec<(u8, Vec<u8>)> = a
            .link
            .take()
            .into_iter()
            .filter(|d| d.1 == FLAG_RELEASE_ALL)
            .map(|d| (d.0, d.3))
            .collect();
        released.sort();
        assert_eq!(
            released,
            vec![
                (0, vec![0; 8]),
                (1, vec![2, 0, 0, 0, 0, 0, 0, 0]),
                (1, vec![3, 0, 0, 0, 0]),
                (1, vec![4, 0]),
                // ID 8 lives in a vendor (0xFFBC) collection: skipped like
                // HID++, a zero report there means nothing.
            ]
        );

        // Mouse (ID 2) is not resent; consumer (ID 3) and HID++ are.
        let d1 = Device::new(device(1, &i1));
        assert!(d1.is_mouse_report(&[2, 0, 1, 0, 0, 0, 0, 0]));
        assert!(!d1.is_mouse_report(&[3, 0xe9, 0, 0, 0]));
        assert!(!Device::new(device(2, &i2)).is_mouse_report(&[0x10, 1, 2, 3, 4, 5, 6]));
    }

    #[test]
    fn mouse_reports_are_classified() {
        let d = Device::new(device(1, UNIFYING_IFACE1));
        let info = &d.info;
        let mouse_id = info
            .inputs
            .iter()
            .find(|r| r.app_usage == USAGE_MOUSE)
            .and_then(|r| r.report_id)
            .unwrap();
        assert!(d.is_mouse_report(&[mouse_id, 0, 1, 0]));
        let other = info
            .inputs
            .iter()
            .find(|r| r.app_usage != USAGE_MOUSE)
            .and_then(|r| r.report_id)
            .unwrap();
        assert!(!d.is_mouse_report(&[other, 0]));
        assert!(!Device::new(device(0, BOOT_KEYBOARD)).is_mouse_report(&[0; 8]));
    }
}
