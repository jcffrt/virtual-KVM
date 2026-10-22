//! Linux hidraw access: enumeration, descriptors, blocking reads, and the
//! evdev grab that keeps the hub's own console from consuming input.
//!
//! Identity (descriptor, name, bus/VID/PID) comes from the hidraw ioctls;
//! sysfs provides what the ioctls do not: the driver, the USB interface
//! number and the evdev nodes created for the same HID device.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, warn};

pub const BUS_USB: u16 = 0x03;
const HID_MAX_DESCRIPTOR_SIZE: usize = 4096;

#[repr(C)]
pub struct HidrawReportDescriptor {
    size: u32,
    value: [u8; HID_MAX_DESCRIPTOR_SIZE],
}

#[repr(C)]
#[derive(Default)]
pub struct HidrawDevinfo {
    bustype: u32,
    vendor: i16,
    product: i16,
}

mod ioctl {
    use super::{HidrawDevinfo, HidrawReportDescriptor};
    nix::ioctl_read!(rdesc_size, b'H', 0x01, libc::c_int);
    nix::ioctl_read!(rdesc, b'H', 0x02, HidrawReportDescriptor);
    nix::ioctl_read!(raw_info, b'H', 0x03, HidrawDevinfo);
    nix::ioctl_read_buf!(raw_name, b'H', 0x04, u8);
    nix::ioctl_read_buf!(raw_phys, b'H', 0x05, u8);
    nix::ioctl_write_int!(evdev_grab, b'E', 0x90);
}

/// One hidraw node.
#[derive(Debug, Clone)]
pub struct RawInterface {
    pub hidraw: PathBuf,
    /// Physical path, e.g. `usb-5200000.usb-1/input2`. Stable across
    /// re-plugging into the same port; used as the device key.
    pub phys: String,
    pub name: String,
    pub bus: u16,
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    /// USB interface number, when the HID device sits directly on a USB
    /// interface (receiver interfaces, mode A).
    pub usb_interface: Option<u8>,
    /// Device index behind a DJ receiver (`/input2:N` suffix of `phys`),
    /// for HID devices synthesized by `hid-logitech-dj` (mode B).
    pub dj_index: Option<u8>,
    /// `(bus, vid, pid)` of the parent HID device, if the parent is one (DJ
    /// children have the receiver as parent).
    pub parent_hid: Option<(u16, u16, u16)>,
    pub driver: Option<String>,
    pub descriptor: Vec<u8>,
    pub event_nodes: Vec<PathBuf>,
}

/// List every hidraw node. Nodes that cannot be opened (permissions) are
/// skipped with a warning.
pub fn enumerate() -> Result<Vec<RawInterface>> {
    let mut out = Vec::new();
    let dir = match fs::read_dir("/sys/class/hidraw") {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).context("reading /sys/class/hidraw"),
    };
    for entry in dir {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        match read_interface(&entry.path(), &name) {
            Ok(i) => out.push(i),
            Err(e) => warn!("skipping {name}: {e:#}"),
        }
    }
    out.sort_by(|a, b| a.phys.cmp(&b.phys));
    Ok(out)
}

fn read_interface(sys: &Path, name: &str) -> Result<RawInterface> {
    let node = Path::new("/dev").join(name);
    let f = File::open(&node).with_context(|| format!("opening {}", node.display()))?;
    let fd = f.as_raw_fd();

    let mut info = HidrawDevinfo::default();
    // SAFETY: valid fd and a correctly sized, writable struct.
    unsafe { ioctl::raw_info(fd, &mut info) }.context("HIDIOCGRAWINFO")?;

    let mut size: libc::c_int = 0;
    // SAFETY: as above.
    unsafe { ioctl::rdesc_size(fd, &mut size) }.context("HIDIOCGRDESCSIZE")?;
    let mut desc = Box::new(HidrawReportDescriptor {
        size: size.clamp(0, HID_MAX_DESCRIPTOR_SIZE as i32) as u32,
        value: [0; HID_MAX_DESCRIPTOR_SIZE],
    });
    // SAFETY: as above; the kernel reads `size` and fills `value`.
    unsafe { ioctl::rdesc(fd, &mut *desc) }.context("HIDIOCGRDESC")?;
    let descriptor = desc.value[..desc.size as usize].to_vec();

    let name_str = ioctl_string(fd, ioctl::raw_name).context("HIDIOCGRAWNAME")?;
    let phys = ioctl_string(fd, ioctl::raw_phys).context("HIDIOCGRAWPHYS")?;

    let dev = sys.join("device");
    let mut driver = None;
    let mut serial = None;
    if let Ok(uevent) = fs::read_to_string(dev.join("uevent")) {
        for (k, v) in uevent.lines().filter_map(|l| l.split_once('=')) {
            match k {
                "DRIVER" => driver = Some(v.to_string()),
                "HID_UNIQ" if !v.is_empty() => serial = Some(v.to_string()),
                _ => {}
            }
        }
    }

    let parent = fs::canonicalize(&dev)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let usb_interface = parent
        .as_ref()
        .and_then(|p| fs::read_to_string(p.join("bInterfaceNumber")).ok())
        .and_then(|s| u8::from_str_radix(s.trim(), 16).ok());
    let parent_hid = parent
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| parse_hid_dir_name(&n.to_string_lossy()));

    Ok(RawInterface {
        hidraw: node,
        usb_interface,
        dj_index: parse_dj_index(&phys),
        parent_hid,
        phys,
        name: name_str,
        bus: info.bustype as u16,
        vid: info.vendor as u16,
        pid: info.product as u16,
        serial,
        driver,
        descriptor,
        event_nodes: event_nodes(&dev),
    })
}

