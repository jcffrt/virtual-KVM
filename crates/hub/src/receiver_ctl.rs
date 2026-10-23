//! The receiver's paired devices, over HID++ (Phase 3 C and D).
//!
//! One thread owns the [`Hidpp`] channel: it reads the slots, keeps the
//! keyboards' OS layout (MULTIPLATFORM) on the OS of the active agent, and
//! runs pairing and unpairing for the widgets. Everything reaches it as a
//! [`Cmd`] through a channel, so the router and the reader threads never
//! wait for a HID++ reply.
//!
//! Receiver notifications (Solaar `notifications.py`, `hid-logitech-dj.c`):
//!
//! ```text
//! 10 <slot> 41 <proto> <flags> <pid lo> <pid hi>  device connection;
//!                                                 flags & 0x40: link down
//! 10 <slot> 40 02 ...                             device unpaired
//! 10 ff     4A <b3> <err> ...                     pairing lock: b3 & 1 open,
//!                                                 err 1 timeout, 2 not
//!                                                 supported, 3 too many
//!                                                 devices, 6 sequence timeout
//! ```

use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use proto::{Os, PairedDevice, PairingEvent};
use tracing::{debug, info, warn};

use crate::hidpp::{Error, Hidpp, OsKind, SlotInfo, RECEIVER};

pub const SLOTS: std::ops::RangeInclusive<u8> = 1..=6;

const SUB_DISCONNECT: u8 = 0x40;
const SUB_CONNECT: u8 = 0x41;
const SUB_PAIRING_LOCK: u8 = 0x4A;
const LINK_DOWN: u8 = 0x40;

/// Receiver registers (HID++ 1.0).
const REG_NOTIFICATIONS: u8 = 0x00;
const REG_PAIRING: u8 = 0xB2;
/// Wireless (0x000100) and software present (0x000800) notifications, as
/// Solaar `enable_connection_notifications` and the kernel's DJ driver.
const NOTIFY_FLAGS: [u8; 3] = [0x00, 0x09, 0x00];
const PAIRING_OPEN: u8 = 0x01;
const PAIRING_CLOSE: u8 = 0x02;
const PAIRING_UNPAIR: u8 = 0x03;

/// How long past the requested timeout to wait for the receiver's own
/// "pairing lock closed" before giving up.
const PAIRING_GRACE: Duration = Duration::from_secs(5);

/// What the thread is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// The active agent changed: the layout its keyboard should have
    /// (`None`: leave the keyboards alone).
    Profile(Option<OsKind>),
    /// A receiver notification read from the HID++ interface.
    Notification(Vec<u8>),
    PairingStart {
        timeout_s: u8,
    },
    PairingStop,
    Unpair {
        slot: u8,
    },
}

/// Where the thread's results go (the router).
pub trait Events: Send + 'static {
    fn devices(&self, devices: Vec<PairedDevice>);
    fn pairing(&self, event: PairingEvent);
}

/// A receiver notification the thread wants.
pub fn is_notification(r: &[u8]) -> bool {
    r.len() >= 7
        && matches!(r[0], 0x10 | 0x11)
        && matches!(r[2], SUB_DISCONNECT | SUB_CONNECT | SUB_PAIRING_LOCK)
}

/// Start the thread; it ends when the sender is dropped.
pub fn spawn(h: Arc<Hidpp>, events: impl Events, os_profile: bool) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("receiver".into())
        .spawn(move || Manager::new(h, events, os_profile).run(rx));
    if let Err(e) = spawned {
        warn!("receiver thread not started: {e}");
    }
    tx
}

struct Known {
    info: SlotInfo,
    online: bool,
}

struct Pairing {
    deadline: Instant,
    /// Slot of the device that connected during the session.
    found: Option<u8>,
}

pub struct Manager<E: Events> {
    h: Arc<Hidpp>,
    events: E,
    os_profile: bool,
    slots: BTreeMap<u8, Known>,
    want: Option<OsKind>,
    /// (slot, message) already logged, to warn once.
    warned: HashSet<(u8, String)>,
    pairing: Option<Pairing>,
}

impl<E: Events> Manager<E> {
    pub fn new(h: Arc<Hidpp>, events: E, os_profile: bool) -> Self {
        Self {
            h,
            events,
            os_profile,
            slots: BTreeMap::new(),
            want: None,
            warned: HashSet::new(),
            pairing: None,
        }
    }

