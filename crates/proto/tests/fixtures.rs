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
