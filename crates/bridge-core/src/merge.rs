//! One report descriptor for several HID interfaces (ADR 0033).
//!
//! A USB receiver has one descriptor per interface; a Bluetooth HID device
//! has a single descriptor. The interfaces' descriptors are concatenated
//! with every report given a report ID of its own: interfaces without IDs
//! get a `Report ID` item after the start of each top-level collection, and
//! the IDs of interfaces that have them are renumbered so no two clash.
//! Input reports then carry the new ID; output reports (LEDs) are mapped
//! back to the interface and the ID it declared.
//!
//! Vendor-defined top-level collections (usage page >= 0xFF00: HID++) are
//! left out, as in the gadget's priority (ADR 0031).

use proto::HidDevice;

/// Item prefix bits: size (0..=3 -> 0, 1, 2, 4 bytes), type, tag.
const TYPE_MAIN: u8 = 0;
const TYPE_GLOBAL: u8 = 1;
const TYPE_LOCAL: u8 = 2;
const MAIN_COLLECTION: u8 = 0xA;
const MAIN_END_COLLECTION: u8 = 0xC;
const GLOBAL_USAGE_PAGE: u8 = 0x0;
const GLOBAL_REPORT_ID: u8 = 0x8;
const GLOBAL_PUSH: u8 = 0xA;
const GLOBAL_POP: u8 = 0xB;
const LOCAL_USAGE: u8 = 0x0;
const LONG_ITEM: u8 = 0xFE;
const REPORT_ID_ITEM: u8 = 0x85;

/// Globals an interface may leave set; reset (to 0) before the next one,
/// which expects the initial state: logical min/max, physical min/max,
/// unit exponent, unit, report size, report count.
const RESET_GLOBALS: [u8; 16] = [
    0x15, 0x00, 0x25, 0x00, 0x35, 0x00, 0x45, 0x00, 0x55, 0x00, 0x65, 0x00, 0x75, 0x00, 0x95, 0x00,
];

#[derive(Debug, Clone, Copy)]
struct Item<'a> {
    prefix: u8,
    /// The whole item, prefix included.
    raw: &'a [u8],
    data: &'a [u8],
}

impl Item<'_> {
    fn kind(&self) -> u8 {
        (self.prefix >> 2) & 0x3
    }
    fn tag(&self) -> u8 {
        self.prefix >> 4
    }
    fn is(&self, kind: u8, tag: u8) -> bool {
        self.prefix != LONG_ITEM && self.kind() == kind && self.tag() == tag
    }
    fn value(&self) -> u32 {
        let mut b = [0u8; 4];
        b[..self.data.len()].copy_from_slice(self.data);
        u32::from_le_bytes(b)
    }
}

fn items(desc: &[u8]) -> Result<Vec<Item<'_>>, MergeError> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        let (head, len) = if prefix == LONG_ITEM {
            (3, *desc.get(i + 1).ok_or(MergeError::Truncated)? as usize)
        } else {
            (1, [0, 1, 2, 4][(prefix & 0x3) as usize])
        };
        let end = i + head + len;
        if end > desc.len() {
            return Err(MergeError::Truncated);
        }
        v.push(Item {
            prefix,
            raw: &desc[i..end],
            data: &desc[i + head..end],
        });
        i = end;
    }
    Ok(v)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeError {
    Truncated,
    /// More than 255 reports.
    TooManyReports,
    /// No standard collection in any interface.
    Empty,
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MergeError::Truncated => "a report descriptor is truncated",
            MergeError::TooManyReports => "more than 255 reports in all",
            MergeError::Empty => "no keyboard, mouse or other standard collection to expose",
        })
    }
}

impl std::error::Error for MergeError {}

/// One report of the merged descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    dev_id: u8,
    /// The ID the interface declared; `None` if it uses none.
    original: Option<u8>,
    merged: u8,
}

/// The merged descriptor and how reports map to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub descriptor: Vec<u8>,
    routes: Vec<Route>,
    /// Interfaces with report IDs of their own.
    with_ids: Vec<u8>,
}

