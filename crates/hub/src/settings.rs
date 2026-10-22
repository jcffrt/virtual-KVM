//! Switching settings changed from a widget, kept in
//! `<state_dir>/settings.toml`. Once present it overrides `[switching]` of
//! `hub.toml` (`hysteresis_ms`, `hotkey`), like `layout.toml` overrides the
//! `[[layout]]` seed; delete it to go back to the config.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proto::HubSettings;

use crate::config;

pub const FILE: &str = "settings.toml";

pub fn path_in(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// `None` if the file does not exist.
pub fn load(path: &Path) -> Result<Option<HubSettings>> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
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
            hotkey: false,
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap(), Some(s));
        fs::remove_dir_all(dir).unwrap();
    }
}
