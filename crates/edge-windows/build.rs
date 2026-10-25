//! Embed the git commit in the binary (`--version` and the start-up log),
//! to tell which build a board is running after a deploy; and, for the
//! MSVC release build, a version resource, a manifest and an icon. An
//! unsigned program without them is what Microsoft Defender's machine
//! learning flags (Trojan:Win32/Bearfoos.A!ml on the bare 0.5.0 build).
fn main() {
    let rev = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=RAWKVM_COMMIT={rev}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    resources();
}

/// Only with MSVC (rc.exe comes with it): the cross-compiled windows-gnu
/// builds (CI checks, the Mac) have no resource compiler at hand.
fn resources() {
    let target = |k: &str| std::env::var(k).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") != "windows" || target("CARGO_CFG_TARGET_ENV") != "msvc" {
        return;
    }
    println!("cargo:rerun-if-changed=rawkvm.ico");
    println!("cargo:rerun-if-changed=rawkvm-edge.manifest");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("rawkvm.ico")
        .set_manifest_file("rawkvm-edge.manifest")
        .set("ProductName", "RawKVM")
        .set("FileDescription", "RawKVM edge helper")
        .set("CompanyName", "RawKVM (github.com/jcffrt/virtual-KVM)")
        .set("LegalCopyright", "MIT License")
        .set("OriginalFilename", "rawkvm-edge.exe")
        .set("InternalName", "rawkvm-edge");
    if let Err(e) = res.compile() {
        panic!("Windows resources: {e}");
    }
}