    pub fn run(mut self, rx: Receiver<Cmd>) {
        self.start();
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(cmd) => {
                    // A burst of switches: only the last profile matters.
                    let mut cmds = vec![cmd];
                    cmds.extend(rx.try_iter());
                    let last_profile = cmds.iter().rposition(|c| matches!(c, Cmd::Profile(_)));
                    for (i, c) in cmds.into_iter().enumerate() {
                        if matches!(c, Cmd::Profile(_)) && Some(i) != last_profile {
                            continue;
                        }
                        self.handle(c);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.tick(Instant::now());
        }
    }

    /// Enable notifications and read every slot.
    pub fn start(&mut self) {
        let req = crate::hidpp::Request::write_register(RECEIVER, REG_NOTIFICATIONS, &NOTIFY_FLAGS);
        if let Err(e) = self.h.request(&req) {
            warn!("enabling receiver notifications: {e}; pairing events may not arrive");
        }
        for slot in SLOTS {
            self.read_slot(slot);
        }
        self.publish();
    }

    pub fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Profile(want) => {
                self.want = want;
                let slots: Vec<u8> = self.slots.keys().copied().collect();
                for slot in slots {
                    self.apply(slot);
                }
                self.publish();
            }
            Cmd::Notification(r) => self.on_notification(&r),
            Cmd::PairingStart { timeout_s } => self.pairing_start(timeout_s),
            Cmd::PairingStop => {
                if let Err(e) = self.write_pairing(PAIRING_CLOSE, 0, 0) {
                    warn!("closing the pairing lock: {e}");
                }
                if let Some(p) = &mut self.pairing {
                    // The receiver confirms with a 0x4A notification.
                    p.deadline = p.deadline.min(Instant::now() + Duration::from_secs(2));
                }
            }
            Cmd::Unpair { slot } => self.unpair(slot),
        }
    }

    /// End a pairing session the receiver never closed.
    pub fn tick(&mut self, now: Instant) {
        if self.pairing.as_ref().is_some_and(|p| now >= p.deadline) {
            self.pairing = None;
            info!("pairing: no answer from the receiver; timed out");
            self.events.pairing(PairingEvent::TimedOut);
        }
    }

    /// Survey `slot` again; false if it is empty.
    fn read_slot(&mut self, slot: u8) -> bool {
        match self.h.survey(slot) {
            Ok(Some(info)) => {
                let online = info.protocol.is_some();
                debug!(slot, name = ?info.pairing.name, online, "receiver slot");
                self.slots.insert(slot, Known { info, online });
                self.apply(slot);
                true
            }
            Ok(None) => {
                self.slots.remove(&slot);
                false
            }
            Err(e) => {
                self.warn_once(slot, format!("reading receiver slot {slot}: {e}"));
                false
            }
        }
    }

    /// Put the layout of `slot` on `want`, if it has MULTIPLATFORM.
    fn apply(&mut self, slot: u8) {
        let Some(want) = self.want.filter(|_| self.os_profile) else {
            return;
        };
        let Some(k) = self.slots.get(&slot) else {
            return;
        };
        let Some((index, mp)) = k.info.multiplatform.filter(|(_, m)| m.settable()) else {
            return;
        };
        if !k.online {
            return; // applied when it connects
        }
        let Some(platform) = k.info.platform_for(want) else {
            self.warn_once(
                slot,
                format!("slot {slot}: no MULTIPLATFORM layout for {want:?}"),
            );
            return;
        };
        if mp.current == platform {
            return;
        }
        let name = k.info.pairing.name.clone().unwrap_or_default();
        match self.h.set_platform(slot, index, platform) {
            Ok(()) => {
                info!(slot, %name, ?want, platform, "keyboard layout set");
                if let Some(k) = self.slots.get_mut(&slot) {
                    if let Some((_, m)) = &mut k.info.multiplatform {
                        m.current = platform;
                    }
                }
                self.warned.retain(|(s, _)| *s != slot);
            }
            Err(e) => {
                if e.is_unreachable() {
                    if let Some(k) = self.slots.get_mut(&slot) {
                        k.online = false;
                    }
                }
                self.warn_once(
                    slot,
                    format!("setting the layout of slot {slot} ({name}): {e}"),
                );
            }
        }
    }

    fn on_notification(&mut self, r: &[u8]) {
        let slot = r[1];
        match r[2] {
            SUB_CONNECT if SLOTS.contains(&slot) => {
                let up = r[4] & LINK_DOWN == 0;
                debug!(slot, up, "device connection notification");
                let new = !self.slots.contains_key(&slot);
                if !up {
                    if let Some(k) = self.slots.get_mut(&slot) {
                        if k.online {
                            k.online = false;
                            self.publish();
                        }
                    }
                    return;
                }
                let was_online = self.slots.get(&slot).is_some_and(|k| k.online);
                if was_online && !new {
                    return;
                }
                if !self.read_slot(slot) {
                    return;
                }
                if let (Some(p), true) = (&mut self.pairing, new) {
                    p.found = Some(slot);
                    let name = self.slot_name(slot);
                    info!(slot, %name, "pairing: device found");
                    self.events.pairing(PairingEvent::DeviceFound { name });
                }
                self.publish();
            }
            SUB_DISCONNECT if SLOTS.contains(&slot) && r[3] == 0x02 => {
                if self.slots.remove(&slot).is_some() {
                    info!(slot, "device unpaired");
                    self.publish();
                }
            }
            SUB_PAIRING_LOCK if slot == RECEIVER => {
                let open = r[3] & 0x01 != 0;
                let err = r[4];
                debug!(open, err, "pairing lock notification");
                if open {
                    return;
                }
                let Some(p) = self.pairing.take() else {
                    return;
                };
                let event = match (err, p.found) {
                    (0, Some(slot)) => {
                        let k = &self.slots[&slot];
                        PairingEvent::Paired {
                            name: self.slot_name(slot),
                            kind: k.info.pairing.kind_name().into(),
                        }
                    }
                    (0, None) => PairingEvent::TimedOut,
                    (e, _) => PairingEvent::Failed {
                        reason: pairing_error(e).into(),
                    },
                };
                info!(?event, "pairing ended");
                self.events.pairing(event);
                self.publish();
            }
            _ => {}
        }
    }

    fn pairing_start(&mut self, timeout_s: u8) {
        let timeout_s = timeout_s.clamp(1, 60);
        match self.write_pairing(PAIRING_OPEN, 0, timeout_s) {
            Ok(()) => {
                info!(timeout_s, "pairing lock open");
                self.pairing = Some(Pairing {
                    deadline: Instant::now()
                        + Duration::from_secs(timeout_s as u64)
                        + PAIRING_GRACE,
                    found: None,
                });
                self.events.pairing(PairingEvent::Started { timeout_s });
            }
            Err(e) => {
                warn!("opening the pairing lock: {e}");
                self.events.pairing(PairingEvent::Failed {
                    reason: format!("the receiver did not open pairing: {e}"),
                });
            }
        }
    }

    fn unpair(&mut self, slot: u8) {
        if !SLOTS.contains(&slot) {
            return;
        }
        match self.write_pairing(PAIRING_UNPAIR, slot, 0) {
            Ok(()) => {
                info!(slot, "device unpaired by a widget");
                self.slots.remove(&slot);
                self.publish();
            }
            Err(e) => {
                warn!(slot, "unpairing: {e}");
                self.events.pairing(PairingEvent::Failed {
                    reason: format!("unpairing slot {slot}: {e}"),
                });
            }
        }
    }

    fn write_pairing(&self, action: u8, slot: u8, timeout_s: u8) -> Result<(), Error> {
        let req = crate::hidpp::Request::write_register(
            RECEIVER,
            REG_PAIRING,
            &[action, slot, timeout_s],
        );
        self.h.request(&req).map(drop)
    }

    fn slot_name(&self, slot: u8) -> String {
        self.slots
            .get(&slot)
            .and_then(|k| k.info.pairing.name.clone())
            .unwrap_or_else(|| format!("slot {slot}"))
    }

    fn warn_once(&mut self, slot: u8, msg: String) {
        if self.warned.insert((slot, msg.clone())) {
            warn!("{msg}");
        }
    }

    pub fn devices(&self) -> Vec<PairedDevice> {
        self.slots
            .iter()
            .map(|(&slot, k)| PairedDevice {
                slot,
                name: k
                    .info
                    .pairing
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{:04x}", k.info.pairing.wireless_pid)),
                kind: k.info.pairing.kind_name().into(),
                wireless_pid: k.info.pairing.wireless_pid,
                online: k.online,
                os_profiles: k.info.multiplatform.is_some_and(|(_, m)| m.settable()),
                os_profile: k.info.current_os().map(|o| match o {
                    OsKind::Windows => Os::Windows,
                    OsKind::MacOs => Os::MacOs,
                    OsKind::Linux => Os::Linux,
                }),
            })
            .collect()
    }

    fn publish(&self) {
        self.events.devices(self.devices());
    }
}

