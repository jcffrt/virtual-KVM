//! Keeps the receiver's hidraw interfaces open and feeds the router.
//!
//! A scan thread looks for matching interfaces every few seconds, so the
//! receiver can be plugged in after the hub started or re-plugged. Each open
//! interface gets a blocking reader thread. Device ids are keyed by the
//! physical path and stay stable while the hub runs.

use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use proto::descriptor::DescriptorInfo;
use proto::HidDevice;
use tracing::{error, info, warn};

use crate::config::{MatchMode, ReceiverMatch};
use crate::hidraw::{self, RawInterface, BUS_USB};
use crate::receiver;
use crate::router::Router;

const SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Should this hidraw node be forwarded?
///
/// * `receiver` (mode A): the receiver's own USB interfaces. Requires
///   `hid-logitech-dj` to be blacklisted so `hid-generic` binds them.
/// * `dj-children` (mode B): the per-device HID nodes `hid-logitech-dj`
///   synthesizes under the receiver.
pub fn is_forwarded(iface: &RawInterface, m: &ReceiverMatch) -> bool {
    if iface.bus != BUS_USB {
        return false;
    }
    match m.mode {
        MatchMode::Receiver => m.matches(iface.vid, iface.pid) && iface.parent_hid.is_none(),
        MatchMode::DjChildren => iface
            .parent_hid
            .is_some_and(|(bus, vid, pid)| bus == BUS_USB && m.matches(vid, pid)),
    }
}

pub fn to_hid_device(iface: &RawInterface, id: u8) -> HidDevice {
    HidDevice {
        id,
        name: iface.name.clone(),
        vid: iface.vid,
        pid: iface.pid,
        serial: iface.serial.clone(),
        interface: iface.interface(),
        report_descriptor: iface.descriptor.clone(),
        uses_report_ids: DescriptorInfo::parse(&iface.descriptor).uses_report_ids,
    }
}

/// Why `iface` must not be opened despite matching, if it must not.
pub fn refusal(iface: &RawInterface, m: &ReceiverMatch) -> Option<String> {
    if m.mode != MatchMode::Receiver || !m.require_hid_generic {
        return None;
    }
    match iface.driver.as_deref() {
        Some("hid-generic") => None,
        driver => Some(format!(
            "{} is bound to {:?}, not hid-generic; not forwarding it. Probably \
             hid_logitech_dj was loaded from the initramfs: run `update-initramfs -u` \
             and reboot, or re-run deploy/hub-linux/install.sh (docs/adr/0003)",
            iface.hidraw.display(),
            driver.unwrap_or("no driver"),
        )),
    }
}

/// Device id for `iface`, stable while the hub runs. In `receiver` mode it
/// is the USB interface number (0 keyboard, 1 mouse, 2 HID++), so agents see
/// the same ids across hub restarts.
pub fn assign_id(
    ids: &mut HashMap<String, u8>,
    iface: &RawInterface,
    mode: MatchMode,
) -> Option<u8> {
    if let Some(&id) = ids.get(&iface.phys) {
        return Some(id);
    }
    let preferred = (mode == MatchMode::Receiver)
        .then_some(iface.usb_interface)
        .flatten()
        .filter(|i| !ids.values().any(|v| v == i));
    let id = preferred.or_else(|| (0..=u8::MAX).find(|i| !ids.values().any(|v| v == i)))?;
    ids.insert(iface.phys.clone(), id);
    Some(id)
}

/// Take the receiver out of DJ mode (see [`crate::receiver`]).
fn force_normal_mode(iface: &RawInterface, file: &File) {
    use std::io::Write;
    if !receiver::is_dj_interface(&DescriptorInfo::parse(&iface.descriptor)) {
        return;
    }
    let report = receiver::normal_mode_report();
    let mut f = file;
    match f.write_all(&report) {
        Ok(()) => info!("{}: DJ mode switched off", iface.hidraw.display()),
        Err(e) => warn!(
            "{}: switching DJ mode off failed: {e}",
            iface.hidraw.display()
        ),
    }
}

struct OpenIface {
    grabs: HashMap<PathBuf, File>,
}

/// Grab the evdev nodes of `iface` not grabbed yet. They may appear a moment
/// after the hidraw node, so this runs on every scan.
fn grab_new(iface: &RawInterface, grabs: &mut HashMap<PathBuf, File>) {
    grabs.retain(|p, _| iface.event_nodes.contains(p));
    for node in &iface.event_nodes {
        if grabs.contains_key(node) {
            continue;
        }
        match hidraw::grab(node) {
            Ok(f) => {
                grabs.insert(node.clone(), f);
            }
            Err(e) => warn!("{e:#}"),
        }
    }
}

