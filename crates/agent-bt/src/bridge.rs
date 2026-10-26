//! The bridge task: the hub's devices merged into one descriptor, the
//! bonded PC, and the Bluetooth stack (ADR 0033). The `HidSink` in
//! `sink.rs` feeds it commands; the stack feeds it events.

use std::time::{Duration, Instant};

use agent_core::{ControlSender, OutputReport, OutputSender};
use bridge_core::host::{descriptor_hash, Host};
use bridge_core::merge::{MergeError, Merged};
use proto::{BridgeStatus, Control, HidDevice};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info, warn};

use crate::stack::{HidStack, StackEvent};
use crate::store::Store;

/// From the sink (the hub connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// The devices of the receivers exposed to this bridge (the hub sends
    /// only those), once the list has settled.
    Devices(Vec<HidDevice>),
    Input(u8, Vec<u8>),
    /// Open pairing for `secs` (0: close).
    Discoverable(u8),
    Forget,
    /// A session with the hub started: send the status again.
    Hello,
}

pub struct Bridge<S> {
    stack: S,
    host: Host,
    merged: Option<Merged>,
    store: Option<Store>,
    to_hub: ControlSender,
    outputs: OutputSender,
    sent: Option<BridgeStatus>,
}

impl<S: HidStack> Bridge<S> {
    pub fn new(
        stack: S,
        store: Option<Store>,
        to_hub: ControlSender,
        outputs: OutputSender,
    ) -> anyhow::Result<Self> {
        let saved = match &store {
            Some(s) => s.load()?,
            None => Default::default(),
        };
        if let Some((addr, name)) = &saved.bonded {
            info!(%addr, name, "bonded PC");
        }
        Ok(Self {
            stack,
            host: Host::new(saved),
            merged: None,
            store,
            to_hub,
            outputs,
            sent: None,
        })
    }

    pub fn status(&self, now: Instant) -> BridgeStatus {
        self.host.status(now)
    }

    pub async fn on_cmd(&mut self, cmd: Cmd, now: Instant) {
        match cmd {
            Cmd::Input(dev_id, report) => {
                if !self.host.can_send() {
                    return;
                }
                let Some(r) = self.merged.as_ref().and_then(|m| m.input(dev_id, &report)) else {
                    return; // HID++ or a device not in the descriptor
                };
                if let Err(e) = self.stack.send(&r).await {
                    debug!("input report not sent: {e:#}");
                }
                return; // hot path: nothing else changes
            }
            Cmd::Devices(devices) => self.set_devices(&devices).await,
            Cmd::Discoverable(secs) => {
                info!(secs, "pairing requested");
                if let Err(e) = self.host.discoverable(&mut self.stack, secs, now).await {
                    warn!("making the bridge discoverable: {e:#}");
                }
            }
            Cmd::Forget => {
                info!("forgetting the bonded PC");
                if let Err(e) = self.host.forget(&mut self.stack).await {
                    warn!("forgetting the bonded PC: {e:#}");
                }
            }
            Cmd::Hello => self.sent = None,
        }
        self.after(now);
    }

    async fn set_devices(&mut self, devices: &[HidDevice]) {
        if devices.is_empty() {
            // No receiver exposed to us: keep the published descriptor so
            // the PC stays bonded; nothing arrives to send anyway.
            info!("no receiver exposed to this bridge");
            self.merged = None;
            return;
        }
        let merged = match Merged::build(devices) {
            Ok(m) => m,
            Err(MergeError::Empty) => {
                warn!("the exposed receivers have only vendor interfaces: nothing to present");
                self.merged = None;
                return;
            }
            Err(e) => {
                warn!("merging the report descriptors: {e}");
                self.merged = None;
                return;
            }
        };
        if self.merged.as_ref().map(|m| &m.descriptor) != Some(&merged.descriptor) {
            info!(
                devices = devices.len(),
                reports = merged.report_count(),
                bytes = merged.descriptor.len(),
                "merged report descriptor"
            );
            if let Err(e) = self.stack.set_descriptor(&merged.descriptor).await {
                warn!("publishing the descriptor: {e:#}");
            }
            self.host
                .set_descriptor(descriptor_hash(&merged.descriptor));
        }
        self.merged = Some(merged);
    }

