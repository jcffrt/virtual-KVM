//! Wire format of Karabiner-DriverKit-VirtualHIDDevice's daemon, as of
//! package 8.6.0 (client protocol 7, driver 1.8.0). See ADR 0005.
//!
//! Transport (`pqrs::unix_domain_stream`): a Unix stream socket carrying
//! frames `u32 BE body_len | u8 type | [u64 BE request_id] | payload`.
//! A client request payload is `u16 client_protocol_version | u8 request |
//! data`, where `data` is the C++ struct copied with `memcpy` (packed,
//! little-endian on every Mac). Status replies are pairs `(response, bool)`.

/// Must equal the daemon's `embedded_client_protocol_version`, otherwise it
/// answers every request with an empty payload.
pub const CLIENT_PROTOCOL_VERSION: u16 = 7;

pub const SOCKET_PATH: &str =
    "/Library/Application Support/org.pqrs/tmp/rootonly/karabiner_virtual_hid_device_service.sock";

/// `unix_domain_stream_max_message_size`.
pub const MAX_MESSAGE: usize = 1024;

const HEADER: usize = 4;
const REQUEST_ID: usize = 8;

/// `pqrs::unix_domain_stream::impl::protocol::message_type`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Heartbeat = 0,
    UserData = 1,
    HealthCheck = 2,
    HealthCheckResponse = 3,
    Request = 4,
    Response = 5,
}

impl MessageType {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Heartbeat,
            1 => Self::UserData,
            2 => Self::HealthCheck,
            3 => Self::HealthCheckResponse,
            4 => Self::Request,
            5 => Self::Response,
            _ => return None,
        })
    }
}

/// `virtual_hid_device_service::request`. Mirrors the C++ enum in full; not
/// every request is used.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Request {
    KeyboardInitialize = 0,
    KeyboardTerminate = 1,
    KeyboardReset = 2,
    PointingInitialize = 3,
    PointingTerminate = 4,
    PointingReset = 5,
    PostKeyboard = 6,
    PostConsumer = 7,
    PostAppleVendorKeyboard = 8,
    PostAppleVendorTopCase = 9,
    PostGenericDesktop = 10,
    PostPointing = 11,
}

/// `virtual_hid_device_service::response`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    DriverActivated,
    DriverConnected,
    DriverVersionMismatched,
    KeyboardReady,
    PointingReady,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub driver_activated: Option<bool>,
    pub driver_connected: Option<bool>,
    pub driver_version_mismatched: Option<bool>,
    pub keyboard_ready: Option<bool>,
    pub pointing_ready: Option<bool>,
}

impl Status {
    /// Apply a status payload. Returns the entries that changed.
    pub fn apply(&mut self, payload: &[u8]) -> Vec<(StatusKind, bool)> {
        let mut changed = Vec::new();
        if payload.len() % 2 != 0 {
            return changed;
        }
        for pair in payload.chunks_exact(2) {
            let value = pair[1] != 0;
            let (kind, slot) = match pair[0] {
                1 => (StatusKind::DriverActivated, &mut self.driver_activated),
                2 => (StatusKind::DriverConnected, &mut self.driver_connected),
                3 => (
                    StatusKind::DriverVersionMismatched,
                    &mut self.driver_version_mismatched,
                ),
                4 => (StatusKind::KeyboardReady, &mut self.keyboard_ready),
                5 => (StatusKind::PointingReady, &mut self.pointing_ready),
                _ => continue, // 0 = none
            };
            if *slot != Some(value) {
                *slot = Some(value);
                changed.push((kind, value));
            }
        }
        changed
    }
}

/// A frame as sent on the socket.
pub fn frame(kind: MessageType, request_id: Option<u64>, payload: &[u8]) -> Vec<u8> {
    let id_len = if request_id.is_some() { REQUEST_ID } else { 0 };
    let body = 1 + id_len + payload.len();
    let mut f = Vec::with_capacity(HEADER + body);
    f.extend_from_slice(&(body as u32).to_be_bytes());
    f.push(kind as u8);
    if let Some(id) = request_id {
        f.extend_from_slice(&id.to_be_bytes());
    }
    f.extend_from_slice(payload);
    f
}

/// Payload of a client request.
pub fn request_payload(req: Request, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(3 + data.len());
    p.extend_from_slice(&CLIENT_PROTOCOL_VERSION.to_le_bytes());
    p.push(req as u8);
    p.extend_from_slice(data);
    p
}

