//! HID++ over the receiver's HID++ interface (mode A: `hidraw2`).
//!
//! Requests and replies are matched the way Solaar's `base.request` and the
//! kernel's `hidpp_send_message_sync` (`hid-logitech-hidpp.c`) do:
//!
//! ```text
//! B0 report ID  0x10 short (7 bytes) | 0x11 long (20 bytes)
//! B1 device     1..=6 slot, 0xFF receiver
//! B2 sub ID     HID++ 1.0 register op (0x80..=0x83) | HID++ 2.0 feature index
//! B3 address    register | function << 4 | software id
//! B4..          parameters
//! ```
//!
//! A reply has the request's B1..B3 (and, for a 0xB5 page read, its B4).
//! Errors: HID++ 1.0 `10 dd 8F <B2> <B3> <code>`, HID++ 2.0
//! `11 dd FF <B2> <B3> <code>`. Notifications carry software id 0, so they
//! never match a request; neither does anything sent by another program
//! with its own software id.
//!
//! Requests block the calling thread (CLI, a task of its own), never a
//! reader thread: those only hand reports to [`Hidpp::on_report`].

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::router::ReportWriter;

pub const REPORT_SHORT: u8 = 0x10;
pub const REPORT_LONG: u8 = 0x11;
pub const SHORT_LEN: usize = 7;
pub const LONG_LEN: usize = 20;
pub const RECEIVER: u8 = 0xFF;

/// Our software id, in the low nibble of B3 of HID++ 2.0 requests. Not 0
/// (notifications), 0x01 (the kernel), 0x0B (Solaar) or 0x0F (firmware);
/// 0x07, 0x0A and 0x0D are other tools' (Solaar `base.py`).
pub const SW_ID: u8 = 0x0C;

const SUB_SET_REGISTER: u8 = 0x80;
const SUB_GET_REGISTER: u8 = 0x81;
const SUB_GET_LONG_REGISTER: u8 = 0x83;
const SUB_ERROR_10: u8 = 0x8F;
const SUB_ERROR_20: u8 = 0xFF;

/// Receiver register with pairing information pages.
pub const REG_RECEIVER_INFO: u8 = 0xB5;
const PAGE_PAIRING: u8 = 0x20;
const PAGE_EXTENDED: u8 = 0x30;
const PAGE_NAME: u8 = 0x40;

pub const TIMEOUT: Duration = Duration::from_millis(300);
pub const RETRIES: u32 = 2;

/// HID++ 1.0 error codes (`hidpp10_constants.ErrorCode`).
pub mod err10 {
    pub const INVALID_SUB_ID: u8 = 0x01;
    pub const CONNECTION_REQUEST_FAILED: u8 = 0x04;
    pub const UNKNOWN_DEVICE: u8 = 0x08;
    pub const RESOURCE_ERROR: u8 = 0x09;
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("no reply")]
    Timeout,
    #[error("HID++ 1.0 error {code:#04x} ({name})", code = .0, name = err10_name(*.0))]
    Hidpp10(u8),
    #[error("HID++ 2.0 error {code:#04x} ({name})", code = .0, name = err20_name(*.0))]
    Hidpp20(u8),
    #[error("writing the request failed")]
    Write,
    #[error("malformed reply")]
    Malformed,
}

impl Error {
    /// The device is paired but cannot be reached now (off, asleep, out of
    /// range), or the slot is empty.
    pub fn is_unreachable(&self) -> bool {
        matches!(
            self,
            Error::Timeout
                | Error::Hidpp10(
                    err10::CONNECTION_REQUEST_FAILED
                        | err10::UNKNOWN_DEVICE
                        | err10::RESOURCE_ERROR
                )
        )
    }
}

fn err10_name(c: u8) -> &'static str {
    match c {
        0x01 => "invalid sub id",
        0x02 => "invalid address",
        0x03 => "invalid value",
        0x04 => "connection request failed",
        0x05 => "too many devices",
        0x06 => "already exists",
        0x07 => "busy",
        0x08 => "unknown device",
        0x09 => "resource error",
        0x0A => "request unavailable",
        0x0B => "unsupported parameter value",
        0x0C => "wrong pin code",
        _ => "?",
    }
}

fn err20_name(c: u8) -> &'static str {
    match c {
        0x01 => "unknown",
        0x02 => "invalid argument",
        0x03 => "out of range",
        0x04 => "hardware error",
        0x05 => "not allowed",
        0x06 => "invalid feature index",
        0x07 => "invalid function",
        0x08 => "busy",
        0x09 => "unsupported",
        _ => "?",
    }
}

/// One request: B1..B3, parameters, and the extra byte a reply must echo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub device: u8,
    pub sub_id: u8,
    pub address: u8,
    pub params: Vec<u8>,
    /// B4 the reply must have (0xB5 page reads).
    pub echo_b4: Option<u8>,
}

impl Request {
    /// HID++ 2.0 call of `function` of the feature at `feature_index`.
    pub fn feature(device: u8, feature_index: u8, function: u8, params: &[u8]) -> Self {
        Self {
            device,
            sub_id: feature_index,
            address: (function << 4) | SW_ID,
            params: params.to_vec(),
            echo_b4: None,
        }
    }

