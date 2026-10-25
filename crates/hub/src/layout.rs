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
//! A target that has reported no screens (no edge helper yet, like a USB
//! gadget PC before its helper is installed) still gets the forward link,
//! entering its screen `to_screen` (default 0): switching to it works, only
//! nobody places its pointer. The backward link needs the target's screens
//! and appears when they are reported.
//!
//! The config links are only the seed. A layout sent by a widget (or
//! loaded with `rawkvm-hub --load-layout`) is kept in
//! `<state_dir>/layout.toml` and wins from then on, also after restarts.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proto::{Edge, EdgeLink, EdgeOptions, Placement, Screen};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config;

pub const FILE: &str = "layout.toml";

/// The persisted layout: resolved links (node ids and screen indices),
/// the edge options and the widget canvas that came with them. Screens are
/// not stored: they are live data from the helpers.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutFile {
    pub options: EdgeOptions,
    pub link: Vec<EdgeLink>,
    pub canvas: Vec<Placement>,
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

    /// The nodes the links and the canvas name that `known` does not know
    /// (not in nodes.toml), each once, in order of appearance: a node
    /// that paired again with a new node_id leaves its old one here, and
    /// those links do nothing.
    pub fn unknown_nodes(&self, known: impl Fn(&Uuid) -> bool) -> Vec<Uuid> {
        let mut out = Vec::new();
        let ids = self
            .link
            .iter()
            .flat_map(|l| [l.from_node, l.to_node])
            .chain(self.canvas.iter().map(|p| p.node));
        for id in ids {
            if !known(&id) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// Whether the links or the canvas name `node`.
    pub fn mentions(&self, node: &Uuid) -> bool {
        self.link
            .iter()
            .any(|l| l.from_node == *node || l.to_node == *node)
            || self.canvas.iter().any(|p| p.node == *node)
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

/// Result of [`compile`].
#[derive(Debug, Default, PartialEq)]
pub struct Compiled {
    pub links: Vec<EdgeLink>,
    /// `(spec index, target node)` of links resolved without the target's
    /// screens (forward direction only).
    pub placeholders: Vec<(usize, Uuid)>,
}

/// Resolve `specs` against the agents known so far. Links whose nodes are
/// unknown (no agent ever had that index) or whose source has not reported
/// screens are skipped; call again when screens change.
pub fn compile(
    specs: &[LinkSpec],
    node_of_index: impl Fn(u8) -> Option<Uuid>,
    screens: &BTreeMap<Uuid, Vec<Screen>>,
) -> Compiled {
    let mut out = Compiled::default();
    for (n, spec) in specs.iter().enumerate() {
        let (Some(from), Some(to)) = (node_of_index(spec.from), node_of_index(spec.to)) else {
            continue;
        };
        let Some(fs) = screens.get(&from).filter(|s| !s.is_empty()) else {
            continue;
        };
        let from_edge = spec.edge;
        let to_edge = from_edge.opposite();
        let Some(from_screen) = pick(fs, spec.from_screen, from_edge) else {
            continue;
        };
        let (to_screen, placeholder) = match screens.get(&to).filter(|s| !s.is_empty()) {
            Some(ts) => match pick(ts, spec.to_screen, to_edge) {
                Some(i) => (i, false),
                None => continue,
            },
            None => (spec.to_screen.unwrap_or(0), true),
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
        if placeholder {
            out.placeholders.push((n, to));
        } else if !spec.oneway {
            out.links.push(EdgeLink {
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
        out.links.push(link);
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
        let c = compile(&specs, index, &screens);
        let links = c.links;
        // Windows -> Ubuntu resolves one way with a placeholder screen.
        assert_eq!(links.len(), 3);
        assert_eq!(c.placeholders, [(1, linux)]);
        let to_ubuntu = links.iter().find(|l| l.to_node == linux).unwrap();
        assert_eq!(
            (to_ubuntu.from_node, to_ubuntu.to_screen, to_ubuntu.to_edge),
            (win, 0, Edge::Left)
        );
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
        let c = compile(&specs, index, &screens);
        assert_eq!((c.links.len(), c.placeholders.len()), (4, 0));
    }

    /// The deployment of 2026-10-22: the Mac has its helper, the Windows PC
    /// behind the gadget does not yet.
    #[test]
    fn target_without_screens_gets_a_one_way_link() {
        let (mac, gadget) = (Uuid::new_v4(), Uuid::new_v4());
        let mut screens = BTreeMap::new();
        screens.insert(
            mac,
            vec![
                screen(mac, 0, 0, 0, 1470, 956),
                screen(mac, 1, 1470, -251, 1720, 1440),
            ],
        );
        let index = |i| {
            [None, Some(gadget), Some(mac)]
                .get(i as usize)
                .copied()
                .flatten()
        };
        let specs = [spec(2, Edge::Right, 1)];
        let c = compile(&specs, index, &screens);
        assert_eq!(c.placeholders, [(0, gadget)]);
        assert_eq!(c.links.len(), 1);
        let l = &c.links[0];
        assert_eq!(
            (
                l.from_node,
                l.from_screen,
                l.from_edge,
                l.to_node,
                l.to_screen,
                l.to_edge
            ),
            (mac, 1, Edge::Right, gadget, 0, Edge::Left)
        );
        // The Windows helper reports its monitor: both directions.
        screens.insert(gadget, vec![screen(gadget, 0, 0, 0, 1920, 1080)]);
        let c = compile(&specs, index, &screens);
        assert_eq!((c.links.len(), c.placeholders.len()), (2, 0));
        // Nothing at all while the source has no screens either.
        screens.remove(&mac);
        assert_eq!(compile(&specs, index, &screens).links.len(), 0);
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
        let links = compile(&[s.clone()], index, &screens).links;
        assert_eq!(links.len(), 1);
        assert_eq!((links[0].from_screen, links[0].from_range), (0, (0.5, 1.0)));
        // A screen index that does not exist skips the link.
        s.from_screen = Some(7);
        assert!(compile(&[s], index, &screens).links.is_empty());
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
            canvas: vec![Placement {
                node: Uuid::new_v4(),
                x: 3190.0,
                y: 0.0,
                scale: None,
            }],
        };
        f.save(&path).unwrap();
        assert_eq!(LayoutFile::load(&path).unwrap(), Some(f));
        fs::write(&path, "link = 3").unwrap();
        assert!(LayoutFile::load(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_nodes_the_store_does_not_know() {
        let (mac, gamer, old_mac) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let link = |from_node, to_node| EdgeLink {
            from_node,
            from_screen: 0,
            from_edge: Edge::Right,
            from_range: (0.0, 1.0),
            to_node,
            to_screen: 0,
            to_edge: Edge::Left,
            to_range: (0.0, 1.0),
        };
        let place = |node| Placement {
            node,
            x: 0.0,
            y: 0.0,
            scale: None,
        };
        let f = LayoutFile {
            link: vec![link(old_mac, gamer), link(gamer, old_mac)],
            canvas: vec![place(gamer), place(old_mac)],
            ..Default::default()
        };
        let known = [mac, gamer];
        assert_eq!(f.unknown_nodes(|id| known.contains(id)), vec![old_mac]);
        assert!(f.mentions(&old_mac) && f.mentions(&gamer) && !f.mentions(&mac));
        let ok = LayoutFile {
            link: vec![link(mac, gamer)],
            ..Default::default()
        };
        assert!(ok.unknown_nodes(|id| known.contains(id)).is_empty());
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
