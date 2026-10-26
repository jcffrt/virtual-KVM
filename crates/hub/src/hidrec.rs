//! `.hidrec`: a text recording of devices and reports, used to replay input
//! without a receiver (simulated hub, tests on any OS).
//!
//! ```text
//! # comment
//! D <id> <interface> <vid hex> <pid hex> <descriptor hex> <name...>
//! R <t_us> <id> <report hex>
//! ```
//!
//! `t_us` is the time since the start of the recording in microseconds.
//!
//! A `D` line does not say which receiver the device belongs to: a replay
//! takes it from the router's receiver blocks, the first one matching the
//! device's `vid` and `pid`, on every pass ([`Replay::sync`]). A device no
//! block matches is announced as unclaimed and, once a widget adds a block
//! for it, replayed from the next pass (ADR 0032).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use proto::descriptor::DescriptorInfo;
use proto::HidDevice;
use tracing::info;

use crate::config::ReceiverMatch;
use crate::router::Router;

#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub devices: Vec<HidDevice>,
    pub reports: Vec<(u64, u8, Vec<u8>)>,
}

pub fn parse(text: &str) -> Result<Recording> {
    let mut rec = Recording {
        devices: Vec::new(),
        reports: Vec::new(),
    };
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let ctx = || format!("line {}", n + 1);
        let mut f = line.split_whitespace();
        match f.next() {
            Some("D") => {
                let id: u8 = f.next().context("id").with_context(ctx)?.parse()?;
                let interface: u8 = f.next().context("interface").with_context(ctx)?.parse()?;
                let vid = u16::from_str_radix(f.next().context("vid").with_context(ctx)?, 16)?;
                let pid = u16::from_str_radix(f.next().context("pid").with_context(ctx)?, 16)?;
                let desc = hex::decode(f.next().context("descriptor").with_context(ctx)?)
                    .with_context(ctx)?;
                let name = f.collect::<Vec<_>>().join(" ");
                rec.devices.push(HidDevice {
                    id,
                    name,
                    vid,
                    pid,
                    serial: None,
                    interface,
                    uses_report_ids: DescriptorInfo::parse(&desc).uses_report_ids,
                    report_descriptor: desc,
                    receiver: 0,
                });
            }
            Some("R") => {
                let t: u64 = f.next().context("time").with_context(ctx)?.parse()?;
                let id: u8 = f.next().context("id").with_context(ctx)?.parse()?;
                let data =
                    hex::decode(f.next().context("report").with_context(ctx)?).with_context(ctx)?;
                if !rec.devices.iter().any(|d| d.id == id) {
                    bail!("line {}: report for undeclared device {id}", n + 1);
                }
                rec.reports.push((t, id, data));
            }
            Some(other) => bail!("line {}: unknown record type {other:?}", n + 1),
            None => {}
        }
    }
    Ok(rec)
}

/// The block a recorded device belongs to: the first one matching its
/// `vid` and `pid` (`None` entries are removed blocks).
pub fn receiver_of(d: &HidDevice, receivers: &[Option<ReceiverMatch>]) -> Option<usize> {
    receivers
        .iter()
        .position(|r| r.as_ref().is_some_and(|r| r.matches(d.vid, d.pid)))
}

/// The devices of a recording, kept in step with the router's blocks.
///
/// A device is added with the id the hub gives a real interface,
/// `16 × receiver + interface` (ADR 0030), so a block added from the
/// widget as receiver 2 gives 32, 33...; the recorded ids only name the
/// devices in the `R` lines ([`Replay::live_id`]).
pub struct Replay {
    rec: Recording,
    /// Recorded id -> (receiver, id in the router).
    present: HashMap<u8, (u8, u8)>,
}

impl Replay {
    pub fn new(rec: Recording) -> Self {
        Self {
            rec,
            present: HashMap::new(),
        }
    }

    /// The router's id of recorded device `recorded`, while a block
    /// matches it.
    pub fn live_id(&self, recorded: u8) -> Option<u8> {
        self.present.get(&recorded).map(|&(_, id)| id)
    }

    /// `16 × receiver + interface` if free, else the first free id.
    fn assign(&self, receiver: usize, interface: u8) -> Option<u8> {
        let taken = |id: u8| self.present.values().any(|&(_, t)| t == id);
        u8::try_from(receiver * 16 + interface as usize)
            .ok()
            .filter(|&id| interface < 16 && !taken(id))
            .or_else(|| (0..=u8::MAX).find(|&id| !taken(id)))
    }

