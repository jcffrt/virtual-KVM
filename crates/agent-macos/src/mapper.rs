//! Raw hub reports -> Karabiner reports.
//!
//! macOS gets its virtual devices from Karabiner, whose report descriptors
//! are fixed, so the receiver's reports cannot be forwarded byte for byte.
//! Instead each report is decoded with its own descriptor and the active
//! usages are copied 1:1 (same page, same usage) into Karabiner's reports.
//! Nothing is reinterpreted: macOS applies its own modifier handling on top
//! of a genuine HID keyboard. See ADR 0005.
//!
//! | Hub usage | Karabiner |
//! |---|---|
//! | Keyboard 0xE0..=0xE7 | `keyboard_input.modifiers` (same bit order) |
//! | Keyboard 0x04..=0xDF, 0xE8..=0xFF | `keyboard_input.keys` |
//! | Consumer (array, or absolute variable) <= 0x300 | `consumer_input.keys` |
//! | Generic Desktop system controls 0x81..=0xB7 | `generic_desktop_input.keys` |
//! | Button 1..=32 | `pointing_input.buttons` |
//! | GD X / Y / Wheel, Consumer AC Pan (relative) | `pointing_input` x / y / vertical / horizontal |
//! | Vendor pages (HID++) and anything else | ignored |

use std::collections::BTreeMap;

use proto::descriptor::{DescriptorInfo, Value};
use proto::HidDevice;

use crate::karabiner::{KeyboardInput, KeysReport, PointingInput, KEYS};

const PAGE_GENERIC_DESKTOP: u16 = 0x01;
const PAGE_KEYBOARD: u16 = 0x07;
const PAGE_BUTTON: u16 = 0x09;
const PAGE_CONSUMER: u16 = 0x0C;

const GD_X: u16 = 0x30;
const GD_Y: u16 = 0x31;
const GD_WHEEL: u16 = 0x38;
const CONSUMER_AC_PAN: u16 = 0x238;
/// Karabiner's consumer collection accepts usages up to 0x300.
const CONSUMER_MAX: u16 = 0x300;

/// Most i8 steps one hub report may be split into. A 12-bit delta (2047)
/// needs 17.
const MAX_SPLIT: usize = 20;

/// One Karabiner report to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Keyboard(KeyboardInput),
    Keys(KeysReport, Vec<u16>),
    Pointing(PointingInput),
}

type Source = (u8, Option<u8>);

/// Which kinds of state one report ID of a descriptor carries.
#[derive(Debug, Default, Clone, Copy)]
struct Present {
    keyboard: bool,
    consumer: bool,
    system: bool,
    buttons: bool,
    motion: bool,
}

impl Present {
    fn of(info: &DescriptorInfo, rid: Option<u8>) -> Self {
        let mut p = Present::default();
        for f in info.fields.iter().filter(|f| f.report_id == rid) {
            let pages: Vec<(u16, u16)> = if f.usages.is_empty() {
                let (min, max) = f.usage_range.unwrap_or((0, 0));
                vec![(f.usage_page, min), (f.usage_page, max)]
            } else {
                f.usages.clone()
            };
            for (page, usage) in pages {
                match page {
                    PAGE_KEYBOARD => p.keyboard = true,
                    PAGE_BUTTON => p.buttons = true,
                    PAGE_CONSUMER if f.is_relative() => p.motion = true,
                    PAGE_CONSUMER => p.consumer = true,
                    PAGE_GENERIC_DESKTOP if f.is_relative() => p.motion = true,
                    PAGE_GENERIC_DESKTOP if (0x81..=0xB7).contains(&usage) => p.system = true,
                    _ => {}
                }
            }
        }
        p
    }

    fn any(self) -> bool {
        self.keyboard || self.consumer || self.system || self.buttons || self.motion
    }
}

#[derive(Default)]
pub struct Mapper {
    devices: BTreeMap<u8, DescriptorInfo>,
    /// Per (device, report id) state, unioned when building a report.
    modifiers: BTreeMap<Source, u8>,
    keys: BTreeMap<Source, Vec<u16>>,
    consumer: BTreeMap<Source, Vec<u16>>,
    system: BTreeMap<Source, Vec<u16>>,
    buttons: BTreeMap<Source, u32>,
    last_keyboard: KeyboardInput,
    last_consumer: Vec<u16>,
    last_system: Vec<u16>,
    last_buttons: u32,
}

