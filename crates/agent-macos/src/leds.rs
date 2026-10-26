//! Keyboard LEDs of the Mac, for the real keyboard on the hub.
//!
//! The Karabiner daemon tells its clients nothing about LEDs, but the
//! virtual keyboard's driver echoes every LED output report macOS sets
//! (report ID 5) as an input report on the LED page (report ID 6;
//! `org_pqrs_Karabiner_DriverKit_VirtualHIDKeyboard.cpp`, `setReport`).
//! Karabiner-Elements reads it the same way: it opens the virtual keyboard
//! with IOHIDManager, not seized, and watches page 0x08 values. A thread
//! here does that and turns the state into a boot keyboard LED byte (bit 0
//! Num Lock, bit 1 Caps Lock, bit 2 Scroll Lock) for every keyboard
//! interface of the hub's receivers, as macOS does with its own keyboards.
//!
//! Opening a keyboard needs Input Monitoring (TCC) approval for the agent
//! binary; without it the open fails and only a warning is logged. The
//! state is known from the first change after the open.
//!
//! Karabiner creates the virtual keyboard again whenever its parameters
//! change, and the agent changes them as soon as it gets the hub's device
//! list (the keyboard takes the receiver's vid and pid). The watcher must
//! follow the new device: every few seconds it enumerates the HID devices
//! with a fresh IOHIDManager (an unscheduled manager keeps returning the
//! set it saw first) and compares registry entry ids. Watching the first
//! keyboard only, it stayed on a dead device and never saw an LED.
// Off macOS only the stub of `ffi` is built.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use agent_core::{OutputReport, OutputSender};
use proto::descriptor::{DescriptorInfo, USAGE_KEYBOARD, USAGE_PAGE_GENERIC_DESKTOP};
use proto::HidDevice;
use tracing::{debug, warn};

const MANUFACTURER: &str = "pqrs.org";
const PRODUCT_PREFIX: &str = "Karabiner DriverKit VirtualHIDKeyboard";
const USAGE_PAGE_LEDS: u32 = 0x08;
const RESCAN: Duration = Duration::from_secs(5);

/// The hub devices and report IDs of the keyboard LED reports: one per
/// receiver keyboard (with two receivers, the last one used to win alone).
pub type Target = Arc<Mutex<Vec<(u8, Option<u8>)>>>;

/// The keyboard LED output report of `dev`, if it has one: one byte on a
/// keyboard application collection.
pub fn led_report(dev: &HidDevice) -> Option<(u8, Option<u8>)> {
    DescriptorInfo::parse(&dev.report_descriptor)
        .outputs
        .iter()
        .find(|r| {
            r.app_usage_page == USAGE_PAGE_GENERIC_DESKTOP
                && r.app_usage == USAGE_KEYBOARD
                && r.len == 1
        })
        .map(|r| (dev.id, r.report_id))
}

/// Bit of the boot keyboard LED byte for an LED page usage.
fn led_bit(usage: u32) -> Option<u8> {
    match usage {
        1 => Some(0x01), // Num Lock
        2 => Some(0x02), // Caps Lock
        3 => Some(0x04), // Scroll Lock
        _ => None,
    }
}

/// The LED byte after a value of `usage` changed to `on`.
pub fn apply(leds: u8, usage: u32, on: bool) -> u8 {
    match led_bit(usage) {
        Some(b) if on => leds | b,
        Some(b) => leds & !b,
        None => leds,
    }
}

/// The output report for `target` with `leds`.
pub fn report(target: (u8, Option<u8>), leds: u8) -> OutputReport {
    let (dev_id, rid) = target;
    OutputReport {
        dev_id,
        report: match rid {
            Some(id) => vec![id, leds],
            None => vec![leds],
        },
    }
}

/// Watch the virtual keyboard's LEDs from a thread of its own.
pub fn spawn(tx: OutputSender, target: Target) {
    let spawned = thread::Builder::new()
        .name("leds".into())
        .spawn(move || ffi::run(tx, target));
    if let Err(e) = spawned {
        warn!("keyboard LEDs not watched: {e}");
    }
}

/// What the watcher does after a scan, from the registry entry id of the
/// device it watches and the one it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Keep,
    /// The device is gone: stop watching it.
    Close,
    /// Watch the found device (closing the current one, if any).
    Open(u64),
}

fn step(current: Option<u64>, found: Option<u64>) -> Step {
    match (current, found) {
        (Some(c), Some(f)) if c == f => Step::Keep,
        (_, Some(f)) => Step::Open(f),
        (Some(_), None) => Step::Close,
        (None, None) => Step::Keep,
    }
}

/// What the input value callback needs; lives as long as the thread.
struct Ctx {
    tx: OutputSender,
    target: Target,
    leds: Mutex<u8>,
}

