//! `HidSink` that presents the hub's devices to a PC as a USB gadget.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use agent_core::{HidSink, OutputReceiver, OutputSender};
use anyhow::{anyhow, Context, Result};
use proto::descriptor::{DescriptorInfo, USAGE_MOUSE, USAGE_PAGE_GENERIC_DESKTOP};
use proto::HidDevice;
use tracing::{debug, info, warn};

use crate::configfs::{self, Backend, GadgetSpec, Options, RealFs};
use crate::hidg::{self, Hidg};

/// Where things live; overridable for tests.
#[derive(Debug, Clone)]
pub struct Paths {
    pub configfs: PathBuf,
    pub gadget_name: String,
    pub udc_class: PathBuf,
    pub sys_dev_char: PathBuf,
    /// UDC to bind to; `None` = the first one in `udc_class`.
    pub udc: Option<String>,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            configfs: PathBuf::from(configfs::DEFAULT_ROOT),
            gadget_name: configfs::DEFAULT_NAME.into(),
            udc_class: PathBuf::from("/sys/class/udc"),
            sys_dev_char: PathBuf::from("/sys/dev/char"),
            udc: None,
        }
    }
}

/// Mouse motion is worth little once stale; keys are not.
const PATIENCE_MOUSE: Duration = Duration::from_millis(8);
const PATIENCE_OTHER: Duration = Duration::from_millis(50);

/// The hub announces the receiver's interfaces one by one as it opens them;
/// rebuilding for each would re-enumerate the gadget on the PC every time.
pub const SETTLE_TIME: Duration = Duration::from_millis(500);

/// Whether the PC on the cable is reading reports. While it is not (off,
/// suspended, unplugged, not enumerated yet), writes are tried without
/// waiting and the change is logged once instead of per report.
#[derive(Debug, Default)]
pub struct HostState {
    away: bool,
}

#[derive(Debug, PartialEq)]
pub enum HostChange {
    Lost,
    Back,
}

impl HostState {
    pub fn is_away(&self) -> bool {
        self.away
    }

    /// Record the outcome of one write.
    pub fn on_write(&mut self, delivered: bool) -> Option<HostChange> {
        match (self.away, delivered) {
            (false, false) => {
                self.away = true;
                Some(HostChange::Lost)
            }
            (true, true) => {
                self.away = false;
                Some(HostChange::Back)
            }
            _ => None,
        }
    }
}

struct Port {
    hidg: Option<Hidg>,
    info: DescriptorInfo,
    release: Vec<Vec<u8>>,
}

impl Port {
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

enum BuildError {
    /// `usb_f_hid` ran out of instances after creating `fitted` functions.
    NoRoom {
        fitted: usize,
    },
    Other(anyhow::Error),
}

impl From<anyhow::Error> for BuildError {
    fn from(e: anyhow::Error) -> Self {
        BuildError::Other(e)
    }
}

pub struct GadgetSink {
    paths: Paths,
    opts: Options,
    fs: Box<dyn Backend + Send>,
    pending: Vec<HidDevice>,
    /// The device list the gadget was last built for, sorted by id.
    built_from: Vec<HidDevice>,
    spec: Option<GadgetSpec>,
    /// Devices that should be on the PC and are not (ADR 0031).
    unexposed: Vec<u8>,
    ports: BTreeMap<u8, Port>,
    dropped: u64,
    host: HostState,
    outputs: (OutputSender, Option<OutputReceiver>),
}

impl GadgetSink {
    pub fn new(paths: Paths, opts: Options) -> Self {
        Self::with_backend(paths, opts, Box::new(RealFs))
    }

    /// With other filesystem primitives (tests: a simulated ConfigFS).
    pub fn with_backend(paths: Paths, opts: Options, fs: Box<dyn Backend + Send>) -> Self {
        Self {
            paths,
            opts,
            fs,
            pending: Vec::new(),
            built_from: Vec::new(),
            spec: None,
            unexposed: Vec::new(),
            ports: BTreeMap::new(),
            dropped: 0,
            host: HostState::default(),
            outputs: {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                (tx, Some(rx))
            },
        }
    }

    fn gadget_dir(&self) -> PathBuf {
        self.paths.configfs.join(&self.paths.gadget_name)
    }

    /// Remove our gadget (also one left over by a crashed run).
    pub fn teardown(&mut self) -> Result<()> {
        self.ports.clear(); // close /dev/hidgN first
        self.spec = None;
        let g = self.gadget_dir();
        if !g.exists() {
            return Ok(());
        }
        let ops = configfs::teardown_ops(&g, &configfs::inspect(&g));
        configfs::apply_on(&ops, self.fs.as_mut())
            .with_context(|| format!("removing {}", g.display()))?;
        info!("USB gadget removed");
        Ok(())
    }

