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
use proto::datagram::{Datagram, SeqTracker, FLAG_RELEASE_ALL};
use proto::descriptor::{DescriptorInfo, USAGE_MOUSE, USAGE_PAGE_GENERIC_DESKTOP};
use proto::{
    Control, EdgeOptions, HidDevice, HubSettings, HubStatus, NodeInfo, NodeKind, Os, RejectReason,
    Screen,
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{NodeOptions, OsProfile};
use crate::hidpp::OsKind;
use crate::hotkey::{Action, Detector};
use crate::layout::{self, LayoutFile, LinkSpec};
use crate::nodes::NodeStore;
use crate::receiver_ctl::Cmd;

/// Agents get indices 1..=MAX_AGENTS (the emergency hotkey digits).
pub const MAX_AGENTS: u8 = 4;

/// After a hub restart, how long the input waits for the agent that was
/// active before going to whichever agent connected first.
pub const RESTORE_GRACE: Duration = Duration::from_secs(10);

/// Unreliable report channel to one node.
pub trait Link: Send + Sync + 'static {
    fn send_datagram(&self, data: Bytes);
    fn rtt(&self) -> Option<Duration>;
}

/// Writes output reports to a receiver interface. Must not block: the
/// router calls it with its lock held.
pub trait ReportWriter: Send + Sync + 'static {
    /// Queue one output report, in HID format (report ID first only if the
    /// interface uses IDs). False if it was dropped.
    fn write(&self, report: &[u8]) -> bool;
}

/// Output reports a node may write to one interface per second (a PC
/// toggling its LEDs in a loop must not flood the receiver).
pub const OUTPUT_RATE: u32 = 50;

#[derive(Debug, Clone)]
pub struct Options {
    pub hysteresis: Duration,
    /// Delay of the redundant copy of non-mouse reports.
    pub resend: Option<Duration>,
    pub hotkey: proto::Hotkey,
    /// Edge links from the hub config (`[[layout]]`).
    pub layout: Vec<LinkSpec>,
    pub edge: EdgeOptions,
    /// Hub build, reported in `HubStatus`.
    pub version: String,
    /// Per-agent options by index (`[[node_options]]`).
    pub node_options: Vec<NodeOptions>,
    /// Set the keyboards' OS layout on every switch (`[hidpp] os_profile`).
    pub os_profile: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            hysteresis: Duration::from_millis(150),
            resend: None,
            hotkey: proto::Hotkey::default(),
            layout: Vec::new(),
            edge: EdgeOptions::default(),
            version: String::new(),
            node_options: Vec::new(),
            os_profile: true,
        }
    }
}

pub use proto::Entry;

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
    writer: Option<Arc<dyn ReportWriter>>,
    /// Start of the current rate window and output reports written in it.
    out_window: (Instant, u32),
}

impl Device {
    fn new(dev: HidDevice, writer: Option<Arc<dyn ReportWriter>>) -> Self {
        let info = DescriptorInfo::parse(&dev.report_descriptor);
        let release = info.release_reports();
        Self {
            dev,
            info,
            release,
            writer,
            out_window: (Instant::now(), 0),
        }
    }

    /// Output reports PCs may set: those of standard usage pages (keyboard
    /// LEDs). Vendor ones (HID++ requests) stay with the hub.
    fn forwarded_outputs(&self) -> impl Iterator<Item = &proto::descriptor::InputReport> {
        self.info
            .outputs
            .iter()
            .filter(|r| r.app_usage_page < 0xFF00)
    }

    /// Key of an output report in [`State::outputs`]: its report ID.
    fn output_key(&self, report: &[u8]) -> Option<u8> {
        self.info
            .uses_report_ids
            .then(|| report.first().copied())
            .flatten()
    }

    /// Write `report` within the rate limit.
    fn write_output(&mut self, report: &[u8], now: Instant) -> bool {
        let Some(w) = &self.writer else {
            return false;
        };
        let (start, count) = &mut self.out_window;
        if now.duration_since(*start) >= Duration::from_secs(1) {
            (*start, *count) = (now, 0);
        }
        if *count >= OUTPUT_RATE {
            return false;
        }
        *count += 1;
        w.write(report)
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
    /// Sequence numbers of the node's output report datagrams.
    out_seq: SeqTracker,
}

impl Node {
    /// The agent this node is an edge helper for.
    fn helps(&self) -> Option<Uuid> {
        match self.hello.kind {
            NodeKind::EdgeHelper { for_node } => Some(for_node),
            _ => None,
        }
    }

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

/// Last output report per (device, report ID).
type OutputState = BTreeMap<(u8, Option<u8>), Vec<u8>>;

#[derive(Default)]
struct State {
    devices: BTreeMap<u8, Device>,
    nodes: HashMap<Uuid, Node>,
    /// Agent index of each node id, kept across reconnections and (with a
    /// path) hub restarts; also the last active agent.
    store: NodeStore,
    /// Until this deadline only the agent that was active before the hub
    /// restarted takes the input when it connects.
    restore: Option<(Uuid, Instant)>,
    active: Option<Uuid>,
    /// Agents in the order they were last active (most recent last).
    history: Vec<Uuid>,
    last_switch: Option<Instant>,
    /// Layout set by a widget (or loaded from `layout.toml`); replaces the
    /// config's `[[layout]]` seed.
    custom_layout: Option<LayoutFile>,
    /// Where `custom_layout` is persisted; `None` in tests and replays.
    layout_path: Option<std::path::PathBuf>,
    /// Last layout sent, to send it to new nodes and only on change.
    layout_sent: Option<Control>,
    /// Screens of each agent (reported by the agent or its edge helper).
    screens: BTreeMap<Uuid, Vec<Screen>>,
    hotkey: Detector,
    /// Devices paired with the receiver, as the receiver thread last
    /// reported them; sent to widgets.
    receiver_devices: Option<Vec<proto::PairedDevice>>,
    /// Last output report each agent sent, by device and report ID, to
    /// apply it again when that agent becomes active (the keyboard LEDs
    /// follow the active PC). Kept across reconnections.
    outputs: HashMap<Uuid, OutputState>,
    /// Hysteresis and hotkey switch, from the config or `settings.toml`.
    settings: HubSettings,
    settings_path: Option<std::path::PathBuf>,
    next_generation: u64,
    /// Why the hub is powering off, once a widget asked.
    shutting_down: Option<String>,
}

pub struct Router {
    hub_id: Uuid,
    hub_name: String,
    opts: Options,
    started: Instant,
    runtime: Option<tokio::runtime::Handle>,
    state: Mutex<State>,
    /// HID++ channel to the receiver, while its HID++ interface is open.
    /// Outside `state`: requests block for a reply.
    hidpp: Mutex<Option<Arc<crate::hidpp::Hidpp>>>,
    /// The receiver thread (`receiver_ctl`), while the HID++ interface is
    /// open. Locked after `state` when both are needed, never before.
    ctl: Mutex<Option<std::sync::mpsc::Sender<Cmd>>>,
    /// Powers the board off (`main`); gets why. `None` in tests that do
    /// not ask for it.
    poweroff: Option<UnboundedSender<String>>,
}

impl Router {
    /// Must be called inside a tokio runtime if `opts.resend` is set.
    pub fn new(hub_id: Uuid, hub_name: String, opts: Options) -> Self {
        let settings = HubSettings {
            hysteresis_ms: opts.hysteresis.as_millis().min(u32::MAX as u128) as u32,
            hotkey: opts.hotkey,
        };
        Self {
            hub_id,
            hub_name,
            runtime: tokio::runtime::Handle::try_current().ok(),
            opts,
            started: Instant::now(),
            state: Mutex::new(State {
                settings,
                ..Default::default()
            }),
            hidpp: Mutex::new(None),
            ctl: Mutex::new(None),
            poweroff: None,
        }
    }

    /// The board did not power off: a later `Shutdown` tries again.
    pub fn poweroff_failed(&self) {
        self.lock().shutting_down = None;
    }

    /// Where a widget's `Shutdown` goes (ADR 0025).
    pub fn with_poweroff(mut self, tx: UnboundedSender<String>) -> Self {
        self.poweroff = Some(tx);
        self
    }

