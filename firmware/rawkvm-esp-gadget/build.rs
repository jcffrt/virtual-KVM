fn main() {
    wifi_credentials();
    linker_be_nice();
    check_xtensa_linker_available();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

#[cfg(unix)]
fn check_xtensa_linker_available() {
    println!("cargo:rerun-if-env-changed=PATH");

    let target = std::env::var("TARGET").unwrap_or_default();
    println!(
        "cargo:rerun-if-env-changed={}",
        cargo_linker_env_var(&target)
    );

    let linker = xtensa_linker(&target);

    let error = match std::process::Command::new(&linker)
        .arg("--version")
        .output()
    {
        Ok(_) => return,
        Err(error) => error,
    };

    let export_file = std::env::var("HOME")
        .map(|home| format!("{home}/export-esp.sh"))
        .unwrap_or_else(|_| "$HOME/export-esp.sh".to_string());

    panic!(
        "Xtensa linker `{linker}` was not found in PATH or could not be executed: {error}.\n\n\
         Xtensa targets on Unix require espup's environment export file to be sourced before building.\n\
         Try: `source {export_file}`\n\n\
         If the export file does not exist, run `espup install` first.\n\
         For more details, see:\n\
         https://github.com/esp-rs/espup?tab=readme-ov-file#environment-variables-setup"
    );
}

#[cfg(not(unix))]
fn check_xtensa_linker_available() {}

#[cfg(unix)]
fn xtensa_linker(target: &str) -> String {
    if let Some(linker) = cargo_configured_linker(target) {
        return linker;
    }

    target
        .strip_prefix("xtensa-")
        .and_then(|target| target.strip_suffix("-none-elf"))
        .map(|chip| format!("xtensa-{chip}-elf-gcc"))
        .unwrap_or_else(|| "xtensa-esp32-elf-gcc".to_string())
}

#[cfg(unix)]
fn cargo_configured_linker(target: &str) -> Option<String> {
    std::env::var(cargo_linker_env_var(target)).ok()
}

#[cfg(unix)]
fn cargo_linker_env_var(target: &str) -> String {
    let target = target.replace('-', "_").to_ascii_uppercase();
    format!("CARGO_TARGET_{target}_LINKER")
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}

/// Settings from `../wifi.env` (KEY=value lines, not in git), or from the
/// environment when a key is missing there:
/// - `WIFI_SSID`, `WIFI_PASS`: the 2.4 GHz network;
/// - `HUB_TOKEN`: the hub's pairing token (`token` in its hub.toml);
/// - `HUB_ADDR`: the hub's IPv4 address (default 192.168.1.39);
/// - `NODE_NAME`: the name the hub and widget show (default "ESP32 gadget");
/// - `NODE_OS`: the PC's OS, `windows` (default), `macos` or `linux`.
fn wifi_credentials() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../wifi.env");
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let get = |key: &str| {
        println!("cargo:rerun-if-env-changed={key}");
        text.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().to_string())
            .or_else(|| std::env::var(key).ok())
    };
    let required = |key: &str| {
        get(key).unwrap_or_else(|| panic!("{key} missing: write it in {}", path.display()))
    };
    let token = required("HUB_TOKEN");
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        panic!("HUB_TOKEN in {} must be 64 hex characters", path.display());
    }
    let addr = get("HUB_ADDR").unwrap_or_else(|| "192.168.1.39".into());
    if addr.parse::<std::net::Ipv4Addr>().is_err() {
        panic!("HUB_ADDR in {} must be an IPv4 address", path.display());
    }
    let os = get("NODE_OS").unwrap_or_else(|| "windows".into()).to_lowercase();
    if !["windows", "macos", "linux"].contains(&os.as_str()) {
        panic!("NODE_OS must be windows, macos or linux");
    }
    let vars = [
        ("WIFI_SSID", required("WIFI_SSID")),
        ("WIFI_PASS", required("WIFI_PASS")),
        ("HUB_TOKEN", token),
        ("HUB_ADDR", addr),
        ("NODE_NAME", get("NODE_NAME").unwrap_or_else(|| "ESP32 gadget".into())),
        ("NODE_OS", os),
    ];
    for (k, v) in vars {
        println!("cargo:rustc-env={k}={v}");
    }
}
