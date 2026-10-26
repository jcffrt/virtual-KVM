//! The bridge's bond, kept across restarts in
//! `/var/lib/rawkvm/agent-bt.toml`: which PC it is bonded with and the
//! descriptor that PC paired with (ADR 0033). The Bluetooth keys
//! themselves stay with the stack (BlueZ keeps them in /var/lib/bluetooth).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bridge_core::host::{BtAddr, Saved};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PATH: &str = "/var/lib/rawkvm/agent-bt.toml";

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(skip_serializing_if = "Option::is_none")]
    bonded: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    /// FNV-1a of the merged descriptor the PC paired with, in hex.
    #[serde(skip_serializing_if = "Option::is_none")]
    descriptor: Option<String>,
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The saved bond; nothing if the file does not exist yet. A file that
    /// cannot be read is an error: better than silently forgetting the PC.
    pub fn load(&self) -> Result<Saved> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Saved::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        let f: File =
            toml::from_str(&text).with_context(|| format!("parsing {}", self.path.display()))?;
        let bonded = f
            .bonded
            .map(|a| a.parse::<BtAddr>().map_err(anyhow::Error::msg))
            .transpose()?
            .map(|a| (a, f.name));
        let paired_descriptor = f
            .descriptor
            .map(|h| u64::from_str_radix(&h, 16))
            .transpose()
            .with_context(|| format!("{}: bad descriptor hash", self.path.display()))?;
        Ok(Saved {
            bonded,
            paired_descriptor,
        })
    }

    /// Write atomically and durably (the board may lose power any time).
    pub fn save(&self, saved: &Saved) -> Result<()> {
        let f = File {
            bonded: saved.bonded.as_ref().map(|(a, _)| a.to_string()),
            name: saved
                .bonded
                .as_ref()
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
            descriptor: saved.paired_descriptor.map(|h| format!("{h:016x}")),
        };
        let text = format!(
            "# RawKVM Bluetooth bridge: the bonded PC. Written by rawkvm-agent-bt.\n\n{}",
            toml::to_string(&f)?
        );
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("tmp");
        {
            use std::io::Write;
            let mut out =
                fs::File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
            out.write_all(text.as_bytes())?;
            out.sync_all()?;
        }
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;
        #[cfg(unix)]
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::File::open(dir).and_then(|d| d.sync_all())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir()
            .join(format!("rawkvm-bt-{}", std::process::id()))
            .join(format!("{:?}", std::thread::current().id()).replace(['(', ')'], ""))
            .join("agent-bt.toml")
    }

    #[test]
    fn round_trip_and_empty() {
        let s = Store::new(tmp());
        let _ = fs::remove_file(s.path());
        assert_eq!(s.load().unwrap(), Saved::default());
        let saved = Saved {
            bonded: Some((BtAddr([0xAA, 0xBB, 0xCC, 0, 0x11, 0x22]), "GAMER".into())),
            paired_descriptor: Some(0x0123_4567_89ab_cdef),
        };
        s.save(&saved).unwrap();
        let text = fs::read_to_string(s.path()).unwrap();
        assert!(text.contains(r#"bonded = "AA:BB:CC:00:11:22""#), "{text}");
        assert!(
            text.contains(r#"descriptor = "0123456789abcdef""#),
            "{text}"
        );
        assert_eq!(s.load().unwrap(), saved);
        // Forgetting writes an empty bond.
        s.save(&Saved::default()).unwrap();
        assert_eq!(s.load().unwrap(), Saved::default());
        let _ = fs::remove_dir_all(s.path().parent().unwrap());
    }

    #[test]
    fn a_damaged_file_is_an_error() {
        let s = Store::new(tmp());
        fs::create_dir_all(s.path().parent().unwrap()).unwrap();
        fs::write(s.path(), "bonded = \"not an address\"\n").unwrap();
        assert!(s.load().is_err());
        fs::write(s.path(), "colour = 3\n").unwrap();
        assert!(s.load().is_err());
        let _ = fs::remove_dir_all(s.path().parent().unwrap());
    }
}
