//! User-mode side of the RawKVM VHF driver interface. Mirrors
//! `drivers/windows-vhf/rawkvm_ioctl.h`; the tests pin the layout.

use proto::HidDevice;
use uuid::Uuid;

pub const DEVICE_PATH: &str = r"\\.\RawKvmVhf";
pub const IOCTL_VERSION: u32 = 1;
pub const MAX_DESCRIPTOR: usize = 4096;
pub const MAX_REPORT: usize = 1024;
pub const FLAG_REPORT_IDS: u8 = 0x01;
pub const CREATE_HEADER_LEN: usize = 32;

const DEVICE_TYPE: u32 = 0x8000;
const METHOD_BUFFERED: u32 = 0;
const FILE_WRITE_ACCESS: u32 = 0x2;

/// `CTL_CODE(DeviceType, Function, Method, Access)`.
pub const fn ctl_code(function: u32) -> u32 {
    (DEVICE_TYPE << 16) | (FILE_WRITE_ACCESS << 14) | (function << 2) | METHOD_BUFFERED
}

pub const IOCTL_CREATE: u32 = ctl_code(0x800);
pub const IOCTL_REPORT: u32 = ctl_code(0x801);
pub const IOCTL_DESTROY: u32 = ctl_code(0x802);
pub const IOCTL_DESTROY_ALL: u32 = ctl_code(0x803);

#[derive(Debug, PartialEq, Eq)]
pub enum EncodeError {
    DescriptorSize(usize),
    ReportSize(usize),
}

/// `RAWKVM_CREATE_HEADER` followed by the report descriptor.
pub fn encode_create(dev: &HidDevice, container: Uuid) -> Result<Vec<u8>, EncodeError> {
    let desc = &dev.report_descriptor;
    if desc.is_empty() || desc.len() > MAX_DESCRIPTOR {
        return Err(EncodeError::DescriptorSize(desc.len()));
    }
    let mut b = Vec::with_capacity(CREATE_HEADER_LEN + desc.len());
    b.extend_from_slice(&IOCTL_VERSION.to_le_bytes());
    b.extend_from_slice(&dev.vid.to_le_bytes());
    b.extend_from_slice(&dev.pid.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // version number
    b.extend_from_slice(&(desc.len() as u16).to_le_bytes());
    // GUID in memory order: Data1..3 little-endian, Data4 as is.
    b.extend_from_slice(&container.to_bytes_le());
    b.push(dev.id);
    b.push(if dev.uses_report_ids {
        FLAG_REPORT_IDS
    } else {
        0
    });
    b.extend_from_slice(&[0, 0]);
    debug_assert_eq!(b.len(), CREATE_HEADER_LEN);
    b.extend_from_slice(desc);
    Ok(b)
}

/// Handle followed by the report, unchanged (report ID byte first only if
/// the descriptor declares IDs, exactly as the receiver sent it).
pub fn encode_report(handle: u32, report: &[u8]) -> Result<Vec<u8>, EncodeError> {
    if report.is_empty() || report.len() > MAX_REPORT {
        return Err(EncodeError::ReportSize(report.len()));
    }
    let mut b = Vec::with_capacity(4 + report.len());
    b.extend_from_slice(&handle.to_le_bytes());
    b.extend_from_slice(report);
    Ok(b)
}

pub fn encode_handle(handle: u32) -> [u8; 4] {
    handle.to_le_bytes()
}

/// Container id shared by all virtual devices of this agent, so Device
/// Manager groups them like the interfaces of one physical receiver.
pub fn container_for(node_id: Uuid) -> Uuid {
    Uuid::new_v5(&node_id, b"rawkvm-vhf-container")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::tests::{BOOT_KEYBOARD, UNIFYING_IFACE1};
    use proto::descriptor::DescriptorInfo;

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

    #[test]
    fn ioctl_codes_match_the_header() {
        // RAWKVM_CTL(fn) = (0x8000 << 16) | (2 << 14) | (fn << 2) | 0
        assert_eq!(IOCTL_CREATE, 0x8000_A000);
        assert_eq!(IOCTL_REPORT, 0x8000_A004);
        assert_eq!(IOCTL_DESTROY, 0x8000_A008);
        assert_eq!(IOCTL_DESTROY_ALL, 0x8000_A00C);
    }

    #[test]
    fn create_header_layout() {
        let container = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        let b = encode_create(&dev(1, UNIFYING_IFACE1), container).unwrap();
        assert_eq!(b.len(), CREATE_HEADER_LEN + UNIFYING_IFACE1.len());
        assert_eq!(&b[0..4], &[1, 0, 0, 0]); // Version
        assert_eq!(&b[4..6], &[0x6d, 0x04]); // VendorId
        assert_eq!(&b[6..8], &[0x2b, 0xc5]); // ProductId
        assert_eq!(&b[8..10], &[0, 0]); // VersionNumber
        assert_eq!(
            u16::from_le_bytes([b[10], b[11]]) as usize,
            UNIFYING_IFACE1.len()
        );
        // GUID {00112233-4455-6677-8899-AABBCCDDEEFF} in memory.
        assert_eq!(
            &b[12..28],
            &[
                0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert_eq!(b[28], 1); // DeviceIndex
        assert_eq!(b[29], FLAG_REPORT_IDS); // mouse interface uses IDs
        assert_eq!(&b[30..32], &[0, 0]);
        assert_eq!(&b[32..], UNIFYING_IFACE1);
    }

    #[test]
    fn boot_keyboard_has_no_report_id_flag() {
        let b = encode_create(&dev(0, BOOT_KEYBOARD), Uuid::nil()).unwrap();
        assert_eq!(b[29], 0);
    }

    #[test]
    fn reports_are_passed_through_unchanged() {
        // Interface 0: 8-byte boot report, no ID byte.
        let kb = [0x02, 0, 0x04, 0, 0, 0, 0, 0];
        assert_eq!(
            encode_report(0x0102, &kb).unwrap(),
            [0x02, 0x01, 0, 0, 0x02, 0, 0x04, 0, 0, 0, 0, 0]
        );
        // Interface 1: mouse report with ID 2 first.
        let m = [0x02, 0x01, 0, 0x05, 0, 0, 0, 0];
        assert_eq!(&encode_report(7, &m).unwrap()[4..], &m);
        assert_eq!(encode_report(1, &[]), Err(EncodeError::ReportSize(0)));
        assert!(encode_report(1, &[0; MAX_REPORT + 1]).is_err());
        assert!(encode_create(&dev(0, &[]), Uuid::nil()).is_err());
    }

    #[test]
    fn container_is_stable_per_node() {
        let n = Uuid::new_v4();
        assert_eq!(container_for(n), container_for(n));
        assert_ne!(container_for(n), container_for(Uuid::new_v4()));
    }
}