/// A received frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: MessageType,
    pub request_id: Option<u64>,
    pub payload: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    Size(u32),
    UnknownType(u8),
}

/// Incremental frame decoder for a byte stream.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        if self.buf.len() < HEADER {
            return Ok(None);
        }
        let body = u32::from_be_bytes(self.buf[..HEADER].try_into().unwrap());
        if body < 1 || body as usize > MAX_MESSAGE + 1 + REQUEST_ID {
            return Err(FrameError::Size(body));
        }
        let total = HEADER + body as usize;
        if self.buf.len() < total {
            return Ok(None);
        }
        let raw: Vec<u8> = self.buf.drain(..total).collect();
        let kind = MessageType::from_u8(raw[HEADER]).ok_or(FrameError::UnknownType(raw[HEADER]))?;
        let rest = &raw[HEADER + 1..];
        let (request_id, payload) = match kind {
            MessageType::Request | MessageType::Response => {
                if rest.len() < REQUEST_ID {
                    return Err(FrameError::Size(body));
                }
                let id = u64::from_be_bytes(rest[..REQUEST_ID].try_into().unwrap());
                (Some(id), rest[REQUEST_ID..].to_vec())
            }
            _ => (None, rest.to_vec()),
        };
        Ok(Some(Frame {
            kind,
            request_id,
            payload,
        }))
    }
}

/// `virtual_hid_keyboard_parameters`: three `uint64_t`.
pub fn keyboard_parameters(vendor_id: u64, product_id: u64, country_code: u64) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..8].copy_from_slice(&vendor_id.to_le_bytes());
    b[8..16].copy_from_slice(&product_id.to_le_bytes());
    b[16..24].copy_from_slice(&country_code.to_le_bytes());
    b
}

/// Number of usages each Karabiner keys array holds.
pub const KEYS: usize = 32;

/// `hid_report::keyboard_input`: report id 1, modifiers, reserved, 32 x u16.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyboardInput {
    pub modifiers: u8,
    pub keys: Vec<u16>,
}

impl KeyboardInput {
    pub fn to_bytes(&self) -> [u8; 67] {
        let mut b = [0u8; 67];
        b[0] = 1;
        b[1] = self.modifiers;
        put_keys(&mut b[3..], &self.keys);
        b
    }
}

/// The reports that are only a usage array: consumer (2), Apple top case
/// (3), Apple vendor keyboard (4) and generic desktop (7). The Apple ones
/// stay unused: the receiver exposes no Apple vendor usages.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysReport {
    Consumer,
    AppleVendorTopCase,
    AppleVendorKeyboard,
    GenericDesktop,
}

impl KeysReport {
    fn report_id(self) -> u8 {
        match self {
            KeysReport::Consumer => 2,
            KeysReport::AppleVendorTopCase => 3,
            KeysReport::AppleVendorKeyboard => 4,
            KeysReport::GenericDesktop => 7,
        }
    }

    pub fn request(self) -> Request {
        match self {
            KeysReport::Consumer => Request::PostConsumer,
            KeysReport::AppleVendorTopCase => Request::PostAppleVendorTopCase,
            KeysReport::AppleVendorKeyboard => Request::PostAppleVendorKeyboard,
            KeysReport::GenericDesktop => Request::PostGenericDesktop,
        }
    }

    pub fn to_bytes(self, keys: &[u16]) -> [u8; 65] {
        let mut b = [0u8; 65];
        b[0] = self.report_id();
        put_keys(&mut b[1..], keys);
        b
    }
}

fn put_keys(out: &mut [u8], keys: &[u16]) {
    for (slot, k) in out.chunks_exact_mut(2).zip(keys.iter().take(KEYS)) {
        slot.copy_from_slice(&k.to_le_bytes());
    }
}

/// `hid_report::pointing_input`: u32 buttons, then x, y, vertical wheel and
/// horizontal wheel as signed 8-bit relative values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PointingInput {
    pub buttons: u32,
    pub x: i8,
    pub y: i8,
    pub vertical_wheel: i8,
    pub horizontal_wheel: i8,
}