/// Split a relative delta into i8 steps that add up to it.
fn split(mut v: i32) -> Vec<i8> {
    let mut out = Vec::new();
    while v != 0 && out.len() < MAX_SPLIT {
        let step = v.clamp(-127, 127);
        out.push(step as i8);
        v -= step;
    }
    out
}

fn union<T: Copy + PartialEq>(maps: &BTreeMap<Source, Vec<T>>) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for v in maps.values().flatten() {
        if !out.contains(v) {
            out.push(*v);
        }
    }
    out.truncate(KEYS);
    out
}

impl Mapper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_device(&mut self, dev: &HidDevice) {
        self.devices
            .insert(dev.id, DescriptorInfo::parse(&dev.report_descriptor));
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Forget all devices. Returns the reports that release whatever was
    /// still held.
    pub fn clear(&mut self) -> Vec<Output> {
        self.devices.clear();
        self.release_all()
    }

    /// Reports that release everything, and the state reset to match.
    pub fn release_all(&mut self) -> Vec<Output> {
        self.modifiers.clear();
        self.keys.clear();
        self.consumer.clear();
        self.system.clear();
        self.buttons.clear();
        self.last_keyboard = KeyboardInput::default();
        self.last_consumer.clear();
        self.last_system.clear();
        self.last_buttons = 0;
        vec![
            Output::Keyboard(KeyboardInput::default()),
            Output::Keys(KeysReport::Consumer, Vec::new()),
            Output::Keys(KeysReport::GenericDesktop, Vec::new()),
            Output::Pointing(PointingInput::default()),
        ]
    }

    /// Reports carrying the whole current state, whether or not it changed.
    /// Posted again after a failed post, so a release that never reached
    /// Karabiner cannot leave a key down.
    pub fn full_state(&self) -> Vec<Output> {
        vec![
            Output::Keyboard(self.last_keyboard.clone()),
            Output::Keys(KeysReport::Consumer, self.last_consumer.clone()),
            Output::Keys(KeysReport::GenericDesktop, self.last_system.clone()),
            Output::Pointing(PointingInput {
                buttons: self.last_buttons,
                ..Default::default()
            }),
        ]
    }

    /// A key or modifier is held.
    pub fn keyboard_held(&self) -> bool {
        self.last_keyboard != KeyboardInput::default()
    }

    /// Whether `report` of device `dev_id` carries keyboard state.
    pub fn is_keyboard_report(&self, dev_id: u8, report: &[u8]) -> bool {
        let Some(info) = self.devices.get(&dev_id) else {
            return false;
        };
        let rid = if info.uses_report_ids {
            report.first().copied()
        } else {
            None
        };
        Present::of(info, rid).keyboard
    }