    /// Use (and keep up to date) a persisted [`NodeStore`]. Its last active
    /// agent gets the input back if it connects within `grace`; call
    /// [`end_restore`](Self::end_restore) once `grace` has passed.
    pub fn with_store(self, store: NodeStore, grace: Duration) -> Self {
        {
            let mut st = self.lock();
            st.restore = store.last_active().map(|id| (id, Instant::now() + grace));
            if let Some((id, _)) = st.restore {
                info!(node = %id, "waiting up to {:.0} s for the last active agent", grace.as_secs_f32());
            }
            st.store = store;
        }
        self
    }

    /// The restore grace is over: if nobody is active, the input goes to a
    /// connected agent (the lowest index).
    pub fn end_restore(&self) {
        let mut st = self.lock();
        if st.restore.take().is_none() || st.active.is_some() {
            return;
        }
        match Self::fallback(&st) {
            Some(next) => {
                info!(node = %next, "last active agent did not come back; activating another");
                self.switch_locked(&mut st, next, None, true);
                self.broadcast_status(&st);
            }
            None => debug!("last active agent did not come back; no agent connected"),
        }
    }

    /// The node to activate when nobody is: the most recently active
    /// connected agent, else the connected agent with the lowest index.
    fn fallback(st: &State) -> Option<Uuid> {
        st.history
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
            })
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

    /// The receiver's HID++ channel, if its interface is open.
    pub fn hidpp(&self) -> Option<Arc<crate::hidpp::Hidpp>> {
        self.hidpp.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_hidpp(&self, h: Option<Arc<crate::hidpp::Hidpp>>) {
        *self.hidpp.lock().unwrap_or_else(|e| e.into_inner()) = h;
    }

    /// The receiver's HID++ interface is open: start the receiver thread
    /// (paired devices, keyboard layout, pairing) on it.
    pub fn attach_receiver(self: &Arc<Self>, h: Arc<crate::hidpp::Hidpp>) {
        self.set_hidpp(Some(h.clone()));
        let tx =
            crate::receiver_ctl::spawn(h, RouterEvents(Arc::downgrade(self)), self.opts.os_profile);
        self.attach_ctl(tx);
    }

    /// Use `tx` as the receiver thread (tests use a plain channel).
    pub fn attach_ctl(&self, tx: std::sync::mpsc::Sender<Cmd>) {
        let profile = {
            let st = self.lock();
            st.active
                .and_then(|a| st.nodes.get(&a))
                .map(|n| self.profile_for(n))
        };
        if let Some(p) = profile {
            let _ = tx.send(Cmd::Profile(p));
        }
        *self.ctl.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }

    /// The HID++ interface closed (receiver unplugged).
    pub fn detach_receiver(&self) {
        self.set_hidpp(None);
        self.ctl.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.set_receiver_devices(Vec::new());
    }

    /// A receiver notification read from the HID++ interface.
    pub fn receiver_notification(&self, report: &[u8]) {
        self.send_ctl(Cmd::Notification(report.to_vec()));
    }

    fn send_ctl(&self, cmd: Cmd) -> bool {
        match &*self.ctl.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(tx) => tx.send(cmd).is_ok(),
            None => false,
        }
    }

    /// The keyboard layout for `node`: its `os_profile` option, else its OS.
    fn profile_for(&self, node: &Node) -> Option<OsKind> {
        let forced = self
            .opts
            .node_options
            .iter()
            .find(|o| o.index == node.index && node.is_agent())
            .and_then(|o| o.os_profile);
        match forced {
            Some(OsProfile::Windows) => Some(OsKind::Windows),
            Some(OsProfile::Macos) => Some(OsKind::MacOs),
            Some(OsProfile::Linux) => Some(OsKind::Linux),
            Some(OsProfile::None) => None,
            None => match node.hello.os {
                Os::Windows => Some(OsKind::Windows),
                Os::MacOs => Some(OsKind::MacOs),
                Os::Linux => Some(OsKind::Linux),
                Os::Other => None,
            },
        }
    }

    /// The receiver thread's device list; sent to widgets on change.
    pub fn set_receiver_devices(&self, devices: Vec<proto::PairedDevice>) {
        let mut st = self.lock();
        if st.receiver_devices.as_ref() == Some(&devices) {
            return;
        }
        for w in st
            .nodes
            .values()
            .filter(|n| n.hello.kind == NodeKind::Widget)
        {
            w.send(Control::ReceiverDevices {
                devices: devices.clone(),
            });
        }
        st.receiver_devices = Some(devices);
    }

    /// A pairing event, for every widget.
    pub fn pairing_event(&self, event: proto::PairingEvent) {
        let st = self.lock();
        for w in st
            .nodes
            .values()
            .filter(|n| n.hello.kind == NodeKind::Widget)
        {
            w.send(Control::PairingEvent(event.clone()));
        }
    }

    pub fn add_device(&self, dev: HidDevice) {
        self.add_device_with_writer(dev, None);
    }

    /// Add a device whose output reports can be written with `writer`.
    pub fn add_device_with_writer(&self, dev: HidDevice, writer: Option<Arc<dyn ReportWriter>>) {
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
        let id = dev.id;
        st.devices.insert(id, Device::new(dev, writer));
        if let Some(active) = st.active {
            Self::apply_outputs(&mut st, active, Some(id));
        }
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
            NodeKind::Widget | NodeKind::EdgeHelper { .. } => 0,
            NodeKind::Agent => Self::assign_index(&mut st, id, &hello.node_name)?,
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
            out_seq: SeqTracker::new(),
        };
        node.send(Control::HelloAck {
            hub_id: self.hub_id,
            hub_name: self.hub_name.clone(),
            assigned_index: index,
        });
        node.send(Control::DeviceList {
            devices: Self::device_list(&st),
        });
        if let Some(layout) = &st.layout_sent {
            node.send(layout.clone());
        }
        if node.hello.kind == NodeKind::Widget {
            node.send(Control::Settings(st.settings));
            if let Some(devices) = &st.receiver_devices {
                node.send(Control::ReceiverDevices {
                    devices: devices.clone(),
                });
            }
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
        let helps = node.helps();
        st.nodes.insert(id, node);

        if let Some(of) = helps {
            if st.active == Some(of) {
                // Tell the helper its machine is active; it does not move
                // the pointer for an Activate without an edge.
                st.nodes[&id].send(Control::Activate { entry: None });
            }
        }
        if is_agent {
            if st.active == Some(id) {
                // Same machine came back while still active.
                st.nodes[&id].send(Control::Activate { entry: None });
            } else if st.active.is_none() {
                match st.restore {
                    Some((last, until)) if last != id && Instant::now() < until => {
                        info!(node = %id, "not activated: waiting for the last active agent")
                    }
                    _ => {
                        self.switch_locked(&mut st, id, None, true);
                    }
                }
            }
        }
        // A new agent index may complete a configured link.
        self.update_layout(&mut st);
        self.broadcast_status(&st);
        Ok(Registration { generation, index })
    }