    pub async fn on_event(&mut self, ev: StackEvent, now: Instant) {
        match ev {
            StackEvent::Paired { addr, name } => {
                info!(%addr, name, "PC paired");
                if let Err(e) = self.host.on_paired(&mut self.stack, addr, name).await {
                    warn!("after pairing: {e:#}");
                }
            }
            StackEvent::Connected(addr) => {
                if self.host.on_connected(addr) {
                    info!(%addr, "PC connected");
                } else {
                    warn!(%addr, "connection from a PC that is not the bonded one; closed");
                    let _ = self.stack.disconnect().await;
                }
            }
            StackEvent::Disconnected => {
                info!("PC disconnected");
                self.host.on_disconnected(now);
            }
            StackEvent::Unplugged => {
                info!("the PC removed the device");
                if let Err(e) = self.host.on_unplugged(&mut self.stack).await {
                    warn!("forgetting the PC: {e:#}");
                }
            }
            StackEvent::Output(report) => {
                match self.merged.as_ref().and_then(|m| m.output(&report)) {
                    Some((dev_id, report)) => {
                        let _ = self.outputs.send(OutputReport { dev_id, report });
                    }
                    None => debug!(?report, "output report for no device"),
                }
            }
        }
        self.after(now);
    }

    pub async fn tick(&mut self, now: Instant) {
        self.host.tick(&mut self.stack, now).await;
        self.after(now);
    }

    /// Save the bond if it changed; tell the hub if the status did.
    fn after(&mut self, now: Instant) {
        if self.host.take_dirty() {
            if let Some(s) = &self.store {
                if let Err(e) = s.save(self.host.saved()) {
                    warn!("{e:#}");
                }
            }
        }
        let status = self.host.status(now);
        if is_news(self.sent.as_ref(), &status) {
            if status.repair_needed && !self.sent.as_ref().is_some_and(|s| s.repair_needed) {
                warn!(
                    "the report descriptor changed since the PC paired: forget this device on \
                     the PC and pair it again; input is held back until then"
                );
            }
            let _ = self.to_hub.send(Control::BtStatus {
                status: status.clone(),
            });
            self.sent = Some(status);
        }
    }

    /// Serve until both channels close.
    pub async fn run(
        mut self,
        mut cmds: UnboundedReceiver<Cmd>,
        mut events: UnboundedReceiver<StackEvent>,
    ) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                c = cmds.recv() => match c {
                    Some(c) => self.on_cmd(c, Instant::now()).await,
                    None => return,
                },
                e = events.recv() => match e {
                    Some(e) => self.on_event(e, Instant::now()).await,
                    None => return,
                },
                _ = tick.tick() => self.tick(Instant::now()).await,
            }
        }
    }
}