    /// Convert one raw report of device `dev_id`.
    pub fn on_report(&mut self, dev_id: u8, report: &[u8]) -> Vec<Output> {
        let Some(info) = self.devices.get(&dev_id) else {
            return Vec::new();
        };
        let rid = if info.uses_report_ids {
            report.first().copied()
        } else {
            None
        };
        let src = (dev_id, rid);
        let present = Present::of(info, rid);
        if !present.any() {
            return Vec::new(); // unknown report ID, or only vendor/constant data
        }
        let values: Vec<Value> = info.decode(report);

        // Everything this report ID carries starts empty, so a report with
        // nothing pressed (an all-zero array decodes to no values) still
        // releases what the previous report held.
        let mut mods: Option<u8> = present.keyboard.then_some(0);
        let mut keys: Option<Vec<u16>> = present.keyboard.then(Vec::new);
        let mut consumer: Option<Vec<u16>> = present.consumer.then(Vec::new);
        let mut system: Option<Vec<u16>> = present.system.then(Vec::new);
        let mut buttons: Option<u32> = present.buttons.then_some(0);
        let (mut dx, mut dy, mut wheel, mut pan) = (0i32, 0i32, 0i32, 0i32);

        for v in &values {
            match (v.usage_page, v.usage) {
                (PAGE_KEYBOARD, u @ 0xE0..=0xE7) => {
                    let m = mods.get_or_insert(0);
                    if v.value != 0 {
                        *m |= 1 << (u - 0xE0);
                    }
                }
                (PAGE_KEYBOARD, u) => {
                    let k = keys.get_or_insert_with(Vec::new);
                    // 1..=3 are the "error" codes (rollover, POST fail, undefined).
                    if v.value != 0 && u >= 4 && !k.contains(&u) {
                        k.push(u);
                    }
                }
                (PAGE_CONSUMER, CONSUMER_AC_PAN) if v.relative => pan += v.value,
                (PAGE_CONSUMER, u) => {
                    let c = consumer.get_or_insert_with(Vec::new);
                    if v.value != 0 && u <= CONSUMER_MAX && !c.contains(&u) {
                        c.push(u);
                    }
                }
                (PAGE_BUTTON, u) => {
                    let b = buttons.get_or_insert(0);
                    if v.value != 0 && (1..=32).contains(&u) {
                        *b |= 1 << (u - 1);
                    }
                }
                (PAGE_GENERIC_DESKTOP, GD_X) if v.relative => dx += v.value,
                (PAGE_GENERIC_DESKTOP, GD_Y) if v.relative => dy += v.value,
                (PAGE_GENERIC_DESKTOP, GD_WHEEL) if v.relative => wheel += v.value,
                (PAGE_GENERIC_DESKTOP, u @ 0x81..=0xB7) => {
                    let s = system.get_or_insert_with(Vec::new);
                    if v.value != 0 && !s.contains(&u) {
                        s.push(u);
                    }
                }
                _ => {}
            }
        }

        // The report replaces the state of its (device, report id).
        if mods.is_some() || keys.is_some() {
            self.modifiers.insert(src, mods.unwrap_or(0));
            self.keys.insert(src, keys.unwrap_or_default());
        }
        if let Some(c) = consumer {
            self.consumer.insert(src, c);
        }
        if let Some(s) = system {
            self.system.insert(src, s);
        }
        if let Some(b) = buttons {
            self.buttons.insert(src, b);
        }

        let mut out = Vec::new();
        let kb = KeyboardInput {
            modifiers: self.modifiers.values().fold(0, |a, m| a | m),
            keys: union(&self.keys),
        };
        if kb != self.last_keyboard {
            self.last_keyboard = kb.clone();
            out.push(Output::Keyboard(kb));
        }
        let c = union(&self.consumer);
        if c != self.last_consumer {
            self.last_consumer = c.clone();
            out.push(Output::Keys(KeysReport::Consumer, c));
        }
        let s = union(&self.system);
        if s != self.last_system {
            self.last_system = s.clone();
            out.push(Output::Keys(KeysReport::GenericDesktop, s));
        }

        let b = self.buttons.values().fold(0, |a, b| a | b);
        let (sx, sy, sw, sp) = (split(dx), split(dy), split(wheel), split(pan));
        let steps = sx.len().max(sy.len()).max(sw.len()).max(sp.len());
        if steps == 0 && b != self.last_buttons {
            out.push(Output::Pointing(PointingInput {
                buttons: b,
                ..Default::default()
            }));
        }
        for i in 0..steps {
            out.push(Output::Pointing(PointingInput {
                buttons: b,
                x: sx.get(i).copied().unwrap_or(0),
                y: sy.get(i).copied().unwrap_or(0),
                vertical_wheel: sw.get(i).copied().unwrap_or(0),
                horizontal_wheel: sp.get(i).copied().unwrap_or(0),
            }));
        }
        self.last_buttons = b;
        out
    }

