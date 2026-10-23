//! Switching settings changed from a widget, kept in
//! `<state_dir>/settings.toml`. Once present it overrides `[switching]` of
//! `hub.toml` (`hysteresis_ms`, `hotkey*`), like `layout.toml` overrides the
//! `[[layout]]` seed; delete it to go back to the config.
//!
//! Files written before protocol v6 have `hotkey = true|false`; they still
//! load, with the other hotkey options at their defaults.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proto::{Hotkey, HubSettings};
use serde::Deserialize;

use crate::config;

pub const FILE: &str = "settings.toml";

pub fn path_in(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// What `settings.toml` may hold: the current form or the pre-v6 one.
#[derive(Deserialize)]
struct File {
    hysteresis_ms: u32,
    hotkey: HotkeyFile,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HotkeyFile {
    Enabled(bool),
    Full(Hotkey),
}

/// `None` if the file does not exist.
pub fn load(path: &Path) -> Result<Option<HubSettings>> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str::<File>(&text)
            .map(|f| {
                Some(HubSettings {
                    hysteresis_ms: f.hysteresis_ms,
                    hotkey: match f.hotkey {
                        HotkeyFile::Enabled(enabled) => Hotkey {
                            enabled,
                            ..Hotkey::default()
                        },
                        HotkeyFile::Full(h) => h,
                    },
                })
            })
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &Path, s: &HubSettings) -> Result<()> {
    if let Some(dir) = path.parent() {
        config::ensure_dir(dir)?;
    }
    let text = format!(
        "# RawKVM hub: switching settings set from the widget. Overrides\n\
         # [switching] in hub.toml; delete this file to go back to it.\n\n{}",
        toml::to_string_pretty(s)?
    );
    config::write_private(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("rawkvm-settings-{}", uuid::Uuid::new_v4()));
        let path = path_in(&dir);
        assert_eq!(load(&path).unwrap(), None);
        let s = HubSettings {
            hysteresis_ms: 300,
            hotkey: Hotkey {
                enabled: true,
                modifier: proto::HotkeyModifier::Gui,
                previous: false,
                release_keys: true,
            },
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap(), Some(s));

        // Written by a v5 hub.
        fs::write(&path, "hysteresis_ms = 200\nhotkey = false\n").unwrap();
        assert_eq!(
            load(&path).unwrap(),
            Some(HubSettings {
                hysteresis_ms: 200,
                hotkey: Hotkey {
                    enabled: false,
                    ..Hotkey::default()
                },
            })
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