    fn build(&mut self, spec: GadgetSpec) -> Result<(), BuildError> {
        self.teardown()?;
        let g = self.gadget_dir();
        if let Err(e) = configfs::apply_on(&configfs::build_ops(&g, &spec), self.fs.as_mut()) {
            // usb_f_hid is out of instances (HIDG_MINORS, or minors held
            // by another gadget): build with the functions that fitted.
            let fitted = e
                .failed_function(&g)
                .and_then(|inst| spec.functions.iter().position(|f| f.instance == inst));
            if let Some(fitted) = fitted.filter(|&k| k > 0) {
                return Err(BuildError::NoRoom { fitted });
            }
            let hint = if fitted == Some(0) {
                " (no free HID function: another gadget holds all of usb_f_hid's instances)"
            } else if e.is_missing_support() {
                " (is libcomposite loaded and ConfigFS mounted?)"
            } else {
                ""
            };
            return Err(anyhow::Error::new(e)
                .context(format!("creating {}{hint}", g.display()))
                .into());
        }
        let udc = match &self.paths.udc {
            Some(u) => u.clone(),
            None => configfs::find_udc(&self.paths.udc_class).ok_or_else(|| {
                anyhow!(
                    "no USB device controller in {} (dtoverlay=dwc2 / OTG port in peripheral mode?)",
                    self.paths.udc_class.display()
                )
            })?,
        };
        self.fs
            .write(&g.join("UDC"), format!("{udc}\n").as_bytes())
            .with_context(|| format!("binding to {udc} (is another gadget using it?)"))?;
        info!(
            "USB gadget {:04x}:{:04x} {:?} bound to {udc} with {} interfaces",
            spec.vendor_id,
            spec.product_id,
            spec.product,
            spec.functions.len()
        );

        for f in &spec.functions {
            let dev = self
                .pending
                .iter()
                .find(|d| d.id == f.dev_id)
                .expect("spec comes from pending");
            let info = DescriptorInfo::parse(&dev.report_descriptor);
            let func_dir = g.join("functions").join(format!("hid.{}", f.instance));
            let hidg = hidg::device_node(&func_dir, &self.paths.sys_dev_char).and_then(|node| {
                hidg::wait_for(&node, Duration::from_secs(2))?;
                Hidg::open(&node, Some((f.dev_id, self.outputs.0.clone())))
            });
            let hidg = match hidg {
                Ok(h) => {
                    info!(id = f.dev_id, "interface ready on {}", h.path().display());
                    Some(h)
                }
                Err(e) => {
                    warn!(id = f.dev_id, "opening the gadget device: {e}");
                    None
                }
            };
            self.ports.insert(
                f.dev_id,
                Port {
                    hidg,
                    release: info.release_reports(),
                    info,
                },
            );
        }
        self.spec = Some(spec);
        Ok(())
    }

    fn release_all(&mut self) {
        let patience = if self.host.is_away() {
            Duration::ZERO
        } else {
            PATIENCE_OTHER
        };
        for p in self.ports.values() {
            if let Some(h) = &p.hidg {
                for r in &p.release {
                    let _ = h.write_report(r, patience);
                }
            }
        }
    }

    fn note_write(&mut self, delivered: bool) {
        if !delivered {
            self.dropped += 1;
        }
        match self.host.on_write(delivered) {
            Some(HostChange::Lost) => info!(
                "the USB host is not reading reports (PC off, suspended or unplugged?); \
                 dropping them until it is back"
            ),
            Some(HostChange::Back) => info!(
                dropped = self.dropped,
                "the USB host is reading reports again"
            ),
            None => {}
        }
    }

    /// Release everything and remove the gadget (on exit).
    pub fn shutdown(&mut self) {
        self.release_all();
        if let Err(e) = self.teardown() {
            warn!("{e:#}");
        }
    }

    pub fn is_bound(&self) -> bool {
        self.spec.is_some()
    }
}

impl HidSink for GadgetSink {
    fn create(&mut self, dev: &HidDevice) -> Result<()> {
        self.pending.push(dev.clone());
        Ok(())
    }

    fn settle_time(&self) -> Duration {
        SETTLE_TIME
    }

