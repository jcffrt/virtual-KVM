//! Minimal HID report descriptor parser.
//!
//! The hub never interprets input reports for forwarding; it only needs to
//! know three things about a descriptor:
//!
//! 1. whether report IDs are used (reports then start with the ID byte);
//! 2. the size of every input report, to synthesize "all released" reports;
//! 3. where the modifier byte and key array of a keyboard report live, for
//!    the emergency hotkey.
//!
//! Agents that cannot hand the descriptor to the OS as is (macOS, where the
//! virtual device's descriptor is fixed by Karabiner) also need a general
//! decoder: [`DescriptorInfo::fields`] describes every input field and
//! [`DescriptorInfo::decode`] turns a report into `(page, usage, value)`
//! triples. That is a 1:1 reading of the usages, not a reinterpretation.
//!
//! This parser handles short items, `Push`/`Pop`, and multiple report IDs. It
//! ignores long items (none exist in practice).

use std::collections::BTreeMap;

pub const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
pub const USAGE_PAGE_KEYBOARD: u16 = 0x07;
pub const USAGE_PAGE_BUTTON: u16 = 0x09;
pub const USAGE_PAGE_CONSUMER: u16 = 0x0C;
pub const USAGE_MOUSE: u16 = 0x02;
pub const USAGE_KEYBOARD: u16 = 0x06;

/// Layout of a keyboard input report, as bit offsets inside the report
/// payload (after the report ID byte, if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardLayout {
    pub report_id: Option<u8>,
    /// Bit offset of the 8 modifier bits (usages 0xE0..=0xE7).
    pub modifiers_bit: usize,
    /// Byte offset of the key array.
    pub keys_byte: usize,
    pub keys_len: usize,
}

/// One input report declared by the descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputReport {
    pub report_id: Option<u8>,
    /// Payload size in bytes, not counting the report ID byte.
    pub len: usize,
    /// Usage page of the enclosing top-level application collection.
    pub app_usage_page: u16,
    pub app_usage: u16,
}

/// Main item flags (HID 1.11 §6.2.2.5).
pub const FLAG_CONSTANT: u32 = 0x01;
pub const FLAG_VARIABLE: u32 = 0x02;
pub const FLAG_RELATIVE: u32 = 0x04;

/// One `Input` main item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub report_id: Option<u8>,
    /// Bit offset inside the payload (after the report ID byte, if any).
    pub bit_offset: usize,
    pub size: u32,
    pub count: u32,
    pub usage_page: u16,
    /// Explicit `Usage` items as (page, usage). 32-bit usages carry their
    /// own page; 16-bit ones take the current usage page.
    pub usages: Vec<(u16, u16)>,
    /// `Usage Minimum` / `Usage Maximum`.
    pub usage_range: Option<(u16, u16)>,
    pub logical_min: i32,
    pub logical_max: i32,
    pub flags: u32,
}

impl Field {
    pub fn is_constant(&self) -> bool {
        self.flags & FLAG_CONSTANT != 0
    }
    pub fn is_variable(&self) -> bool {
        self.flags & FLAG_VARIABLE != 0
    }
    pub fn is_relative(&self) -> bool {
        self.flags & FLAG_RELATIVE != 0
    }

    /// Usage of element `i` of a variable field.
    fn variable_usage(&self, i: usize) -> Option<(u16, u16)> {
        if let Some(u) = self.usages.get(i).or(self.usages.last()) {
            return Some(*u);
        }
        let (min, max) = self.usage_range?;
        let u = (min as usize + i).min(max as usize) as u16;
        Some((self.usage_page, u))
    }

    /// Usage selected by value `v` of an array field, or `None` for the
    /// "nothing" value.
    fn array_usage(&self, v: i32) -> Option<(u16, u16)> {
        if v < self.logical_min || v > self.logical_max {
            return None;
        }
        let idx = (v - self.logical_min) as usize;
        if !self.usages.is_empty() {
            return self.usages.get(idx).copied();
        }
        let (min, max) = self.usage_range?;
        let u = min as usize + idx;
        if u > max as usize || u == 0 {
            return None; // usage 0 means "no event"
        }
        Some((self.usage_page, u as u16))
    }
}

