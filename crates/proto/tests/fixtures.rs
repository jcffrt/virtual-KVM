//! Parses the real descriptors captured from the hub's Unifying receiver (see
//! fixtures/unifying-c52b/README.md). Skips silently if the directory is
//! absent so the crate can be tested standalone.

use proto::descriptor::DescriptorInfo;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/unifying-c52b")
}

#[test]
fn real_descriptors_parse() {
    let dir = fixtures_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("no fixtures at {}", dir.display());
        return;
    };
    let mut seen = 0;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("bin") {
            continue;
        }
        let bytes = std::fs::read(&p).unwrap();
        if bytes.is_empty() {
            continue;
        }
        let d = DescriptorInfo::parse(&bytes);
        println!(
            "{}: {} bytes, report_ids={}, apps={:?}, inputs={:?}, keyboard={:?}, release={}",
            p.file_name().unwrap().to_string_lossy(),
            bytes.len(),
            d.uses_report_ids,
            d.applications,
            d.inputs
                .iter()
                .map(|r| (r.report_id, r.len))
                .collect::<Vec<_>>(),
            d.keyboard,
            d.release_reports().len()
        );
        assert!(
            !d.applications.is_empty(),
            "{}: no application collection",
            p.display()
        );
        assert!(!d.inputs.is_empty(), "{}: no input reports", p.display());
        seen += 1;
    }
    println!("parsed {seen} fixtures");
}

mod decode {
    use super::fixtures_dir;
    use proto::descriptor::{DescriptorInfo, Value};

    fn load(name: &str) -> Option<DescriptorInfo> {
        let bytes = std::fs::read(fixtures_dir().join(name)).ok()?;
        Some(DescriptorInfo::parse(&bytes))
    }

    fn active(values: &[Value], page: u16) -> Vec<(u16, i32)> {
        values
            .iter()
            .filter(|v| v.usage_page == page && v.value != 0)
            .map(|v| (v.usage, v.value))
            .collect()
    }

    #[test]
    fn interface0_boot_keyboard() {
        let Some(d) = load("0003-046D-C52B.0007.nodj.bin") else {
            return;
        };
        // Left Shift + Right Alt, keys 'a' and '1'
        let v = d.decode(&[0x42, 0x00, 0x04, 0x1E, 0, 0, 0, 0]);
        assert_eq!(
            active(&v, 0x07),
            vec![(0xE1, 1), (0xE6, 1), (0x04, 1), (0x1E, 1)]
        );
        // All eight modifier bits are reported, pressed or not.
        assert_eq!(
            v.iter()
                .filter(|x| (0xE0..=0xE7).contains(&x.usage))
                .count(),
            8
        );
        assert!(active(&d.decode(&[0; 8]), 0x07).is_empty());
    }

    #[test]
    fn interface1_mouse_consumer_system() {
        let Some(d) = load("0003-046D-C52B.0008.nodj.bin") else {
            return;
        };
        // ID 2: buttons 1 and 3, X = -5, Y = +300, wheel -1, AC Pan +2.
        let x: i32 = -5;
        let y: i32 = 300;
        let xy = ((x as u32) & 0xFFF) | (((y as u32) & 0xFFF) << 12);
        let r = [
            0x02,
            0b0000_0101,
            0x00,
            xy as u8,
            (xy >> 8) as u8,
            (xy >> 16) as u8,
            0xFF,
            0x02,
        ];
        let v = d.decode(&r);
        assert_eq!(active(&v, 0x09), vec![(1, 1), (3, 1)]);
        assert!(v.iter().all(|x| x.usage_page != 0x09 || !x.relative));
        let gd = active(&v, 0x01);
        assert_eq!(gd, vec![(0x30, -5), (0x31, 300), (0x38, -1)]);
        assert!(v
            .iter()
            .filter(|x| x.usage_page == 0x01)
            .all(|x| x.relative));
        assert_eq!(active(&v, 0x0C), vec![(0x238, 2)]);

        // ID 3: two consumer usages, Volume Up (0xE9) and Mute (0xE2).
        let v = d.decode(&[0x03, 0xE9, 0x00, 0xE2, 0x00]);
        assert_eq!(active(&v, 0x0C), vec![(0xE9, 1), (0xE2, 1)]);
        assert!(d.decode(&[0x03, 0, 0, 0, 0]).is_empty());

        // ID 4: system control, value 2 selects the second usage (Power).
        assert_eq!(active(&d.decode(&[0x04, 0x02]), 0x01), vec![(0x81, 1)]);
        assert!(d.decode(&[0x04, 0x00]).is_empty());

        // Unknown or truncated reports never panic.
        assert!(d.decode(&[0x09, 1, 2]).is_empty());
        let _ = d.decode(&[0x02, 1]);
        let _ = d.decode(&[]);
    }
}