    fn devices_ready(&mut self) {
        let mut devices = self.pending.clone();
        devices.sort_by_key(|d| d.id);
        if self.spec.is_some() && devices == self.built_from {
            // Same receivers as before a reconnection: keep the gadget, the
            // PC sees no USB disconnect.
            debug!("device list unchanged; keeping the USB gadget");
            return;
        }
        // A new list: try the full limit again, even if a previous build
        // found the kernel had less room.
        let mut opts = self.opts.clone();
        loop {
            let Some(spec) = configfs::spec_for(&devices, &opts) else {
                warn!("no device to present over USB");
                break;
            };
            if let Some(cur) = &mut self.spec {
                if cur.same_gadget(&spec) {
                    debug!("same interfaces fit; keeping the USB gadget");
                    cur.left_out = spec.left_out;
                    break;
                }
            }
            match self.build(spec) {
                Ok(()) => break,
                Err(BuildError::NoRoom { fitted }) => {
                    warn!(
                        "the kernel has room for only {fitted} HID functions \
                         (usb_f_hid allows {} in all); building the gadget with {fitted}",
                        configfs::HIDG_MINORS
                    );
                    opts.max_functions = fitted;
                }
                Err(BuildError::Other(e)) => {
                    warn!("{e:#}");
                    // Do not hold usb_f_hid instances with a half-built tree.
                    if let Err(e) = self.teardown() {
                        warn!("{e:#}");
                    }
                    break;
                }
            }
        }
        self.built_from = devices;
        self.unexposed = match &self.spec {
            Some(spec) => spec.left_out.clone(),
            // Nothing bound: every device that should be there is missing.
            None => self
                .built_from
                .iter()
                .filter(|d| {
                    self.opts.expose_hidpp
                        || !configfs::is_vendor_only(&DescriptorInfo::parse(&d.report_descriptor))
                })
                .map(|d| d.id)
                .collect(),
        };
        // An emptied list keeps the old gadget; nothing is missing then.
        let listed: Vec<u8> = self.built_from.iter().map(|d| d.id).collect();
        self.unexposed.retain(|id| listed.contains(id));
        if !self.unexposed.is_empty() {
            let names: Vec<String> = self
                .unexposed
                .iter()
                .filter_map(|id| self.built_from.iter().find(|d| d.id == *id))
                .map(|d| format!("{} ({}, interface {})", d.id, d.name, d.interface))
                .collect();
            warn!(
                "not presented to the PC on the USB cable: {}",
                names.join(", ")
            );
        }
    }

    fn unexposed(&self) -> Vec<u8> {
        self.unexposed.clone()
    }

    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()> {
        let Some(port) = self.ports.get(&dev_id) else {
            return Ok(()); // not exposed (HID++ hidden) or not bound
        };
        let Some(h) = &port.hidg else {
            return Ok(());
        };
        let patience = if self.host.is_away() {
            Duration::ZERO
        } else if port.is_mouse_report(report) {
            PATIENCE_MOUSE
        } else {
            PATIENCE_OTHER
        };
        let delivered = h.write_report(report, patience)?;
        self.note_write(delivered);
        Ok(())
    }

    fn take_outputs(&mut self) -> Option<OutputReceiver> {
        self.outputs.1.take()
    }

    /// The hub connection ended: release keys but keep the gadget, so a
    /// reconnection does not re-enumerate on the PC.
    fn destroy_all(&mut self) {
        self.release_all();
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configfs::tests::{two_receivers, SimConfigFs};
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn fixture(name: &str) -> Option<Vec<u8>> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/unifying-c52b")
            .join(name);
        fs::read(p).ok()
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
            receiver: 0,
            report_descriptor: desc,
        }
    }

    #[test]
    fn builds_binds_and_keeps_the_gadget_across_reconnects() {
        let (Some(k), Some(m)) = (
            fixture("0003-046D-C52B.0007.nodj.bin"),
            fixture("0003-046D-C52B.0008.nodj.bin"),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("rawkvm-gsink-{}", uuid::Uuid::new_v4()));
        let paths = Paths {
            configfs: root.join("configfs"),
            gadget_name: "rawkvm".into(),
            udc_class: root.join("udc"),
            sys_dev_char: root.join("sys"),
            udc: None,
        };
        fs::create_dir_all(&paths.configfs).unwrap();
        fs::create_dir_all(paths.udc_class.join("3f980000.usb")).unwrap();
        let opts = Options {
            expose_hidpp: true,
            serial: "X".into(),
            max_functions: configfs::HIDG_MINORS,
        };
        let mut s = GadgetSink::new(paths.clone(), opts);

        s.create(&device(0, k.clone())).unwrap();
        s.create(&device(1, m.clone())).unwrap();
        s.devices_ready();
        assert!(s.is_bound());
        let g = paths.configfs.join("rawkvm");
        assert_eq!(fs::read_to_string(g.join("UDC")).unwrap(), "3f980000.usb\n");
        // No /dev/hidgN in a test: ports exist but are not open.
        assert_eq!(s.ports.len(), 2);
        s.input(0, &[0; 8]).unwrap();

        // Reconnection with the same devices: no rebuild.
        fs::write(g.join("marker"), "x").unwrap();
        s.destroy_all();
        s.create(&device(0, k)).unwrap();
        s.create(&device(1, m)).unwrap();
        s.devices_ready();
        assert!(g.join("marker").exists());

        fs::remove_dir_all(root).unwrap();
    }

