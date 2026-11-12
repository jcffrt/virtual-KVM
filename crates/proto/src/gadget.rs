//! Which interfaces a USB gadget presents to its PC, and as what (ADR
//! 0007, ADR 0031). Shared by the Linux gadget agent and the hub's light
//! link for microcontroller gadgets.

use crate::descriptor::DescriptorInfo;
use crate::HidDevice;

/// What the PC will see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GadgetSpec {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
    pub functions: Vec<FunctionSpec>,
    /// Devices that should be presented but did not fit in the limit, by
    /// hub id (ADR 0031).
    pub left_out: Vec<u8>,
}

impl GadgetSpec {
    /// The PC would see the same device: equal but for `left_out`.
    pub fn same_gadget(&self, other: &GadgetSpec) -> bool {
        let strip = |s: &GadgetSpec| GadgetSpec {
            left_out: Vec::new(),
            ..s.clone()
        };
        strip(self) == strip(other)
    }
}

/// One HID interface (`functions/hid.<instance>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSpec {
    pub dev_id: u8,
    pub instance: String,
    pub subclass: u8,
    pub protocol: u8,
    pub report_length: u16,
    pub report_desc: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Also present the HID++ / DJ interface. Hosts running
    /// `hid-logitech-dj` (Linux) should not see it: see ADR 0007.
    pub expose_hidpp: bool,
    pub serial: String,
    /// Most HID functions to create: [`HIDG_MINORS`] on a real kernel.
    pub max_functions: usize,
}

/// Longest input report including its ID byte: the interrupt IN endpoint's
/// packet size. `f_hid` truncates writes to this length.
pub fn report_length(info: &DescriptorInfo) -> u16 {
    info.inputs
        .iter()
        .map(|r| r.len + usize::from(r.report_id.is_some()))
        .max()
        .unwrap_or(8)
        .clamp(1, u16::MAX as usize) as u16
}

/// Interface subclass and protocol, like the real receiver: boot keyboard
/// (1/1) for a keyboard collection, boot mouse (1/2) for a mouse, none
/// (0/0) otherwise (HID++).
pub fn boot_class(info: &DescriptorInfo) -> (u8, u8) {
    if info.is_keyboard() {
        (1, 1)
    } else if info.is_mouse() {
        (1, 2)
    } else {
        (0, 0)
    }
}

/// Only vendor-defined collections (the HID++ / DJ interface).
pub fn is_vendor_only(info: &DescriptorInfo) -> bool {
    !info.applications.is_empty() && info.applications.iter().all(|(page, _)| *page >= 0xFF00)
}

/// How much an interface is worth when not all of them fit (ADR 0031):
/// 0 keyboard or mouse, 1 any other standard collection (consumer keys,
/// system control), 2 vendor-defined only (HID++) or unknown.
pub fn priority(info: &DescriptorInfo) -> u8 {
    const KEYBOARD_OR_MOUSE: [u16; 4] = [0x01, 0x02, 0x06, 0x07]; // pointer, mouse, keyboard, keypad
    let apps = &info.applications;
    if apps
        .iter()
        .any(|&(page, usage)| page == 0x01 && KEYBOARD_OR_MOUSE.contains(&usage))
    {
        0
    } else if apps.iter().any(|&(page, _)| page < 0xFF00) {
        1
    } else {
        2
    }
}

/// The gadget for a device list. At most `opts.max_functions` interfaces,
/// chosen by [`priority`] and then hub id (the first receiver wins a tie);
/// those are linked in hub id order so their numbers follow the
/// receiver's. `None` if nothing is left to present.
pub fn spec_for(devices: &[HidDevice], opts: &Options) -> Option<GadgetSpec> {
    let mut devs: Vec<(&HidDevice, DescriptorInfo)> = devices
        .iter()
        .map(|d| (d, DescriptorInfo::parse(&d.report_descriptor)))
        .filter(|(_, info)| opts.expose_hidpp || !is_vendor_only(info))
        .collect();
    devs.sort_by_key(|(d, info)| (priority(info), d.id));
    let left_out: Vec<u8> = {
        let mut v: Vec<u8> = devs
            .iter()
            .skip(opts.max_functions)
            .map(|(d, _)| d.id)
            .collect();
        v.sort_unstable();
        v
    };
    devs.truncate(opts.max_functions);
    devs.sort_by_key(|(d, _)| d.id);
    let functions: Vec<FunctionSpec> = devs
        .iter()
        .map(|(d, info)| {
            let (subclass, protocol) = boot_class(info);
            FunctionSpec {
                dev_id: d.id,
                instance: format!("usb{}", d.id),
                subclass,
                protocol,
                report_length: report_length(info),
                report_desc: d.report_descriptor.clone(),
            }
        })
        .collect();
    // Identity of the lowest id overall, as before: the receiver the PC
    // sees even if its HID++ interface is hidden.
    let first = devices.iter().min_by_key(|d| d.id)?;
    if functions.is_empty() {
        return None;
    }
    Some(GadgetSpec {
        vendor_id: first.vid,
        product_id: first.pid,
        manufacturer: if first.vid == 0x046d {
            "Logitech".into()
        } else {
            "RawKVM".into()
        },
        product: first.name.clone(),
        serial: first.serial.clone().unwrap_or_else(|| opts.serial.clone()),
        functions,
        left_out,
    })
}