    /// HID++ 1.0 short register read.
    pub fn read_register(device: u8, register: u8, params: &[u8]) -> Self {
        Self {
            device,
            sub_id: SUB_GET_REGISTER,
            address: register,
            params: params.to_vec(),
            echo_b4: None,
        }
    }

    /// HID++ 1.0 short register write.
    pub fn write_register(device: u8, register: u8, params: &[u8]) -> Self {
        Self {
            device,
            sub_id: SUB_SET_REGISTER,
            address: register,
            params: params.to_vec(),
            echo_b4: None,
        }
    }

    /// HID++ 1.0 long register read (sent as a short report, like Solaar).
    pub fn read_long_register(device: u8, register: u8, page: u8) -> Self {
        Self {
            device,
            sub_id: SUB_GET_LONG_REGISTER,
            address: register,
            params: vec![page],
            echo_b4: (register == REG_RECEIVER_INFO).then_some(page),
        }
    }

    /// The report bytes: short if the parameters fit in 3 bytes.
    pub fn encode(&self) -> Vec<u8> {
        let (id, len) = if self.params.len() <= SHORT_LEN - 4 {
            (REPORT_SHORT, SHORT_LEN)
        } else {
            (REPORT_LONG, LONG_LEN)
        };
        let mut r = vec![0u8; len];
        r[0] = id;
        r[1] = self.device;
        r[2] = self.sub_id;
        r[3] = self.address;
        let n = self.params.len().min(len - 4);
        r[4..4 + n].copy_from_slice(&self.params[..n]);
        r
    }

    /// What `report` means for this request, if it answers it.
    pub fn answer(&self, report: &[u8]) -> Option<Result<Vec<u8>, Error>> {
        if report.len() < 6
            || !matches!(report[0], REPORT_SHORT | REPORT_LONG)
            || report[1] != self.device
        {
            return None;
        }
        let (b2, b3) = (report[2], report[3]);
        if b2 == SUB_ERROR_10
            && report[0] == REPORT_SHORT
            && report[3] == self.sub_id
            && report[4] == self.address
        {
            return Some(Err(Error::Hidpp10(report[5])));
        }
        if b2 == SUB_ERROR_20 && report[3] == self.sub_id && report[4] == self.address {
            return Some(Err(Error::Hidpp20(report[5])));
        }
        if b2 != self.sub_id || b3 != self.address {
            return None;
        }
        if self.echo_b4.is_some_and(|p| report[4] != p) {
            return None;
        }
        Some(Ok(report[4..].to_vec()))
    }
}

struct Pending {
    req: Request,
    tx: mpsc::Sender<Result<Vec<u8>, Error>>,
}

/// HID++ requests over one receiver interface.
pub struct Hidpp {
    writer: Arc<dyn ReportWriter>,
    pending: Mutex<Option<Pending>>,
    /// One request in flight at a time.
    serial: Mutex<()>,
    trace: Option<Trace>,
}

/// See [`Hidpp::with_trace`].
type Trace = Box<dyn Fn(char, &[u8]) + Send + Sync>;

impl Hidpp {
    pub fn new(writer: Arc<dyn ReportWriter>) -> Self {
        Self {
            writer,
            pending: Mutex::new(None),
            serial: Mutex::new(()),
            trace: None,
        }
    }

    /// Call `f('>', request)` and `f('<', report)` for every report (CLI
    /// `--hidpp-trace`, to record fixtures).
    pub fn with_trace(mut self, f: impl Fn(char, &[u8]) + Send + Sync + 'static) -> Self {
        self.trace = Some(Box::new(f));
        self
    }

    /// A report read from the interface. True if it answered our request:
    /// it must not be forwarded to the agents.
    pub fn on_report(&self, report: &[u8]) -> bool {
        if let Some(t) = &self.trace {
            t('<', report);
        }
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let Some(answer) = p.as_ref().and_then(|p| p.req.answer(report)) else {
            return false;
        };
        if let Some(p) = p.take() {
            let _ = p.tx.send(answer);
        }
        true
    }

    /// Send `req` and wait for its answer: [`TIMEOUT`] per try, [`RETRIES`]
    /// more tries on timeout. Returns the reply from B4 on.
    pub fn request(&self, req: &Request) -> Result<Vec<u8>, Error> {
        self.request_with(req, TIMEOUT, RETRIES)
    }

