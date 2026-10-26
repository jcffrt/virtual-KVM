//! The USB gadget as a ConfigFS tree (Documentation/usb/gadget_configfs.rst,
//! drivers/usb/gadget/function/f_hid.c).
//!
//! Building and tearing down are expressed as lists of filesystem
//! operations computed by pure functions, then applied to a root directory.
//! Tests apply them to a temporary directory; the agent applies them to
//! `/sys/kernel/config/usb_gadget`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use proto::descriptor::DescriptorInfo;
use proto::HidDevice;

pub const DEFAULT_ROOT: &str = "/sys/kernel/config/usb_gadget";
pub const DEFAULT_NAME: &str = "rawkvm";

/// What the PC will see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GadgetSpec {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
    pub functions: Vec<FunctionSpec>,
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

/// The gadget for a device list, interfaces in hub id order so their
/// numbers match the receiver's. `None` if nothing is left to present.
pub fn spec_for(devices: &[HidDevice], opts: &Options) -> Option<GadgetSpec> {
    let mut devs: Vec<&HidDevice> = devices.iter().collect();
    devs.sort_by_key(|d| d.id);
    let functions: Vec<FunctionSpec> = devs
        .iter()
        .filter_map(|d| {
            let info = DescriptorInfo::parse(&d.report_descriptor);
            if !opts.expose_hidpp && is_vendor_only(&info) {
                return None;
            }
            let (subclass, protocol) = boot_class(&info);
            Some(FunctionSpec {
                dev_id: d.id,
                instance: format!("usb{}", d.id),
                subclass,
                protocol,
                report_length: report_length(&info),
                report_desc: d.report_descriptor.clone(),
            })
        })
        .collect();
    let first = devs.first()?;
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
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Mkdir(PathBuf),
    Write(PathBuf, Vec<u8>),
    Symlink { target: PathBuf, link: PathBuf },
    Unlink(PathBuf),
    Rmdir(PathBuf),
}

fn text(s: impl std::fmt::Display) -> Vec<u8> {
    format!("{s}\n").into_bytes()
}

/// Create the gadget (not bound to a UDC yet).
pub fn build_ops(gadget: &Path, spec: &GadgetSpec) -> Vec<Op> {
    let mut ops = vec![
        Op::Mkdir(gadget.to_path_buf()),
        Op::Write(
            gadget.join("idVendor"),
            text(format!("0x{:04x}", spec.vendor_id)),
        ),
        Op::Write(
            gadget.join("idProduct"),
            text(format!("0x{:04x}", spec.product_id)),
        ),
        Op::Write(gadget.join("bcdDevice"), text("0x0100")),
        Op::Write(gadget.join("bcdUSB"), text("0x0200")),
    ];
    let strings = gadget.join("strings/0x409");
    ops.push(Op::Mkdir(gadget.join("strings")));
    ops.push(Op::Mkdir(strings.clone()));
    ops.push(Op::Write(
        strings.join("manufacturer"),
        text(&spec.manufacturer),
    ));
    ops.push(Op::Write(strings.join("product"), text(&spec.product)));
    ops.push(Op::Write(strings.join("serialnumber"), text(&spec.serial)));

    let config = gadget.join("configs/c.1");
    ops.push(Op::Mkdir(gadget.join("configs")));
    ops.push(Op::Mkdir(config.clone()));
    // Bus powered with remote wakeup, 98 mA: what the receiver declares.
    ops.push(Op::Write(config.join("bmAttributes"), text("0xa0")));
    ops.push(Op::Write(config.join("MaxPower"), text(98)));
    ops.push(Op::Mkdir(config.join("strings")));
    ops.push(Op::Mkdir(config.join("strings/0x409")));
    ops.push(Op::Write(
        config.join("strings/0x409/configuration"),
        text("RawKVM"),
    ));

    ops.push(Op::Mkdir(gadget.join("functions")));
    for f in &spec.functions {
        let dir = gadget.join("functions").join(format!("hid.{}", f.instance));
        ops.push(Op::Mkdir(dir.clone()));
        ops.push(Op::Write(dir.join("subclass"), text(f.subclass)));
        ops.push(Op::Write(dir.join("protocol"), text(f.protocol)));
        ops.push(Op::Write(dir.join("report_length"), text(f.report_length)));
        ops.push(Op::Write(dir.join("report_desc"), f.report_desc.clone()));
    }
    // Interface numbers follow the order functions are linked.
    for f in &spec.functions {
        let name = format!("hid.{}", f.instance);
        ops.push(Op::Symlink {
            target: gadget.join("functions").join(&name),
            link: config.join(&name),
        });
    }
    ops
}