impl Merged {
    /// Merge `devices` in hub id order.
    pub fn build(devices: &[HidDevice]) -> Result<Self, MergeError> {
        let mut devs: Vec<&HidDevice> = devices.iter().collect();
        devs.sort_by_key(|d| d.id);
        let mut m = Merged {
            descriptor: Vec::new(),
            routes: Vec::new(),
            with_ids: Vec::new(),
        };
        let mut kept_any = false;
        for d in devs {
            let its = items(&d.report_descriptor)?;
            let uses_ids = its.iter().any(|i| i.is(TYPE_GLOBAL, GLOBAL_REPORT_ID));
            // An interface with nothing kept (HID++) leaves no byte behind,
            // so adding or removing it never changes the descriptor.
            let before = m.descriptor.len();
            if kept_any {
                m.descriptor.extend_from_slice(&RESET_GLOBALS);
            }
            if m.add(d.id, uses_ids, &its)? {
                kept_any = true;
                if uses_ids {
                    m.with_ids.push(d.id);
                }
            } else {
                m.descriptor.truncate(before);
            }
        }
        if !kept_any {
            return Err(MergeError::Empty);
        }
        Ok(m)
    }

    fn route(&mut self, dev_id: u8, original: Option<u8>) -> Result<u8, MergeError> {
        if let Some(r) = self
            .routes
            .iter()
            .find(|r| r.dev_id == dev_id && r.original == original)
        {
            return Ok(r.merged);
        }
        let merged = u8::try_from(self.routes.len() + 1).map_err(|_| MergeError::TooManyReports)?;
        self.routes.push(Route {
            dev_id,
            original,
            merged,
        });
        Ok(merged)
    }

