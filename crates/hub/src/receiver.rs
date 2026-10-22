//! Receiver housekeeping that is not HID forwarding.
//!
//! The DJ mode of a Unifying receiver survives unloading `hid-logitech-dj`
//! and a USB reset. While it is on, interfaces 0 and 1 stay silent and all
//! input arrives multiplexed on the HID++ interface. Sending the DJ command
//! "switch and keep alive" with an empty device bitfield returns every
//! paired device to normal (boot) reporting. It is the command
//! `logi_dj_recv_switch_to_dj_mode` sends in `drivers/hid/hid-logitech-dj.c`,
//! there with bitfield 0x3F.

use proto::descriptor::DescriptorInfo;

/// Short DJ report ID.
pub const REPORT_ID_DJ_SHORT: u8 = 0x20;
/// Payload size of the short DJ report, after the ID.
const DJ_SHORT_LEN: usize = 14;
/// Device index addressing the receiver itself.
const RECEIVER_INDEX: u8 = 0xFF;
const CMD_SWITCH_AND_KEEP_ALIVE: u8 = 0x80;

/// The 15-byte output report that turns DJ mode off.
pub fn normal_mode_report() -> [u8; 1 + DJ_SHORT_LEN] {
    let mut r = [0u8; 1 + DJ_SHORT_LEN];
    r[0] = REPORT_ID_DJ_SHORT;
    r[1] = RECEIVER_INDEX;
    r[2] = CMD_SWITCH_AND_KEEP_ALIVE;
    // r[3]: bitfield of devices kept in DJ mode = none; r[4]: timeout = 0.
    r
}

/// True if this interface is the receiver's HID++/DJ interface: a vendor
/// collection with the short DJ report.
pub fn is_dj_interface(info: &DescriptorInfo) -> bool {
    info.inputs
        .iter()
        .any(|r| r.report_id == Some(REPORT_ID_DJ_SHORT) && r.app_usage_page >= 0xFF00)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_bytes() {
        let r = normal_mode_report();
        assert_eq!(r.len(), 15);
        assert_eq!(&r[..3], &[0x20, 0xFF, 0x80]);
        assert!(r[3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn finds_the_dj_interface() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/unifying-c52b");
        let read = |n: &str| std::fs::read(dir.join(n)).ok();
        let (Some(i0), Some(i1), Some(i2)) = (
            read("0003:046D:C52B.0007.nodj.bin"),
            read("0003:046D:C52B.0008.nodj.bin"),
            read("0003:046D:C52B.0009.nodj.bin"),
        ) else {
            eprintln!("fixtures missing; skipped");
            return;
        };
        assert!(!is_dj_interface(&DescriptorInfo::parse(&i0)));
        assert!(!is_dj_interface(&DescriptorInfo::parse(&i1)));
        assert!(is_dj_interface(&DescriptorInfo::parse(&i2)));
        // The short DJ report the descriptor declares has the size we send.
        let info = DescriptorInfo::parse(&i2);
        let dj = info
            .inputs
            .iter()
            .find(|r| r.report_id == Some(0x20))
            .unwrap();
        assert_eq!(dj.len + 1, normal_mode_report().len());
    }
}