    /// Vendor and product id to give the Karabiner keyboard: those of the
    /// first device with a keyboard collection, so macOS identifies the
    /// keyboard type as it would for the real receiver.
    pub fn keyboard_identity(devices: &[HidDevice]) -> Option<(u16, u16)> {
        devices
            .iter()
            .find(|d| DescriptorInfo::parse(&d.report_descriptor).is_keyboard())
            .map(|d| (d.vid, d.pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Vec<u8>> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/unifying-c52b")
            .join(name);
        std::fs::read(p).ok()
    }

    fn device(id: u8, desc: Vec<u8>) -> HidDevice {
        HidDevice {
            id,
            name: "Logitech USB Receiver".into(),
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            interface: id,
            uses_report_ids: DescriptorInfo::parse(&desc).uses_report_ids,
            report_descriptor: desc,
        }
    }

    /// The three receiver interfaces (mode A), if the fixtures are there.
    fn receiver() -> Option<Mapper> {
        let mut m = Mapper::new();
        m.add_device(&device(0, fixture("0003-046D-C52B.0007.nodj.bin")?));
        m.add_device(&device(1, fixture("0003-046D-C52B.0008.nodj.bin")?));
        m.add_device(&device(2, fixture("0003-046D-C52B.0009.nodj.bin")?));
        Some(m)
    }

    fn mouse(buttons: u16, x: i32, y: i32, wheel: i8, pan: i8) -> Vec<u8> {
        let xy = ((x as u32) & 0xFFF) | (((y as u32) & 0xFFF) << 12);
        vec![
            0x02,
            buttons as u8,
            (buttons >> 8) as u8,
            xy as u8,
            (xy >> 8) as u8,
            (xy >> 16) as u8,
            wheel as u8,
            pan as u8,
        ]
    }

    #[test]
    fn keyboard_modifiers_and_keys() {
        let Some(mut m) = receiver() else { return };
        // Left Ctrl + Left Cmd (GUI) + 'c'
        let out = m.on_report(0, &[0x09, 0, 0x06, 0, 0, 0, 0, 0]);
        assert_eq!(
            out,
            [Output::Keyboard(KeyboardInput {
                modifiers: 0x09,
                keys: vec![0x06],
            })]
        );
        // Same state again: nothing to post.
        assert!(m.on_report(0, &[0x09, 0, 0x06, 0, 0, 0, 0, 0]).is_empty());
        // Right Alt (AltGr / right option) alone, keys released.
        assert_eq!(
            m.on_report(0, &[0x40, 0, 0, 0, 0, 0, 0, 0]),
            [Output::Keyboard(KeyboardInput {
                modifiers: 0x40,
                keys: vec![],
            })]
        );
        // Rollover error codes are not keys.
        assert_eq!(
            m.on_report(0, &[0, 0, 1, 1, 1, 1, 1, 1]),
            [Output::Keyboard(KeyboardInput::default())]
        );
    }

    #[test]
    fn mouse_buttons_motion_and_wheels() {
        let Some(mut m) = receiver() else { return };
        let out = m.on_report(1, &mouse(0b1, 10, -3, 1, -2));
        assert_eq!(
            out,
            [Output::Pointing(PointingInput {
                buttons: 1,
                x: 10,
                y: -3,
                vertical_wheel: 1,
                horizontal_wheel: -2,
            })]
        );
        // Button release with no motion still produces a report.
        assert_eq!(
            m.on_report(1, &mouse(0, 0, 0, 0, 0)),
            [Output::Pointing(PointingInput::default())]
        );
        // Nothing changed, nothing moved.
        assert!(m.on_report(1, &mouse(0, 0, 0, 0, 0)).is_empty());
    }

    #[test]
    fn large_deltas_are_split_not_clipped() {
        let Some(mut m) = receiver() else { return };
        let out = m.on_report(1, &mouse(0, 300, -2000, 0, 0));
        let (sx, sy): (i32, i32) = out.iter().fold((0, 0), |(x, y), o| match o {
            Output::Pointing(p) => (x + p.x as i32, y + p.y as i32),
            _ => (x, y),
        });
        assert_eq!((sx, sy), (300, -2000));
        assert_eq!(out.len(), 16); // ceil(2000 / 127)
        assert_eq!(split(0), Vec::<i8>::new());
        assert_eq!(split(-128), vec![-127, -1]);
    }

    #[test]
    fn consumer_and_system_controls() {
        let Some(mut m) = receiver() else { return };
        assert_eq!(
            m.on_report(1, &[0x03, 0xE9, 0x00, 0x00, 0x00]),
            [Output::Keys(KeysReport::Consumer, vec![0xE9])]
        );
        assert_eq!(
            m.on_report(1, &[0x03, 0, 0, 0, 0]),
            [Output::Keys(KeysReport::Consumer, vec![])]
        );
        // System control value 1 = Sleep (first usage), 0 releases it.
        assert_eq!(
            m.on_report(1, &[0x04, 0x01]),
            [Output::Keys(KeysReport::GenericDesktop, vec![0x82])]
        );
        assert_eq!(
            m.on_report(1, &[0x04, 0x00]),
            [Output::Keys(KeysReport::GenericDesktop, vec![])]
        );
    }

    #[test]
    fn hidpp_and_unknown_devices_are_ignored() {
        let Some(mut m) = receiver() else { return };
        assert!(m.on_report(2, &[0x10, 0x01, 0x02, 0, 0, 0, 0]).is_empty());
        assert!(m.on_report(9, &[0; 8]).is_empty());
    }

    #[test]
    fn state_is_unioned_across_devices() {
        // Mode B: two keyboards with their own descriptors.
        let (Some(k230), Some(mxkeys)) = (
            fixture("0003-046D-400D.0005.bin"),
            fixture("0003-046D-408A.0006.bin"),
        ) else {
            return;
        };
        let mut m = Mapper::new();
        m.add_device(&device(0, k230));
        m.add_device(&device(1, mxkeys));
        // Shift held on one keyboard, 'a' typed on the other.
        m.on_report(0, &[0x01, 0x02, 0, 0, 0, 0, 0, 0]);
        let out = m.on_report(1, &[0x01, 0x00, 0x04, 0, 0, 0, 0, 0]);
        assert_eq!(
            out,
            [Output::Keyboard(KeyboardInput {
                modifiers: 0x02,
                keys: vec![0x04],
            })]
        );
    }

    #[test]
    fn release_all_empties_everything() {
        let Some(mut m) = receiver() else { return };
        m.on_report(0, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]);
        m.on_report(1, &mouse(0b11, 0, 0, 0, 0));
        let out = m.release_all();
        assert!(out.contains(&Output::Keyboard(KeyboardInput::default())));
        assert!(out.contains(&Output::Pointing(PointingInput::default())));
        // After a release, the same press is reported again.
        assert_eq!(m.on_report(0, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]).len(), 1);
    }