    pub fn request_with(
        &self,
        req: &Request,
        timeout: Duration,
        retries: u32,
    ) -> Result<Vec<u8>, Error> {
        let _one = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = req.encode();
        for _ in 0..=retries {
            let (tx, rx) = mpsc::channel();
            *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pending {
                req: req.clone(),
                tx,
            });
            if let Some(t) = &self.trace {
                t('>', &bytes);
            }
            if !self.writer.write(&bytes) {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                return Err(Error::Write);
            }
            match rx.recv_timeout(timeout) {
                Ok(r) => return r,
                Err(_) => {
                    self.pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take();
                }
            }
        }
        Err(Error::Timeout)
    }

    // ---- HID++ 2.0 -----------------------------------------------------

    /// IRoot.ping: the device's HID++ version (major, minor). HID++ 1.0
    /// devices answer "invalid sub id" (Solaar `base.ping`, kernel
    /// `hidpp_root_get_protocol_version`).
    pub fn protocol_version(&self, device: u8) -> Result<(u8, u8), Error> {
        const MARK: u8 = 0x5A;
        match self.request(&Request::feature(device, 0, 1, &[0, 0, MARK])) {
            Ok(r) if r.len() >= 3 && r[2] == MARK => Ok((r[0], r[1])),
            Ok(_) => Err(Error::Malformed),
            Err(Error::Hidpp10(err10::INVALID_SUB_ID)) => Ok((1, 0)),
            Err(e) => Err(e),
        }
    }

    /// IRoot.getFeature: the index of `feature` on `device`, `None` if it
    /// does not have it.
    pub fn feature_index(&self, device: u8, feature: u16) -> Result<Option<u8>, Error> {
        let r = self.request(&Request::feature(device, 0, 0, &feature.to_be_bytes()))?;
        match r.first() {
            Some(0) => Ok(None),
            Some(&i) => Ok(Some(i)),
            None => Err(Error::Malformed),
        }
    }

    // ---- receiver (HID++ 1.0) -------------------------------------------

    /// Pairing information of receiver slot `slot` (1..=6), `None` if the
    /// slot is empty.
    pub fn pairing_info(&self, slot: u8) -> Result<Option<PairingInfo>, Error> {
        let page = |base: u8| base + slot - 1;
        let pairing = match self.request(&Request::read_long_register(
            RECEIVER,
            REG_RECEIVER_INFO,
            page(PAGE_PAIRING),
        )) {
            Ok(r) => r,
            Err(Error::Hidpp10(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut info = PairingInfo::parse(&pairing).ok_or(Error::Malformed)?;
        if info.wireless_pid == 0 {
            return Ok(None);
        }
        if let Ok(ext) = self.request(&Request::read_long_register(
            RECEIVER,
            REG_RECEIVER_INFO,
            page(PAGE_EXTENDED),
        )) {
            info.serial = parse_serial(&ext);
        }
        if let Ok(name) = self.request(&Request::read_long_register(
            RECEIVER,
            REG_RECEIVER_INFO,
            page(PAGE_NAME),
        )) {
            info.name = parse_name(&name);
        }
        Ok(Some(info))
    }
}

/// Feature HOSTS_INFO (0x1815).
pub const FEATURE_HOSTS_INFO: u16 = 0x1815;

/// HOSTS_INFO function 0 (Solaar `get_host_names`, `K375sFnSwap`):
/// capability flags, ?, number of hosts, current host (0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostsInfo {
    pub flags: u8,
    pub num_hosts: u8,
    pub current_host: u8,
}

impl HostsInfo {
    pub fn parse(r: &[u8]) -> Option<Self> {
        Some(Self {
            flags: *r.first()?,
            num_hosts: *r.get(2)?,
            current_host: *r.get(3)?,
        })
    }
}

impl Hidpp {
    pub fn hosts_info(&self, device: u8, index: u8) -> Result<HostsInfo, Error> {
        let r = self.request(&Request::feature(device, index, 0, &[]))?;
        HostsInfo::parse(&r).ok_or(Error::Malformed)
    }

    /// HOSTS_INFO function 1, getHostInfo(host): reply from B4 (host,
    /// status, ?, ?, name length, max name length); raw, for the CLI.
    pub fn host_info(&self, device: u8, index: u8, host: u8) -> Result<Vec<u8>, Error> {
        self.request(&Request::feature(device, index, 1, &[host]))
    }

    /// Everything the hub can read about receiver slot `slot`; `None` if
    /// the slot is empty. Failures past the pairing pages are kept in
    /// `error` (the first one) and the rest is still read.
    pub fn survey(&self, slot: u8) -> Result<Option<SlotInfo>, Error> {
        let Some(pairing) = self.pairing_info(slot)? else {
            return Ok(None);
        };
        let mut s = SlotInfo {
            slot,
            pairing,
            protocol: None,
            hosts_info: None,
            hosts: None,
            host_os: None,
            multiplatform: None,
            platforms: Vec::new(),
            dualplatform: None,
            error: None,
        };
        let note = |e: Error, s: &mut SlotInfo| {
            s.error.get_or_insert(e);
        };
        match self.protocol_version(slot) {
            Ok(v) => s.protocol = Some(v),
            Err(e) => {
                note(e, &mut s);
                return Ok(Some(s));
            }
        }
        if s.protocol.is_some_and(|(major, _)| major < 2) {
            return Ok(Some(s));
        }
        match self.feature_index(slot, FEATURE_HOSTS_INFO) {
            Ok(Some(i)) => {
                s.hosts_info = Some(i);
                match self.hosts_info(slot, i) {
                    Ok(h) => {
                        s.hosts = Some(h);
                        match self.host_os(slot, i, h.current_host) {
                            Ok(o) => s.host_os = Some(o),
                            Err(e) => note(e, &mut s),
                        }
                    }
                    Err(e) => note(e, &mut s),
                }
            }
            Ok(None) => {}
            Err(e) => note(e, &mut s),
        }
        match self.feature_index(slot, FEATURE_MULTIPLATFORM) {
            Ok(Some(i)) => match self.multiplatform(slot, i) {
                Ok(m) => {
                    s.multiplatform = Some((i, m));
                    for n in 0..m.descriptors {
                        match self.platform_descriptor(slot, i, n) {
                            Ok(d) => s.platforms.push(d),
                            Err(e) => note(e, &mut s),
                        }
                    }
                }
                Err(e) => note(e, &mut s),
            },
            Ok(None) => {}
            Err(e) => note(e, &mut s),
        }
        match self.feature_index(slot, FEATURE_DUALPLATFORM) {
            Ok(Some(i)) => match self.dualplatform(slot, i) {
                Ok(v) => s.dualplatform = Some((i, v)),
                Err(e) => note(e, &mut s),
            },
            Ok(None) => {}
            Err(e) => note(e, &mut s),
        }
        Ok(Some(s))
    }

    /// Set the keyboard layout of `s` (a fresh survey) to `os` with
    /// `via`, on the host it is connected to now. Returns what was written.
    pub fn set_os(&self, s: &SlotInfo, os: OsKind, via: Via) -> Result<String, SetOsError> {
        let slot = s.slot;
        let multi = || -> Result<String, SetOsError> {
            let (i, m) = s
                .multiplatform
                .ok_or(SetOsError::Unsupported("MULTIPLATFORM"))?;
            if !m.settable() {
                return Err(SetOsError::Unsupported("MULTIPLATFORM (not settable)"));
            }
            let d = s
                .platforms
                .iter()
                .find(|d| d.os_flags & os.multiplatform_bit() != 0)
                .ok_or(SetOsError::NoPlatform(os))?;
            self.set_platform(slot, i, d.platform)?;
            Ok(format!("MULTIPLATFORM: platform {} ({:?})", d.platform, os))
        };
        let hosts = || -> Result<String, SetOsError> {
            let (i, h) = s
                .hosts_info
                .zip(s.hosts)
                .ok_or(SetOsError::Unsupported("HOSTS_INFO"))?;
            if h.flags & HOSTS_CAN_SET_OS == 0 {
                return Err(SetOsError::Unsupported("HOSTS_INFO setHostOsVersion"));
            }
            // Keep the version the device has; only the type matters here.
            let new = HostOs {
                os_type: os.hosts_info_type(),
                ..s.host_os.unwrap_or(HostOs {
                    os_type: 0,
                    major: 0,
                    minor: 0,
                    patch: 0,
                })
            };
            self.set_host_os(slot, i, h.current_host, new)?;
            Ok(format!(
                "HOSTS_INFO: host {} OS type {} ({})",
                h.current_host,
                new.os_type,
                hosts_os_name(new.os_type)
            ))
        };
        let dual = || -> Result<String, SetOsError> {
            let (i, _) = s
                .dualplatform
                .ok_or(SetOsError::Unsupported("DUALPLATFORM"))?;
            let v = os.dualplatform_value();
            self.set_dualplatform(slot, i, v)?;
            Ok(format!("DUALPLATFORM: {v}"))
        };
        match via {
            Via::MultiPlatform => multi(),
            Via::HostsInfo => hosts(),
            Via::DualPlatform => dual(),
            Via::Auto => match multi() {
                Err(SetOsError::Unsupported(_)) => match hosts() {
                    Err(SetOsError::Unsupported(_)) => dual().map_err(|e| match e {
                        SetOsError::Unsupported(_) => SetOsError::Unsupported(
                            "MULTIPLATFORM, HOSTS_INFO setHostOsVersion and DUALPLATFORM",
                        ),
                        e => e,
                    }),
                    r => r,
                },
                r => r,
            },
        }
    }
}

/// Which feature [`Hidpp::set_os`] uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// MULTIPLATFORM, else HOSTS_INFO, else DUALPLATFORM.
    Auto,
    MultiPlatform,
    HostsInfo,
    DualPlatform,
}

