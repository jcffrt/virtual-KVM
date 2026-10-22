//! Agent indices and the last active agent, kept across hub restarts in
//! `<state_dir>/nodes.toml`.
//!
//! An index stays reserved for its node while that node is disconnected,
//! so Ctrl, Ctrl, N always reaches the same machine. `rawkvm-hub
//! --forget-node` releases one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::config;

pub const FILE: &str = "nodes.toml";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: Uuid,
    pub index: u8,
    /// Last name the node reported; informational only.
    #[serde(default)]
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
struct File {
    last_active: Option<Uuid>,
    node: Vec<Entry>,
}

/// In-memory copy of `nodes.toml`; every change is written back at once.
/// Without a path (tests, replay) nothing is written.
#[derive(Debug, Default)]
pub struct NodeStore {
    path: Option<PathBuf>,
    entries: BTreeMap<Uuid, Entry>,
    last_active: Option<Uuid>,
}

impl NodeStore {
    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir.join(FILE)
    }

    /// Load `path`; a missing file is an empty store.
    pub fn load(path: &Path) -> Result<Self> {
        let file: File = match fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path: Some(path.to_path_buf()),
            entries: file.node.into_iter().map(|e| (e.id, e)).collect(),
            last_active: file.last_active,
        })
    }

    pub fn index_of(&self, id: &Uuid) -> Option<u8> {
        self.entries.get(id).map(|e| e.index)
    }

    pub fn last_active(&self) -> Option<Uuid> {
        self.last_active
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        let mut v: Vec<_> = self.entries.values().collect();
        v.sort_by_key(|e| e.index);
        v.into_iter()
    }

    /// Node holding `index`, if any.
    pub fn holder(&self, index: u8) -> Option<Uuid> {
        self.entries
            .values()
            .find(|e| e.index == index)
            .map(|e| e.id)
    }

    /// Give `index` to `id`, taking it from whoever held it.
    pub fn assign(&mut self, id: Uuid, index: u8, name: &str) {
        let before = self.entries.get(&id).cloned();
        self.entries.retain(|k, e| *k == id || e.index != index);
        let entry = Entry {
            id,
            index,
            name: name.to_string(),
        };
        if before.as_ref() != Some(&entry) {
            self.entries.insert(id, entry);
            self.save();
        }
    }

    pub fn set_last_active(&mut self, id: Uuid) {
        if self.last_active != Some(id) {
            self.last_active = Some(id);
            self.save();
        }
    }

    /// Release the index of `id`. Returns the removed entry.
    pub fn forget(&mut self, id: &Uuid) -> Option<Entry> {
        let e = self.entries.remove(id)?;
        if self.last_active == Some(*id) {
            self.last_active = None;
        }
        self.save();
        Some(e)
    }

    /// Resolve a node by UUID or by index ("2").
    pub fn find(&self, key: &str) -> Option<Uuid> {
        if let Ok(id) = key.parse::<Uuid>() {
            return self.entries.contains_key(&id).then_some(id);
        }
        key.parse::<u8>().ok().and_then(|i| self.holder(i))
    }

    fn to_text(&self) -> Result<String> {
        let file = File {
            last_active: self.last_active,
            node: self.entries().cloned().collect(),
        };
        Ok(format!(
            "# RawKVM hub: agent index (Ctrl, Ctrl, N) of each node and the last\n\
             # active agent. Written by the hub; release an index with\n\
             # `rawkvm-hub --forget-node <uuid|index>` while the hub is stopped.\n\n{}",
            toml::to_string_pretty(&file)?
        ))
    }

    /// Write the store, atomically. Failures are logged: losing the file
    /// only costs index stability, not the session.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(e) = self.try_save(path) {
            warn!("saving {}: {e:#}", path.display());
        }
    }

    pub fn try_save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            config::ensure_dir(dir)?;
        }
        config::write_private(path, self.to_text()?.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_assign_and_forget() {
        let dir = std::env::temp_dir().join(format!("rawkvm-nodes-{}", Uuid::new_v4()));
        let path = NodeStore::path_in(&dir);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

        let mut s = NodeStore::load(&path).unwrap();
        assert_eq!(s.entries().count(), 0);
        s.assign(a, 1, "orangepi");
        s.assign(b, 2, "mac");
        s.set_last_active(b);

        let mut s = NodeStore::load(&path).unwrap();
        assert_eq!(s.index_of(&a), Some(1));
        assert_eq!(s.index_of(&b), Some(2));
        assert_eq!(s.last_active(), Some(b));
        assert_eq!(s.find("2"), Some(b));
        assert_eq!(s.find(&a.to_string()), Some(a));
        assert_eq!(s.find("3"), None);

        // Taking an index away from its holder.
        s.assign(a, 2, "orangepi");
        assert_eq!(s.index_of(&b), None);
        assert_eq!(s.holder(2), Some(a));

        assert!(s.forget(&a).is_some());
        assert!(s.forget(&a).is_none());
        let s = NodeStore::load(&path).unwrap();
        assert_eq!(s.entries().count(), 0);
        assert_eq!(s.last_active(), Some(b)); // b was not forgotten
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("rawkvm-nodes-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = NodeStore::path_in(&dir);
        fs::write(&path, "node = 3").unwrap();
        assert!(NodeStore::load(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
