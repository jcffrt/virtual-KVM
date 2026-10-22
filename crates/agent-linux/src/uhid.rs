//! Minimal `/dev/uhid` client (see `linux/uhid.h`).
//!
//! `struct uhid_event` is a packed `u32 type` followed by a union. We build
//! and parse it as plain bytes at the kernel's offsets instead of mirroring
//! the union in Rust: no unsafe, and the layout is fixed ABI.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};
use tracing::{debug, trace};

pub const UHID_DESTROY: u32 = 1;
pub const UHID_START: u32 = 2;
pub const UHID_STOP: u32 = 3;
pub const UHID_OPEN: u32 = 4;
pub const UHID_CLOSE: u32 = 5;
pub const UHID_OUTPUT: u32 = 6;
pub const UHID_GET_REPORT: u32 = 9;
pub const UHID_GET_REPORT_REPLY: u32 = 10;
pub const UHID_CREATE2: u32 = 11;
pub const UHID_INPUT2: u32 = 12;
pub const UHID_SET_REPORT: u32 = 13;
pub const UHID_SET_REPORT_REPLY: u32 = 14;

pub const BUS_USB: u16 = 0x03;
pub const BUS_VIRTUAL: u16 = 0x06;

pub const UHID_DATA_MAX: usize = 4096;
/// `sizeof(struct uhid_event)`: 4 + the largest union member (create2).
pub const EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 4 * 4 + UHID_DATA_MAX;

// create2 offsets (from the start of the event)
const C2_NAME: usize = 4;
const C2_PHYS: usize = C2_NAME + 128;
const C2_UNIQ: usize = C2_PHYS + 64;
const C2_RD_SIZE: usize = C2_UNIQ + 64;
const C2_BUS: usize = C2_RD_SIZE + 2;
const C2_VENDOR: usize = C2_BUS + 2;
const C2_PRODUCT: usize = C2_VENDOR + 4;
const C2_VERSION: usize = C2_PRODUCT + 4;
const C2_COUNTRY: usize = C2_VERSION + 4;
const C2_RD_DATA: usize = C2_COUNTRY + 4;

const EIO: u16 = 5;

#[derive(Debug, Clone)]
pub struct CreateParams<'a> {
    pub name: &'a str,
    pub phys: &'a str,
    pub uniq: &'a str,
    pub bus: u16,
    pub vendor: u32,
    pub product: u32,
    pub version: u32,
    pub descriptor: &'a [u8],
}

fn put_str(buf: &mut [u8], at: usize, max: usize, s: &str) {
    // NUL terminated, truncated to fit.
    let b = s.as_bytes();
    let n = b.len().min(max - 1);
    buf[at..at + n].copy_from_slice(&b[..n]);
}

pub fn encode_create2(p: &CreateParams) -> Result<Vec<u8>> {
    if p.descriptor.len() > UHID_DATA_MAX {
        bail!(
            "report descriptor of {} bytes is too large",
            p.descriptor.len()
        );
    }
    let mut b = vec![0u8; EVENT_SIZE];
    b[0..4].copy_from_slice(&UHID_CREATE2.to_ne_bytes());
    put_str(&mut b, C2_NAME, 128, p.name);
    put_str(&mut b, C2_PHYS, 64, p.phys);
    put_str(&mut b, C2_UNIQ, 64, p.uniq);
    b[C2_RD_SIZE..C2_RD_SIZE + 2].copy_from_slice(&(p.descriptor.len() as u16).to_ne_bytes());
    b[C2_BUS..C2_BUS + 2].copy_from_slice(&p.bus.to_ne_bytes());
    b[C2_VENDOR..C2_VENDOR + 4].copy_from_slice(&p.vendor.to_ne_bytes());
    b[C2_PRODUCT..C2_PRODUCT + 4].copy_from_slice(&p.product.to_ne_bytes());
    b[C2_VERSION..C2_VERSION + 4].copy_from_slice(&p.version.to_ne_bytes());
    b[C2_COUNTRY..C2_COUNTRY + 4].copy_from_slice(&0u32.to_ne_bytes());
    b[C2_RD_DATA..C2_RD_DATA + p.descriptor.len()].copy_from_slice(p.descriptor);
    Ok(b)
}

/// `input2`: u16 size, data. The kernel zero-fills the rest of a short
/// write, so only the used bytes are sent.
pub fn encode_input2(report: &[u8]) -> Result<Vec<u8>> {
    if report.len() > UHID_DATA_MAX {
        bail!("report of {} bytes is too large", report.len());
    }
    let mut b = Vec::with_capacity(6 + report.len());
    b.extend_from_slice(&UHID_INPUT2.to_ne_bytes());
    b.extend_from_slice(&(report.len() as u16).to_ne_bytes());
    b.extend_from_slice(report);
    Ok(b)
}