/// One decoded value of an input report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value {
    pub usage_page: u16,
    pub usage: u16,
    /// Sign-extended when the field's logical minimum is negative. For an
    /// array field the usage is the pressed one and the value is 1.
    pub value: i32,
    pub relative: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DescriptorInfo {
    pub uses_report_ids: bool,
    pub inputs: Vec<InputReport>,
    /// Every non-constant input field, in descriptor order.
    pub fields: Vec<Field>,
    pub keyboard: Option<KeyboardLayout>,
    /// Top-level application collections as (usage page, usage).
    pub applications: Vec<(u16, u16)>,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u16,
    logical_min: i32,
    logical_max: i32,
    /// `Logical Maximum` read as unsigned, see the Input handling.
    logical_max_unsigned: i32,
    report_size: u32,
    report_count: u32,
    report_id: Option<u8>,
}

/// Sign-extend an item's data according to its size.
fn signed(data: u32, size: usize) -> i32 {
    match size {
        1 => data as u8 as i8 as i32,
        2 => data as u16 as i16 as i32,
        _ => data as i32,
    }
}

/// Read `size` bits at `bit` (LSB first, as HID packs fields).
fn extract(payload: &[u8], bit: usize, size: u32) -> Option<u32> {
    if size == 0 || size > 32 || bit + size as usize > payload.len() * 8 {
        return None;
    }
    let mut v: u32 = 0;
    for k in 0..size as usize {
        let b = bit + k;
        if (payload[b / 8] >> (b % 8)) & 1 == 1 {
            v |= 1 << k;
        }
    }
    Some(v)
}