    /// The incident of 2026-10-23 (a key autorepeating on the Mac): the
    /// mapper must not keep anything after a reorder, a late duplicate or a
    /// reconnection. Datagrams go through `SeqTracker` as in agent-core.
    #[test]
    fn reordered_and_late_copies_leave_nothing_pressed() {
        use proto::datagram::SeqTracker;
        let Some(mut m) = receiver() else { return };
        let comma = [0, 0, 0x36, 0, 0, 0, 0, 0];
        let none = [0u8; 8];
        let mut t = SeqTracker::new();
        let feed = |m: &mut Mapper, t: &mut SeqTracker, seq: u16, r: &[u8]| {
            if t.observe(0, seq).is_some() {
                m.on_report(0, r);
            }
        };
        // press, release, then the redundant copy of the press (same seq).
        feed(&mut m, &mut t, 10, &comma);
        feed(&mut m, &mut t, 11, &none);
        feed(&mut m, &mut t, 10, &comma);
        assert!(!m.keyboard_held(), "late copy of the press");
        // release overtakes the press: the press arrives late and is dropped.
        feed(&mut m, &mut t, 13, &none);
        feed(&mut m, &mut t, 12, &comma);
        assert!(!m.keyboard_held(), "press after its release");
        // A report from another source (mouse interface, report ID 2) in
        // between does not resurrect the key.
        feed(&mut m, &mut t, 14, &comma);
        m.on_report(1, &mouse(0, 3, 0, 0, 0));
        feed(&mut m, &mut t, 15, &none);
        assert!(!m.keyboard_held());

        // Held key, then the hub restarts: destroy_all clears the mapper,
        // the new session re-creates the devices and the tracker restarts.
        feed(&mut m, &mut t, 16, &comma);
        assert!(m.keyboard_held());
        let out = m.clear();
        assert!(out.contains(&Output::Keyboard(KeyboardInput::default())));
        assert!(!m.keyboard_held());
        let mut fresh = receiver().unwrap();
        std::mem::swap(&mut m, &mut fresh);
        let mut t = SeqTracker::new();
        // First report of the new session: a '0' only.
        feed(&mut m, &mut t, 0, &[0, 0, 0x27, 0, 0, 0, 0, 0]);
        let Output::Keyboard(k) = m.full_state().remove(0) else {
            panic!()
        };
        assert_eq!(k.keys, [0x27], "no ghost key after a reconnection");
        feed(&mut m, &mut t, 1, &none);
        assert!(!m.keyboard_held());
    }

    #[test]
    fn full_state_reposts_what_is_held() {
        let Some(mut m) = receiver() else { return };
        m.on_report(0, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]);
        // Same state again: nothing new to post...
        assert!(m.on_report(0, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]).is_empty());
        // ...but after a failed post the whole state goes out again.
        let full = m.full_state();
        assert!(matches!(&full[0], Output::Keyboard(k) if k.modifiers == 0x02 && k.keys == [0x04]));
        assert!(m.is_keyboard_report(0, &[0; 8]));
        assert!(!m.is_keyboard_report(1, &mouse(0, 1, 0, 0, 0)));
    }

    #[test]
    fn keyboard_identity_comes_from_the_keyboard_interface() {
        let Some(k) = fixture("0003-046D-C52B.0007.nodj.bin") else {
            return;
        };
        let mut hidpp = device(2, vec![0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0xC0]);
        hidpp.pid = 0x1111;
        assert_eq!(
            Mapper::keyboard_identity(&[hidpp, device(0, k)]),
            Some((0x046d, 0xc52b))
        );
        assert_eq!(Mapper::keyboard_identity(&[]), None);
    }
}