impl Ctx {
    /// A new virtual keyboard: its LEDs are unknown until macOS sets them.
    fn forget(&self) {
        *self.leds.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    }

    fn on_value(&self, usage_page: u32, usage: u32, value: isize) {
        if usage_page != USAGE_PAGE_LEDS {
            return;
        }
        let mut leds = self.leds.lock().unwrap_or_else(|e| e.into_inner());
        let new = apply(*leds, usage, value != 0);
        if new == *leds {
            return;
        }
        *leds = new;
        debug!(leds = new, "Mac keyboard LEDs");
        let targets = self
            .target
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for t in targets {
            let _ = self.tx.send(report(t, new));
        }
    }
}

/// Off macOS (CI builds the workspace on Linux) there is nothing to watch.
#[cfg(not(target_os = "macos"))]
mod ffi {
    use super::*;

    pub(super) fn run(tx: OutputSender, target: Target) {
        drop((tx, target));
        warn!("keyboard LEDs are only watched on macOS");
    }
}

#[cfg(target_os = "macos")]
mod ffi {
    use super::*;
    use std::ffi::{c_char, c_void, CStr, CString};
    use tracing::info;

    type CFIndex = isize;
    type IOReturn = i32;
    type Ref = *mut c_void;
    type ValueCallback =
        extern "C" fn(context: *mut c_void, result: IOReturn, sender: *mut c_void, value: Ref);

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CF_RUN_LOOP_RUN_FINISHED: i32 = 1;
    const K_IOHID_OPTIONS_TYPE_NONE: u32 = 0;
    const K_IO_RETURN_NOT_PERMITTED: IOReturn = 0xE000_02E2_u32 as i32;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> Ref;
        fn IOHIDManagerSetDeviceMatching(manager: Ref, matching: *const c_void);
        fn IOHIDManagerCopyDevices(manager: Ref) -> Ref;
        fn IOHIDDeviceGetService(device: Ref) -> u32;
        fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
        fn IOHIDDeviceGetProperty(device: Ref, key: *const c_void) -> *const c_void;
        fn IOHIDDeviceOpen(device: Ref, options: u32) -> IOReturn;
        fn IOHIDDeviceClose(device: Ref, options: u32) -> IOReturn;
        fn IOHIDDeviceRegisterInputValueCallback(
            device: Ref,
            callback: Option<ValueCallback>,
            context: *mut c_void,
        );
        fn IOHIDDeviceScheduleWithRunLoop(device: Ref, run_loop: Ref, mode: *const c_void);
        fn IOHIDDeviceUnscheduleFromRunLoop(device: Ref, run_loop: Ref, mode: *const c_void);
        fn IOHIDValueGetElement(value: Ref) -> Ref;
        fn IOHIDValueGetIntegerValue(value: Ref) -> CFIndex;
        fn IOHIDElementGetUsagePage(element: Ref) -> u32;
        fn IOHIDElementGetUsage(element: Ref) -> u32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRelease(cf: *const c_void);
        fn CFRetain(cf: *const c_void) -> *const c_void;
        fn CFSetGetCount(set: *const c_void) -> CFIndex;
        fn CFSetGetValues(set: *const c_void, values: *mut *const c_void);
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            s: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFStringGetCString(
            s: *const c_void,
            buf: *mut c_char,
            size: CFIndex,
            encoding: u32,
        ) -> u8;
        fn CFRunLoopGetCurrent() -> Ref;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after: u8) -> i32;
    }

    /// An owned CFString.
    struct CfStr(*const c_void);

    impl CfStr {
        fn new(s: &str) -> Self {
            let c = CString::new(s).expect("no NUL");
            // SAFETY: valid C string; the result is released in Drop.
            Self(unsafe {
                CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
            })
        }
    }

    impl Drop for CfStr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: created by us.
                unsafe { CFRelease(self.0) }
            }
        }
    }

    /// A string property of `device` (get rule: not owned).
    fn string_property(device: Ref, key: &CfStr) -> Option<String> {
        // SAFETY: valid device and key; the value is not retained.
        let v = unsafe { IOHIDDeviceGetProperty(device, key.0) };
        // SAFETY: CFGetTypeID accepts any CF object.
        if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
            return None;
        }
        let mut buf = [0 as c_char; 256];
        // SAFETY: v is a CFString; the buffer size is passed.
        let ok = unsafe {
            CFStringGetCString(
                v,
                buf.as_mut_ptr(),
                buf.len() as CFIndex,
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: CFStringGetCString NUL-terminated it.
        Some(
            unsafe { CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    extern "C" fn on_value(
        context: *mut c_void,
        _result: IOReturn,
        _sender: *mut c_void,
        value: Ref,
    ) {
        if context.is_null() || value.is_null() {
            return;
        }
        // SAFETY: context is the leaked Ctx of `run`; value is valid during
        // the callback.
        let (ctx, page, usage, v) = unsafe {
            let element = IOHIDValueGetElement(value);
            (
                &*(context as *const Ctx),
                IOHIDElementGetUsagePage(element),
                IOHIDElementGetUsage(element),
                IOHIDValueGetIntegerValue(value),
            )
        };
        ctx.on_value(page, usage, v);
    }

    /// The registry entry id of `device`, which tells two incarnations of
    /// the virtual keyboard apart.
    fn registry_id(device: Ref) -> Option<u64> {
        let mut id = 0u64;
        // SAFETY: valid device; the service is not retained (get rule).
        let r =
            unsafe { IORegistryEntryGetRegistryEntryID(IOHIDDeviceGetService(device), &mut id) };
        (r == 0).then_some(id)
    }

    /// The Karabiner virtual keyboard now, retained, with its registry id:
    /// enumerated with a fresh manager every time.
    fn find_keyboard(manufacturer: &CfStr, product: &CfStr) -> Option<(Ref, u64)> {
        // SAFETY: plain IOKit calls; manager and set are released below.
        let manager = unsafe { IOHIDManagerCreate(std::ptr::null(), 0) };
        if manager.is_null() {
            return None;
        }
        // SAFETY: null matching = every HID device; filtered below.
        unsafe { IOHIDManagerSetDeviceMatching(manager, std::ptr::null()) };
        // SAFETY: the set is owned by us and released below.
        let set = unsafe { IOHIDManagerCopyDevices(manager) };
        let mut found = None;
        if !set.is_null() {
            // SAFETY: valid set; the buffer has room for every value.
            let devices = unsafe {
                let n = CFSetGetCount(set).max(0) as usize;
                let mut v = vec![std::ptr::null::<c_void>(); n];
                CFSetGetValues(set, v.as_mut_ptr());
                v
            };
            found = devices
                .into_iter()
                .map(|d| d as Ref)
                .filter(|&d| {
                    string_property(d, manufacturer).as_deref() == Some(MANUFACTURER)
                        && string_property(d, product)
                            .is_some_and(|p| p.starts_with(PRODUCT_PREFIX))
                })
                .find_map(|d| registry_id(d).map(|id| (d, id)))
                .map(|(d, id)| {
                    // SAFETY: keep it beyond the set and the manager.
                    unsafe { CFRetain(d) };
                    (d, id)
                });
            // SAFETY: copied above.
            unsafe { CFRelease(set) };
        }
        // SAFETY: created above.
        unsafe { CFRelease(manager) };
        found
    }

    pub(super) fn run(tx: OutputSender, target: Target) {
        let ctx: &'static Ctx = Box::leak(Box::new(Ctx {
            tx,
            target,
            leds: Mutex::new(0),
        }));
        let manufacturer = CfStr::new("Manufacturer");
        let product = CfStr::new("Product");
        // SAFETY: the current thread's run loop and the default mode.
        let (run_loop, mode) = unsafe { (CFRunLoopGetCurrent(), kCFRunLoopDefaultMode) };
        // The device watched, retained and scheduled, and its registry id.
        let mut current: Option<(Ref, u64)> = None;
        let mut warned = false;
        loop {
            let mut found = find_keyboard(&manufacturer, &product);
            let close_current = |current: &mut Option<(Ref, u64)>| {
                if let Some((d, _)) = current.take() {
                    // SAFETY: d was opened, scheduled and retained by us.
                    unsafe {
                        IOHIDDeviceUnscheduleFromRunLoop(d, run_loop, mode);
                        IOHIDDeviceClose(d, K_IOHID_OPTIONS_TYPE_NONE);
                        CFRelease(d);
                    }
                }
            };
            match step(current.map(|c| c.1), found.map(|f| f.1)) {
                Step::Keep => {}
                Step::Close => {
                    info!("the Karabiner virtual keyboard is gone");
                    close_current(&mut current);
                }
                Step::Open(id) => {
                    if current.is_some() {
                        info!(
                            registry_id = id,
                            "the Karabiner virtual keyboard was created again"
                        );
                    }
                    close_current(&mut current);
                    // Taken: watched from here, or released if it cannot be.
                    let (d, _) = found.take().expect("found when the step is Open");
                    // SAFETY: valid device; not seized, like Karabiner-Elements.
                    let r = unsafe { IOHIDDeviceOpen(d, K_IOHID_OPTIONS_TYPE_NONE) };
                    if r == 0 {
                        ctx.forget();
                        // SAFETY: d is retained while registered; ctx is 'static.
                        unsafe {
                            IOHIDDeviceRegisterInputValueCallback(
                                d,
                                Some(on_value),
                                ctx as *const Ctx as *mut c_void,
                            );
                            IOHIDDeviceScheduleWithRunLoop(d, run_loop, mode);
                        }
                        info!(
                            registry_id = id,
                            "watching the keyboard LEDs of the Karabiner virtual keyboard"
                        );
                        // Retained by find_keyboard: released with `current`.
                        current = Some((d, id));
                        warned = false;
                    } else {
                        // SAFETY: retained by find_keyboard.
                        unsafe { CFRelease(d) };
                        if !warned {
                            let why = if r == K_IO_RETURN_NOT_PERMITTED {
                                "not permitted: allow the agent in System Settings > Privacy & \
                                 Security > Input Monitoring (docs/macos-instalacion.md)"
                            } else {
                                "IOHIDDeviceOpen failed"
                            };
                            warn!(code = format!("{r:#x}"), "keyboard LEDs not watched: {why}");
                            warned = true;
                        }
                    }
                }
            }
            if let Some((d, _)) = found {
                // SAFETY: retained by find_keyboard and not kept.
                unsafe { CFRelease(d) };
            }
            // SAFETY: runs this thread's run loop (the callbacks) for a while.
            let r = unsafe { CFRunLoopRunInMode(mode, RESCAN.as_secs_f64(), 0) };
            if r == K_CF_RUN_LOOP_RUN_FINISHED {
                // No source scheduled: the call returned at once.
                thread::sleep(RESCAN);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::tests::{BOOT_KEYBOARD, UNIFYING_IFACE1};

    fn dev(id: u8, desc: &[u8]) -> HidDevice {
        HidDevice {
            id,
            name: "Logitech USB Receiver".into(),
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            interface: id,
            report_descriptor: desc.to_vec(),
            uses_report_ids: false,
            receiver: 0,
        }
    }

    #[test]
    fn leds_become_a_boot_keyboard_report() {
        assert_eq!(led_report(&dev(0, BOOT_KEYBOARD)), Some((0, None)));
        assert_eq!(led_report(&dev(1, UNIFYING_IFACE1)), None);
        let mut leds = apply(0, 2, true);
        assert_eq!(leds, 0x02);
        leds = apply(leds, 1, true);
        assert_eq!(leds, 0x03);
        leds = apply(leds, 2, false);
        assert_eq!(leds, 0x01);
        assert_eq!(apply(leds, 0x4B, true), leds);
        assert_eq!(report((0, None), 0x02).report, [0x02]);
        assert_eq!(report((3, Some(1)), 0x02).report, [1, 0x02]);
    }

    #[test]
    fn only_changes_of_led_values_are_sent() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = Ctx {
            tx,
            target: Arc::new(Mutex::new(vec![(0, None)])),
            leds: Mutex::new(0),
        };
        ctx.on_value(USAGE_PAGE_LEDS, 2, 1);
        ctx.on_value(USAGE_PAGE_LEDS, 2, 1);
        ctx.on_value(0x07, 0x39, 1); // the Caps Lock key itself
        ctx.on_value(USAGE_PAGE_LEDS, 2, 0);
        let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|o| o.report)
            .collect();
        assert_eq!(got, [vec![0x02], vec![0x00]]);

        // Two receivers (Logitech id 0, Compx id 16 with report ID 1): both
        // keyboards get the LEDs, not only the last one listed.
        *ctx.target.lock().unwrap() = vec![(0, None), (16, Some(1))];
        ctx.on_value(USAGE_PAGE_LEDS, 2, 1);
        let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|o| (o.dev_id, o.report))
            .collect();
        assert_eq!(got, [(0, vec![0x02]), (16, vec![1, 0x02])]);
    }

    /// The virtual keyboard is created again when the agent sets its
    /// identity: the watcher must move to the new one.
    #[test]
    fn the_watcher_follows_a_recreated_keyboard() {
        assert_eq!(step(None, None), Step::Keep);
        assert_eq!(step(None, Some(7)), Step::Open(7));
        assert_eq!(step(Some(7), Some(7)), Step::Keep);
        assert_eq!(step(Some(7), Some(9)), Step::Open(9));
        assert_eq!(step(Some(9), None), Step::Close);

        // A new keyboard starts from unknown LEDs: the first value is sent
        // even if it equals what the old one had.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = Ctx {
            tx,
            target: Arc::new(Mutex::new(vec![(0, None)])),
            leds: Mutex::new(0),
        };
        ctx.on_value(USAGE_PAGE_LEDS, 2, 1);
        ctx.forget();
        ctx.on_value(USAGE_PAGE_LEDS, 2, 1);
        assert_eq!(std::iter::from_fn(|| rx.try_recv().ok()).count(), 2);
    }
}