impl DescriptorInfo {
    pub fn parse(desc: &[u8]) -> Self {
        let mut info = DescriptorInfo::default();
        let mut g = Globals::default();
        let mut stack: Vec<Globals> = Vec::new();
        let mut usages: Vec<(u16, u16)> = Vec::new();
        let mut usage_min: Option<u16> = None;
        let mut usage_max: Option<u16> = None;
        // bit position per report id, for input reports
        let mut bit_pos: BTreeMap<Option<u8>, usize> = BTreeMap::new();
        let mut app: Option<(u16, u16)> = None;
        let mut depth = 0u32;
        let mut keyboard_mods: Option<(Option<u8>, usize)> = None;
        let mut keyboard_keys: Option<(Option<u8>, usize, usize)> = None;

        let mut i = 0;
        while i < desc.len() {
            let prefix = desc[i];
            i += 1;
            if prefix == 0xFE {
                // long item: skip
                if i + 1 < desc.len() {
                    let len = desc[i] as usize;
                    i += 2 + len;
                }
                continue;
            }
            let size = match prefix & 0x03 {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 4,
            };
            if i + size > desc.len() {
                break;
            }
            let mut data: u32 = 0;
            for k in 0..size {
                data |= (desc[i + k] as u32) << (8 * k);
            }
            i += size;
            let typ = (prefix >> 2) & 0x03;
            let tag = prefix >> 4;
            match (typ, tag) {
                // ---- main items ----
                (0, 0x8) => {
                    // Input
                    let bits = (g.report_size * g.report_count) as usize;
                    let pos = bit_pos.entry(g.report_id).or_insert(0);
                    let start = *pos;
                    if g.usage_page == USAGE_PAGE_KEYBOARD {
                        let is_const = data & 0x01 != 0;
                        let is_var = data & 0x02 != 0;
                        if !is_const
                            && is_var
                            && g.report_size == 1
                            && g.report_count == 8
                            && usage_min == Some(0xE0)
                            && keyboard_mods.is_none()
                        {
                            keyboard_mods = Some((g.report_id, start));
                        } else if !is_const
                            && !is_var
                            && g.report_size == 8
                            && g.report_count >= 1
                            && start % 8 == 0
                            && keyboard_keys.is_none()
                        {
                            keyboard_keys = Some((g.report_id, start / 8, g.report_count as usize));
                        }
                    }
                    if data & FLAG_CONSTANT == 0 && bits > 0 {
                        // A broken `Logical Maximum 0xFF` (one byte) reads
                        // as -1: take it as unsigned then.
                        let max = if g.logical_max < g.logical_min {
                            g.logical_max_unsigned
                        } else {
                            g.logical_max
                        };
                        info.fields.push(Field {
                            report_id: g.report_id,
                            bit_offset: start,
                            size: g.report_size,
                            count: g.report_count,
                            usage_page: g.usage_page,
                            usages: usages.clone(),
                            usage_range: usage_min.map(|m| (m, usage_max.unwrap_or(m))),
                            logical_min: g.logical_min,
                            logical_max: max,
                            flags: data,
                        });
                    }
                    *pos += bits;
                    usages.clear();
                    usage_min = None;
                    usage_max = None;
                }
                (0, 0x9) | (0, 0xB) => {
                    // Output / Feature: ignore sizes, clear locals
                    usages.clear();
                    usage_min = None;
                    usage_max = None;
                }
                (0, 0xA) => {
                    // Collection
                    if depth == 0 && data == 0x01 {
                        let (page, usage) = usages.first().copied().unwrap_or((g.usage_page, 0));
                        app = Some((page, usage));
                        info.applications.push((page, usage));
                    }
                    depth += 1;
                    usages.clear();
                    usage_min = None;
                    usage_max = None;
                }
                (0, 0xC) => {
                    // End collection
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        // flush input reports of this application
                        let (ap, au) = app.take().unwrap_or((0, 0));
                        for (rid, bits) in bit_pos.iter() {
                            if *bits == 0 {
                                continue;
                            }
                            info.inputs.push(InputReport {
                                report_id: *rid,
                                len: bits.div_ceil(8),
                                app_usage_page: ap,
                                app_usage: au,
                            });
                        }
                        bit_pos.clear();
                    }
                }
                // ---- global items ----
                (1, 0x0) => g.usage_page = data as u16,
                (1, 0x1) => g.logical_min = signed(data, size),
                (1, 0x2) => {
                    g.logical_max = signed(data, size);
                    g.logical_max_unsigned = data as i32;
                }
                (1, 0x7) => g.report_size = data,
                (1, 0x8) => {
                    g.report_id = Some(data as u8);
                    info.uses_report_ids = true;
                }
                (1, 0x9) => g.report_count = data,
                (1, 0xA) => stack.push(g),
                (1, 0xB) => {
                    if let Some(p) = stack.pop() {
                        g = p;
                    }
                }
                // ---- local items ----
                (2, 0x0) => {
                    let page = if size == 4 {
                        (data >> 16) as u16
                    } else {
                        g.usage_page
                    };
                    usages.push((page, data as u16));
                }
                (2, 0x1) => usage_min = Some(data as u16),
                (2, 0x2) => usage_max = Some(data as u16),
                _ => {}
            }
        }
        // descriptor without a closing End Collection: flush anyway
        if !bit_pos.is_empty() {
            let (ap, au) = app.unwrap_or((0, 0));
            for (rid, bits) in bit_pos.iter() {
                if *bits == 0 {
                    continue;
                }
                info.inputs.push(InputReport {
                    report_id: *rid,
                    len: bits.div_ceil(8),
                    app_usage_page: ap,
                    app_usage: au,
                });
            }
        }
        if let (Some((rid_m, mods)), Some((rid_k, keys, len))) = (keyboard_mods, keyboard_keys) {
            if rid_m == rid_k {
                info.keyboard = Some(KeyboardLayout {
                    report_id: rid_m,
                    modifiers_bit: mods,
                    keys_byte: keys,
                    keys_len: len,
                });
            }
        }
        info
    }

    /// Synthesize one "nothing pressed" report per input report of the
    /// standard HID pages. Vendor pages (HID++) are skipped: a zero report
    /// there means nothing and might confuse a driver.
    pub fn release_reports(&self) -> Vec<Vec<u8>> {
        self.inputs
            .iter()
            .filter(|r| r.app_usage_page < 0xFF00)
            .map(|r| {
                let mut v = Vec::with_capacity(r.len + 1);
                if let Some(id) = r.report_id {
                    v.push(id);
                }
                v.resize(v.len() + r.len, 0);
                v
            })
            .collect()
    }

    /// Decode a raw report (with its report ID byte, if the descriptor uses
    /// IDs) into the values of its fields. Variable fields yield one value
    /// per element, zeros included; array fields yield one value per
    /// selected usage. Unknown report IDs yield nothing.
    pub fn decode(&self, report: &[u8]) -> Vec<Value> {
        let (id, payload) = if self.uses_report_ids {
            match report.split_first() {
                Some((id, rest)) => (Some(*id), rest),
                None => return Vec::new(),
            }
        } else {
            (None, report)
        };
        let mut out = Vec::new();
        for f in self.fields.iter().filter(|f| f.report_id == id) {
            for i in 0..f.count as usize {
                let Some(raw) = extract(payload, f.bit_offset + i * f.size as usize, f.size) else {
                    break;
                };
                let v = if f.logical_min < 0 && f.size < 32 {
                    let shift = 32 - f.size;
                    ((raw << shift) as i32) >> shift
                } else {
                    raw as i32
                };
                if f.is_variable() {
                    if let Some((page, usage)) = f.variable_usage(i) {
                        out.push(Value {
                            usage_page: page,
                            usage,
                            value: v,
                            relative: f.is_relative(),
                        });
                    }
                } else if let Some((page, usage)) = f.array_usage(v) {
                    out.push(Value {
                        usage_page: page,
                        usage,
                        value: 1,
                        relative: false,
                    });
                }
            }
        }
        out
    }

    /// True if the descriptor contains a keyboard application collection.
    pub fn is_keyboard(&self) -> bool {
        self.applications
            .contains(&(USAGE_PAGE_GENERIC_DESKTOP, USAGE_KEYBOARD))
    }

    pub fn is_mouse(&self) -> bool {
        self.applications
            .contains(&(USAGE_PAGE_GENERIC_DESKTOP, USAGE_MOUSE))
    }
}