    /// Append one interface. Returns whether a collection was kept.
    fn add(&mut self, dev_id: u8, uses_ids: bool, its: &[Item]) -> Result<bool, MergeError> {
        // Top-level segments: the items before a top-level collection and
        // the collection itself. Whether it is kept depends on its page.
        let mut kept_any = false;
        let mut page: u16 = 0;
        let mut pages: Vec<u16> = Vec::new(); // Push/Pop of the usage page
        let mut start = 0;
        let mut i = 0;
        while i < its.len() {
            let it = its[i];
            if it.is(TYPE_GLOBAL, GLOBAL_USAGE_PAGE) {
                page = it.value() as u16;
            } else if it.is(TYPE_GLOBAL, GLOBAL_PUSH) {
                pages.push(page);
            } else if it.is(TYPE_GLOBAL, GLOBAL_POP) {
                page = pages.pop().unwrap_or(page);
            }
            if !it.is(TYPE_MAIN, MAIN_COLLECTION) {
                i += 1;
                continue;
            }
            // A 4-byte usage names its page itself.
            let coll_page = its[start..i]
                .iter()
                .rev()
                .find(|x| x.is(TYPE_LOCAL, LOCAL_USAGE))
                .filter(|u| u.data.len() == 4)
                .map_or(page, |u| (u.value() >> 16) as u16);
            // The matching End Collection.
            let mut depth = 0usize;
            let mut end = i;
            while end < its.len() {
                if its[end].is(TYPE_MAIN, MAIN_COLLECTION) {
                    depth += 1;
                } else if its[end].is(TYPE_MAIN, MAIN_END_COLLECTION) {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                end += 1;
            }
            let end = end.min(its.len() - 1);
            let segment = &its[start..=end];
            if coll_page < 0xFF00 {
                kept_any = true;
                let own = if uses_ids {
                    None
                } else {
                    Some(self.route(dev_id, None)?)
                };
                for (k, x) in segment.iter().enumerate() {
                    if x.is(TYPE_GLOBAL, GLOBAL_REPORT_ID) && uses_ids {
                        let id = self.route(dev_id, Some(x.value() as u8))?;
                        self.descriptor.extend_from_slice(&[REPORT_ID_ITEM, id]);
                    } else {
                        self.descriptor.extend_from_slice(x.raw);
                    }
                    if start + k == i {
                        if let Some(id) = own {
                            self.descriptor.extend_from_slice(&[REPORT_ID_ITEM, id]);
                        }
                    }
                }
            } else {
                // Left out, but its globals (other than the report ID)
                // may still be relied on by what follows.
                self.keep_globals(segment);
            }
            // Track page changes inside the segment for what follows.
            for x in &its[i + 1..=end] {
                if x.is(TYPE_GLOBAL, GLOBAL_USAGE_PAGE) {
                    page = x.value() as u16;
                } else if x.is(TYPE_GLOBAL, GLOBAL_PUSH) {
                    pages.push(page);
                } else if x.is(TYPE_GLOBAL, GLOBAL_POP) {
                    page = pages.pop().unwrap_or(page);
                }
            }
            i = end + 1;
            start = i;
        }
        self.keep_globals(&its[start..]);
        Ok(kept_any)
    }

    fn keep_globals(&mut self, its: &[Item]) {
        for x in its {
            if x.prefix != LONG_ITEM && x.kind() == TYPE_GLOBAL && x.tag() != GLOBAL_REPORT_ID {
                self.descriptor.extend_from_slice(x.raw);
            }
        }
    }

    /// An input report of interface `dev_id`, as the hub sends it (with
    /// its own ID byte if it declares IDs), with the merged ID in front.
    /// `None` for a report of a collection left out.
    pub fn input(&self, dev_id: u8, report: &[u8]) -> Option<Vec<u8>> {
        let (original, payload) = if self.with_ids.contains(&dev_id) {
            let (id, rest) = report.split_first()?;
            (Some(*id), rest)
        } else {
            (None, report)
        };
        let r = self
            .routes
            .iter()
            .find(|r| r.dev_id == dev_id && r.original == original)?;
        let mut v = Vec::with_capacity(payload.len() + 1);
        v.push(r.merged);
        v.extend_from_slice(payload);
        Some(v)
    }

    /// An output report of the merged descriptor (merged ID first) as the
    /// interface declared it: its device id and the report, with its own
    /// ID byte if it uses IDs.
    pub fn output(&self, report: &[u8]) -> Option<(u8, Vec<u8>)> {
        let (id, payload) = report.split_first()?;
        let r = self.routes.iter().find(|r| r.merged == *id)?;
        let mut v = Vec::with_capacity(report.len());
        if let Some(o) = r.original {
            v.push(o);
        }
        v.extend_from_slice(payload);
        Some((r.dev_id, v))
    }

    /// Number of reports (merged IDs 1..=n).
    pub fn report_count(&self) -> usize {
        self.routes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::DescriptorInfo;
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/unifying-c52b")
            .join(name);
        std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn dev(id: u8, desc: Vec<u8>) -> HidDevice {
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

    /// The C52B: keyboard without IDs, mouse with IDs, HID++.
    fn c52b() -> Vec<HidDevice> {
        vec![
            dev(0, fixture("0003-046D-C52B.0007.nodj.bin")),
            dev(1, fixture("0003-046D-C52B.0008.nodj.bin")),
            dev(2, fixture("0003-046D-C52B.0009.nodj.bin")),
        ]
    }

    /// Devices of `fixtures/two-receivers.hidrec` (`D id iface vid pid hex`).
    fn two_receivers() -> Vec<HidDevice> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/two-receivers.hidrec");
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .filter_map(|l| l.strip_prefix("D "))
            .map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let desc = (0..f[4].len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&f[4][i..i + 2], 16).unwrap())
                    .collect();
                let mut d = dev(f[0].parse().unwrap(), desc);
                d.vid = u16::from_str_radix(f[2], 16).unwrap();
                d.pid = u16::from_str_radix(f[3], 16).unwrap();
                d
            })
            .collect()
    }

    fn ids(info: &DescriptorInfo) -> Vec<(Option<u8>, usize)> {
        info.inputs.iter().map(|r| (r.report_id, r.len)).collect()
    }

    #[test]
    fn c52b_merges_into_unique_ids_without_hidpp() {
        let devs = c52b();
        let m = Merged::build(&devs).unwrap();
        let merged = DescriptorInfo::parse(&m.descriptor);
        assert!(merged.uses_report_ids);
        // No vendor collection survives.
        assert!(merged.applications.iter().all(|(p, _)| *p < 0xFF00));
        assert!(merged.is_keyboard() && merged.is_mouse());

        // Every original standard report is there once, same length.
        let mut want = Vec::new();
        for d in &devs[..2] {
            let info = DescriptorInfo::parse(&d.report_descriptor);
            for r in info.inputs.iter().filter(|r| r.app_usage_page < 0xFF00) {
                want.push(r.len);
            }
        }
        let got = ids(&merged);
        assert_eq!(got.iter().map(|(_, l)| *l).collect::<Vec<_>>(), want);
        let mut seen: Vec<u8> = got.iter().map(|(id, _)| id.unwrap()).collect();
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n, "report IDs are unique: {got:?}");
        assert_eq!(seen[0], 1);
        // The keyboard's LED output report survives, under the keyboard's ID.
        let kbd_id = m.input(0, &[0; 8]).unwrap()[0];
        assert!(merged
            .outputs
            .iter()
            .any(|o| o.report_id == Some(kbd_id) && o.len == 1));
    }

    #[test]
    fn interfaces_left_out_leave_no_trace() {
        let devs = c52b();
        let without = Merged::build(&devs[..2]).unwrap();
        let with = Merged::build(&devs).unwrap();
        assert_eq!(with.descriptor, without.descriptor);
        assert_eq!(with, without);
        // HID++ first in id order would not change it either.
        let mut hidpp_first = devs[2].clone();
        hidpp_first.id = 0;
        let mut rest = devs[..2].to_vec();
        rest[0].id = 1;
        rest[1].id = 3;
        let a = Merged::build(&[hidpp_first, rest[0].clone(), rest[1].clone()]).unwrap();
        let b = Merged::build(&rest).unwrap();
        assert_eq!(a.descriptor, b.descriptor);
    }

    #[test]
    fn reports_round_trip() {
        let devs = c52b();
        let m = Merged::build(&devs).unwrap();
        let merged = DescriptorInfo::parse(&m.descriptor);

        // Keyboard (no IDs): "a" pressed.
        let key = [0, 0, 0x04, 0, 0, 0, 0, 0];
        let k = m.input(0, &key).unwrap();
        assert_eq!(&k[1..], &key);
        let r = merged
            .inputs
            .iter()
            .find(|r| r.report_id == Some(k[0]))
            .unwrap();
        assert_eq!(r.len, key.len());
        assert!(merged
            .decode(&k)
            .iter()
            .any(|v| v.usage == 0x04 && v.value != 0));

        // Mouse (ID 2 on the receiver): renumbered, payload unchanged.
        let mouse_info = DescriptorInfo::parse(&devs[1].report_descriptor);
        let mr = mouse_info
            .inputs
            .iter()
            .find(|r| r.app_usage == proto::descriptor::USAGE_MOUSE)
            .unwrap();
        let mut report = vec![mr.report_id.unwrap()];
        report.extend(std::iter::repeat(0).take(mr.len));
        report[2] = 5; // some movement
        let mm = m.input(1, &report).unwrap();
        assert_ne!(mm[0], k[0]);
        assert_eq!(&mm[1..], &report[1..]);
        assert_eq!(
            merged
                .inputs
                .iter()
                .find(|r| r.report_id == Some(mm[0]))
                .unwrap()
                .len,
            mr.len
        );

        // HID++ reports have nowhere to go.
        assert_eq!(m.input(2, &[0x10, 0xFF, 0, 0, 0, 0, 0]), None);
        assert_eq!(m.input(9, &key), None);

        // LEDs back to the keyboard interface, without an ID byte.
        assert_eq!(m.output(&[k[0], 0x02]), Some((0, vec![0x02])));
        assert_eq!(m.output(&[200, 0x02]), None);
    }

    #[test]
    fn two_receivers_get_distinct_ids() {
        let devs = two_receivers();
        assert_eq!(
            devs.iter().map(|d| d.id).collect::<Vec<_>>(),
            [0, 1, 16, 17],
            "fixture changed"
        );
        let m = Merged::build(&devs).unwrap();
        let merged = DescriptorInfo::parse(&m.descriptor);
        let first = m.input(0, &[0; 8]).unwrap()[0];
        let second = m.input(16, &[0; 8]).unwrap()[0];
        assert_ne!(first, second);
        // Same length per interface, and the reports of both keyboards.
        let kbds = merged.inputs.iter().filter(|r| r.len == 8).count();
        assert!(kbds >= 2);
        // Both mice keep their payload length under distinct IDs.
        let info1 = DescriptorInfo::parse(&devs[1].report_descriptor);
        let n_std = info1
            .inputs
            .iter()
            .filter(|r| r.app_usage_page < 0xFF00)
            .count();
        assert_eq!(m.report_count(), 2 * (1 + n_std));
        // Globals do not leak from one interface into the next: each
        // merged report keeps the length it had alone.
        assert_eq!(
            merged.inputs.len(),
            m.report_count(),
            "every merged report parses: {:?}",
            ids(&merged)
        );
    }

    #[test]
    fn only_vendor_collections_is_an_error() {
        let devs = vec![dev(2, fixture("0003-046D-C52B.0009.nodj.bin"))];
        assert_eq!(Merged::build(&devs), Err(MergeError::Empty));
        let truncated = vec![dev(0, vec![0x05, 0x01, 0x26])];
        assert_eq!(Merged::build(&truncated), Err(MergeError::Truncated));
    }
}