/// Alt <-> GUI swap (ADR 0011) on real keyboard reports: the boot keyboard
/// of interface 0 (8 B: mods, reserved, 6 keys) and the 7 B DJ layout
/// (ID 1: mods + 6 keys) of the K230 fixture.
#[test]
fn swap_alt_gui_on_real_layouts() {
    let (Ok(boot), Ok(dj)) = (
        std::fs::read(fixtures_dir().join("0003-046D-C52B.0007.nodj.bin")),
        std::fs::read(fixtures_dir().join("0003-046D-400D.0005.bin")),
    ) else {
        eprintln!("fixtures missing; skipped");
        return;
    };
    let b = DescriptorInfo::parse(&boot);
    let kb = b.keyboard.clone().unwrap();
    let swap = |r: &[u8]| kb.swap_alt_gui(b.uses_report_ids, r).unwrap();
    // (input, expected): A = 0x04 key
    let cases: &[([u8; 8], [u8; 8], &str)] = &[
        (
            [0x04, 0, 0x04, 0, 0, 0, 0, 0],
            [0x08, 0, 0x04, 0, 0, 0, 0, 0],
            "left Alt + A",
        ),
        (
            [0x08, 0, 0x04, 0, 0, 0, 0, 0],
            [0x04, 0, 0x04, 0, 0, 0, 0, 0],
            "left GUI + A",
        ),
        (
            [0x0C, 0, 0x06, 0, 0, 0, 0, 0],
            [0x0C, 0, 0x06, 0, 0, 0, 0, 0],
            "both left: unchanged",
        ),
        (
            [0x40, 0, 0x04, 0, 0, 0, 0, 0],
            [0x80, 0, 0x04, 0, 0, 0, 0, 0],
            "right Alt (AltGr)",
        ),
        (
            [0x80, 0, 0, 0, 0, 0, 0, 0],
            [0x40, 0, 0, 0, 0, 0, 0, 0],
            "right GUI",
        ),
        (
            [0x37, 0, 0x04, 0, 0, 0, 0, 0],
            [0x3B, 0, 0x04, 0, 0, 0, 0, 0],
            "Ctrl/Shift kept, left Alt moved",
        ),
        (
            [0, 0, 0xE2, 0xE3, 0x04, 0, 0, 0],
            [0, 0, 0xE3, 0xE2, 0x04, 0, 0, 0],
            "usages in the array",
        ),
        (
            [0, 0, 0xE6, 0xE7, 0xE0, 0, 0, 0],
            [0, 0, 0xE7, 0xE6, 0xE0, 0, 0, 0],
            "right usages; Ctrl kept",
        ),
        (
            [0, 0x04, 0, 0, 0, 0, 0, 0],
            [0, 0x04, 0, 0, 0, 0, 0, 0],
            "reserved byte untouched",
        ),
        ([0; 8], [0; 8], "release all"),
    ];
    for (input, want, why) in cases {
        assert_eq!(swap(input), want, "{why}");
    }
    assert_eq!(kb.swap_alt_gui(false, &[0x04, 0]), None, "too short");

    let d = DescriptorInfo::parse(&dj);
    let k = d.keyboard.clone().unwrap();
    assert!(d.uses_report_ids);
    let r = [0x01, 0x48, 0x04, 0xE3, 0, 0, 0, 0];
    assert_eq!(
        k.swap_alt_gui(true, &r).unwrap(),
        [0x01, 0x84, 0x04, 0xE2, 0, 0, 0, 0],
        "DJ ID 1: left Alt + right GUI become left GUI + right Alt"
    );
    // Consumer report (ID 3) of the same interface: not a keyboard report.
    assert_eq!(k.swap_alt_gui(true, &[0x03, 0xE9, 0x00, 0x00, 0x00]), None);
}