type BufIoctl = unsafe fn(libc::c_int, &mut [u8]) -> nix::Result<libc::c_int>;

fn ioctl_string(fd: libc::c_int, f: BufIoctl) -> Result<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer length is encoded in the request.
    unsafe { f(fd, &mut buf) }?;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

impl RawInterface {
    /// The value exposed as `HidDevice::interface`.
    pub fn interface(&self) -> u8 {
        self.usb_interface
            .or(self.dj_index)
            .or_else(|| parse_interface(&self.phys))
            .unwrap_or(0)
    }
}

/// `usb-xxx-1/input2` -> 2
pub fn parse_interface(phys: &str) -> Option<u8> {
    phys.rsplit_once("/input")?.1.parse().ok()
}

/// `usb-xxx-1/input2:3` -> 3
pub fn parse_dj_index(phys: &str) -> Option<u8> {
    phys.rsplit_once("/input")?
        .1
        .split_once(':')?
        .1
        .parse()
        .ok()
}

/// HID device directory name `0003:046D:C52B.0003` -> (bus, vid, pid).
pub fn parse_hid_dir_name(name: &str) -> Option<(u16, u16, u16)> {
    let (ids, _instance) = name.split_once('.')?;
    let mut it = ids.split(':');
    let bus = u16::from_str_radix(it.next()?, 16).ok()?;
    let vid = u16::from_str_radix(it.next()?, 16).ok()?;
    let pid = u16::from_str_radix(it.next()?, 16).ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((bus, vid, pid))
}

/// `/dev/input/eventN` nodes belonging to the HID device at `dev`.
fn event_nodes(dev: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(inputs) = fs::read_dir(dev.join("input")) else {
        return out;
    };
    for input in inputs.flatten() {
        let Ok(children) = fs::read_dir(input.path()) else {
            continue;
        };
        for c in children.flatten() {
            let n = c.file_name().to_string_lossy().into_owned();
            if n.starts_with("event") {
                out.push(Path::new("/dev/input").join(n));
            }
        }
    }
    out.sort();
    out
}

/// Open a hidraw node for reading (and writing, for HID++ later).
pub fn open(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))
        .with_context(|| format!("opening {}", path.display()))
}

/// Take an exclusive grab on an evdev node. The grab lasts while the
/// returned file stays open.
pub fn grab(path: &Path) -> Result<File> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: valid fd; EVIOCGRAB takes an int by value.
    unsafe { ioctl::evdev_grab(f.as_raw_fd(), 1) }
        .with_context(|| format!("EVIOCGRAB {}", path.display()))?;
    debug!("grabbed {}", path.display());
    Ok(f)
}

/// Read reports until the device goes away. Each `read` returns exactly one
/// report.
pub fn read_loop(mut f: File, mut on_report: impl FnMut(&[u8])) -> io::Error {
    let mut buf = [0u8; 4096];
    loop {
        match f.read(&mut buf) {
            Ok(0) => return io::ErrorKind::UnexpectedEof.into(),
            Ok(n) => on_report(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_from_phys() {
        assert_eq!(parse_interface("usb-5200000.usb-1/input2"), Some(2));
        assert_eq!(parse_interface("usb-0000:00:14.0-3/input0"), Some(0));
        assert_eq!(parse_interface("bluetooth"), None);
        assert_eq!(parse_interface("usb-5101400.usb-1/input2:1"), None);
        assert_eq!(parse_dj_index("usb-5101400.usb-1/input2:1"), Some(1));
        assert_eq!(parse_dj_index("usb-5101400.usb-1/input2"), None);
    }

    #[test]
    fn hid_dir_names() {
        assert_eq!(
            parse_hid_dir_name("0003:046D:C52B.0003"),
            Some((3, 0x046d, 0xc52b))
        );
        assert_eq!(parse_hid_dir_name("4-1:1.2"), None);
        assert_eq!(parse_hid_dir_name("usb4"), None);
    }
}