    fn assign_index(st: &mut State, id: Uuid, name: &str) -> Result<u8, RejectReason> {
        let used: Vec<u8> = st
            .nodes
            .values()
            .filter(|n| n.is_agent())
            .map(|n| n.index)
            .collect();
        let sticky = st.store.index_of(&id).filter(|i| !used.contains(i));
        let unreserved =
            || (1..=MAX_AGENTS).find(|&i| !used.contains(&i) && st.store.holder(i).is_none());
        let index = match sticky.or_else(unreserved) {
            Some(i) => i,
            None => {
                // Every free index is reserved for a disconnected node:
                // take one rather than refuse the agent.
                let i = (1..=MAX_AGENTS)
                    .find(|i| !used.contains(i))
                    .ok_or(RejectReason::TooManyNodes)?;
                warn!(
                    index = i,
                    from = ?st.store.holder(i),
                    "every index is reserved; reassigning (release with --forget-node)"
                );
                i
            }
        };
        st.store.assign(id, index, name);
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
            match Self::fallback(&st) {
                Some(next) => {
                    info!(node = %next, "active node lost; falling back");
                    self.switch_locked(&mut st, next, None, true);
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
            Control::SwitchTo { target, entry } => {
                // The machine whose pointer reached the edge.
                let machine = sender.helps().unwrap_or(from);
                let allowed = kind == NodeKind::Widget || st.active == Some(machine);
                if !allowed {
                    debug!(node = %from, "SwitchTo from an inactive node ignored");
                    return;
                }
                if kind == NodeKind::Widget && st.active == Some(target) {
                    // "Release keys": a switch to the node that is already
                    // active (it would do nothing otherwise).
                    info!(node = %from, "release of every key requested by a widget");
                    self.release_everywhere(&mut st);
                    return;
                }
                let entry = entry.map(|e| Entry {
                    pos_ratio: e.pos_ratio.clamp(0.0, 1.0),
                    ..e
                });
                // A widget's menu switch is deliberate: no hysteresis.
                let force = kind == NodeKind::Widget;
                self.switch_locked(&mut st, target, entry, force);
            }
            Control::Screens { mut screens } if kind != NodeKind::Widget => {
                let machine = sender.helps().unwrap_or(from);
                for s in &mut screens {
                    s.node = machine;
                }
                info!(node = %machine, screens = screens.len(), "screens reported");
                for w in st
                    .nodes
                    .values()
                    .filter(|n| n.hello.kind == NodeKind::Widget)
                {
                    w.send(Control::Screens {
                        screens: screens.clone(),
                    });
                }
                st.screens.insert(machine, screens);
                self.update_layout(&mut st);
                self.broadcast_status(&st);
            }
            Control::Layout {
                edges,
                options,
                canvas,
                ..
            } if kind == NodeKind::Widget => {
                if edges.is_empty() && canvas.is_empty() {
                    // "Back to the seed": a real canvas always has blocks.
                    info!(node = %from, "widget reset the layout to the config seed");
                    if let Some(path) = &st.layout_path {
                        match std::fs::remove_file(path) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => warn!("removing {}: {e}", path.display()),
                        }
                    }
                    st.custom_layout = None;
                    self.update_layout(&mut st);
                    return;
                }
                info!(node = %from, links = edges.len(), blocks = canvas.len(), "layout set by a widget");
                let file = LayoutFile {
                    options,
                    link: edges,
                    canvas,
                };
                if let Some(path) = &st.layout_path {
                    if let Err(e) = file.save(path) {
                        warn!("{e:#}");
                    }
                }
                st.custom_layout = Some(file);
                self.update_layout(&mut st);
            }
            Control::Settings(new) if kind == NodeKind::Widget => {
                info!(node = %from, ?new, "settings set by a widget");
                if let Some(path) = &st.settings_path {
                    if let Err(e) = crate::settings::save(path, &new) {
                        warn!("{e:#}");
                    }
                }
                st.settings = new;
                for w in st
                    .nodes
                    .values()
                    .filter(|n| n.hello.kind == NodeKind::Widget)
                {
                    w.send(Control::Settings(new));
                }
            }
            Control::PairingStart { timeout_s, .. } if kind == NodeKind::Widget => {
                info!(node = %from, timeout_s, "pairing requested by a widget");
                self.receiver_cmd(sender, Cmd::PairingStart { timeout_s });
            }
            Control::PairingStop if kind == NodeKind::Widget => {
                self.receiver_cmd(sender, Cmd::PairingStop);
            }
            Control::Unpair { slot } if kind == NodeKind::Widget => {
                info!(node = %from, slot, "unpair requested by a widget");
                self.receiver_cmd(sender, Cmd::Unpair { slot });
            }
            Control::SetPointerSpeed { slot, value } if kind == NodeKind::Widget => {
                info!(node = %from, slot, value, "pointer speed requested by a widget");
                self.receiver_cmd(sender, Cmd::SetPointerSpeed { slot, value });
            }
            Control::SetFnSwap { slot, on } if kind == NodeKind::Widget => {
                info!(node = %from, slot, on, "Fn swap requested by a widget");
                self.receiver_cmd(sender, Cmd::SetFnSwap { slot, on });
            }
            Control::Shutdown if kind == NodeKind::Widget => {
                if let Some(reason) = &st.shutting_down {
                    sender.send(Control::ShuttingDown {
                        reason: reason.clone(),
                    });
                    return;
                }
                let reason = format!("requested from {}", sender.hello.node_name);
                warn!(node = %from, name = %sender.hello.node_name, "power-off requested by a widget");
                // Nothing stays pressed on any PC.
                self.release_everywhere(&mut st);
                for n in st.nodes.values() {
                    n.send(Control::ShuttingDown {
                        reason: reason.clone(),
                    });
                }
                match &self.poweroff {
                    Some(tx) if tx.send(reason.clone()).is_ok() => {
                        st.shutting_down = Some(reason);
                    }
                    _ => warn!("no power-off handler; the hub stays up"),
                }
            }
            other => warn!(node = %from, msg = other.name(), "unexpected message"),
        }
    }

    /// Hand `cmd` to the receiver thread, or tell `sender` why not.
    fn receiver_cmd(&self, sender: &Node, cmd: Cmd) {
        if !self.send_ctl(cmd) {
            sender.send(Control::PairingEvent(proto::PairingEvent::Failed {
                reason: "the receiver's HID++ interface is not open".into(),
            }));
        }
    }

    /// Keep the layout in `path` (`<state_dir>/layout.toml`), loading it
    /// now if it exists.
    pub fn with_layout_file(self, path: std::path::PathBuf) -> Result<Self, anyhow::Error> {
        {
            let mut st = self.lock();
            st.custom_layout = LayoutFile::load(&path)?;
            if let Some(l) = &st.custom_layout {
                info!(
                    links = l.link.len(),
                    "using the layout in {}",
                    path.display()
                );
            }
            st.layout_path = Some(path);
        }
        Ok(self)
    }

    /// Keep widget-set switching settings in `path`
    /// (`<state_dir>/settings.toml`), loading them now if it exists.
    pub fn with_settings_file(self, path: std::path::PathBuf) -> Result<Self, anyhow::Error> {
        {
            let mut st = self.lock();
            if let Some(s) = crate::settings::load(&path)? {
                info!(?s, "using the settings in {}", path.display());
                st.settings = s;
            }
            st.settings_path = Some(path);
        }
        Ok(self)
    }

    pub fn settings(&self) -> HubSettings {
        self.lock().settings
    }

    /// Re-read `layout.toml` (after `--load-layout`, on SIGHUP) and send the
    /// result. A missing file goes back to the config seed.
    pub fn reload_layout(&self) -> Result<(), anyhow::Error> {
        let mut st = self.lock();
        if let Some(path) = st.layout_path.clone() {
            st.custom_layout = LayoutFile::load(&path)?;
            info!(
                custom = st.custom_layout.is_some(),
                "layout reloaded from {}",
                path.display()
            );
        }
        self.update_layout(&mut st);
        Ok(())
    }

    /// The layout nodes should have: the custom one, else the config seed
    /// resolved against the screens known so far. Screens are always the
    /// live ones.
    #[cfg(test)]
    fn current_layout(&self, st: &State) -> Option<Control> {
        self.resolve_layout(st).0
    }

    /// The layout, and the seed links resolved with a placeholder screen.
    fn resolve_layout(&self, st: &State) -> (Option<Control>, Vec<(usize, Uuid)>) {
        let (edges, options, canvas, placeholders) = match &st.custom_layout {
            Some(l) => (l.link.clone(), l.options, l.canvas.clone(), Vec::new()),
            None if self.opts.layout.is_empty() => return (None, Vec::new()),
            None => {
                let c = layout::compile(&self.opts.layout, |i| st.store.holder(i), &st.screens);
                (c.links, self.opts.edge, Vec::new(), c.placeholders)
            }
        };
        let screens: Vec<Screen> = st.screens.values().flatten().cloned().collect();
        if screens.is_empty() && edges.is_empty() {
            return (None, placeholders);
        }
        let layout = Control::Layout {
            screens,
            edges,
            options,
            canvas,
        };
        (Some(layout), placeholders)
    }

    /// Send the layout to every node if it changed.
    fn update_layout(&self, st: &mut State) {
        let (layout, placeholders) = self.resolve_layout(st);
        if layout == st.layout_sent {
            return;
        }
        for (n, node) in placeholders {
            info!(
                "layout: link {} resolved with a placeholder screen for node {node} \
                 (no screens reported; one way only until its edge helper connects)",
                n + 1
            );
        }
        if let Some(Control::Layout { screens, edges, .. }) = &layout {
            info!(screens = screens.len(), links = edges.len(), "layout");
        }
        // Going back to no layout still tells the nodes to drop their links.
        let send = layout.clone().unwrap_or_else(|| Control::Layout {
            screens: st.screens.values().flatten().cloned().collect(),
            edges: Vec::new(),
            options: self.opts.edge,
            canvas: Vec::new(),
        });
        for n in st.nodes.values() {
            n.send(send.clone());
        }
        st.layout_sent = layout;
    }