/// A change worth telling the hub: anything but the discoverable
/// countdown ticking down (the widget counts down itself).
fn is_news(sent: Option<&BridgeStatus>, now: &BridgeStatus) -> bool {
    match sent {
        None => true,
        Some(s) => {
            s.bonded != now.bonded
                || s.connected != now.connected
                || s.repair_needed != now.repair_needed
                || s.discoverable_secs.is_some() != now.discoverable_secs.is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::host::{BtAddr, Radio};
    use proto::descriptor::DescriptorInfo;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeStack(Arc<Mutex<Vec<String>>>);

    impl FakeStack {
        fn calls(&self) -> Vec<String> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
        fn log(&self, s: String) {
            self.0.lock().unwrap().push(s);
        }
    }

    impl Radio for FakeStack {
        async fn set_discoverable(&mut self, secs: Option<u32>) -> anyhow::Result<()> {
            self.log(format!("discoverable {secs:?}"));
            Ok(())
        }
        async fn remove(&mut self, addr: BtAddr) -> anyhow::Result<()> {
            self.log(format!("remove {addr}"));
            Ok(())
        }
        async fn connect(&mut self, addr: BtAddr) -> anyhow::Result<()> {
            self.log(format!("connect {addr}"));
            Ok(())
        }
    }

    impl HidStack for FakeStack {
        async fn set_descriptor(&mut self, d: &[u8]) -> anyhow::Result<()> {
            self.log(format!("descriptor {} bytes", d.len()));
            Ok(())
        }
        async fn send(&mut self, report: &[u8]) -> anyhow::Result<()> {
            self.log(format!("send {report:02x?}"));
            Ok(())
        }
        async fn disconnect(&mut self) -> anyhow::Result<()> {
            self.log("disconnect".into());
            Ok(())
        }
    }

    const PC: BtAddr = BtAddr([0xAA, 0xBB, 0xCC, 0, 0x11, 0x22]);

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/unifying-c52b")
                .join(name),
        )
        .unwrap()
    }

    fn dev(id: u8, file: &str) -> HidDevice {
        let desc = fixture(file);
        HidDevice {
            id,
            name: "Logitech USB Receiver".into(),
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            interface: id % 16,
            uses_report_ids: DescriptorInfo::parse(&desc).uses_report_ids,
            receiver: id / 16,
            report_descriptor: desc,
        }
    }

    fn c52b() -> Vec<HidDevice> {
        vec![
            dev(0, "0003-046D-C52B.0007.nodj.bin"),
            dev(1, "0003-046D-C52B.0008.nodj.bin"),
            dev(2, "0003-046D-C52B.0009.nodj.bin"),
        ]
    }

    struct Rig {
        b: Bridge<FakeStack>,
        stack: FakeStack,
        hub: UnboundedReceiver<Control>,
        leds: UnboundedReceiver<OutputReport>,
    }

    fn rig(store: Option<Store>) -> Rig {
        let stack = FakeStack::default();
        let (hub_tx, hub) = tokio::sync::mpsc::unbounded_channel();
        let (out_tx, leds) = tokio::sync::mpsc::unbounded_channel();
        Rig {
            b: Bridge::new(stack.clone(), store, hub_tx, out_tx).unwrap(),
            stack,
            hub,
            leds,
        }
    }

    fn statuses(rx: &mut UnboundedReceiver<Control>) -> Vec<BridgeStatus> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|m| match m {
                Control::BtStatus { status } => Some(status),
                _ => None,
            })
            .collect()
    }

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn pair_connect_type_and_get_the_leds_back() {
        block(async {
            let mut r = rig(None);
            let t = Instant::now();
            r.b.on_cmd(Cmd::Devices(c52b()), t).await;
            let calls = r.stack.calls();
            assert_eq!(calls.len(), 1);
            assert!(calls[0].starts_with("descriptor "), "{calls:?}");
            assert_eq!(statuses(&mut r.hub), [BridgeStatus::default()]);

            // Not bonded: input goes nowhere.
            r.b.on_cmd(Cmd::Input(0, vec![0, 0, 4, 0, 0, 0, 0, 0]), t)
                .await;
            assert!(r.stack.calls().is_empty());

            r.b.on_cmd(Cmd::Discoverable(60), t).await;
            assert_eq!(statuses(&mut r.hub)[0].discoverable_secs, Some(60));
            r.b.on_event(
                StackEvent::Paired {
                    addr: PC,
                    name: "GAMER".into(),
                },
                t,
            )
            .await;
            r.b.on_event(StackEvent::Connected(PC), t).await;
            let st = statuses(&mut r.hub);
            let last = st.last().unwrap();
            assert_eq!(last.bonded.as_deref(), Some("GAMER"));
            assert!(last.connected && last.discoverable_secs.is_none());
            r.stack.calls();

            // "a" on the keyboard: merged ID first, payload as it was.
            r.b.on_cmd(Cmd::Input(0, vec![0, 0, 4, 0, 0, 0, 0, 0]), t)
                .await;
            // HID++ has no place in the descriptor.
            r.b.on_cmd(Cmd::Input(2, vec![0x10, 0xFF, 0, 0, 0, 0, 0]), t)
                .await;
            assert_eq!(
                r.stack.calls(),
                ["send [01, 00, 00, 04, 00, 00, 00, 00, 00]"]
            );

            // Caps Lock LED from the PC, back to the keyboard interface.
            r.b.on_event(StackEvent::Output(vec![1, 0x02]), t).await;
            let o = r.leds.try_recv().unwrap();
            assert_eq!((o.dev_id, o.report), (0, vec![0x02]));

            // Hub reconnected: the status again, unchanged.
            assert!(statuses(&mut r.hub).is_empty());
            r.b.on_cmd(Cmd::Hello, t).await;
            assert_eq!(statuses(&mut r.hub).len(), 1);
        });
    }

    #[test]
    fn a_changed_receiver_holds_input_until_paired_again() {
        block(async {
            let mut r = rig(None);
            let t = Instant::now();
            r.b.on_cmd(Cmd::Devices(c52b()[..2].to_vec()), t).await;
            r.b.on_event(
                StackEvent::Paired {
                    addr: PC,
                    name: String::new(),
                },
                t,
            )
            .await;
            r.b.on_event(StackEvent::Connected(PC), t).await;
            statuses(&mut r.hub);
            r.stack.calls();

            // The same list again: no new descriptor.
            r.b.on_cmd(Cmd::Devices(c52b()), t).await; // + HID++, left out
            assert!(r.stack.calls().is_empty());

            // A second receiver exposed to the bridge: a new descriptor.
            let mut more = c52b();
            more.push(dev(16, "0003-046D-C52B.0007.nodj.bin"));
            r.b.on_cmd(Cmd::Devices(more), t).await;
            assert_eq!(r.stack.calls().len(), 1);
            assert!(statuses(&mut r.hub)[0].repair_needed);
            r.b.on_cmd(Cmd::Input(0, vec![0; 8]), t).await;
            assert!(r.stack.calls().is_empty(), "held back");

            // Paired again: input flows.
            r.b.on_event(
                StackEvent::Paired {
                    addr: PC,
                    name: String::new(),
                },
                t,
            )
            .await;
            assert!(!statuses(&mut r.hub).last().unwrap().repair_needed);
            r.stack.calls();
            r.b.on_cmd(Cmd::Input(16, vec![0; 8]), t).await;
            assert_eq!(r.stack.calls().len(), 1);

            // Nothing exposed any more: the descriptor stays published.
            r.b.on_cmd(Cmd::Devices(Vec::new()), t).await;
            r.b.on_cmd(Cmd::Input(0, vec![0; 8]), t).await;
            assert!(r.stack.calls().is_empty());
        });
    }

    #[test]
    fn strangers_are_refused_and_the_bond_is_saved() {
        block(async {
            let path = std::env::temp_dir()
                .join(format!("rawkvm-bridge-{}", std::process::id()))
                .join("agent-bt.toml");
            let _ = std::fs::remove_file(&path);
            let mut r = rig(Some(Store::new(&path)));
            let t = Instant::now();
            r.b.on_cmd(Cmd::Devices(c52b()), t).await;
            r.b.on_event(StackEvent::Connected(PC), t).await;
            assert!(r.stack.calls().contains(&"disconnect".to_string()));
            r.b.on_event(
                StackEvent::Paired {
                    addr: PC,
                    name: "GAMER".into(),
                },
                t,
            )
            .await;
            assert!(std::fs::read_to_string(&path).unwrap().contains("GAMER"));

            // A new process with the same store knows the PC and tries to
            // reach it.
            let mut r2 = rig(Some(Store::new(&path)));
            r2.b.on_cmd(Cmd::Devices(c52b()), t).await;
            assert!(!r2.b.status(t).repair_needed);
            r2.b.tick(t).await;
            assert!(r2
                .stack
                .calls()
                .contains(&"connect AA:BB:CC:00:11:22".to_string()));

            // The PC removes the device: forgotten, on disk too.
            r2.b.on_event(StackEvent::Unplugged, t).await;
            assert!(!std::fs::read_to_string(&path).unwrap().contains("GAMER"));
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        });
    }
}
