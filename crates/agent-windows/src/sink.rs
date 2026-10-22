//! `HidSink` over the RawKVM VHF driver, generic over the driver handle so
//! it can be tested anywhere.

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

use agent_core::HidSink;
use anyhow::{anyhow, Context, Result};
use proto::descriptor::DescriptorInfo;
use proto::HidDevice;
use tracing::{info, warn};
use uuid::Uuid;

use crate::ioctl;

/// An open handle to `\\.\RawKvmVhf`. Closing it makes the driver delete
/// every device it created.
pub trait Control {
    fn create(&mut self, request: &[u8]) -> io::Result<u32>;
    fn report(&mut self, request: &[u8]) -> io::Result<()>;
    fn destroy(&mut self, handle: u32) -> io::Result<()>;
}

pub trait Opener {
    fn open(&mut self) -> io::Result<Box<dyn Control>>;
}

const REOPEN_EVERY: Duration = Duration::from_secs(1);

struct Device {
    dev: HidDevice,
    handle: Option<u32>,
    release: Vec<Vec<u8>>,
}

pub struct VhfSink<O: Opener> {
    opener: O,
    control: Option<Box<dyn Control>>,
    devices: BTreeMap<u8, Device>,
    container: Uuid,
    last_open: Option<Instant>,
}

impl<O: Opener> VhfSink<O> {
    pub fn new(opener: O, node_id: Uuid) -> Self {
        Self {
            opener,
            control: None,
            devices: BTreeMap::new(),
            container: ioctl::container_for(node_id),
            last_open: None,
        }
    }

    /// Open the driver if needed (at most once per second).
    fn control(&mut self) -> Result<&mut Box<dyn Control>> {
        if self.control.is_none() {
            if self.last_open.is_some_and(|t| t.elapsed() < REOPEN_EVERY) {
                return Err(anyhow!("RawKVM VHF driver not available"));
            }
            self.last_open = Some(Instant::now());
            let c = self.opener.open().with_context(|| {
                format!(
                    "opening {} (is the RawKVM VHF driver installed?)",
                    ioctl::DEVICE_PATH
                )
            })?;
            info!("opened {}", ioctl::DEVICE_PATH);
            self.control = Some(c);
            // A fresh handle owns no device: recreate what we had.
            let ids: Vec<u8> = self.devices.keys().copied().collect();
            for id in ids {
                self.devices.get_mut(&id).expect("listed").handle = None;
                if let Err(e) = self.create_on_driver(id) {
                    warn!(id, "recreating virtual device: {e:#}");
                }
            }
        }
        Ok(self.control.as_mut().expect("opened above"))
    }

    fn create_on_driver(&mut self, id: u8) -> Result<()> {
        let request = {
            let d = &self.devices[&id];
            ioctl::encode_create(&d.dev, self.container)
                .map_err(|e| anyhow!("device {id}: {e:?}"))?
        };
        let control = self
            .control
            .as_mut()
            .ok_or_else(|| anyhow!("driver not open"))?;
        let handle = control.create(&request).context("IOCTL_RAWKVM_CREATE")?;
        self.devices.get_mut(&id).expect("exists").handle = Some(handle);
        Ok(())
    }

    fn drop_control(&mut self) {
        // Closing the handle deletes the driver-side devices.
        self.control = None;
        for d in self.devices.values_mut() {
            d.handle = None;
        }
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }
}

impl<O: Opener> HidSink for VhfSink<O> {
    fn create(&mut self, dev: &HidDevice) -> Result<()> {
        let release = DescriptorInfo::parse(&dev.report_descriptor).release_reports();
        self.devices.insert(
            dev.id,
            Device {
                dev: dev.clone(),
                handle: None,
                release,
            },
        );
        // Opening may already recreate it; create only if still missing.
        self.control()?;
        if self.devices[&dev.id].handle.is_none() {
            self.create_on_driver(dev.id)?;
        }
        Ok(())
    }

    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()> {
        if !self.devices.contains_key(&dev_id) {
            return Ok(());
        }
        for attempt in 0..2 {
            self.control()?;
            let Some(handle) = self.devices[&dev_id].handle else {
                self.create_on_driver(dev_id)?;
                continue;
            };
            let request = ioctl::encode_report(handle, report)
                .map_err(|e| anyhow!("report for device {dev_id}: {e:?}"))?;
            match self.control.as_mut().expect("opened").report(&request) {
                Ok(()) => return Ok(()),
                Err(e) if attempt == 0 => {
                    warn!(
                        dev_id,
                        "IOCTL_RAWKVM_REPORT failed ({e}); reopening the driver"
                    );
                    self.drop_control();
                    self.last_open = None;
                }
                Err(e) => return Err(e).context("IOCTL_RAWKVM_REPORT"),
            }
        }
        Ok(())
    }