    /// Send every agent a "nothing pressed" report for every device, twice
    /// (the copy after `resend`, as for keys), in case a key is stuck.
    fn release_everywhere(&self, st: &mut State) {
        let State { devices, nodes, .. } = st;
        for n in nodes.values_mut().filter(|n| n.is_agent()) {
            for d in devices.values() {
                for r in &d.release {
                    let Some(b) = n.datagram(d.dev.id, FLAG_RELEASE_ALL, r) else {
                        continue;
                    };
                    n.link.send_datagram(b.clone());
                    if let (Some(delay), Some(rt)) = (self.opts.resend, &self.runtime) {
                        let link = n.link.clone();
                        rt.spawn(async move {
                            tokio::time::sleep(delay).await;
                            link.send_datagram(b);
                        });
                    }
                }
            }
        }
    }

    fn hotkey_action(&self, st: &mut State, action: Action) {
        let target = match action {
            Action::ReleaseKeys => {
                info!("emergency hotkey: release keys");
                self.release_everywhere(st);
                return;
            }
            Action::Agent(index) => st
                .nodes
                .values()
                .find(|n| n.is_agent() && n.index == index)
                .map(|n| n.hello.node_id),
            // The most recently active connected agent but the current one.
            Action::Previous => st
                .history
                .iter()
                .rev()
                .find(|id| Some(**id) != st.active && st.nodes.get(id).is_some_and(Node::is_agent))
                .copied(),
        };
        match target {
            Some(t) => {
                info!(?action, "emergency hotkey");
                self.switch_locked(st, t, None, true);
            }
            None => info!(?action, "emergency hotkey: no such agent"),
        }
    }

    /// Make `target` the active agent. `force` bypasses the hysteresis.
    fn switch_locked(
        &self,
        st: &mut State,
        target: Uuid,
        entry: Option<Entry>,
        force: bool,
    ) -> bool {
        if st.active == Some(target) {
            return false;
        }
        let now = Instant::now();
        if !force
            && st.last_switch.is_some_and(|t| {
                now.duration_since(t) < Duration::from_millis(st.settings.hysteresis_ms as u64)
            })
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
            for h in st.nodes.values().filter(|n| n.helps() == Some(old)) {
                h.send(Control::Deactivate);
            }
        }
        st.active = Some(target);
        Self::apply_outputs(st, target, None);
        st.restore = None;
        st.store.set_last_active(target);
        st.history.retain(|n| *n != target);
        st.history.push(target);
        st.last_switch = Some(now);
        let n = &st.nodes[&target];
        info!(node = %target, name = %n.hello.node_name, index = n.index, ?entry, "switched");
        self.send_ctl(Cmd::Profile(self.profile_for(n)));
        let activate = Control::Activate { entry };
        for n in st
            .nodes
            .values()
            .filter(|n| n.hello.node_id == target || n.helps() == Some(target))
        {
            n.send(activate.clone());
        }
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

        if st.settings.hotkey.enabled {
            if let Some(kb) = &dev.info.keyboard {
                if let Some(ks) = kb.decode(dev.info.uses_report_ids, report) {
                    let cfg = st.settings.hotkey;
                    if let Some(action) =
                        st.hotkey.feed(Instant::now(), &cfg, ks.modifiers, ks.keys)
                    {
                        self.hotkey_action(st, action);
                        // The key is not delivered anywhere.
                        return;
                    }
                }
            }
        }

        let resend = !dev.is_mouse_report(report);
        let Some(node) = st.active.and_then(|id| st.nodes.get_mut(&id)) else {
            return;
        };
        // ADR 0011: opt-in Alt <-> GUI swap for this target, on keyboard
        // reports only, after the hotkey saw the original.
        let swapped = self
            .opts
            .node_options
            .iter()
            .any(|o| o.swap_alt_gui && o.index == node.index && node.is_agent())
            .then_some(dev.info.keyboard.as_ref())
            .flatten()
            .and_then(|kb| kb.swap_alt_gui(dev.info.uses_report_ids, report));
        let report = swapped.as_deref().unwrap_or(report);
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

    /// An output report datagram from node `from` (protocol v4). Kept as
    /// that node's last state; written to the receiver only if the node is
    /// the active agent.
    pub fn on_output(&self, from: Uuid, dev_id: u8, seq: u16, report: &[u8]) {
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(node) = st.nodes.get_mut(&from) else {
            return;
        };
        if !node.is_agent() {
            debug!(node = %from, "output report from a non-agent ignored");
            return;
        }
        if node.out_seq.observe(dev_id, seq).is_none() {
            return; // redundant copy or late
        }
        let Some(dev) = st.devices.get_mut(&dev_id) else {
            return;
        };
        let declared = dev
            .info
            .output_for(report)
            .is_some_and(|r| r.app_usage_page < 0xFF00);
        if !declared {
            debug!(
                node = %from,
                dev_id,
                len = report.len(),
                "output report not declared by the interface (or vendor); dropped"
            );
            return;
        }
        let key = (dev_id, dev.output_key(report));
        let stored = st.outputs.entry(from).or_default();
        if stored.get(&key).map(Vec::as_slice) == Some(report) {
            return;
        }
        stored.insert(key, report.to_vec());
        if st.active == Some(from) {
            debug!(dev_id, ?report, "output report written");
            if !dev.write_output(report, Instant::now()) {
                debug!(
                    dev_id,
                    "output report not written (rate limit or queue full)"
                );
            }
        }
    }

    /// Write `node`'s last output reports to the receiver (all devices, or
    /// only `only`); a report it never sent is written as all zeros (LEDs
    /// off), so the LEDs show the state of the PC that is now active.
    fn apply_outputs(st: &mut State, node: Uuid, only: Option<u8>) {
        let State {
            devices, outputs, ..
        } = st;
        let stored = outputs.get(&node);
        let now = Instant::now();
        for dev in devices.values_mut() {
            if dev.writer.is_none() || only.is_some_and(|o| o != dev.dev.id) {
                continue;
            }
            let reports: Vec<Vec<u8>> = dev
                .forwarded_outputs()
                .map(|r| {
                    stored
                        .and_then(|s| s.get(&(dev.dev.id, r.report_id)))
                        .cloned()
                        .unwrap_or_else(|| {
                            let mut z = vec![0u8; r.len];
                            if let Some(id) = r.report_id {
                                z.insert(0, id);
                            }
                            z
                        })
                })
                .collect();
            for r in reports {
                dev.write_output(&r, now);
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
                screens: st
                    .screens
                    .get(&n.hello.node_id)
                    .map_or(0, |s| s.len().min(u8::MAX as usize) as u8),
            })
            .collect();
        nodes.sort_by_key(|n| (n.kind == NodeKind::Widget, n.assigned_index));
        HubStatus {
            hub_name: self.hub_name.clone(),
            active: st.active,
            nodes,
            device_count: st.devices.len().min(u8::MAX as usize) as u8,
            uptime_s: self.started.elapsed().as_secs(),
            version: self.opts.version.clone(),
        }
    }

    fn broadcast_status(&self, st: &State) {
        let status = self.build_status(st);
        for n in st.nodes.values() {
            n.send(Control::Status(status.clone()));
        }
    }
}

/// [`crate::receiver_ctl::Events`] for the router.
struct RouterEvents(std::sync::Weak<Router>);

impl crate::receiver_ctl::Events for RouterEvents {
    fn devices(&self, devices: Vec<proto::PairedDevice>) {
        if let Some(r) = self.0.upgrade() {
            r.set_receiver_devices(devices);
        }
    }
    fn pairing(&self, event: proto::PairingEvent) {
        if let Some(r) = self.0.upgrade() {
            r.pairing_event(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::tests::{BOOT_KEYBOARD, UNIFYING_IFACE1};
    use proto::Edge;
    use proto::EdgeLink;
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
        Router::new(Uuid::new_v4(), "hub".into(), Options::default())
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
    fn only_a_widget_powers_the_hub_off() {
        let (tx, mut off) = unbounded_channel();
        let r = router().with_poweroff(tx);
        r.add_device(device(0, BOOT_KEYBOARD));
        let mut a = join(&r, NodeKind::Agent).unwrap();
        let mut w = join(&r, NodeKind::Widget).unwrap();
        a.msgs();
        a.link.take();
        w.msgs();

        r.on_control(a.id, Control::Shutdown);
        assert!(off.try_recv().is_err());
        assert!(a.events().is_empty());

        r.on_control(w.id, Control::Shutdown);
        assert_eq!(off.try_recv().unwrap(), "requested from n");
        let shutting = Control::ShuttingDown {
            reason: "requested from n".into(),
        };
        assert_eq!(a.msgs(), std::slice::from_ref(&shutting));
        assert_eq!(w.msgs(), std::slice::from_ref(&shutting));
        // Every key released on the agent first.
        assert!(a
            .link
            .take()
            .iter()
            .any(|(_, flags, _, _)| flags & FLAG_RELEASE_ALL != 0));

        // A second request is only answered.
        r.on_control(w.id, Control::Shutdown);
        assert!(off.try_recv().is_err());
        assert_eq!(w.msgs(), [shutting]);
        assert!(a.msgs().is_empty());
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
        assert_eq!(w.events(), ["HelloAck", "DeviceList", "Settings"]);
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
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 0.5,
                }),
            },
        );
        assert_eq!(r.active(), Some(a.id));

