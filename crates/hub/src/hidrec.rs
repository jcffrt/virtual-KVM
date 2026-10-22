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
            for d in &rec.devices {
                router.add_device(d.clone());
            }
            if rec.reports.is_empty() {
                info!("recording has no reports; devices only");
                return;
            }
            let span = rec.reports.last().map(|r| r.0).unwrap_or(0);
            loop {
                let start = Instant::now();
                for (t, id, data) in &rec.reports {
                    let due = start + Duration::from_micros(*t);
                    if let Some(wait) = due.checked_duration_since(Instant::now()) {
                        thread::sleep(wait);
                    }
                    router.on_report(*id, data);
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
    fn bundled_fixtures_parse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "hidrec") {
                load(&e.path()).unwrap();
            }
        }
    }
}
