//! `HidSink` that presents the hub's devices to a PC as a USB gadget.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use agent_core::HidSink;
use anyhow::{anyhow, Context, Result};
use proto::descriptor::{DescriptorInfo, USAGE_MOUSE, USAGE_PAGE_GENERIC_DESKTOP};
use proto::HidDevice;
use tracing::{debug, info, warn};

use crate::configfs::{self, GadgetSpec, Options};
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

pub struct GadgetSink {
    paths: Paths,
    opts: Options,
    pending: Vec<HidDevice>,
    spec: Option<GadgetSpec>,
    ports: BTreeMap<u8, Port>,
    dropped: u64,
}

impl GadgetSink {
    pub fn new(paths: Paths, opts: Options) -> Self {
        Self {
            paths,
            opts,
            pending: Vec::new(),
            spec: None,
            ports: BTreeMap::new(),
            dropped: 0,
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
        configfs::apply(&ops).with_context(|| format!("removing {}", g.display()))?;
        info!("USB gadget removed");
        Ok(())
    }

    fn build(&mut self, spec: GadgetSpec) -> Result<()> {
        self.teardown()?;
        let g = self.gadget_dir();
        configfs::apply(&configfs::build_ops(&g, &spec))
            .with_context(|| format!("creating {} (is libcomposite loaded?)", g.display()))?;
        let udc = match &self.paths.udc {
            Some(u) => u.clone(),
            None => configfs::find_udc(&self.paths.udc_class).ok_or_else(|| {
                anyhow!(
                    "no USB device controller in {} (dtoverlay=dwc2 / OTG port in peripheral mode?)",
                    self.paths.udc_class.display()
                )
            })?,
        };
        std::fs::write(g.join("UDC"), format!("{udc}\n"))
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
                Hidg::open(&node)
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
        for p in self.ports.values() {
            if let Some(h) = &p.hidg {
                for r in &p.release {
                    let _ = h.write_report(r, PATIENCE_OTHER);
                }
            }
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

    fn devices_ready(&mut self) {
        let Some(spec) = configfs::spec_for(&self.pending, &self.opts) else {
            warn!("no device to present over USB");
            return;
        };
        if self.spec.as_ref() == Some(&spec) {
            // Same receiver as before a reconnection: keep the gadget, the
            // PC sees no USB disconnect.
            debug!("device list unchanged; keeping the USB gadget");
            return;
        }
        if let Err(e) = self.build(spec) {
            warn!("{e:#}");
        }
    }

    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()> {
        let Some(port) = self.ports.get(&dev_id) else {
            return Ok(()); // not exposed (HID++ hidden) or not bound
        };
        let Some(h) = &port.hidg else {
            return Ok(());
        };
        let patience = if port.is_mouse_report(report) {
            PATIENCE_MOUSE
        } else {
            PATIENCE_OTHER
        };
        if !h.write_report(report, patience)? {
            self.dropped += 1;
            if self.dropped.is_power_of_two() {
                debug!(
                    dropped = self.dropped,
                    "host not polling (suspended or unplugged?)"
                );
            }
        }
        Ok(())
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
    use std::fs;

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
            },
        );
        s.create(&device(0, k)).unwrap();
        let spec = configfs::spec_for(&s.pending, &s.opts).unwrap();
        let e = s.build(spec).unwrap_err();
        assert!(format!("{e:#}").contains("no USB device controller"));
        fs::remove_dir_all(root).unwrap();
    }
}