fn pairing_error(code: u8) -> &'static str {
    match code {
        0x01 => "no device turned up in time",
        0x02 => "device not supported by this receiver",
        0x03 => "the receiver has no free slot",
        0x06 => "pairing sequence timed out",
        _ => "pairing failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hidpp::tests::{hex, Scripted};
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<PairedDevice>>>);

    impl Events for Rec {
        fn devices(&self, d: Vec<PairedDevice>) {
            *self.1.lock().unwrap() = d;
        }
        fn pairing(&self, e: PairingEvent) {
            self.0.lock().unwrap().push(format!("{e:?}"));
        }
    }

    /// A trace recorded with `--hidpp-trace`: `<ms> > <hex>` / `<ms> < <hex>`.
    fn trace(name: &str) -> Vec<(Vec<u8>, Vec<Vec<u8>>)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/hidpp")
            .join(name);
        let text = std::fs::read_to_string(path).unwrap();
        let mut out: Vec<(Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(_t), Some(dir)) = (it.next(), it.next()) else {
                continue;
            };
            let bytes: String = it.collect::<Vec<_>>().join(" ");
            match dir {
                ">" => out.push((hex(&bytes), Vec::new())),
                "<" => {
                    if let Some(last) = out.last_mut() {
                        last.1.push(hex(&bytes));
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn long(s: &str) -> Vec<u8> {
        let mut v = hex(s);
        v.resize(crate::hidpp::LONG_LEN, 0);
        v
    }

    /// The real receiver of the Pi, from `fixtures/hidpp/hidpp-list.txt`,
    /// preceded by the notification enable.
    fn real_receiver(extra: Vec<(Vec<u8>, Vec<Vec<u8>>)>) -> Arc<Scripted> {
        let mut script = vec![(
            hex("10 ff 80 00 00 09 00"),
            vec![hex("10 ff 80 00 00 00 00")],
        )];
        script.extend(trace("hidpp-list.txt"));
        script.extend(extra);
        Scripted::new(script)
    }

    #[test]
    fn real_slots_become_paired_devices() {
        let t = real_receiver(vec![]);
        let rec = Rec::default();
        let mut m = Manager::new(t.attach(), rec.clone(), true);
        m.start();
        let d = rec.1.lock().unwrap().clone();
        let summary: Vec<_> = d
            .iter()
            .map(|d| {
                (
                    d.slot,
                    d.name.as_str(),
                    d.kind.as_str(),
                    d.online,
                    d.os_profiles,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (1, "MX Ergo", "trackball", true, false),
                (2, "K230", "keyboard", false, false),
                (3, "MX Keys", "keyboard", true, true),
            ]
        );
        assert_eq!(d[2].wireless_pid, 0x408a);
        assert_eq!(d[2].os_profile, Some(Os::MacOs));
    }

    #[test]
    fn switching_sets_the_layout_once_and_maps_linux_to_windows() {
        // Recorded: set platform 0 (Windows) on slot 3, MULTIPLATFORM at 0x10.
        let set_windows = trace("hidpp-set-windows.txt")
            .into_iter()
            .find(|(req, _)| req[1..4] == [0x03, 0x10, 0x3c])
            .expect("the set request is in the trace");
        let set_macos = trace("hidpp-set-macos.txt")
            .into_iter()
            .find(|(req, _)| req[1..4] == [0x03, 0x10, 0x3c])
            .expect("the set request is in the trace");
        let t = real_receiver(vec![set_windows.clone(), set_macos.clone()]);
        let rec = Rec::default();
        let mut m = Manager::new(t.attach(), rec.clone(), true);
        m.start();
        // Linux has no layout of its own on the MX Keys: Windows.
        m.handle(Cmd::Profile(Some(OsKind::Linux)));
        assert_eq!(rec.1.lock().unwrap()[2].os_profile, Some(Os::Windows));
        // Already on the Windows layout: nothing is written.
        m.handle(Cmd::Profile(Some(OsKind::Windows)));
        m.handle(Cmd::Profile(None));
        m.handle(Cmd::Profile(Some(OsKind::MacOs)));
        assert_eq!(rec.1.lock().unwrap()[2].os_profile, Some(Os::MacOs));
        let sets: Vec<_> = t
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w[1..4] == [0x03, 0x10, 0x3c])
            .cloned()
            .collect();
        assert_eq!(sets, [set_windows.0, set_macos.0]);
    }

    #[test]
    fn profiles_off_writes_nothing() {
        let t = real_receiver(vec![]);
        let mut m = Manager::new(t.attach(), Rec::default(), false);
        m.start();
        let before = t.writes.lock().unwrap().len();
        m.handle(Cmd::Profile(Some(OsKind::Windows)));
        assert_eq!(t.writes.lock().unwrap().len(), before);
    }

    #[test]
    fn pairing_session() {
        let t = Scripted::new(vec![
            (
                hex("10 ff 80 00 00 09 00"),
                vec![hex("10 ff 80 00 00 00 00")],
            ),
            // Empty receiver.
            (
                hex("10 ff 83 b5 20 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 21 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 22 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 23 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 24 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 25 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            // Open for 30 s.
            (
                hex("10 ff 80 b2 01 00 1e"),
                vec![hex("10 ff 80 b2 00 00 00")],
            ),
            // The new keyboard in slot 1: a HID++ 1.0 device.
            (
                hex("10 ff 83 b5 20 00 00"),
                vec![long("11 ff 83 b5 20 08 14 40 0d 04 02 01 01")],
            ),
            (hex("10 ff 83 b5 30 00 00"), vec![long("11 ff 83 b5 30")]),
            (
                hex("10 ff 83 b5 40 00 00"),
                vec![long("11 ff 83 b5 40 04 4b 32 33 30")],
            ),
            (
                hex("10 01 00 1c 00 00 5a"),
                vec![hex("10 01 8f 00 1c 01 00")],
            ),
            // Unpair it.
            (
                hex("10 ff 80 b2 03 01 00"),
                vec![hex("10 ff 80 b2 00 00 00")],
            ),
            // A second session the user cancels.
            (
                hex("10 ff 80 b2 01 00 05"),
                vec![hex("10 ff 80 b2 00 00 00")],
            ),
            (
                hex("10 ff 80 b2 02 00 00"),
                vec![hex("10 ff 80 b2 00 00 00")],
            ),
        ]);
        let rec = Rec::default();
        let mut m = Manager::new(t.attach(), rec.clone(), true);
        m.start();
        assert!(rec.1.lock().unwrap().is_empty());

        m.handle(Cmd::PairingStart { timeout_s: 30 });
        m.handle(Cmd::Notification(hex("10 ff 4a 01 00 00 00")));
        m.handle(Cmd::Notification(hex("10 01 41 04 01 0d 40")));
        m.handle(Cmd::Notification(hex("10 ff 4a 00 00 00 00")));
        assert_eq!(
            *rec.0.lock().unwrap(),
            [
                "Started { timeout_s: 30 }",
                "DeviceFound { name: \"K230\" }",
                "Paired { name: \"K230\", kind: \"keyboard\" }",
            ]
        );
        assert_eq!(rec.1.lock().unwrap()[0].name, "K230");

        m.handle(Cmd::Unpair { slot: 1 });
        assert!(rec.1.lock().unwrap().is_empty());

        rec.0.lock().unwrap().clear();
        m.handle(Cmd::PairingStart { timeout_s: 5 });
        m.handle(Cmd::PairingStop);
        m.handle(Cmd::Notification(hex("10 ff 4a 00 00 00 00")));
        // The receiver reported an error in another session: not ours.
        m.handle(Cmd::Notification(hex("10 ff 4a 00 03 00 00")));
        assert_eq!(
            *rec.0.lock().unwrap(),
            ["Started { timeout_s: 5 }", "TimedOut"]
        );
    }

    #[test]
    fn pairing_errors_and_silence() {
        let t = Scripted::new(vec![
            (
                hex("10 ff 80 b2 01 00 0a"),
                vec![hex("10 ff 80 b2 00 00 00")],
            ),
            (
                hex("10 ff 80 b2 01 00 0a"),
                vec![hex("10 ff 80 b2 00 00 00")],
            ),
        ]);
        let rec = Rec::default();
        let mut m = Manager::new(t.attach(), rec.clone(), true);
        m.handle(Cmd::PairingStart { timeout_s: 10 });
        m.handle(Cmd::Notification(hex("10 ff 4a 00 03 00 00")));
        m.handle(Cmd::PairingStart { timeout_s: 10 });
        m.tick(Instant::now() + Duration::from_secs(16));
        assert_eq!(
            *rec.0.lock().unwrap(),
            [
                "Started { timeout_s: 10 }",
                "Failed { reason: \"the receiver has no free slot\" }",
                "Started { timeout_s: 10 }",
                "TimedOut",
            ]
        );
        assert!(is_notification(&hex("10 ff 4a 00 03 00 00")));
        assert!(!is_notification(&hex("10 03 10 00 00 00 03")));
    }

    #[test]
    fn a_sleeping_keyboard_gets_the_layout_when_it_connects() {
        // K230-like slot 2 is unreachable at start; MX Keys-like data when
        // it connects later.
        let t = Scripted::new(vec![
            (
                hex("10 ff 80 00 00 09 00"),
                vec![hex("10 ff 80 00 00 00 00")],
            ),
            (
                hex("10 ff 83 b5 20 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 21 00 00"),
                vec![long("11 ff 83 b5 21 0a 14 40 8a 04 02 01 07")],
            ),
            (hex("10 ff 83 b5 31 00 00"), vec![long("11 ff 83 b5 31")]),
            (
                hex("10 ff 83 b5 41 00 00"),
                vec![long("11 ff 83 b5 41 02 4b 42")],
            ),
            (
                hex("10 02 00 1c 00 00 5a"),
                vec![hex("10 02 8f 00 1c 09 00")],
            ),
            (
                hex("10 ff 83 b5 22 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 23 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 24 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            (
                hex("10 ff 83 b5 25 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
            // It wakes up.
            (
                hex("10 ff 83 b5 21 00 00"),
                vec![long("11 ff 83 b5 21 0a 14 40 8a 04 02 01 07")],
            ),
            (hex("10 ff 83 b5 31 00 00"), vec![long("11 ff 83 b5 31")]),
            (
                hex("10 ff 83 b5 41 00 00"),
                vec![long("11 ff 83 b5 41 02 4b 42")],
            ),
            (
                hex("10 02 00 1c 00 00 5a"),
                vec![hex("10 02 00 1c 04 05 5a")],
            ),
            (hex("10 02 00 0c 18 15 00"), vec![long("11 02 00 0c 00")]),
            (hex("10 02 00 0c 45 31 00"), vec![long("11 02 00 0c 10")]),
            (
                hex("10 02 10 0c 00 00 00"),
                vec![long("11 02 10 0c 03 00 02 04 03 00 01")],
            ),
            (
                hex("10 02 10 1c 00 00 00"),
                vec![long("11 02 10 1c 00 00 01 00")],
            ),
            (
                hex("10 02 10 1c 01 00 00"),
                vec![long("11 02 10 1c 01 01 20 00")],
            ),
            (hex("10 02 00 0c 45 30 00"), vec![long("11 02 00 0c 00")]),
            (hex("10 02 10 3c ff 00 00"), vec![long("11 02 10 3c ff 00")]),
        ]);
        let rec = Rec::default();
        let mut m = Manager::new(t.attach(), rec.clone(), true);
        m.start();
        m.handle(Cmd::Profile(Some(OsKind::Windows)));
        assert!(!rec.1.lock().unwrap()[0].online);
        m.handle(Cmd::Notification(hex("10 02 41 04 01 8a 40")));
        let d = rec.1.lock().unwrap()[0].clone();
        assert!(d.online);
        assert_eq!(d.os_profile, Some(Os::Windows));
        // Link down: offline again, nothing requested.
        m.handle(Cmd::Notification(hex("10 02 41 04 41 8a 40")));
        assert!(!rec.1.lock().unwrap()[0].online);
    }
}