impl Via {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "multiplatform" | "4531" => Some(Self::MultiPlatform),
            "hostsinfo" | "hosts-info" | "1815" => Some(Self::HostsInfo),
            "dualplatform" | "4530" => Some(Self::DualPlatform),
            _ => None,
        }
    }
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum SetOsError {
    #[error("the device has no {0}")]
    Unsupported(&'static str),
    #[error("MULTIPLATFORM has no platform for {0:?}")]
    NoPlatform(OsKind),
    #[error(transparent)]
    Hidpp(#[from] Error),
}

/// Feature MULTIPLATFORM (0x4531): the keyboard's OS layout (Solaar
/// `Multiplatform` setting; kernel patch "HID: logitech-hidpp: Add support
/// for HID++ Multi-Platform feature (0x4531)").
pub const FEATURE_MULTIPLATFORM: u16 = 0x4531;
/// Feature DUALPLATFORM (0x4530): two layouts (Solaar `DualPlatform`).
pub const FEATURE_DUALPLATFORM: u16 = 0x4530;

/// The OS a profile is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Windows,
    MacOs,
    Linux,
}

impl OsKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "windows" => Some(Self::Windows),
            "macos" | "mac" => Some(Self::MacOs),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }

    /// Bit in MULTIPLATFORM's `os_flags` (Solaar `Multiplatform.OSS`).
    pub fn multiplatform_bit(self) -> u16 {
        match self {
            Self::Windows => 0x0100,
            Self::Linux => 0x0400,
            Self::MacOs => 0x2000,
        }
    }

    /// HOSTS_INFO OS type (Logi Options+ `HostInfo.OSType`, as recovered
    /// by OpenLogi `feature/hosts_info.rs`): 1 Windows, 3 Linux, 6 macOS.
    pub fn hosts_info_type(self) -> u8 {
        match self {
            Self::Windows => 1,
            Self::Linux => 3,
            Self::MacOs => 6,
        }
    }

    /// DUALPLATFORM value: 0 iOS/macOS, 1 Android/Windows.
    pub fn dualplatform_value(self) -> u8 {
        match self {
            Self::MacOs => 0,
            Self::Windows | Self::Linux => 1,
        }
    }
}