pub fn encode_destroy() -> Vec<u8> {
    UHID_DESTROY.to_ne_bytes().to_vec()
}

/// Reply for `get_report` / `set_report`: u32 id, u16 err, (u16 size, data).
fn encode_reply(kind: u32, id: u32, err: u16) -> Vec<u8> {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&kind.to_ne_bytes());
    b.extend_from_slice(&id.to_ne_bytes());
    b.extend_from_slice(&err.to_ne_bytes());
    if kind == UHID_GET_REPORT_REPLY {
        b.extend_from_slice(&0u16.to_ne_bytes());
    }
    b
}

/// Events the kernel sends us.
#[derive(Debug, PartialEq, Eq)]
pub enum KernelEvent {
    Start,
    Stop,
    Open,
    Close,
    Output { rtype: u8, data: Vec<u8> },
    GetReport { id: u32, rnum: u8, rtype: u8 },
    SetReport { id: u32, rnum: u8, rtype: u8 },
    Other(u32),
}

pub fn decode_event(b: &[u8]) -> Option<KernelEvent> {
    let u32_at = |o: usize| {
        b.get(o..o + 4)
            .map(|s| u32::from_ne_bytes(s.try_into().unwrap()))
    };
    let u16_at = |o: usize| {
        b.get(o..o + 2)
            .map(|s| u16::from_ne_bytes(s.try_into().unwrap()))
    };
    Some(match u32_at(0)? {
        UHID_START => KernelEvent::Start,
        UHID_STOP => KernelEvent::Stop,
        UHID_OPEN => KernelEvent::Open,
        UHID_CLOSE => KernelEvent::Close,
        UHID_OUTPUT => {
            // data[4096] at 4, u16 size at 4100, u8 rtype at 4102
            let size = (u16_at(4 + UHID_DATA_MAX)? as usize).min(UHID_DATA_MAX);
            KernelEvent::Output {
                rtype: *b.get(4 + UHID_DATA_MAX + 2)?,
                data: b.get(4..4 + size)?.to_vec(),
            }
        }
        UHID_GET_REPORT => KernelEvent::GetReport {
            id: u32_at(4)?,
            rnum: *b.get(8)?,
            rtype: *b.get(9)?,
        },
        UHID_SET_REPORT => KernelEvent::SetReport {
            id: u32_at(4)?,
            rnum: *b.get(8)?,
            rtype: *b.get(9)?,
        },
        other => KernelEvent::Other(other),
    })
}

/// One virtual HID device. Dropping it destroys the device.
pub struct UhidDevice {
    file: Arc<File>,
    destroyed: Arc<AtomicBool>,
    name: String,
}

impl UhidDevice {
    pub fn create(path: &Path, p: &CreateParams) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| {
                format!(
                    "opening {} (is the udev rule installed and the user in group `input`?)",
                    path.display()
                )
            })?;
        let file = Arc::new(file);
        (&*file)
            .write_all(&encode_create2(p)?)
            .context("UHID_CREATE2")?;

        // The kernel blocks probing until GET/SET_REPORT are answered (up to
        // 5 s each), so a thread answers them right away. We hold no state
        // for feature/output reports: GET fails with EIO, SET is accepted.
        let reader = file.clone();
        let destroyed = Arc::new(AtomicBool::new(false));
        let flag = destroyed.clone();
        let name = p.name.to_string();
        thread::Builder::new()
            .name("uhid-events".into())
            .spawn(move || event_loop(&reader, &flag, &name))?;
        Ok(Self {
            file,
            destroyed,
            name: p.name.to_string(),
        })
    }

    pub fn input(&self, report: &[u8]) -> io::Result<()> {
        let ev = encode_input2(report).map_err(io::Error::other)?;
        (&*self.file).write_all(&ev)
    }
}

impl Drop for UhidDevice {
    fn drop(&mut self) {
        // Closing the fd would destroy the device too, but the reader thread
        // holds it. Destroy explicitly; the kernel then queues UHID_STOP,
        // which wakes the reader, sees the flag and exits, closing the fd.
        self.destroyed.store(true, Ordering::SeqCst);
        if let Err(e) = (&*self.file).write_all(&encode_destroy()) {
            debug!(name = %self.name, "UHID_DESTROY: {e}");
        }
    }
}

