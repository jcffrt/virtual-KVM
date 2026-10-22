//! Edge links written by hand in `hub.toml`, turned into the [`EdgeLink`]s
//! of a `Layout` once the screens of the nodes involved are known.
//!
//! ```toml
//! [[layout]]
//! from = 2          # agent index (rawkvm-hub --list-nodes)
//! edge = "right"
//! to = 1
//! ```
//!
//! Without `from_screen`, the link starts on the node's screen that is
//! outermost in the direction of `edge` (for "right", the one whose right
//! side is furthest right); without `to_screen`, it ends on the target's
//! screen outermost on the opposite side (for "right", its leftmost
//! screen). Ties go to the lowest screen index. Every link also works
//! backwards unless `oneway = true`.
//!
//! The config links are only the seed. A layout sent by a widget (or
//! loaded with `rawkvm-hub --load-layout`) is kept in
//! `<state_dir>/layout.toml` and wins from then on, also after restarts.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proto::{Edge, EdgeLink, EdgeOptions, Screen};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config;

pub const FILE: &str = "layout.toml";

/// The persisted layout: resolved links (node ids and screen indices) and
/// the edge options that came with them. Screens are not stored: they are
/// live data from the helpers.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutFile {
    pub options: EdgeOptions,
    pub link: Vec<EdgeLink>,
}

impl LayoutFile {
    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir.join(FILE)
    }

    /// `None` if the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map(Some)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn to_text(&self) -> Result<String> {
        Ok(format!(
            "# RawKVM hub: edge links set by the widget or `rawkvm-hub --load-layout`.\n\
             # Overrides [[layout]] in hub.toml; delete this file to go back to it.\n\n{}",
            toml::to_string_pretty(self)?
        ))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            config::ensure_dir(dir)?;
        }
        config::write_private(path, self.to_text()?.as_bytes())
    }
}

fn full() -> (f32, f32) {
    (0.0, 1.0)
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    /// Agent index of the node the pointer leaves.
    pub from: u8,
    pub edge: Edge,
    /// Agent index of the node the pointer enters (through the opposite
    /// edge).
    pub to: u8,
    pub from_screen: Option<u8>,
    pub to_screen: Option<u8>,
    /// Part of the edge that switches, as fractions 0.0..=1.0 along it
    /// (top to bottom, left to right).
    #[serde(default = "full")]
    pub from_range: (f32, f32),
    #[serde(default = "full")]
    pub to_range: (f32, f32),
    #[serde(default)]
    pub oneway: bool,
}

/// How far out `s` reaches in the direction of `edge` (larger = further).
fn reach(s: &Screen, edge: Edge) -> i64 {
    match edge {
        Edge::Right => s.x as i64 + s.w as i64,
        Edge::Left => -(s.x as i64),
        Edge::Bottom => s.y as i64 + s.h as i64,
        Edge::Top => -(s.y as i64),
    }
}

/// The screen of a node a link without a screen index attaches to.
pub fn outermost(screens: &[Screen], edge: Edge) -> Option<u8> {
    screens
        .iter()
        .max_by_key(|s| (reach(s, edge), std::cmp::Reverse(s.index)))
        .map(|s| s.index)
}

fn pick(screens: &[Screen], wanted: Option<u8>, edge: Edge) -> Option<u8> {
    match wanted {
        Some(i) => screens.iter().any(|s| s.index == i).then_some(i),
        None => outermost(screens, edge),
    }
}