/// Name of a HOSTS_INFO OS type.
pub fn hosts_os_name(t: u8) -> &'static str {
    match t {
        0 => "unknown",
        1 => "Windows",
        2 => "Windows Embedded",
        3 => "Linux",
        4 => "ChromeOS",
        5 => "Android",
        6 => "macOS",
        7 => "iOS",
        8 => "webOS",
        9 => "Tizen",
        _ => "?",
    }
}

/// Names of the OSes in a MULTIPLATFORM `os_flags` mask.
pub fn multiplatform_os_names(flags: u16) -> Vec<&'static str> {
    [
        ("Windows", 0x0100),
        ("WinEmb", 0x0200),
        ("Linux", 0x0400),
        ("Chrome", 0x0800),
        ("Android", 0x1000),
        ("macOS", 0x2000),
        ("iOS", 0x4000),
        ("webOS", 0x8000),
        ("Tizen", 0x0001),
    ]
    .into_iter()
    .filter(|(_, b)| flags & b != 0)
    .map(|(n, _)| n)
    .collect()
}

/// MULTIPLATFORM function 0: flags (0x02 = the platform can be set), ?,
/// number of descriptors, ..., current platform at byte 6 (Solaar reads it
/// with `read_skip_byte_count=6`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultiPlatform {
    pub flags: u8,
    pub descriptors: u8,
    pub current: u8,
}

impl MultiPlatform {
    pub fn parse(r: &[u8]) -> Option<Self> {
        Some(Self {
            flags: *r.first()?,
            descriptors: *r.get(2)?,
            current: *r.get(6)?,
        })
    }

    pub fn settable(&self) -> bool {
        self.flags & 0x02 != 0
    }
}

/// MULTIPLATFORM function 1 (index): `!BBHHH` platform, index, os_flags,
/// lowest and highest OS version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformDescriptor {
    pub platform: u8,
    pub os_flags: u16,
}

impl PlatformDescriptor {
    pub fn parse(r: &[u8]) -> Option<Self> {
        Some(Self {
            platform: *r.first()?,
            os_flags: u16::from_be_bytes([*r.get(2)?, *r.get(3)?]),
        })
    }
}

/// HOSTS_INFO function 7, getHostOsVersion(host): host, OS type, major,
/// minor (u16 BE), patch (u16 BE). From OpenLogi's reverse engineering of
/// Logi Options+; not in Solaar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostOs {
    pub os_type: u8,
    pub major: u8,
    pub minor: u16,
    pub patch: u16,
}

impl HostOs {
    pub fn parse(r: &[u8]) -> Option<Self> {
        Some(Self {
            os_type: *r.get(1)?,
            major: *r.get(2)?,
            minor: u16::from_be_bytes([*r.get(3)?, *r.get(4)?]),
            patch: u16::from_be_bytes([*r.get(5)?, *r.get(6)?]),
        })
    }
}

/// HOSTS_INFO capability bit of setHostOsVersion (byte 0 bit 4; Options+
/// reads bytes 0..2 as the BE mask 0x1000).
pub const HOSTS_CAN_SET_OS: u8 = 0x10;

impl Hidpp {
    pub fn multiplatform(&self, device: u8, index: u8) -> Result<MultiPlatform, Error> {
        let r = self.request(&Request::feature(device, index, 0, &[]))?;
        MultiPlatform::parse(&r).ok_or(Error::Malformed)
    }

    pub fn platform_descriptor(
        &self,
        device: u8,
        index: u8,
        n: u8,
    ) -> Result<PlatformDescriptor, Error> {
        let r = self.request(&Request::feature(device, index, 1, &[n]))?;
        PlatformDescriptor::parse(&r).ok_or(Error::Malformed)
    }

    /// MULTIPLATFORM function 3, setCurrentPlatform: `FF <platform>`
    /// (0xFF = the current host).
    pub fn set_platform(&self, device: u8, index: u8, platform: u8) -> Result<(), Error> {
        self.request(&Request::feature(device, index, 3, &[0xFF, platform]))
            .map(drop)
    }