fn event_loop(file: &File, destroyed: &AtomicBool, name: &str) {
    let mut buf = vec![0u8; EVENT_SIZE];
    while !destroyed.load(Ordering::SeqCst) {
        let n = match (&*file).read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let reply = match decode_event(&buf[..n]) {
            Some(KernelEvent::GetReport { id, rnum, rtype }) => {
                trace!(name, rnum, rtype, "GET_REPORT -> EIO");
                Some(encode_reply(UHID_GET_REPORT_REPLY, id, EIO))
            }
            Some(KernelEvent::SetReport { id, rnum, rtype }) => {
                trace!(name, rnum, rtype, "SET_REPORT -> ok");
                Some(encode_reply(UHID_SET_REPORT_REPLY, id, 0))
            }
            Some(KernelEvent::Output { rtype, data }) => {
                // Keyboard LEDs and HID++ requests from the host. Not
                // forwarded to the hub yet (phase 0).
                trace!(name, rtype, len = data.len(), "output report ignored");
                None
            }
            Some(KernelEvent::Stop) => {
                debug!(name, "stopped");
                None
            }
            Some(e) => {
                debug!(name, ?e, "uhid event");
                None
            }
            None => None,
        };
        if let Some(r) = reply {
            if (&*file).write_all(&r).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_kernel_abi() {
        // sizeof(struct uhid_event): 4 + sizeof(create2) = 4 + 4372.
        assert_eq!(EVENT_SIZE, 4376);
        assert_eq!(C2_RD_SIZE, 260);
        assert_eq!(C2_RD_DATA, 280);
    }

    #[test]
    fn create2_fields() {
        let desc = [0x05, 0x01, 0x09, 0x06];
        let b = encode_create2(&CreateParams {
            name: "Logitech USB Receiver",
            phys: "rawkvm/0",
            uniq: "",
            bus: BUS_VIRTUAL,
            vendor: 0x046d,
            product: 0xc52b,
            version: 0x111,
            descriptor: &desc,
        })
        .unwrap();
        assert_eq!(b.len(), EVENT_SIZE);
        assert_eq!(
            u32::from_ne_bytes(b[0..4].try_into().unwrap()),
            UHID_CREATE2
        );
        assert_eq!(&b[4..25], b"Logitech USB Receiver");
        assert_eq!(b[25], 0);
        assert_eq!(u16::from_ne_bytes([b[260], b[261]]), 4);
        assert_eq!(u16::from_ne_bytes([b[262], b[263]]), BUS_VIRTUAL);
        assert_eq!(u32::from_ne_bytes(b[264..268].try_into().unwrap()), 0x046d);
        assert_eq!(u32::from_ne_bytes(b[268..272].try_into().unwrap()), 0xc52b);
        assert_eq!(&b[280..284], &desc);
    }

    #[test]
    fn long_names_are_truncated_with_nul() {
        let name = "x".repeat(300);
        let b = encode_create2(&CreateParams {
            name: &name,
            phys: "",
            uniq: "",
            bus: BUS_USB,
            vendor: 0,
            product: 0,
            version: 0,
            descriptor: &[],
        })
        .unwrap();
        assert_eq!(b[4 + 126], b'x');
        assert_eq!(b[4 + 127], 0);
    }

    #[test]
    fn input2_and_replies() {
        let b = encode_input2(&[1, 2, 3]).unwrap();
        assert_eq!(b, [12, 0, 0, 0, 3, 0, 1, 2, 3]);
        assert_eq!(
            encode_reply(UHID_GET_REPORT_REPLY, 7, EIO),
            [10, 0, 0, 0, 7, 0, 0, 0, 5, 0, 0, 0]
        );
        assert_eq!(
            encode_reply(UHID_SET_REPORT_REPLY, 9, 0),
            [14, 0, 0, 0, 9, 0, 0, 0, 0, 0]
        );
        assert!(encode_input2(&[0; UHID_DATA_MAX + 1]).is_err());
    }

    #[test]
    fn decode_kernel_events() {
        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&UHID_GET_REPORT.to_ne_bytes());
        ev[4..8].copy_from_slice(&42u32.to_ne_bytes());
        ev[8] = 3;
        ev[9] = 1;
        assert_eq!(
            decode_event(&ev),
            Some(KernelEvent::GetReport {
                id: 42,
                rnum: 3,
                rtype: 1
            })
        );
        let mut out = vec![0u8; EVENT_SIZE];
        out[0..4].copy_from_slice(&UHID_OUTPUT.to_ne_bytes());
        out[4] = 0x02; // caps lock LED
        out[4100..4102].copy_from_slice(&1u16.to_ne_bytes());
        out[4102] = 1;
        assert_eq!(
            decode_event(&out),
            Some(KernelEvent::Output {
                rtype: 1,
                data: vec![2]
            })
        );
        assert_eq!(decode_event(&2u32.to_ne_bytes()), Some(KernelEvent::Start));
        assert_eq!(decode_event(&[1]), None);
    }
}