/// What exists under a gadget directory, as needed for teardown.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Existing {
    pub bound: bool,
    pub configs: Vec<(String, Vec<String>, Vec<String>)>, // name, links, string langs
    pub functions: Vec<String>,
    pub strings: Vec<String>,
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Inspect a gadget directory.
pub fn inspect(gadget: &Path) -> Existing {
    let udc = fs::read_to_string(gadget.join("UDC")).unwrap_or_default();
    let configs = names(&gadget.join("configs"))
        .into_iter()
        .map(|c| {
            let dir = gadget.join("configs").join(&c);
            let links = names(&dir)
                .into_iter()
                .filter(|n| fs::symlink_metadata(dir.join(n)).is_ok_and(|m| m.is_symlink()))
                .collect();
            let langs = names(&dir.join("strings"));
            (c, links, langs)
        })
        .collect();
    Existing {
        bound: !udc.trim().is_empty(),
        configs,
        functions: names(&gadget.join("functions")),
        strings: names(&gadget.join("strings")),
    }
}

/// Remove a gadget, in the order ConfigFS requires: unbind, unlink
/// functions from configs, then remove configs, functions and strings.
/// In ConfigFS attribute files vanish with their directory, so `rmdir`
/// works on directories that still list attributes.
pub fn teardown_ops(gadget: &Path, existing: &Existing) -> Vec<Op> {
    let mut ops = Vec::new();
    if existing.bound {
        ops.push(Op::Write(gadget.join("UDC"), b"\n".to_vec()));
    }
    for (c, links, langs) in &existing.configs {
        let dir = gadget.join("configs").join(c);
        for l in links {
            ops.push(Op::Unlink(dir.join(l)));
        }
        for lang in langs {
            ops.push(Op::Rmdir(dir.join("strings").join(lang)));
        }
        ops.push(Op::Rmdir(dir));
    }
    for f in &existing.functions {
        ops.push(Op::Rmdir(gadget.join("functions").join(f)));
    }
    for lang in &existing.strings {
        ops.push(Op::Rmdir(gadget.join("strings").join(lang)));
    }
    ops.push(Op::Rmdir(gadget.to_path_buf()));
    ops
}

/// A failed operation, with the original OS error kept for callers.
#[derive(Debug)]
pub struct ApplyError {
    pub op: Op,
    pub source: io::Error,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.op, self.source)
    }
}

impl std::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl ApplyError {
    /// ConfigFS or the gadget function module is missing.
    pub fn is_missing_support(&self) -> bool {
        self.source.kind() == io::ErrorKind::NotFound
            || self.source.raw_os_error() == Some(libc::ENODEV)
    }
}

/// Filesystem primitives, so tests can mimic ConfigFS on a plain directory.
pub trait Backend {
    fn mkdir(&mut self, p: &Path) -> io::Result<()>;
    fn write(&mut self, p: &Path, data: &[u8]) -> io::Result<()>;
    fn symlink(&mut self, target: &Path, link: &Path) -> io::Result<()>;
    fn unlink(&mut self, p: &Path) -> io::Result<()>;
    fn rmdir(&mut self, p: &Path) -> io::Result<()>;
}

/// The real filesystem (ConfigFS on the board).
pub struct RealFs;

impl Backend for RealFs {
    fn mkdir(&mut self, p: &Path) -> io::Result<()> {
        fs::create_dir(p)
    }
    fn write(&mut self, p: &Path, data: &[u8]) -> io::Result<()> {
        fs::write(p, data)
    }
    fn symlink(&mut self, target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    fn unlink(&mut self, p: &Path) -> io::Result<()> {
        fs::remove_file(p)
    }
    fn rmdir(&mut self, p: &Path) -> io::Result<()> {
        fs::remove_dir(p)
    }
}

/// Apply operations in order. `Mkdir` of an existing directory succeeds:
/// ConfigFS creates `strings/`, `configs/` and `functions/` itself (and
/// `strings/` inside each config), and a crashed run may leave a partial
/// tree behind.
pub fn apply_on(ops: &[Op], fs: &mut dyn Backend) -> Result<(), ApplyError> {
    for op in ops {
        let r = match op {
            Op::Mkdir(p) => match fs.mkdir(p) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && p.is_dir() => Ok(()),
                r => r,
            },
            Op::Write(p, data) => fs.write(p, data),
            Op::Symlink { target, link } => fs.symlink(target, link),
            Op::Unlink(p) => fs.unlink(p),
            Op::Rmdir(p) => fs.rmdir(p),
        };
        r.map_err(|source| ApplyError {
            op: op.clone(),
            source,
        })?;
    }
    Ok(())
}