    /// DUALPLATFORM function 0: the current value (byte 0).
    pub fn dualplatform(&self, device: u8, index: u8) -> Result<u8, Error> {
        let r = self.request(&Request::feature(device, index, 0, &[]))?;
        r.first().copied().ok_or(Error::Malformed)
    }

    /// DUALPLATFORM function 2: set the value.
    pub fn set_dualplatform(&self, device: u8, index: u8, value: u8) -> Result<(), Error> {
        self.request(&Request::feature(device, index, 2, &[value]))
            .map(drop)
    }

    pub fn host_os(&self, device: u8, index: u8, host: u8) -> Result<HostOs, Error> {
        let r = self.request(&Request::feature(device, index, 7, &[host]))?;
        HostOs::parse(&r).ok_or(Error::Malformed)
    }

    /// HOSTS_INFO function 8, setHostOsVersion: host, type, major, minor
    /// (BE), patch (BE). Layout from OpenLogi (Options+ static analysis);
    /// UNVERIFIED on hardware.
    pub fn set_host_os(&self, device: u8, index: u8, host: u8, os: HostOs) -> Result<(), Error> {
        let [mn0, mn1] = os.minor.to_be_bytes();
        let [p0, p1] = os.patch.to_be_bytes();
        self.request(&Request::feature(
            device,
            index,
            8,
            &[host, os.os_type, os.major, mn0, mn1, p0, p1],
        ))
        .map(drop)
    }
}

/// One paired device as [`Hidpp::survey`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInfo {
    pub slot: u8,
    pub pairing: PairingInfo,
    /// HID++ version; `None` if the device did not answer (off, asleep).
    pub protocol: Option<(u8, u8)>,
    /// Feature index of HOSTS_INFO, if the device has it.
    pub hosts_info: Option<u8>,
    pub hosts: Option<HostsInfo>,
    /// OS declared for the current host (HOSTS_INFO function 7).
    pub host_os: Option<HostOs>,
    /// Feature index and state of MULTIPLATFORM, and its descriptors.
    pub multiplatform: Option<(u8, MultiPlatform)>,
    pub platforms: Vec<PlatformDescriptor>,
    /// Feature index and value of DUALPLATFORM.
    pub dualplatform: Option<(u8, u8)>,
    /// The first request that failed, if any.
    pub error: Option<Error>,
}

/// From a 0xB5 pairing information page (0x20 + slot - 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingInfo {
    pub wireless_pid: u16,
    pub kind: u8,
    pub name: Option<String>,
    pub serial: Option<u32>,
}

impl PairingInfo {
    /// `r` from B4: page, ?, polling rate, PID (big endian, B7..B8), ...,
    /// kind in the low nibble of B11 (Solaar `receiver.extract_*`).
    pub fn parse(r: &[u8]) -> Option<Self> {
        if r.len() < 8 {
            return None;
        }
        Some(Self {
            wireless_pid: u16::from_be_bytes([r[3], r[4]]),
            kind: r[7] & 0x0F,
            name: None,
            serial: None,
        })
    }

    pub fn kind_name(&self) -> &'static str {
        kind_name(self.kind)
    }
}

/// Unifying device kinds (`hidpp10_constants.DEVICE_KIND`).
pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        1 => "keyboard",
        2 => "mouse",
        3 => "numpad",
        4 => "presenter",
        7 => "remote",
        8 => "trackball",
        9 => "touchpad",
        0x0A => "tablet",
        0x0B => "gamepad",
        0x0C => "joystick",
        0x0D => "headset",
        0x0E => "remote control",
        0x0F => "receiver",
        _ => "unknown",
    }
}

/// Extended pairing page (0x30 + slot - 1): serial in B5..B8.
fn parse_serial(r: &[u8]) -> Option<u32> {
    r.get(1..5)
        .map(|b| u32::from_be_bytes(b.try_into().expect("4 bytes")))
}