pub fn spawn(router: Arc<Router>, m: ReceiverMatch) {
    thread::Builder::new()
        .name("hid-scan".into())
        .spawn(move || scan_loop(router, m))
        .expect("spawning the scan thread");
}

fn scan_loop(router: Arc<Router>, m: ReceiverMatch) {
    let open: Arc<Mutex<HashMap<String, OpenIface>>> = Arc::default();
    let mut ids: HashMap<String, u8> = HashMap::new();
    let mut warned_empty = false;
    // phys -> driver already reported as refused, to log once per change.
    let mut refused: HashMap<String, Option<String>> = HashMap::new();
    loop {
        let found = match hidraw::enumerate() {
            Ok(list) => list,
            Err(e) => {
                error!("enumerating hidraw: {e:#}");
                Vec::new()
            }
        };
        let matching: Vec<_> = found.into_iter().filter(|i| is_forwarded(i, &m)).collect();
        if matching.is_empty() && !warned_empty {
            warn!(
                mode = ?m.mode,
                "no matching hidraw node; waiting for the receiver (run --list to see what is there)"
            );
        }
        warned_empty = matching.is_empty();

        for iface in matching {
            if let Some(why) = refusal(&iface, &m) {
                if refused.get(&iface.phys) != Some(&iface.driver) {
                    error!("{why}");
                    refused.insert(iface.phys.clone(), iface.driver.clone());
                }
                continue;
            }
            refused.remove(&iface.phys);
            let mut open_g = open.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(o) = open_g.get_mut(&iface.phys) {
                if m.grab_evdev {
                    grab_new(&iface, &mut o.grabs);
                }
                continue;
            }
            let Some(id) = assign_id(&mut ids, &iface, m.mode) else {
                error!("out of device ids");
                continue;
            };
            let file = match hidraw::open(&iface.hidraw) {
                Ok(f) => f,
                Err(e) => {
                    warn!("{e:#}");
                    continue;
                }
            };
            if m.mode == MatchMode::Receiver && m.force_normal_mode {
                force_normal_mode(&iface, &file);
            }
            if m.mode == MatchMode::Receiver && iface.driver.as_deref() != Some("hid-generic") {
                warn!(
                    "{} is bound to {:?}, not hid-generic; reports may not be raw (see docs/adr/0003)",
                    iface.hidraw.display(),
                    iface.driver
                );
            }
            let mut o = OpenIface {
                grabs: HashMap::new(),
            };
            if m.grab_evdev {
                grab_new(&iface, &mut o.grabs);
            }
            open_g.insert(iface.phys.clone(), o);
            drop(open_g);

            info!(
                "opened {} ({}, interface {})",
                iface.hidraw.display(),
                iface.phys,
                iface.interface()
            );
            router.add_device(to_hid_device(&iface, id));

            let router = router.clone();
            let open = open.clone();
            let phys = iface.phys.clone();
            let path = iface.hidraw.clone();
            let spawned = thread::Builder::new()
                .name(format!("hid-{id}"))
                .spawn(move || {
                    let err = hidraw::read_loop(file, |r| router.on_report(id, r));
                    info!("{} closed: {err}", path.display());
                    router.remove_device(id);
                    open.lock().unwrap_or_else(|e| e.into_inner()).remove(&phys);
                });
            if let Err(e) = spawned {
                error!("spawning reader: {e}");
            }
        }
        thread::sleep(SCAN_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(driver: Option<&str>) -> RawInterface {
        RawInterface {
            hidraw: PathBuf::from("/dev/hidraw0"),
            phys: "usb-5200000.usb-1/input2".into(),
            name: "Logitech USB Receiver".into(),
            bus: BUS_USB,
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            usb_interface: Some(2),
            dj_index: None,
            parent_hid: None,
            driver: driver.map(Into::into),
            descriptor: Vec::new(),
            event_nodes: Vec::new(),
        }
    }

    #[test]
    fn receiver_mode_refuses_the_dj_driver() {
        let m = ReceiverMatch::default();
        assert!(is_forwarded(&iface(Some("logitech-djreceiver")), &m));
        let why = refusal(&iface(Some("logitech-djreceiver")), &m).unwrap();
        assert!(why.contains("logitech-djreceiver") && why.contains("update-initramfs"));
        assert!(refusal(&iface(None), &m).is_some());
        assert!(refusal(&iface(Some("hid-generic")), &m).is_none());

        let lax = ReceiverMatch {
            require_hid_generic: false,
            ..m.clone()
        };
        assert!(refusal(&iface(Some("logitech-djreceiver")), &lax).is_none());
        let dj = ReceiverMatch {
            mode: MatchMode::DjChildren,
            ..m
        };
        assert!(refusal(&iface(Some("logitech-hidpp-device")), &dj).is_none());
    }
}