/// Decoded view of a keyboard report, used by the hub for the emergency
/// hotkey only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardState<'a> {
    pub modifiers: u8,
    pub keys: &'a [u8],
}

impl KeyboardLayout {
    /// Decode a raw report (including the report ID byte if the descriptor
    /// uses IDs). Returns `None` if the report belongs to another report ID
    /// or is too short.
    pub fn decode<'a>(&self, uses_report_ids: bool, report: &'a [u8]) -> Option<KeyboardState<'a>> {
        let payload = if uses_report_ids {
            if report.first().copied() != self.report_id {
                return None;
            }
            &report[1..]
        } else {
            report
        };
        if self.modifiers_bit % 8 != 0 {
            return None;
        }
        let mod_byte = self.modifiers_bit / 8;
        let keys_end = self.keys_byte + self.keys_len;
        if payload.len() <= mod_byte || payload.len() < keys_end {
            return None;
        }
        Some(KeyboardState {
            modifiers: payload[mod_byte],
            keys: &payload[self.keys_byte..keys_end],
        })
    }

    /// A copy of `report` with Alt and GUI swapped, or `None` if it is not
    /// a report of this keyboard layout. Modifier bits LeftAlt 0x04 <->
    /// LeftGUI 0x08 and RightAlt 0x40 <-> RightGUI 0x80; in the key array
    /// usages 0xE2 <-> 0xE3 and 0xE6 <-> 0xE7. Nothing else changes.
    ///
    /// This is the one deliberate deviation from raw forwarding (ADR 0011),
    /// for a target whose OS expects the other layout.
    pub fn swap_alt_gui(&self, uses_report_ids: bool, report: &[u8]) -> Option<Vec<u8>> {
        self.decode(uses_report_ids, report)?;
        let mut out = report.to_vec();
        let base = usize::from(uses_report_ids);
        let m = &mut out[base + self.modifiers_bit / 8];
        *m = (*m & 0x33) | ((*m & 0x44) << 1) | ((*m & 0x88) >> 1);
        for k in &mut out[base + self.keys_byte..base + self.keys_byte + self.keys_len] {
            *k = match *k {
                0xE2 => 0xE3,
                0xE3 => 0xE2,
                0xE6 => 0xE7,
                0xE7 => 0xE6,
                other => other,
            };
        }
        Some(out)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod tests {
    //! Reference descriptors, also useful to other crates' tests.
    #[cfg_attr(not(test), allow(unused_imports))]
    use super::*;

    /// Standard USB boot keyboard (HID 1.11 appendix E.6), 63 bytes.
    pub const BOOT_KEYBOARD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25,
        0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05,
        0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91,
        0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65,
        0x81, 0x00, 0xC0,
    ];

    /// Standard boot mouse (HID 1.11 appendix E.10), 50 bytes, no report ID.
    pub const BOOT_MOUSE: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
        0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
        0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95,
        0x02, 0x81, 0x06, 0xC0, 0xC0,
    ];

    /// Mouse (ID 2, 16-bit buttons, 12-bit X/Y, wheel) + consumer control
    /// (ID 3) + system control (ID 4), modelled on the Unifying receiver's
    /// second interface.
    pub const UNIFYING_IFACE1: &[u8] = &[
        // mouse
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19,
        0x01, 0x29, 0x10, 0x15, 0x00, 0x25, 0x01, 0x95, 0x10, 0x75, 0x01, 0x81, 0x02, 0x05, 0x01,
        0x16, 0x01, 0xF8, 0x26, 0xFF, 0x07, 0x75, 0x0C, 0x95, 0x02, 0x09, 0x30, 0x09, 0x31, 0x81,
        0x06, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x09, 0x38, 0x81, 0x06, 0x05, 0x0C,
        0x0A, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0, // consumer control
        0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x03, 0x75, 0x10, 0x95, 0x02, 0x15, 0x01, 0x26,
        0xFF, 0x02, 0x19, 0x01, 0x2A, 0xFF, 0x02, 0x81, 0x00, 0xC0, // system control
        0x05, 0x01, 0x09, 0x80, 0xA1, 0x01, 0x85, 0x04, 0x75, 0x02, 0x95, 0x01, 0x15, 0x01, 0x25,
        0x03, 0x09, 0x82, 0x09, 0x81, 0x09, 0x83, 0x81, 0x60, 0x75, 0x06, 0x81, 0x03, 0xC0,
    ];

    /// Vendor HID++ interface: short (0x10, 6 bytes) and long (0x11, 19
    /// bytes) reports on usage page 0xFF00.
    pub const UNIFYING_IFACE2: &[u8] = &[
        0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x10, 0x75, 0x08, 0x95, 0x06, 0x15, 0x00,
        0x26, 0xFF, 0x00, 0x09, 0x01, 0x81, 0x00, 0x09, 0x01, 0x91, 0x00, 0xC0, 0x06, 0x00, 0xFF,
        0x09, 0x02, 0xA1, 0x01, 0x85, 0x11, 0x75, 0x08, 0x95, 0x13, 0x15, 0x00, 0x26, 0xFF, 0x00,
        0x09, 0x02, 0x81, 0x00, 0x09, 0x02, 0x91, 0x00, 0xC0,
    ];

    #[test]
    fn boot_keyboard() {
        let d = DescriptorInfo::parse(BOOT_KEYBOARD);
        assert!(!d.uses_report_ids);
        assert!(d.is_keyboard());
        assert!(!d.is_mouse());
        assert_eq!(d.inputs.len(), 1);
        assert_eq!(d.inputs[0].len, 8);
        assert_eq!(d.inputs[0].report_id, None);
        let kb = d.keyboard.clone().expect("keyboard layout");
        assert_eq!(
            kb,
            KeyboardLayout {
                report_id: None,
                modifiers_bit: 0,
                keys_byte: 2,
                keys_len: 6
            }
        );
        let rel = d.release_reports();
        assert_eq!(rel, vec![vec![0u8; 8]]);
        let st = kb.decode(false, &[0x01, 0, 0x1E, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(st.modifiers, 0x01);
        assert_eq!(st.keys, &[0x1E, 0, 0, 0, 0, 0]);
        assert!(kb.decode(false, &[0x01, 0]).is_none());
    }

    #[test]
    fn boot_mouse() {
        let d = DescriptorInfo::parse(BOOT_MOUSE);
        assert!(!d.uses_report_ids);
        assert!(d.is_mouse());
        assert_eq!(d.inputs.len(), 1);
        assert_eq!(d.inputs[0].len, 3);
        assert!(d.keyboard.is_none());
    }

    #[test]
    fn unifying_iface1_multiple_report_ids() {
        let d = DescriptorInfo::parse(UNIFYING_IFACE1);
        assert!(d.uses_report_ids);
        assert!(d.is_mouse());
        assert_eq!(d.applications.len(), 3);
        let lens: Vec<(Option<u8>, usize)> =
            d.inputs.iter().map(|r| (r.report_id, r.len)).collect();
        // mouse: 16 + 24 + 8 + 8 bits = 7 bytes; consumer: 4 bytes; system: 1 byte
        assert_eq!(lens, vec![(Some(2), 7), (Some(3), 4), (Some(4), 1)]);
        let rel = d.release_reports();
        assert_eq!(rel[0], vec![2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(rel[1], vec![3, 0, 0, 0, 0]);
        assert_eq!(rel[2], vec![4, 0]);
    }

    #[test]
    fn vendor_interface_has_no_release_reports() {
        let d = DescriptorInfo::parse(UNIFYING_IFACE2);
        assert!(d.uses_report_ids);
        assert_eq!(d.inputs.len(), 2);
        assert_eq!(d.inputs[0].len, 6);
        assert_eq!(d.inputs[1].len, 19);
        assert!(d.release_reports().is_empty());
        assert!(!d.is_keyboard());
    }

    #[test]
    fn keyboard_with_report_id_and_push_pop() {
        // boot keyboard wrapped with report id 1, exercising push/pop
        let mut desc = vec![0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0xA4, 0xB4];
        desc.extend_from_slice(&BOOT_KEYBOARD[6..]);
        let d = DescriptorInfo::parse(&desc);
        assert!(d.uses_report_ids);
        let kb = d.keyboard.clone().unwrap();
        assert_eq!(kb.report_id, Some(1));
        assert_eq!(kb.keys_byte, 2);
        assert!(kb.decode(true, &[2, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
        let st = kb.decode(true, &[1, 0x11, 0, 0x04, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(st.modifiers, 0x11);
        assert_eq!(st.keys[0], 0x04);
        assert_eq!(d.release_reports(), vec![vec![1, 0, 0, 0, 0, 0, 0, 0, 0]]);
    }

    #[test]
    fn garbage_does_not_panic() {
        let _ = DescriptorInfo::parse(&[0xFE, 0x05]);
        let _ = DescriptorInfo::parse(&[0x75]);
        let _ = DescriptorInfo::parse(&[]);
        let _ = DescriptorInfo::parse(&[0x81, 0x02, 0xC0, 0xC0, 0xC0]);
    }
}