    /// Add the devices a block now matches, remove those whose block went
    /// away, and announce the rest as unclaimed.
    pub fn sync(&mut self, router: &Router) {
        let receivers = router.receivers();
        let mut unclaimed = Vec::new();
        for d in &self.rec.devices {
            match receiver_of(d, &receivers) {
                Some(r) if self.present.get(&d.id).map(|p| p.0) == Some(r as u8) => {}
                Some(r) => {
                    if let Some((_, old)) = self.present.remove(&d.id) {
                        router.remove_device(old);
                    }
                    let Some(id) = self.assign(r, d.interface) else {
                        continue;
                    };
                    router.add_device(HidDevice {
                        id,
                        receiver: r as u8,
                        ..d.clone()
                    });
                    self.present.insert(d.id, (r as u8, id));
                }
                None => {
                    if let Some((_, old)) = self.present.remove(&d.id) {
                        router.remove_device(old);
                    }
                    unclaimed.push(crate::receivers::Candidate {
                        vid: d.vid,
                        pid: d.pid,
                        serial: d.serial.clone(),
                        name: d.name.clone(),
                        phys: format!("replay-{:04x}:{:04x}/input{}", d.vid, d.pid, d.interface),
                        descriptor: d.report_descriptor.clone(),
                    });
                }
            }
        }
        router.set_unclaimed(crate::receivers::group_unclaimed(&unclaimed));
    }
}

pub fn load(path: &Path) -> Result<Recording> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("parsing {}", path.display()))
}

pub struct Writer {
    w: BufWriter<File>,
    t0: Instant,
}

impl Writer {
    pub fn create(path: &Path, devices: &[HidDevice]) -> Result<Self> {
        let f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut w = BufWriter::new(f);
        writeln!(w, "# rawkvm hidrec v1")?;
        for d in devices {
            writeln!(
                w,
                "D {} {} {:04x} {:04x} {} {}",
                d.id,
                d.interface,
                d.vid,
                d.pid,
                hex::encode(&d.report_descriptor),
                d.name
            )?;
        }
        Ok(Self {
            w,
            t0: Instant::now(),
        })
    }

    pub fn report(&mut self, id: u8, data: &[u8]) -> Result<()> {
        let t = self.t0.elapsed().as_micros();
        writeln!(self.w, "R {t} {id} {}", hex::encode(data))?;
        self.w.flush()?;
        Ok(())
    }
}

/// Register the recorded devices and play the reports in a loop.
pub fn spawn_replay(router: Arc<Router>, rec: Recording) {
    thread::Builder::new()
        .name("replay".into())
        .spawn(move || {
            let reports = rec.reports.clone();
            let mut replay = Replay::new(rec);
            if reports.is_empty() {
                info!("recording has no reports; devices only");
                loop {
                    replay.sync(&router);
                    thread::sleep(Duration::from_secs(2));
                }
            }
            let span = reports.last().map(|r| r.0).unwrap_or(0);
            loop {
                replay.sync(&router);
                let start = Instant::now();
                for (t, id, data) in &reports {
                    let due = start + Duration::from_micros(*t);
                    if let Some(wait) = due.checked_duration_since(Instant::now()) {
                        thread::sleep(wait);
                    }
                    if let Some(id) = replay.live_id(*id) {
                        router.on_report(id, data);
                    }
                }
                // Pause between loops so a replayed key never repeats.
                thread::sleep(Duration::from_millis(500.max(span / 10_000)));
            }
        })
        .expect("spawning the replay thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_write_roundtrip() {
        let text = "# x\nD 0 0 046d c52b 05010906a101c0 Logitech USB Receiver\n\
                    R 0 0 0000040000000000\nR 1500 0 0000000000000000\n";
        let rec = parse(text).unwrap();
        assert_eq!(rec.devices.len(), 1);
        assert_eq!(rec.devices[0].name, "Logitech USB Receiver");
        assert_eq!(rec.devices[0].pid, 0xc52b);
        assert_eq!(rec.reports[1], (1500, 0, vec![0; 8]));

        let path = std::env::temp_dir().join(format!("rawkvm-{}.hidrec", uuid::Uuid::new_v4()));
        let mut w = Writer::create(&path, &rec.devices).unwrap();
        w.report(0, &[1, 2, 3]).unwrap();
        drop(w);
        let back = load(&path).unwrap();
        assert_eq!(back.devices, rec.devices);
        assert_eq!(back.reports[0].2, vec![1, 2, 3]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_undeclared_devices() {
        assert!(parse("R 0 3 00\n").is_err());
        assert!(parse("X\n").is_err());
    }

    #[test]
    fn receivers_come_from_the_config() {
        let cfg = crate::config::HubConfig::parse(
            "[[receiver]]\nname = \"logi\"\n[[receiver]]\nname = \"other\"\nvid = 0x248a\n",
        )
        .unwrap();
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/two-receivers.hidrec");
        let rec = load(&path).unwrap();
        let blocks: Vec<_> = cfg.receivers.into_iter().map(Some).collect();
        let by_id: Vec<_> = rec
            .devices
            .iter()
            .map(|d| (d.id, receiver_of(d, &blocks)))
            .collect();
        assert_eq!(
            by_id,
            [(0, Some(0)), (1, Some(0)), (16, Some(1)), (17, Some(1))]
        );
        let only_logitech = [blocks[0].clone()];
        assert_eq!(receiver_of(&rec.devices[2], &only_logitech), None);
    }

    #[test]
    fn bundled_fixtures_parse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "hidrec") {
                load(&e.path()).unwrap();
            }
        }
    }
}