pub fn apply(ops: &[Op]) -> Result<(), ApplyError> {
    apply_on(ops, &mut RealFs)
}

/// The first USB device controller, e.g. `fe980000.usb` or `3f980000.usb`.
pub fn find_udc(class_dir: &Path) -> Option<String> {
    names(class_dir).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Vec<u8>> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
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

    /// The three receiver interfaces in a scrambled order.
    fn receiver() -> Option<Vec<HidDevice>> {
        Some(vec![
            device(2, fixture("0003-046D-C52B.0009.nodj.bin")?),
            device(0, fixture("0003-046D-C52B.0007.nodj.bin")?),
            device(1, fixture("0003-046D-C52B.0008.nodj.bin")?),
        ])
    }

    /// ConfigFS semantics on a plain directory: `mkdir` of a gadget creates
    /// `strings/`, `configs/` and `functions/`; `mkdir` of a config creates
    /// its `strings/`; `rmdir` drops attribute files and default groups.
    pub(crate) struct SimConfigFs {
        pub root: PathBuf,
    }

    impl Backend for SimConfigFs {
        fn mkdir(&mut self, p: &Path) -> io::Result<()> {
            fs::create_dir(p)?;
            let parent = p.parent().unwrap_or(Path::new(""));
            if parent == self.root {
                for d in ["strings", "configs", "functions"] {
                    fs::create_dir(p.join(d))?;
                }
            } else if parent.file_name().is_some_and(|n| n == "configs") {
                fs::create_dir(p.join("strings"))?;
            }
            Ok(())
        }
        fn write(&mut self, p: &Path, data: &[u8]) -> io::Result<()> {
            fs::write(p, data)
        }
        fn symlink(&mut self, target: &Path, link: &Path) -> io::Result<()> {
            std::os::unix::fs::symlink(target, link)
        }
        fn unlink(&mut self, p: &Path) -> io::Result<()> {
            fs::remove_file(p)
        }
        fn rmdir(&mut self, p: &Path) -> io::Result<()> {
            // ConfigFS refuses only if user-made subdirectories or links
            // remain; attributes and default groups go with the directory.
            for e in fs::read_dir(p)?.flatten() {
                let ft = e.file_type()?;
                let name = e.file_name();
                let default_group = ["strings", "configs", "functions"]
                    .iter()
                    .any(|d| name == *d)
                    && fs::read_dir(e.path())?.next().is_none();
                if ft.is_symlink() || (ft.is_dir() && !default_group) {
                    return Err(io::Error::from_raw_os_error(libc::ENOTEMPTY));
                }
            }
            fs::remove_dir_all(p)
        }
    }

    fn opts(expose_hidpp: bool) -> Options {
        Options {
            expose_hidpp,
            serial: "RK0001".into(),
        }
    }

    #[test]
    fn report_lengths_of_the_real_interfaces() {
        let Some(devs) = receiver() else { return };
        let spec = spec_for(&devs, &opts(true)).unwrap();
        let got: Vec<(u8, u8, u8, u16)> = spec
            .functions
            .iter()
            .map(|f| (f.dev_id, f.subclass, f.protocol, f.report_length))
            .collect();
        assert_eq!(
            got,
            [
                (0, 1, 1, 8),  // boot keyboard, no report ID
                (1, 1, 2, 8),  // mouse ID 2: 7 bytes + ID
                (2, 0, 0, 32), // DJ long report 0x21: 31 bytes + ID
            ]
        );
        assert_eq!(spec.vendor_id, 0x046d);
        assert_eq!(spec.product_id, 0xc52b);
        assert_eq!(spec.manufacturer, "Logitech");
        assert_eq!(spec.product, "Logitech USB Receiver");
        assert_eq!(spec.serial, "RK0001");
    }

    #[test]
    fn hidpp_can_be_hidden() {
        let Some(devs) = receiver() else { return };
        let spec = spec_for(&devs, &opts(false)).unwrap();
        let ids: Vec<u8> = spec.functions.iter().map(|f| f.dev_id).collect();
        assert_eq!(ids, [0, 1]);
        assert!(spec_for(&[], &opts(true)).is_none());
    }

    #[test]
    fn builds_the_tree_with_functions_linked_in_order() {
        let Some(devs) = receiver() else { return };
        let spec = spec_for(&devs, &opts(true)).unwrap();
        let root = std::env::temp_dir().join(format!("rawkvm-gadget-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let g = root.join(DEFAULT_NAME);
        let ops = build_ops(&g, &spec);
        let mut sim = SimConfigFs { root: root.clone() };
        apply_on(&ops, &mut sim).unwrap();

        let read = |p: &str| fs::read_to_string(g.join(p)).unwrap();
        assert_eq!(read("idVendor"), "0x046d\n");
        assert_eq!(read("idProduct"), "0xc52b\n");
        assert_eq!(read("strings/0x409/product"), "Logitech USB Receiver\n");
        assert_eq!(read("configs/c.1/MaxPower"), "98\n");
        assert_eq!(read("functions/hid.usb1/protocol"), "2\n");
        assert_eq!(read("functions/hid.usb2/report_length"), "32\n");
        assert_eq!(
            fs::read(g.join("functions/hid.usb0/report_desc")).unwrap(),
            spec.functions[0].report_desc
        );

        // Links are created in interface order 0, 1, 2.
        let links: Vec<&PathBuf> = ops
            .iter()
            .filter_map(|o| match o {
                Op::Symlink { link, .. } => Some(link),
                _ => None,
            })
            .collect();
        let names: Vec<String> = links
            .iter()
            .map(|l| l.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["hid.usb0", "hid.usb1", "hid.usb2"]);
        // Every function is configured before any is linked.
        let first_link = ops
            .iter()
            .position(|o| matches!(o, Op::Symlink { .. }))
            .unwrap();
        assert!(ops[first_link..]
            .iter()
            .all(|o| matches!(o, Op::Symlink { .. })));

        // Teardown order, from what is on disk.
        fs::write(g.join("UDC"), "fe980000.usb\n").unwrap();
        let existing = inspect(&g);
        assert!(existing.bound);
        assert_eq!(existing.configs[0].1, ["hid.usb0", "hid.usb1", "hid.usb2"]);
        let td = teardown_ops(&g, &existing);
        assert_eq!(td[0], Op::Write(g.join("UDC"), b"\n".to_vec()));
        let pos = |want: &Op| td.iter().position(|o| o == want).unwrap();
        let unlink0 = pos(&Op::Unlink(g.join("configs/c.1/hid.usb0")));
        let rm_config = pos(&Op::Rmdir(g.join("configs/c.1")));
        let rm_func = pos(&Op::Rmdir(g.join("functions/hid.usb0")));
        assert!(unlink0 < rm_config && rm_config < rm_func);
        assert_eq!(td.last(), Some(&Op::Rmdir(g.clone())));
        apply_on(&td, &mut sim).unwrap();
        assert!(!g.exists());

        fs::remove_dir_all(root).unwrap();
    }

    /// What the Orange Pi was left with: the gadget directory with its
    /// default groups and nothing else. Teardown must remove it and a new
    /// build must succeed on top of the automatic directories.
    #[test]
    fn partial_tree_from_a_failed_run_is_cleaned_and_rebuilt() {
        let Some(devs) = receiver() else { return };
        let spec = spec_for(&devs, &opts(true)).unwrap();
        let root = std::env::temp_dir().join(format!("rawkvm-gadget-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let g = root.join(DEFAULT_NAME);
        let mut sim = SimConfigFs { root: root.clone() };
        apply_on(&[Op::Mkdir(g.clone())], &mut sim).unwrap();
        assert!(g.join("strings").is_dir() && g.join("configs").is_dir());

        let td = teardown_ops(&g, &inspect(&g));
        assert_eq!(td, [Op::Rmdir(g.clone())]);
        apply_on(&td, &mut sim).unwrap();
        assert!(!g.exists());

        // Building over the automatic directories (here: without the
        // teardown) works too.
        apply_on(&[Op::Mkdir(g.clone())], &mut sim).unwrap();
        apply_on(&build_ops(&g, &spec), &mut sim).unwrap();
        assert_eq!(fs::read_to_string(g.join("idVendor")).unwrap(), "0x046d\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mkdir_over_a_file_is_still_an_error() {
        let root = std::env::temp_dir().join(format!("rawkvm-gadget-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x"), "").unwrap();
        let e = apply(&[Op::Mkdir(root.join("x"))]).unwrap_err();
        assert_eq!(e.source.kind(), io::ErrorKind::AlreadyExists);
        assert!(!e.is_missing_support());
        let e = apply(&[Op::Mkdir(root.join("no/such/dir"))]).unwrap_err();
        assert!(e.is_missing_support());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_the_udc() {
        let dir = std::env::temp_dir().join(format!("rawkvm-udc-{}", uuid::Uuid::new_v4()));
        assert_eq!(find_udc(&dir), None);
        fs::create_dir_all(dir.join("fe980000.usb")).unwrap();
        assert_eq!(find_udc(&dir).as_deref(), Some("fe980000.usb"));
        fs::remove_dir_all(dir).unwrap();
    }
}