/// Name page (0x40 + slot - 1): length in B5, ASCII from B6.
fn parse_name(r: &[u8]) -> Option<String> {
    let len = *r.get(1)? as usize;
    let bytes = r.get(2..2 + len)?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A receiver that answers from a script of (request, replies). Replies
    /// are delivered from the writing thread, as if read back at once.
    /// A request and the reports that follow it.
    type Exchange = (Vec<u8>, Vec<Vec<u8>>);

    pub struct Scripted {
        script: Mutex<VecDeque<Exchange>>,
        hidpp: Mutex<Option<std::sync::Weak<Hidpp>>>,
        pub writes: Mutex<Vec<Vec<u8>>>,
    }

    impl Scripted {
        pub fn new(script: Vec<Exchange>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                hidpp: Mutex::new(None),
                writes: Mutex::default(),
            })
        }

        pub fn attach(self: &Arc<Self>) -> Arc<Hidpp> {
            let h = Arc::new(Hidpp::new(self.clone()));
            *self.hidpp.lock().unwrap() = Some(Arc::downgrade(&h));
            h
        }
    }

    impl ReportWriter for Scripted {
        fn write(&self, report: &[u8]) -> bool {
            self.writes.lock().unwrap().push(report.to_vec());
            let next = {
                let mut s = self.script.lock().unwrap();
                match s.front() {
                    Some((req, _)) if req == report => s.pop_front(),
                    _ => None, // unexpected: no answer (timeout)
                }
            };
            let h = self
                .hidpp
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|w| w.upgrade());
            if let (Some((_, replies)), Some(h)) = (next, h) {
                // Deliver after the caller starts waiting.
                std::thread::spawn(move || {
                    for r in replies {
                        h.on_report(&r);
                    }
                });
            }
            true
        }
    }

    pub fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    fn long(s: &str) -> Vec<u8> {
        let mut v = hex(s);
        v.resize(LONG_LEN, 0);
        v
    }

    #[test]
    fn requests_are_encoded_like_solaar() {
        assert_eq!(
            Request::read_long_register(RECEIVER, 0xB5, 0x20).encode(),
            hex("10 ff 83 b5 20 00 00")
        );
        assert_eq!(
            Request::feature(1, 0, 0, &0x1815u16.to_be_bytes()).encode(),
            hex("10 01 00 0c 18 15 00")
        );
        assert_eq!(
            Request::write_register(RECEIVER, 0xB2, &[0x03, 0x02]).encode(),
            hex("10 ff 80 b2 03 02 00")
        );
        let long = Request::feature(2, 5, 4, &[1, 2, 3, 4]).encode();
        assert_eq!(long.len(), LONG_LEN);
        assert_eq!(&long[..8], &hex("11 02 05 4c 01 02 03 04")[..]);
    }

    #[test]
    fn replies_errors_and_strangers() {
        let r = Request::feature(1, 0, 0, &[0x18, 0x15]);
        assert_eq!(
            r.answer(&hex(
                "11 01 00 0c 09 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00"
            )),
            Some(Ok(long("09 00 01")[..16].to_vec()))
        );
        // Another device, another software id, a notification: not ours.
        assert_eq!(r.answer(&hex("11 02 00 0c 09 00 01")), None);
        assert_eq!(r.answer(&hex("11 01 00 0b 09 00 01")), None);
        assert_eq!(r.answer(&hex("10 01 41 04 01 8a 40")), None);
        // HID++ 2.0 and 1.0 errors.
        assert_eq!(
            r.answer(&hex("11 01 ff 00 0c 06 00 00")),
            Some(Err(Error::Hidpp20(6)))
        );
        assert_eq!(
            r.answer(&hex("10 01 8f 00 0c 09 00")),
            Some(Err(Error::Hidpp10(err10::RESOURCE_ERROR)))
        );
        // A 0xB5 read only accepts its own page.
        let p = Request::read_long_register(RECEIVER, 0xB5, 0x41);
        assert_eq!(p.answer(&long("11 ff 83 b5 40 06")), None);
        assert!(p.answer(&long("11 ff 83 b5 41 06")).is_some());
        assert!(Error::Hidpp10(err10::UNKNOWN_DEVICE).is_unreachable());
        assert!(!Error::Hidpp20(2).is_unreachable());
    }

    #[test]
    fn matched_replies_are_consumed_and_others_forwarded() {
        let t = Scripted::new(vec![(
            hex("10 01 00 1c 00 00 5a"),
            vec![
                hex("10 01 41 04 01 8a 40"), // a notification first
                hex("10 01 00 1c 04 05 5a"),
            ],
        )]);
        let h = t.attach();
        assert_eq!(h.protocol_version(1), Ok((4, 5)));
        assert!(
            !h.on_report(&hex("10 01 00 1c 04 05 5a")),
            "nothing pending"
        );
    }

    #[test]
    fn timeouts_retry_and_offline_devices() {
        let t = Scripted::new(vec![
            // HID++ 1.0 device; offline device.
            (
                hex("10 02 00 1c 00 00 5a"),
                vec![hex("10 02 8f 00 1c 01 00")],
            ),
            (
                hex("10 03 00 1c 00 00 5a"),
                vec![hex("10 03 8f 00 1c 09 00")],
            ),
        ]);
        let h = t.attach();
        assert_eq!(h.protocol_version(2), Ok((1, 0)));
        let e = h.protocol_version(3).unwrap_err();
        assert!(e.is_unreachable());
        // Nobody answers slot 4: three tries, then Timeout.
        let r = Request::feature(4, 0, 1, &[0, 0, 0x5A]);
        assert_eq!(
            h.request_with(&r, Duration::from_millis(20), RETRIES),
            Err(Error::Timeout)
        );
        let tries = t
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w[1] == 4)
            .count();
        assert_eq!(tries, 1 + RETRIES as usize);
    }

    #[test]
    fn pairing_pages() {
        let t = Scripted::new(vec![
            (
                hex("10 ff 83 b5 20 00 00"),
                vec![long("11 ff 83 b5 20 08 14 40 8a 1a 40 01")],
            ),
            (
                hex("10 ff 83 b5 30 00 00"),
                vec![long("11 ff 83 b5 30 12 34 56 78")],
            ),
            (
                hex("10 ff 83 b5 40 00 00"),
                vec![long("11 ff 83 b5 40 07 4d 58 20 4b 65 79 73")],
            ),
            (
                hex("10 ff 83 b5 21 00 00"),
                vec![hex("10 ff 8f 83 b5 03 00")],
            ),
        ]);
        let h = t.attach();
        let kb = h.pairing_info(1).unwrap().unwrap();
        assert_eq!(kb.wireless_pid, 0x408a);
        assert_eq!(kb.kind_name(), "keyboard");
        assert_eq!(kb.serial, Some(0x1234_5678));
        assert_eq!(kb.name.as_deref(), Some("MX Keys"));
        assert_eq!(h.pairing_info(2), Ok(None));
    }

    #[test]
    fn survey_of_a_keyboard_with_hosts_info() {
        let t = Scripted::new(vec![
            (
                hex("10 ff 83 b5 20 00 00"),
                vec![long("11 ff 83 b5 20 08 14 40 8a 1a 40 01")],
            ),
            (hex("10 ff 83 b5 30 00 00"), vec![long("11 ff 83 b5 30")]),
            (
                hex("10 ff 83 b5 40 00 00"),
                vec![long("11 ff 83 b5 40 07 4d 58 20 4b 65 79 73")],
            ),
            (
                hex("10 01 00 1c 00 00 5a"),
                vec![hex("10 01 00 1c 04 05 5a")],
            ),
            (
                hex("10 01 00 0c 18 15 00"),
                vec![long("11 01 00 0c 0b 00 01")],
            ),
            (
                hex("10 01 0b 0c 00 00 00"),
                vec![long("11 01 0b 0c 03 00 03 01")],
            ),
            // getHostOsVersion(1): Windows 10.0.19045
            (
                hex("10 01 0b 7c 01 00 00"),
                vec![long("11 01 0b 7c 01 01 0a 00 00 4a 65")],
            ),
            (hex("10 01 00 0c 45 31 00"), vec![long("11 01 00 0c 0d 00")]),
            // settable, 2 descriptors, current platform 1
            (
                hex("10 01 0d 0c 00 00 00"),
                vec![long("11 01 0d 0c 02 00 02 00 00 00 01")],
            ),
            (
                hex("10 01 0d 1c 00 00 00"),
                vec![long("11 01 0d 1c 00 00 01 00")],
            ),
            (
                hex("10 01 0d 1c 01 00 00"),
                vec![long("11 01 0d 1c 01 01 20 00")],
            ),
            (hex("10 01 00 0c 45 30 00"), vec![long("11 01 00 0c 00")]),
            // set Windows: MULTIPLATFORM setCurrentPlatform(FF, 0)
            (hex("10 01 0d 3c ff 00 00"), vec![long("11 01 0d 3c ff 00")]),
        ]);
        let h = t.attach();
        let s = h.survey(1).unwrap().unwrap();
        assert_eq!(s.protocol, Some((4, 5)));
        assert_eq!(s.hosts_info, Some(0x0b));
        assert_eq!(
            s.hosts,
            Some(HostsInfo {
                flags: 3,
                num_hosts: 3,
                current_host: 1
            })
        );
        assert_eq!(
            s.host_os,
            Some(HostOs {
                os_type: 1,
                major: 10,
                minor: 0,
                patch: 19045
            })
        );
        assert_eq!(s.multiplatform.map(|m| (m.0, m.1.current)), Some((0x0d, 1)));
        let names: Vec<_> = s
            .platforms
            .iter()
            .map(|d| (d.platform, multiplatform_os_names(d.os_flags)))
            .collect();
        assert_eq!(names, vec![(0, vec!["Windows"]), (1, vec!["macOS"])]);
        assert_eq!(s.dualplatform, None);
        assert_eq!(s.error, None);

        assert_eq!(
            h.set_os(&s, OsKind::Windows, Via::Auto).unwrap(),
            "MULTIPLATFORM: platform 0 (Windows)"
        );
        assert_eq!(
            h.set_os(&s, OsKind::Linux, Via::Auto),
            Err(SetOsError::NoPlatform(OsKind::Linux))
        );
        // HOSTS_INFO without the set-OS capability bit (flags 0x03).
        assert_eq!(
            h.set_os(&s, OsKind::Windows, Via::HostsInfo),
            Err(SetOsError::Unsupported("HOSTS_INFO setHostOsVersion"))
        );
        assert!(
            t.script.lock().unwrap().is_empty(),
            "every request was made"
        );
    }

    #[test]
    fn set_host_os_bytes() {
        let t = Scripted::new(vec![(
            long("11 02 07 8c 00 06 0f 00 01 00 00"),
            vec![long("11 02 07 8c")],
        )]);
        let h = t.attach();
        let os = HostOs {
            os_type: OsKind::MacOs.hosts_info_type(),
            major: 15,
            minor: 1,
            patch: 0,
        };
        assert_eq!(h.set_host_os(2, 7, 0, os), Ok(()));
        assert_eq!(OsKind::parse("Linux"), Some(OsKind::Linux));
        assert_eq!(Via::parse("1815"), Some(Via::HostsInfo));
    }
}