/// Resolve `specs` against the agents known so far. Links whose nodes or
/// screens are not known yet (a machine that never connected) are skipped;
/// call again when screens change.
pub fn compile(
    specs: &[LinkSpec],
    node_of_index: impl Fn(u8) -> Option<Uuid>,
    screens: &BTreeMap<Uuid, Vec<Screen>>,
) -> Vec<EdgeLink> {
    let mut out = Vec::new();
    for spec in specs {
        let (Some(from), Some(to)) = (node_of_index(spec.from), node_of_index(spec.to)) else {
            continue;
        };
        let (Some(fs), Some(ts)) = (screens.get(&from), screens.get(&to)) else {
            continue;
        };
        let from_edge = spec.edge;
        let to_edge = from_edge.opposite();
        let (Some(from_screen), Some(to_screen)) = (
            pick(fs, spec.from_screen, from_edge),
            pick(ts, spec.to_screen, to_edge),
        ) else {
            continue;
        };
        let link = EdgeLink {
            from_node: from,
            from_screen,
            from_edge,
            from_range: spec.from_range,
            to_node: to,
            to_screen,
            to_edge,
            to_range: spec.to_range,
        };
        if !spec.oneway {
            out.push(EdgeLink {
                from_node: link.to_node,
                from_screen: link.to_screen,
                from_edge: link.to_edge,
                from_range: link.to_range,
                to_node: link.from_node,
                to_screen: link.from_screen,
                to_edge: link.from_edge,
                to_range: link.from_range,
            });
        }
        out.push(link);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(node: Uuid, index: u8, x: i32, y: i32, w: u32, h: u32) -> Screen {
        Screen {
            node,
            index,
            x,
            y,
            w,
            h,
            scale: 1.0,
        }
    }

    fn spec(from: u8, edge: Edge, to: u8) -> LinkSpec {
        LinkSpec {
            from,
            edge,
            to,
            from_screen: None,
            to_screen: None,
            from_range: full(),
            to_range: full(),
            oneway: false,
        }
    }

    /// The user's desk (2026-10-22). Geometries invented for the test: Mac
    /// built-in 1512x982 points at the origin, external 2560x1440 to its
    /// right; Windows one 1920x1080 monitor; Ubuntu not connected yet.
    #[test]
    fn mac_external_right_to_windows_left_and_back() {
        let (mac, win, linux) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut screens = BTreeMap::new();
        screens.insert(
            mac,
            vec![
                screen(mac, 0, 0, 0, 1512, 982),
                screen(mac, 1, 1512, -229, 2560, 1440),
            ],
        );
        screens.insert(win, vec![screen(win, 0, 0, 0, 1920, 1080)]);
        let index = |i| match i {
            1 => Some(win),
            2 => Some(mac),
            3 => Some(linux),
            _ => None,
        };
        let specs = [
            spec(2, Edge::Right, 1),
            spec(1, Edge::Right, 3), // Ubuntu, later
        ];
        let links = compile(&specs, index, &screens);
        assert_eq!(links.len(), 2, "the Ubuntu link waits for its screens");
        let fwd = links.iter().find(|l| l.from_node == mac).unwrap();
        assert_eq!(
            (
                fwd.from_screen,
                fwd.from_edge,
                fwd.to_node,
                fwd.to_screen,
                fwd.to_edge
            ),
            (1, Edge::Right, win, 0, Edge::Left)
        );
        let back = links.iter().find(|l| l.from_node == win).unwrap();
        assert_eq!(
            (back.from_edge, back.to_node, back.to_screen, back.to_edge),
            (Edge::Left, mac, 1, Edge::Right)
        );

        // Once Ubuntu reports its screen, its link appears too.
        screens.insert(linux, vec![screen(linux, 0, 0, 0, 1920, 1080)]);
        assert_eq!(compile(&specs, index, &screens).len(), 4);
    }

    #[test]
    fn explicit_screens_ranges_and_oneway() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut screens = BTreeMap::new();
        screens.insert(
            a,
            vec![screen(a, 0, 0, 0, 100, 100), screen(a, 1, 100, 0, 100, 100)],
        );
        screens.insert(b, vec![screen(b, 0, 0, 0, 100, 100)]);
        let index = |i| [None, Some(a), Some(b)].get(i as usize).copied().flatten();
        let mut s = spec(1, Edge::Right, 2);
        s.from_screen = Some(0);
        s.from_range = (0.5, 1.0);
        s.oneway = true;
        let links = compile(&[s.clone()], index, &screens);
        assert_eq!(links.len(), 1);
        assert_eq!((links[0].from_screen, links[0].from_range), (0, (0.5, 1.0)));
        // A screen index that does not exist skips the link.
        s.from_screen = Some(7);
        assert!(compile(&[s], index, &screens).is_empty());
    }

    #[test]
    fn outermost_picks_by_direction() {
        let n = Uuid::new_v4();
        let ss = [
            screen(n, 0, 0, 0, 1512, 982),
            screen(n, 1, 1512, -229, 2560, 1440),
        ];
        assert_eq!(outermost(&ss, Edge::Right), Some(1));
        assert_eq!(outermost(&ss, Edge::Left), Some(0));
        assert_eq!(outermost(&ss, Edge::Top), Some(1));
        assert_eq!(outermost(&ss, Edge::Bottom), Some(1));
        assert_eq!(outermost(&[], Edge::Left), None);
    }

    #[test]
    fn layout_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("rawkvm-layout-{}", Uuid::new_v4()));
        let path = LayoutFile::path_in(&dir);
        assert_eq!(LayoutFile::load(&path).unwrap(), None);
        let f = LayoutFile {
            options: EdgeOptions {
                push_ms: 100,
                ..Default::default()
            },
            link: vec![EdgeLink {
                from_node: Uuid::new_v4(),
                from_screen: 1,
                from_edge: Edge::Right,
                from_range: (0.0, 1.0),
                to_node: Uuid::new_v4(),
                to_screen: 0,
                to_edge: Edge::Left,
                to_range: (0.25, 0.75),
            }],
        };
        f.save(&path).unwrap();
        assert_eq!(LayoutFile::load(&path).unwrap(), Some(f));
        fs::write(&path, "link = 3").unwrap();
        assert!(LayoutFile::load(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_from_toml() {
        #[derive(Deserialize)]
        struct T {
            layout: Vec<LinkSpec>,
        }
        let t: T = toml::from_str(
            "[[layout]]\nfrom = 2\nedge = \"right\"\nto = 1\n\n[[layout]]\nfrom = 1\nedge = \"right\"\nto = 3\nto_range = [0.0, 0.5]\n",
        )
        .unwrap();
        assert_eq!(t.layout[0], spec(2, Edge::Right, 1));
        assert_eq!(t.layout[1].to_range, (0.0, 0.5));
    }
}