        // Registration activated `a` just now: hysteresis blocks the switch.
        r.on_control(
            a.id,
            Control::SwitchTo {
                target: b.id,
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 0.5,
                }),
            },
        );
        assert_eq!(r.active(), Some(a.id));

        r.lock().last_switch = Some(Instant::now() - Duration::from_secs(1));
        r.on_control(
            a.id,
            Control::SwitchTo {
                target: b.id,
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 1.7,
                }),
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
            entry: Some(Entry {
                screen: 0,
                edge: Edge::Left,
                pos_ratio: 1.0,
            }),
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
    fn hotkey_previous_and_release_keys() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        let a = join(&r, NodeKind::Agent).unwrap();
        let b = join(&r, NodeKind::Agent).unwrap();
        let c = join(&r, NodeKind::Agent).unwrap();
        let hotkey = |key: u8| {
            for _ in 0..2 {
                r.on_report(0, &key_report(0x01, 0));
                r.on_report(0, &key_report(0, 0));
            }
            r.on_report(0, &key_report(0, key));
            r.on_report(0, &key_report(0, 0));
        };
        hotkey(0x20); // '3'
        assert_eq!(r.active(), Some(c.id));
        hotkey(0x1F); // '2'
        assert_eq!(r.active(), Some(b.id));
        hotkey(0x27); // '0': back to c
        assert_eq!(r.active(), Some(c.id));
        hotkey(0x27); // and to b again
        assert_eq!(r.active(), Some(b.id));

        for n in [&a, &b, &c] {
            n.link.take();
        }
        hotkey(0x29); // Esc
        assert_eq!(r.active(), Some(b.id));
        // Every agent gets a release-all; the Esc itself reaches nobody.
        for n in [&a, &b, &c] {
            let d = n.link.take();
            assert!(d.iter().any(|d| d.1 == FLAG_RELEASE_ALL));
            assert!(d.iter().all(|d| d.3[2] != 0x29));
        }

        // Alt as the modifier; the widget turns "previous" off.
        let w = join(&r, NodeKind::Widget).unwrap();
        r.on_control(
            w.id,
            Control::Settings(HubSettings {
                hysteresis_ms: 150,
                hotkey: proto::Hotkey {
                    modifier: proto::HotkeyModifier::Alt,
                    previous: false,
                    ..Default::default()
                },
            }),
        );
        hotkey(0x1E); // Ctrl, Ctrl, '1': nothing
        assert_eq!(r.active(), Some(b.id));
        for _ in 0..2 {
            r.on_report(0, &key_report(0x04, 0));
            r.on_report(0, &key_report(0, 0));
        }
        r.on_report(0, &key_report(0, 0x1E));
        assert_eq!(r.active(), Some(a.id));
        for _ in 0..2 {
            r.on_report(0, &key_report(0x40, 0));
            r.on_report(0, &key_report(0, 0));
        }
        r.on_report(0, &key_report(0, 0x27)); // '0' is just a key now
        assert_eq!(r.active(), Some(a.id));
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
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Right,
                    pos_ratio: 0.1,
                }),
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

    fn force_switch(r: &Router, to: Uuid) {
        let mut st = r.lock();
        assert!(r.switch_locked(&mut st, to, None, true));
    }

    fn store_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rawkvm-router-{}", Uuid::new_v4()))
    }

    fn router_with(dir: &std::path::Path, grace: Duration) -> Router {
        let store = NodeStore::load(&NodeStore::path_in(dir)).unwrap();
        router().with_store(store, grace)
    }

    #[test]
    fn indices_and_last_active_survive_a_hub_restart() {
        let dir = store_dir();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        {
            let r = router_with(&dir, RESTORE_GRACE);
            assert_eq!(join_as(&r, a, NodeKind::Agent).unwrap().reg.index, 1);
            assert_eq!(join_as(&r, b, NodeKind::Agent).unwrap().reg.index, 2);
            force_switch(&r, b);
            assert_eq!(r.active(), Some(b));
        }
        // Restart: `a` connects first but `b` was active.
        let r = router_with(&dir, Duration::from_secs(3600));
        let mut na = join_as(&r, a, NodeKind::Agent).unwrap();
        assert_eq!(na.reg.index, 1);
        assert_eq!(r.active(), None);
        assert!(!na.events().contains(&"Activate"));
        let mut nb = join_as(&r, b, NodeKind::Agent).unwrap();
        assert_eq!(nb.reg.index, 2);
        assert_eq!(r.active(), Some(b));
        assert!(nb.events().contains(&"Activate"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_last_active_gives_way_after_the_grace() {
        let dir = store_dir();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        {
            let r = router_with(&dir, RESTORE_GRACE);
            join_as(&r, b, NodeKind::Agent).unwrap();
        }
        let r = router_with(&dir, Duration::from_secs(3600));
        let mut na = join_as(&r, a, NodeKind::Agent).unwrap();
        assert_eq!(r.active(), None);
        r.end_restore();
        assert_eq!(r.active(), Some(a));
        assert!(na.events().contains(&"Activate"));
        // Once over, the grace does not come back.
        let c = join(&r, NodeKind::Agent).unwrap();
        r.end_restore();
        assert_eq!(r.active(), Some(a));
        drop(c);

        // A zero grace behaves like no store at all.
        let r = router_with(&dir, Duration::ZERO);
        join_as(&r, b, NodeKind::Agent).unwrap();
        assert_eq!(r.active(), Some(b));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hotkey_during_the_grace_wins() {
        let dir = store_dir();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        {
            let r = router_with(&dir, RESTORE_GRACE);
            join_as(&r, a, NodeKind::Agent).unwrap();
            join_as(&r, b, NodeKind::Agent).unwrap();
            force_switch(&r, b);
        }
        let r = router_with(&dir, Duration::from_secs(3600));
        r.add_device(device(0, BOOT_KEYBOARD));
        join_as(&r, a, NodeKind::Agent).unwrap();
        for (m, k) in [(1, 0), (0, 0), (1, 0), (0, 0), (0, 0x1E), (0, 0)] {
            r.on_report(0, &key_report(m, k));
        }
        assert_eq!(r.active(), Some(a));
        // `b` coming back later does not take the input.
        join_as(&r, b, NodeKind::Agent).unwrap();
        assert_eq!(r.active(), Some(a));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn indices_stay_reserved_while_disconnected() {
        let dir = store_dir();
        let r = router_with(&dir, RESTORE_GRACE);
        let a = join(&r, NodeKind::Agent).unwrap();
        let _b = join(&r, NodeKind::Agent).unwrap();
        r.unregister(a.id, a.reg);
        assert_eq!(join(&r, NodeKind::Agent).unwrap().reg.index, 3);
        assert_eq!(join_as(&r, a.id, NodeKind::Agent).unwrap().reg.index, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn all_indices_reserved_reassigns_a_disconnected_one() {
        let dir = store_dir();
        let r = router_with(&dir, RESTORE_GRACE);
        let mut kept = Vec::new();
        for _ in 0..4 {
            let n = join(&r, NodeKind::Agent).unwrap();
            if n.reg.index != 3 {
                r.unregister(n.id, n.reg);
            } else {
                kept.push(n);
            }
        }
        // 1, 2 and 4 are reserved for disconnected nodes; 3 is connected.
        assert_eq!(join(&r, NodeKind::Agent).unwrap().reg.index, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn helper(r: &Router, of: Uuid) -> TestNode {
        join(r, NodeKind::EdgeHelper { for_node: of }).unwrap()
    }

    fn scr(index: u8, x: i32, y: i32, w: u32, h: u32) -> Screen {
        Screen {
            node: Uuid::nil(),
            index,
            x,
            y,
            w,
            h,
            scale: 1.0,
        }
    }

    #[test]
    fn edge_helper_switches_for_its_machine() {
        let r = router();
        let mut win = join(&r, NodeKind::Agent).unwrap(); // gadget, index 1
        let mut mac = join(&r, NodeKind::Agent).unwrap();
        let mut mac_edge = helper(&r, mac.id);
        let mut win_edge = helper(&r, win.id);
        assert_eq!((mac_edge.reg.index, win_edge.reg.index), (0, 0));
        // win was first, so it is active and its helper was told.
        assert!(win_edge.events().contains(&"Activate"));
        assert!(!mac_edge.events().contains(&"Activate"));
        force_switch(&r, mac.id);
        assert!(mac_edge.events().contains(&"Activate"));
        assert!(win_edge.events().contains(&"Deactivate"));
        win.msgs();
        mac.msgs();

        // The Windows helper cannot switch while the Mac is active.
        r.lock().last_switch = None;
        r.on_control(
            win_edge.id,
            Control::SwitchTo {
                target: win.id,
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 0.5,
                }),
            },
        );
        assert_eq!(r.active(), Some(mac.id));

        // The Mac's helper can.
        r.on_control(
            mac_edge.id,
            Control::SwitchTo {
                target: win.id,
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 0.4,
                }),
            },
        );
        assert_eq!(r.active(), Some(win.id));
        let activate = Control::Activate {
            entry: Some(Entry {
                screen: 0,
                edge: Edge::Left,
                pos_ratio: 0.4,
            }),
        };
        assert!(win.msgs().contains(&activate));
        assert!(win_edge.msgs().contains(&activate));
        assert!(mac.events().contains(&"Deactivate"));
        assert!(mac_edge.events().contains(&"Deactivate"));
    }

    #[test]
    fn edge_helpers_are_not_agents() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        let orphan = helper(&r, Uuid::new_v4()); // its agent never connected
        assert_eq!(r.active(), None);
        let agents: Vec<_> = (0..4).map(|_| join(&r, NodeKind::Agent).unwrap()).collect();
        assert_eq!(agents[3].reg.index, 4);
        // Ctrl, Ctrl, 0 is not a digit; the hotkey never targets a helper.
        for _ in 0..2 {
            r.on_report(0, &key_report(0x01, 0));
            r.on_report(0, &key_report(0, 0));
        }
        r.on_report(0, &key_report(0, 0x1F));
        assert_eq!(r.active(), Some(agents[1].id));
        assert!(orphan.link.take().is_empty(), "helpers get no reports");
        r.on_control(
            orphan.id,
            Control::SwitchTo {
                target: agents[2].id,
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 0.5,
                }),
            },
        );
        assert_eq!(r.active(), Some(agents[1].id));
    }

    fn layout_of(msgs: &[Control]) -> Option<(Vec<Screen>, Vec<EdgeLink>, EdgeOptions)> {
        msgs.iter().rev().find_map(|m| match m {
            Control::Layout {
                screens,
                edges,
                options,
                ..
            } => Some((screens.clone(), edges.clone(), *options)),
            _ => None,
        })
    }

    #[test]
    fn config_layout_resolves_when_screens_arrive() {
        let r = Router::new(
            Uuid::new_v4(),
            "hub".into(),
            Options {
                layout: toml::from_str::<crate::config::HubConfig>(
                    "[[layout]]\nfrom = 2\nedge = \"right\"\nto = 1\n",
                )
                .unwrap()
                .layout,
                edge: EdgeOptions {
                    push_ms: 80,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let mut win = join(&r, NodeKind::Agent).unwrap();
        let mac = join(&r, NodeKind::Agent).unwrap();
        let mut mac_edge = helper(&r, mac.id);
        let win_edge = helper(&r, win.id);
        assert_eq!(layout_of(&win.msgs()), None, "no screens yet");

        r.on_control(
            mac_edge.id,
            Control::Screens {
                screens: vec![scr(0, 0, 0, 1512, 982), scr(1, 1512, -229, 2560, 1440)],
            },
        );
        // Windows has no helper yet: the Mac -> Windows link resolves one way
        // with a placeholder screen, so switching to the gadget works.
        let (_, edges, _) = layout_of(&win.msgs()).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].from_node, edges[0].to_node, edges[0].to_screen),
            (mac.id, win.id, 0)
        );
        r.on_control(
            win_edge.id,
            Control::Screens {
                screens: vec![scr(0, 0, 0, 1920, 1080)],
            },
        );
        let (screens, edges, options) = layout_of(&mac_edge.msgs()).unwrap();
        assert_eq!(screens.len(), 3);
        assert!(screens.iter().filter(|s| s.node == mac.id).count() == 2);
        assert_eq!(options.push_ms, 80);
        let fwd = edges.iter().find(|l| l.from_node == mac.id).unwrap();
        assert_eq!(
            (fwd.from_screen, fwd.to_node, fwd.to_edge),
            (1, win.id, Edge::Left)
        );
        assert!(edges
            .iter()
            .any(|l| l.from_node == win.id && l.to_node == mac.id));

        // Same screens again: nothing is re-sent.
        r.on_control(
            win_edge.id,
            Control::Screens {
                screens: vec![scr(0, 0, 0, 1920, 1080)],
            },
        );
        assert_eq!(layout_of(&mac_edge.msgs()), None);
        // A node connecting later gets it right away.
        let mut late = join(&r, NodeKind::Widget).unwrap();
        assert_eq!(layout_of(&late.msgs()).map(|l| l.1.len()), Some(2));
    }

    #[test]
    fn widget_layout_is_persisted_and_wins_over_the_seed() {
        let dir = store_dir();
        let path = LayoutFile::path_in(&dir);
        let seeded = || {
            Router::new(
                Uuid::new_v4(),
                "hub".into(),
                Options {
                    layout: vec![crate::layout::LinkSpec {
                        from: 2,
                        edge: Edge::Right,
                        to: 1,
                        from_screen: None,
                        to_screen: None,
                        from_range: (0.0, 1.0),
                        to_range: (0.0, 1.0),
                        oneway: false,
                    }],
                    ..Default::default()
                },
            )
            .with_layout_file(path.clone())
            .unwrap()
        };
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let link = EdgeLink {
            from_node: a,
            from_screen: 0,
            from_edge: Edge::Top,
            from_range: (0.0, 1.0),
            to_node: b,
            to_screen: 0,
            to_edge: Edge::Bottom,
            to_range: (0.0, 1.0),
        };
        {
            let r = seeded();
            let w = join(&r, NodeKind::Widget).unwrap();
            let mut n = join(&r, NodeKind::Agent).unwrap();
            r.on_control(
                w.id,
                Control::Layout {
                    screens: vec![],
                    edges: vec![link.clone()],
                    options: EdgeOptions::default(),
                    canvas: vec![],
                },
            );
            // Applied at once, without a restart.
            assert_eq!(layout_of(&n.msgs()).unwrap().1, std::slice::from_ref(&link));
        }
        // After a restart the saved layout is used, not the seed.
        let r = seeded();
        let mut n = join(&r, NodeKind::Agent).unwrap();
        assert_eq!(layout_of(&n.msgs()).unwrap().1, [link]);

        // Deleting the file and reloading goes back to the seed (which
        // resolves to nothing yet: no screens).
        std::fs::remove_file(&path).unwrap();
        r.reload_layout().unwrap();
        assert_eq!(r.current_layout(&r.lock()), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v3_widget_settings_menu_switch_canvas_and_status() {
        let dir = store_dir();
        let r = Router::new(
            Uuid::new_v4(),
            "hub".into(),
            Options {
                version: "0.1.0 (test)".into(),
                ..Default::default()
            },
        )
        .with_layout_file(LayoutFile::path_in(&dir))
        .unwrap()
        .with_settings_file(crate::settings::path_in(&dir))
        .unwrap();
        let a = join(&r, NodeKind::Agent).unwrap();
        let mut b = join(&r, NodeKind::Agent).unwrap();
        let mut b_edge = helper(&r, b.id);
        let mut w = join(&r, NodeKind::Widget).unwrap();
        w.msgs();

        // Settings: applied, persisted, echoed to widgets.
        let new = HubSettings {
            hysteresis_ms: 5000,
            hotkey: proto::Hotkey {
                enabled: false,
                ..Default::default()
            },
        };
        r.on_control(w.id, Control::Settings(new));
        assert_eq!(r.settings(), new);
        assert_eq!(
            crate::settings::load(&crate::settings::path_in(&dir)).unwrap(),
            Some(new)
        );
        assert!(w.msgs().contains(&Control::Settings(new)));
        // An agent cannot change them.
        r.on_control(a.id, Control::Settings(HubSettings::default()));
        assert_eq!(r.settings(), new);

        // Menu switch from the widget: no entry, no hysteresis (5 s now).
        r.on_control(
            w.id,
            Control::SwitchTo {
                target: b.id,
                entry: None,
            },
        );
        assert_eq!(r.active(), Some(b.id));
        assert!(b.msgs().contains(&Control::Activate { entry: None }));
        assert!(b_edge.msgs().contains(&Control::Activate { entry: None }));
        // An agent's edge switch now waits for the longer hysteresis.
        r.on_control(
            b_edge.id,
            Control::SwitchTo {
                target: a.id,
                entry: Some(Entry {
                    screen: 0,
                    edge: Edge::Left,
                    pos_ratio: 0.5,
                }),
            },
        );
        assert_eq!(r.active(), Some(b.id));

        // Screens count per node and the hub version in the status.
        r.on_control(
            b_edge.id,
            Control::Screens {
                screens: vec![scr(0, 0, 0, 1920, 1080)],
            },
        );
        let st = r.status();
        assert_eq!(st.version, "0.1.0 (test)");
        let count = |id| st.nodes.iter().find(|n| n.node_id == id).unwrap().screens;
        assert_eq!((count(a.id), count(b.id)), (0, 1));

        // Canvas: persisted with the layout and relayed.
        let canvas = vec![proto::Placement {
            node: b.id,
            x: 3190.0,
            y: 0.0,
            scale: Some(1.25),
        }];
        r.on_control(
            w.id,
            Control::Layout {
                screens: vec![],
                edges: vec![],
                options: EdgeOptions::default(),
                canvas: canvas.clone(),
            },
        );
        let saved = LayoutFile::load(&LayoutFile::path_in(&dir))
            .unwrap()
            .unwrap();
        assert_eq!(saved.canvas, canvas);
        let got = w.msgs().into_iter().rev().find_map(|m| match m {
            Control::Layout { canvas, .. } => Some(canvas),
            _ => None,
        });
        assert_eq!(got, Some(canvas));

        // Empty edges and canvas: back to the config seed, file removed.
        r.on_control(
            w.id,
            Control::Layout {
                screens: vec![],
                edges: vec![],
                options: EdgeOptions::default(),
                canvas: vec![],
            },
        );
        assert!(!LayoutFile::path_in(&dir).exists());
        let got = w.msgs().into_iter().rev().find_map(|m| match m {
            Control::Layout { canvas, edges, .. } => Some((canvas, edges)),
            _ => None,
        });
        assert_eq!(got, Some((vec![], vec![])));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn alt_gui_swap_only_for_the_configured_agent() {
        let r = Router::new(
            Uuid::new_v4(),
            "hub".into(),
            Options {
                node_options: vec![NodeOptions {
                    index: 1,
                    swap_alt_gui: true,
                    os_profile: None,
                }],
                ..Default::default()
            },
        );
        r.add_device(device(0, BOOT_KEYBOARD));
        r.add_device(device(1, UNIFYING_IFACE1));
        let win = join(&r, NodeKind::Agent).unwrap(); // index 1, active
        let mac = join(&r, NodeKind::Agent).unwrap();
        let alt_a = [0x04, 0, 0x04, 0, 0, 0, 0, 0];
        r.on_report(0, &alt_a);
        r.on_report(0, &[0, 0, 0xE2, 0, 0, 0, 0, 0]);
        let mouse = [0x02, 0x0C, 0, 0x01, 0x10, 0, 0, 0];
        r.on_report(1, &mouse);
        let got: Vec<Vec<u8>> = win.link.take().into_iter().map(|d| d.3).collect();
        assert_eq!(
            got,
            [
                vec![0x08, 0, 0x04, 0, 0, 0, 0, 0], // left GUI + A
                vec![0, 0, 0xE3, 0, 0, 0, 0, 0],
                mouse.to_vec(), // other interfaces untouched
            ]
        );
        // The other agent gets the reports as they are.
        force_switch(&r, mac.id);
        let released = win.link.take();
        // Release-all is not swapped: all zeros for the keyboard.
        let kb_release: Vec<_> = released.iter().filter(|d| d.0 == 0).collect();
        assert_eq!(kb_release.len(), 1);
        assert_eq!(
            (kb_release[0].1, kb_release[0].3.clone()),
            (FLAG_RELEASE_ALL, vec![0; 8])
        );
        r.on_report(0, &alt_a);
        assert_eq!(mac.link.take()[0].3, alt_a);
    }

    #[test]
    fn widget_switch_to_the_active_node_releases_every_key() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        let a = join(&r, NodeKind::Agent).unwrap();
        let b = join(&r, NodeKind::Agent).unwrap();
        let w = join(&r, NodeKind::Widget).unwrap();
        r.on_report(0, &key_report(0, 0x36));
        a.link.take();
        r.on_control(
            w.id,
            Control::SwitchTo {
                target: a.id,
                entry: None,
            },
        );
        assert_eq!(r.active(), Some(a.id), "no switch");
        for n in [&a, &b] {
            let got = n.link.take();
            assert_eq!(got.len(), 1);
            assert_eq!((got[0].1, got[0].3.clone()), (FLAG_RELEASE_ALL, vec![0; 8]));
        }
        assert!(w.link.take().is_empty());
        // An agent asking the same does nothing.
        r.on_control(
            a.id,
            Control::SwitchTo {
                target: a.id,
                entry: None,
            },
        );
        assert!(a.link.take().is_empty());
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
                options: EdgeOptions::default(),
                canvas: vec![proto::Placement {
                    node: a.id,
                    x: 0.0,
                    y: 0.0,
                    scale: None,
                }],
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
            real_fixture("0003-046D-400D.0005.bin"),
            real_fixture("0003-046D-406F.0004.bin"),
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
        let ergo_dev = Device::new(device(1, &ergo), None);
        assert!(ergo_dev.is_mouse_report(&[0x02, 0, 1, 0, 0, 0, 0, 0]));
        assert!(!ergo_dev.is_mouse_report(&[0x03, 0, 0, 0, 0]));
    }

    /// Mode A: the receiver's own interfaces with hid-generic.
    #[test]
    fn real_receiver_interfaces() {
        let (Some(i0), Some(i1), Some(i2)) = (
            real_fixture("0003-046D-C52B.0007.nodj.bin"),
            real_fixture("0003-046D-C52B.0008.nodj.bin"),
            real_fixture("0003-046D-C52B.0009.nodj.bin"),
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
        let d1 = Device::new(device(1, &i1), None);
        assert!(d1.is_mouse_report(&[2, 0, 1, 0, 0, 0, 0, 0]));
        assert!(!d1.is_mouse_report(&[3, 0xe9, 0, 0, 0]));
        assert!(!Device::new(device(2, &i2), None).is_mouse_report(&[0x10, 1, 2, 3, 4, 5, 6]));

        // Only the keyboard's LED report is forwarded: none on interface 1,
        // and interface 2's are HID++ (vendor).
        let outs = |d: &[u8]| {
            Device::new(device(0, d), None)
                .forwarded_outputs()
                .map(|r| (r.report_id, r.len))
                .collect::<Vec<_>>()
        };
        assert_eq!(outs(&i0), vec![(None, 1)]);
        assert!(outs(&i1).is_empty());
        assert!(outs(&i2).is_empty());
    }

    fn join_os(r: &Router, os: Os) -> TestNode {
        let id = Uuid::new_v4();
        let link = Arc::new(FakeLink::default());
        let (tx, rx) = unbounded_channel();
        let reg = r
            .register(
                Hello {
                    node_id: id,
                    node_name: format!("{os:?}"),
                    os,
                    kind: NodeKind::Agent,
                },
                link.clone(),
                tx,
            )
            .unwrap();
        TestNode { id, link, rx, reg }
    }

    fn profiles(rx: &std::sync::mpsc::Receiver<Cmd>) -> Vec<Option<OsKind>> {
        rx.try_iter()
            .filter_map(|c| match c {
                Cmd::Profile(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn switching_asks_for_the_keyboard_layout_of_the_agent() {
        let r = Router::new(
            Uuid::new_v4(),
            "hub".into(),
            Options {
                node_options: vec![NodeOptions {
                    index: 3,
                    swap_alt_gui: false,
                    os_profile: Some(OsProfile::Windows),
                }],
                ..Default::default()
            },
        );
        let mac = join_os(&r, Os::MacOs);
        let (tx, rx) = std::sync::mpsc::channel();
        // Attaching tells the thread the active agent's layout at once.
        r.attach_ctl(tx);
        assert_eq!(profiles(&rx), [Some(OsKind::MacOs)]);
        let win = join_os(&r, Os::Windows);
        let linux = join_os(&r, Os::Linux); // index 3: forced to Windows
        let other = join_os(&r, Os::Other);
        force_switch(&r, win.id);
        force_switch(&r, linux.id);
        force_switch(&r, other.id);
        force_switch(&r, mac.id);
        assert_eq!(
            profiles(&rx),
            [
                Some(OsKind::Windows),
                Some(OsKind::Windows),
                None,
                Some(OsKind::MacOs)
            ]
        );
    }

    #[test]
    fn pairing_is_for_widgets_and_devices_reach_widgets_only() {
        let r = router();
        r.add_device(device(0, BOOT_KEYBOARD));
        let mut a = join(&r, NodeKind::Agent).unwrap();
        let mut w = join(&r, NodeKind::Widget).unwrap();
        // No receiver thread yet: the widget is told why.
        r.on_control(
            w.id,
            Control::PairingStart {
                receiver: String::new(),
                timeout_s: 30,
            },
        );
        assert!(w.msgs().iter().any(|m| matches!(
            m,
            Control::PairingEvent(proto::PairingEvent::Failed { reason }) if reason.contains("not open")
        )));

        let (tx, rx) = std::sync::mpsc::channel();
        r.attach_ctl(tx);
        let _ = rx.try_iter().count();
        // An agent cannot pair or unpair.
        r.on_control(
            a.id,
            Control::PairingStart {
                receiver: String::new(),
                timeout_s: 30,
            },
        );
        r.on_control(a.id, Control::Unpair { slot: 3 });
        r.on_control(
            a.id,
            Control::SetPointerSpeed {
                slot: 1,
                value: 300,
            },
        );
        r.on_control(a.id, Control::SetFnSwap { slot: 3, on: true });
        assert!(rx.try_iter().next().is_none());
        r.on_control(
            w.id,
            Control::PairingStart {
                receiver: String::new(),
                timeout_s: 30,
            },
        );
        r.on_control(w.id, Control::PairingStop);
        r.on_control(w.id, Control::Unpair { slot: 3 });
        r.on_control(
            w.id,
            Control::SetPointerSpeed {
                slot: 1,
                value: 300,
            },
        );
        r.on_control(w.id, Control::SetFnSwap { slot: 3, on: true });
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            [
                Cmd::PairingStart { timeout_s: 30 },
                Cmd::PairingStop,
                Cmd::Unpair { slot: 3 },
                Cmd::SetPointerSpeed {
                    slot: 1,
                    value: 300
                },
                Cmd::SetFnSwap { slot: 3, on: true },
            ]
        );

        // Results go to widgets; the agents' device list does not change
        // (mode A: a new keyboard types through the same interfaces).
        let _ = a.msgs();
        let _ = w.msgs();
        let kb = proto::PairedDevice {
            slot: 3,
            name: "MX Keys".into(),
            kind: "keyboard".into(),
            wireless_pid: 0x408a,
            online: true,
            os_profiles: true,
            os_profile: Some(Os::Windows),
            battery: None,
            pointer_speed: None,
            fn_swap: None,
        };
        r.pairing_event(proto::PairingEvent::Paired {
            name: "MX Keys".into(),
            kind: "keyboard".into(),
        });
        r.set_receiver_devices(vec![kb.clone()]);
        r.set_receiver_devices(vec![kb.clone()]); // unchanged: not sent again
        assert_eq!(a.events(), Vec::<&str>::new());
        assert_eq!(w.events(), ["PairingEvent", "ReceiverDevices"]);
        // A widget that connects later gets the list.
        let mut w2 = join(&r, NodeKind::Widget).unwrap();
        assert!(w2
            .msgs()
            .contains(&Control::ReceiverDevices { devices: vec![kb] }));

        // The receiver goes away: an empty list.
        r.detach_receiver();
        assert!(w
            .msgs()
            .contains(&Control::ReceiverDevices { devices: vec![] }));
        assert!(r.hidpp().is_none());
    }

    #[derive(Default)]
    struct FakeWriter(Mutex<Vec<Vec<u8>>>);

    impl ReportWriter for FakeWriter {
        fn write(&self, report: &[u8]) -> bool {
            self.0.lock().unwrap().push(report.to_vec());
            true
        }
    }

    impl FakeWriter {
        fn take(&self) -> Vec<Vec<u8>> {
            std::mem::take(&mut self.0.lock().unwrap())
        }
    }

    #[test]
    fn output_reports_follow_the_active_agent() {
        let r = router();
        let kb = Arc::new(FakeWriter::default());
        let hidpp = Arc::new(FakeWriter::default());
        r.add_device_with_writer(device(0, BOOT_KEYBOARD), Some(kb.clone()));
        r.add_device_with_writer(
            device(2, proto::descriptor::tests::UNIFYING_IFACE2),
            Some(hidpp.clone()),
        );
        let a = join(&r, NodeKind::Agent).unwrap();
        assert_eq!(r.active(), Some(a.id));
        // Activating A, which never sent its LEDs: all off.
        assert_eq!(kb.take(), vec![vec![0]]);
        let b = join(&r, NodeKind::Agent).unwrap();
        let w = join(&r, NodeKind::Widget).unwrap();

        // The active agent's Caps Lock reaches the keyboard; a redundant
        // copy (same seq) and an unchanged state are not written again.
        r.on_output(a.id, 0, 0, &[0x02]);
        r.on_output(a.id, 0, 0, &[0x02]);
        r.on_output(a.id, 0, 1, &[0x02]);
        assert_eq!(kb.take(), vec![vec![0x02]]);
        // A late copy of an older state is dropped.
        r.on_output(a.id, 0, 3, &[0x03]);
        r.on_output(a.id, 0, 2, &[0x00]);
        assert_eq!(kb.take(), vec![vec![0x03]]);

        // Inactive agents, widgets, wrong lengths, unknown devices and
        // HID++ requests are not written.
        r.on_output(b.id, 0, 0, &[0x01]);
        r.on_output(w.id, 0, 0, &[0x02]);
        r.on_output(a.id, 0, 4, &[0x02, 0x00]);
        r.on_output(a.id, 7, 0, &[0x02]);
        r.on_output(a.id, 2, 0, &[0x10, 0xFF, 0x81, 0x00, 0, 0, 0]);
        assert!(kb.take().is_empty());
        assert!(hidpp.take().is_empty());

        // Switching applies the new agent's last state, and back.
        force_switch(&r, b.id);
        assert_eq!(kb.take(), vec![vec![0x01]]);
        force_switch(&r, a.id);
        assert_eq!(kb.take(), vec![vec![0x03]]);

        // A re-plugged keyboard gets the active agent's state.
        r.remove_device(0);
        r.add_device_with_writer(device(0, BOOT_KEYBOARD), Some(kb.clone()));
        assert_eq!(kb.take(), vec![vec![0x03]]);
        assert!(hidpp.take().is_empty());
    }

    #[test]
    fn output_reports_are_rate_limited() {
        let r = router();
        let kb = Arc::new(FakeWriter::default());
        r.add_device_with_writer(device(0, BOOT_KEYBOARD), Some(kb.clone()));
        let a = join(&r, NodeKind::Agent).unwrap();
        kb.take();
        for seq in 0..200u16 {
            r.on_output(a.id, 0, seq, &[(seq % 2) as u8]);
        }
        // The activation's "LEDs off" counted against the same second.
        assert_eq!(kb.take().len(), OUTPUT_RATE as usize - 1);
    }

    #[test]
    fn mouse_reports_are_classified() {
        let d = Device::new(device(1, UNIFYING_IFACE1), None);
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
        assert!(!Device::new(device(0, BOOT_KEYBOARD), None).is_mouse_report(&[0; 8]));
    }
}