    /// ConfigFS where `usb_f_hid` has room for `room` instances, like
    /// HIDG_MINORS (or fewer, if another gadget holds some).
    struct LimitedHid {
        sim: SimConfigFs,
        room: Arc<AtomicUsize>,
        live: usize,
        /// Fail the mkdir of the function after this many with EBUSY, once.
        busy_at: Option<usize>,
    }

    fn is_function(p: &Path) -> bool {
        p.parent()
            .and_then(|d| d.file_name())
            .is_some_and(|n| n == "functions")
    }

    impl Backend for LimitedHid {
        fn mkdir(&mut self, p: &Path) -> io::Result<()> {
            if is_function(p) {
                if self.busy_at == Some(self.live) {
                    self.busy_at = None;
                    return Err(io::Error::from_raw_os_error(libc::EBUSY));
                }
                if self.live >= self.room.load(Ordering::SeqCst) {
                    return Err(io::Error::from_raw_os_error(libc::ENODEV));
                }
                self.sim.mkdir(p)?;
                self.live += 1;
                return Ok(());
            }
            self.sim.mkdir(p)
        }
        fn write(&mut self, p: &Path, data: &[u8]) -> io::Result<()> {
            self.sim.write(p, data)
        }
        fn symlink(&mut self, target: &Path, link: &Path) -> io::Result<()> {
            self.sim.symlink(target, link)
        }
        fn unlink(&mut self, p: &Path) -> io::Result<()> {
            self.sim.unlink(p)
        }
        fn rmdir(&mut self, p: &Path) -> io::Result<()> {
            self.sim.rmdir(p)?;
            if is_function(p) {
                self.live -= 1;
            }
            Ok(())
        }
    }

    struct Rig {
        root: PathBuf,
        sink: GadgetSink,
        room: Arc<AtomicUsize>,
    }

    impl Rig {
        fn new(room: usize) -> Self {
            Self::with_busy(room, None)
        }

        fn with_busy(room: usize, busy_at: Option<usize>) -> Self {
            let root = std::env::temp_dir().join(format!("rawkvm-gsink-{}", uuid::Uuid::new_v4()));
            let paths = Paths {
                configfs: root.join("configfs"),
                gadget_name: "rawkvm".into(),
                udc_class: root.join("udc"),
                sys_dev_char: root.join("sys"),
                udc: None,
            };
            fs::create_dir_all(&paths.configfs).unwrap();
            fs::create_dir_all(paths.udc_class.join("fe980000.usb")).unwrap();
            let room = Arc::new(AtomicUsize::new(room));
            let backend = LimitedHid {
                sim: SimConfigFs {
                    root: paths.configfs.clone(),
                },
                room: room.clone(),
                live: 0,
                busy_at,
            };
            let opts = Options {
                expose_hidpp: true,
                serial: "X".into(),
                max_functions: configfs::HIDG_MINORS,
            };
            Rig {
                sink: GadgetSink::with_backend(paths, opts, Box::new(backend)),
                root,
                room,
            }
        }

        fn announce(&mut self, devices: &[HidDevice]) {
            self.sink.destroy_all();
            for d in devices {
                self.sink.create(d).unwrap();
            }
            self.sink.devices_ready();
        }