impl PointingInput {
    pub fn to_bytes(self) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0..4].copy_from_slice(&self.buttons.to_le_bytes());
        b[4] = self.x as u8;
        b[5] = self.y as u8;
        b[6] = self.vertical_wheel as u8;
        b[7] = self.horizontal_wheel as u8;
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame_layout() {
        let p = request_payload(Request::PointingInitialize, &[]);
        assert_eq!(p, [7, 0, 3]);
        let f = frame(MessageType::Request, Some(0x0102), &p);
        assert_eq!(
            f,
            [0, 0, 0, 12, 4, 0, 0, 0, 0, 0, 0, 1, 2, 7, 0, 3],
            "len BE | type | id BE | version LE | request"
        );
        assert_eq!(frame(MessageType::Heartbeat, None, &[]), [0, 0, 0, 1, 0]);
    }

    #[test]
    fn struct_sizes_match_the_cpp_headers() {
        assert_eq!(KeyboardInput::default().to_bytes().len(), 67);
        assert_eq!(KeysReport::Consumer.to_bytes(&[]).len(), 65);
        assert_eq!(PointingInput::default().to_bytes().len(), 8);
        assert_eq!(keyboard_parameters(0, 0, 0).len(), 24);
    }

    #[test]
    fn keyboard_input_bytes() {
        let k = KeyboardInput {
            modifiers: 0x02,
            keys: vec![0x04, 0x1E],
        };
        let b = k.to_bytes();
        assert_eq!(&b[..7], &[1, 0x02, 0, 0x04, 0, 0x1E, 0]);
        assert!(b[7..].iter().all(|&x| x == 0));
        let c = KeysReport::Consumer.to_bytes(&[0xE9]);
        assert_eq!(&c[..3], &[2, 0xE9, 0]);
        assert_eq!(KeysReport::GenericDesktop.to_bytes(&[])[0], 7);
        // More than 32 keys are cut, never overflow.
        let many: Vec<u16> = (1..100).collect();
        let b = KeyboardInput {
            modifiers: 0,
            keys: many,
        }
        .to_bytes();
        assert_eq!(u16::from_le_bytes([b[65], b[66]]), 32);
    }

    #[test]
    fn pointing_and_parameters() {
        let p = PointingInput {
            buttons: 0b101,
            x: -1,
            y: 127,
            vertical_wheel: -127,
            horizontal_wheel: 2,
        };
        assert_eq!(p.to_bytes(), [5, 0, 0, 0, 0xFF, 0x7F, 0x81, 2]);
        let k = keyboard_parameters(0x046d, 0xc52b, 0);
        assert_eq!(&k[..2], &[0x6d, 0x04]);
        assert_eq!(&k[8..10], &[0x2b, 0xc5]);
    }

    #[test]
    fn reader_handles_split_and_joined_frames() {
        let mut r = FrameReader::default();
        let a = frame(MessageType::Request, Some(9), &[4, 1, 5, 0]);
        let b = frame(MessageType::Heartbeat, None, &[]);
        let mut all = a.clone();
        all.extend_from_slice(&b);
        r.push(&all[..3]);
        assert_eq!(r.next_frame(), Ok(None));
        r.push(&all[3..]);
        let f = r.next_frame().unwrap().unwrap();
        assert_eq!(f.kind, MessageType::Request);
        assert_eq!(f.request_id, Some(9));
        assert_eq!(f.payload, [4, 1, 5, 0]);
        assert_eq!(
            r.next_frame().unwrap().unwrap().kind,
            MessageType::Heartbeat
        );
        assert_eq!(r.next_frame(), Ok(None));

        let mut bad = FrameReader::default();
        bad.push(&[0, 0, 0x10, 0, 0]);
        assert!(bad.next_frame().is_err());
        let mut bad = FrameReader::default();
        bad.push(&[0, 0, 0, 1, 9]);
        assert_eq!(bad.next_frame(), Err(FrameError::UnknownType(9)));
    }

    #[test]
    fn status_pairs() {
        let mut s = Status::default();
        let ch = s.apply(&[1, 1, 2, 1, 3, 0, 4, 1]);
        assert_eq!(ch.len(), 4);
        assert_eq!(s.keyboard_ready, Some(true));
        assert!(s.apply(&[1, 1]).is_empty());
        assert_eq!(s.apply(&[5, 1]), [(StatusKind::PointingReady, true)]);
        assert!(s.apply(&[5]).is_empty());
    }
}
