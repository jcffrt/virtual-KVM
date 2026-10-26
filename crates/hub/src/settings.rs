//! Switching settings changed from a widget, kept in
//! `<state_dir>/settings.toml`. Once present it overrides `[switching]` of
//! `hub.toml` (`hysteresis_ms`, `hotkey*`), like `layout.toml` overrides the
//! `[[layout]]` seed; delete it to go back to the config.
//!
//! Files written before protocol v6 have `hotkey = true|false`; they still
//! load, with the other hotkey options at their defaults.
//!
//! It also keeps the receivers exposed exclusively to an agent
//! (`[[exposure]]`, ADR 0030), by receiver name: the index of a block
//! changes if the blocks of `hub.toml` are reordered. A receiver without an
//! entry is `Network`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proto::{Exposure, Hotkey, HubSettings};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    #[serde(default)]
    exposure: Vec<ExposureEntry>,
}

/// One `[[exposure]]`: `receiver` is the block's `name` in `hub.toml`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExposureEntry {
    receiver: String,
    exclusive: Uuid,
}

#[derive(Serialize)]
struct FileOut<'a> {
    hysteresis_ms: u32,
    hotkey: &'a Hotkey,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    exposure: Vec<ExposureEntry>,
}

/// What `settings.toml` holds. `settings.exposures` is empty: the
/// exposures are by name in `exposures`, for the router to resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub settings: HubSettings,
    pub exposures: Vec<(String, Exposure)>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HotkeyFile {
    Enabled(bool),
    Full(Hotkey),
}

/// `None` if the file does not exist.
pub fn load(path: &Path) -> Result<Option<Saved>> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str::<File>(&text)
            .map(|f| {
                Some(Saved {
                    settings: HubSettings {
                        hysteresis_ms: f.hysteresis_ms,
                        hotkey: match f.hotkey {
                            HotkeyFile::Enabled(enabled) => Hotkey {
                                enabled,
                                ..Hotkey::default()
                            },
                            HotkeyFile::Full(h) => h,
                        },
                        exposures: Vec::new(),
                    },
                    exposures: f
                        .exposure
                        .into_iter()
                        .map(|e| (e.receiver, Exposure::Exclusive { agent: e.exclusive }))
                        .collect(),
                })
            })
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Save `s`, with its exposures (by name; `Network` ones are left out).
pub fn save(path: &Path, s: &HubSettings) -> Result<()> {
    if let Some(dir) = path.parent() {
        config::ensure_dir(dir)?;
    }
    let out = FileOut {
        hysteresis_ms: s.hysteresis_ms,
        hotkey: &s.hotkey,
        exposure: s
            .exposures
            .iter()
            .filter_map(|e| match e.mode {
                Exposure::Network => None,
                Exposure::Exclusive { agent } => Some(ExposureEntry {
                    receiver: e.name.clone(),
                    exclusive: agent,
                }),
            })
            .collect(),
    };
    let text = format!(
        "# RawKVM hub: switching settings set from the widget. Overrides\n\
         # [switching] in hub.toml; delete this file to go back to it.\n\n{}",
        toml::to_string_pretty(&out)?
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
        let agent = uuid::Uuid::new_v4();
        let s = HubSettings {
            hysteresis_ms: 300,
            hotkey: Hotkey {
                enabled: true,
                modifier: proto::HotkeyModifier::Gui,
                previous: false,
                release_keys: true,
            },
            exposures: vec![
                proto::ReceiverExposure {
                    receiver: 0,
                    name: "logi".into(),
                    mode: Exposure::Network,
                },
                proto::ReceiverExposure {
                    receiver: 1,
                    name: "other".into(),
                    mode: Exposure::Exclusive { agent },
                },
            ],
        };
        save(&path, &s).unwrap();
        assert_eq!(
            load(&path).unwrap(),
            Some(Saved {
                settings: HubSettings {
                    exposures: Vec::new(),
                    ..s.clone()
                },
                exposures: vec![("other".into(), Exposure::Exclusive { agent })],
            })
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[[exposure]]") && !text.contains("logi"),
            "{text}"
        );

        // Written by a v5 hub.
        fs::write(&path, "hysteresis_ms = 200\nhotkey = false\n").unwrap();
        assert_eq!(
            load(&path).unwrap(),
            Some(Saved {
                settings: HubSettings {
                    hysteresis_ms: 200,
                    hotkey: Hotkey {
                        enabled: false,
                        ..Hotkey::default()
                    },
                    exposures: Vec::new(),
                },
                exposures: Vec::new(),
            })
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