    /// Release everything, then delete the virtual devices.
    fn destroy_all(&mut self) {
        if let Some(control) = self.control.as_mut() {
            for d in self.devices.values() {
                let Some(handle) = d.handle else { continue };
                for r in &d.release {
                    if let Ok(req) = ioctl::encode_report(handle, r) {
                        let _ = control.report(&req);
                    }
                }
                if let Err(e) = control.destroy(handle) {
                    warn!(id = d.dev.id, "IOCTL_RAWKVM_DESTROY: {e}");
                }
            }
        }
        if !self.devices.is_empty() {
            info!("removed {} virtual devices", self.devices.len());
        }
        self.devices.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::tests::{BOOT_KEYBOARD, UNIFYING_IFACE1};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq)]
    enum Op {
        Open,
        Create(u8),
        Report(u32, Vec<u8>),
        Destroy(u32),
    }

    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<Op>>>);

    struct FakeControl {
        log: Log,
        next: u32,
        fail_reports: Arc<Mutex<u32>>,
    }

    impl Control for FakeControl {
        fn create(&mut self, req: &[u8]) -> io::Result<u32> {
            self.next += 1;
            self.log.0.lock().unwrap().push(Op::Create(req[28]));
            Ok(self.next)
        }
        fn report(&mut self, req: &[u8]) -> io::Result<()> {
            let mut fail = self.fail_reports.lock().unwrap();
            if *fail > 0 {
                *fail -= 1;
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let h = u32::from_le_bytes(req[..4].try_into().unwrap());
            self.log
                .0
                .lock()
                .unwrap()
                .push(Op::Report(h, req[4..].to_vec()));
            Ok(())
        }
        fn destroy(&mut self, h: u32) -> io::Result<()> {
            self.log.0.lock().unwrap().push(Op::Destroy(h));
            Ok(())
        }
    }

    struct FakeOpener {
        log: Log,
        fail_reports: Arc<Mutex<u32>>,
        opens: u32,
    }

    impl Opener for FakeOpener {
        fn open(&mut self) -> io::Result<Box<dyn Control>> {
            self.opens += 1;
            self.log.0.lock().unwrap().push(Op::Open);
            Ok(Box::new(FakeControl {
                log: self.log.clone(),
                // Handles differ per open, like a real reopen.
                next: self.opens * 100,
                fail_reports: self.fail_reports.clone(),
            }))
        }
    }

    fn dev(id: u8, desc: &[u8]) -> HidDevice {
        HidDevice {
            id,
            name: "Logitech USB Receiver".into(),
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            interface: id,
            report_descriptor: desc.to_vec(),
            uses_report_ids: DescriptorInfo::parse(desc).uses_report_ids,
        }
    }

    fn sink() -> (VhfSink<FakeOpener>, Log, Arc<Mutex<u32>>) {
        let log = Log::default();
        let fail = Arc::new(Mutex::new(0));
        let opener = FakeOpener {
            log: log.clone(),
            fail_reports: fail.clone(),
            opens: 0,
        };
        (VhfSink::new(opener, Uuid::nil()), log, fail)
    }

    #[test]
    fn creates_and_forwards_reports_verbatim() {
        let (mut s, log, _) = sink();
        s.create(&dev(0, BOOT_KEYBOARD)).unwrap();
        s.create(&dev(1, UNIFYING_IFACE1)).unwrap();
        s.input(0, &[0, 0, 0x04, 0, 0, 0, 0, 0]).unwrap();
        s.input(1, &[0x02, 1, 0, 5, 0, 0, 0, 0]).unwrap();
        s.input(9, &[1, 2, 3]).unwrap(); // unknown device: ignored
        assert_eq!(
            *log.0.lock().unwrap(),
            [
                Op::Open,
                Op::Create(0),
                Op::Create(1),
                Op::Report(101, vec![0, 0, 0x04, 0, 0, 0, 0, 0]),
                Op::Report(102, vec![0x02, 1, 0, 5, 0, 0, 0, 0]),
            ]
        );
    }

    #[test]
    fn destroy_all_releases_before_destroying() {
        let (mut s, log, _) = sink();
        s.create(&dev(0, BOOT_KEYBOARD)).unwrap();
        s.create(&dev(1, UNIFYING_IFACE1)).unwrap();
        log.0.lock().unwrap().clear();
        s.destroy_all();
        let ops = log.0.lock().unwrap().clone();
        assert_eq!(
            ops,
            [
                Op::Report(101, vec![0; 8]),
                Op::Destroy(101),
                Op::Report(102, vec![2, 0, 0, 0, 0, 0, 0, 0]),
                Op::Report(102, vec![3, 0, 0, 0, 0]),
                Op::Report(102, vec![4, 0]),
                Op::Destroy(102),
            ]
        );
        assert_eq!(s.device_count(), 0);
    }

    #[test]
    fn reopens_and_recreates_after_a_driver_error() {
        let (mut s, log, fail) = sink();
        s.create(&dev(0, BOOT_KEYBOARD)).unwrap();
        *fail.lock().unwrap() = 1;
        s.input(0, &[0, 0, 0x05, 0, 0, 0, 0, 0]).unwrap();
        let ops = log.0.lock().unwrap().clone();
        assert_eq!(
            ops,
            [
                Op::Open,
                Op::Create(0),
                Op::Open,
                Op::Create(0),
                Op::Report(201, vec![0, 0, 0x05, 0, 0, 0, 0, 0]),
            ]
        );
    }

    #[test]
    fn missing_driver_is_reported_and_rate_limited() {
        struct NoDriver;
        impl Opener for NoDriver {
            fn open(&mut self) -> io::Result<Box<dyn Control>> {
                Err(io::ErrorKind::NotFound.into())
            }
        }
        let mut s = VhfSink::new(NoDriver, Uuid::nil());
        let e = s.create(&dev(0, BOOT_KEYBOARD)).unwrap_err();
        assert!(format!("{e:#}").contains("RawKvmVhf"));
        let e = s.input(0, &[0; 8]).unwrap_err();
        assert!(format!("{e:#}").contains("not available"));
    }
}