        fn functions(&self) -> Vec<String> {
            let mut v: Vec<String> = fs::read_dir(self.root.join("configfs/rawkvm/functions"))
                .map(|it| {
                    it.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn five_devices_bind_the_four_that_matter() {
        let Some(devs) = two_receivers() else { return };
        let mut rig = Rig::new(configfs::HIDG_MINORS);
        rig.announce(&devs);
        assert!(rig.sink.is_bound());
        assert_eq!(
            rig.functions(),
            ["hid.usb0", "hid.usb1", "hid.usb16", "hid.usb17"]
        );
        assert_eq!(rig.sink.unexposed(), [2]);
        // Reports for the left-out HID++ interface are dropped quietly.
        rig.sink.input(2, &[0x10, 0xff, 0, 0, 0, 0, 0]).unwrap();
    }

    #[test]
    fn less_room_than_expected_is_learned_and_retried_on_a_new_list() {
        let Some(devs) = two_receivers() else { return };
        // Another gadget holds one minor: the fourth mkdir fails.
        let mut rig = Rig::new(3);
        rig.announce(&devs);
        assert!(rig.sink.is_bound());
        assert_eq!(rig.functions(), ["hid.usb0", "hid.usb1", "hid.usb16"]);
        assert_eq!(rig.sink.unexposed(), [2, 17]);

        // The minor is free again, but the same list (a hub reconnection)
        // keeps the gadget: no re-enumeration on the PC.
        rig.room.store(configfs::HIDG_MINORS, Ordering::SeqCst);
        let marker = rig.root.join("configfs/rawkvm/marker");
        fs::write(&marker, "x").unwrap();
        rig.announce(&devs);
        assert!(marker.exists());
        assert_eq!(rig.sink.unexposed(), [2, 17]);

        // A new list is tried at full size again.
        let mut more = devs.clone();
        more.push(HidDevice {
            id: 18,
            interface: 2,
            ..devs[3].clone()
        });
        rig.announce(&more);
        assert_eq!(
            rig.functions(),
            ["hid.usb0", "hid.usb1", "hid.usb16", "hid.usb17"]
        );
        assert_eq!(rig.sink.unexposed(), [2, 18]);
    }

    #[test]
    fn no_room_at_all_reports_every_device() {
        let Some(devs) = two_receivers() else { return };
        let mut rig = Rig::new(0);
        rig.announce(&devs);
        assert!(!rig.sink.is_bound());
        assert_eq!(rig.sink.unexposed(), [0, 1, 2, 16, 17]);
        // The log says why, not "is libcomposite loaded?".
        let spec = configfs::spec_for(&devs, &rig.sink.opts).unwrap();
        let Err(BuildError::Other(e)) = rig.sink.build(spec) else {
            panic!("built without room");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("no free HID function"), "{msg}");
        assert!(!msg.contains("libcomposite"), "{msg}");
    }

    #[test]
    fn a_transient_error_does_not_shrink_the_gadget() {
        let Some(devs) = two_receivers() else { return };
        // The third function's mkdir fails with EBUSY: not a limit.
        let mut rig = Rig::with_busy(configfs::HIDG_MINORS, Some(2));
        rig.announce(&devs);
        assert!(!rig.sink.is_bound());
        // The half-built tree is gone at once (no instances held).
        assert!(!rig.root.join("configfs/rawkvm").exists());
        assert_eq!(rig.sink.unexposed(), [0, 1, 2, 16, 17]);

        // The same list again (hub reconnection) tries the full size.
        rig.announce(&devs);
        assert_eq!(
            rig.functions(),
            ["hid.usb0", "hid.usb1", "hid.usb16", "hid.usb17"]
        );
        assert_eq!(rig.sink.unexposed(), [2]);
    }

    #[test]
    fn host_state_logs_transitions_once() {
        let mut h = HostState::default();
        assert_eq!(h.on_write(true), None);
        assert_eq!(h.on_write(false), Some(HostChange::Lost));
        assert!(h.is_away());
        for _ in 0..1000 {
            assert_eq!(h.on_write(false), None);
        }
        assert_eq!(h.on_write(true), Some(HostChange::Back));
        assert!(!h.is_away());
    }

    #[test]
    fn missing_udc_is_reported() {
        let Some(k) = fixture("0003-046D-C52B.0007.nodj.bin") else {
            return;
        };
        let root = std::env::temp_dir().join(format!("rawkvm-gsink-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("configfs")).unwrap();
        let paths = Paths {
            configfs: root.join("configfs"),
            gadget_name: "rawkvm".into(),
            udc_class: root.join("no-udc"),
            sys_dev_char: root.join("sys"),
            udc: None,
        };
        let mut s = GadgetSink::new(
            paths,
            Options {
                expose_hidpp: true,
                serial: "X".into(),
                max_functions: configfs::HIDG_MINORS,
            },
        );
        s.create(&device(0, k)).unwrap();
        let spec = configfs::spec_for(&s.pending, &s.opts).unwrap();
        let Err(BuildError::Other(e)) = s.build(spec) else {
            panic!("built without a UDC");
        };
        assert!(format!("{e:#}").contains("no USB device controller"));
        fs::remove_dir_all(root).unwrap();
    }
}
