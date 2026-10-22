//! Geometry of the widget's monitor canvas: where each machine's block of
//! screens sits, and which [`EdgeLink`]s that arrangement produces. Pure and
//! tested here so the widget's TypeScript only draws and drags.
//!
//! Units: the canvas is in logical units (points). A node's screens come in
//! its native units (macOS: points; Windows: physical pixels) and
//! `Screen::scale` is native units per logical unit (proto v3), so a node's
//! block is its screens times `1 / scale` of its primary screen, or the
//! manual `Placement::scale`. One factor per node keeps the block rigid:
//! the OS moves the pointer between a machine's own screens, not us.
//!
//! Two screens of *different* nodes whose sides touch (within the snap
//! tolerance) and overlap along that side give one link each way, with
//! `from_range` / `to_range` the overlapping stretch of each side (0.0 at
//! the top / left end, as in `edge`).

use std::collections::BTreeMap;

use proto::{Edge, EdgeLink, Placement, Screen};
use uuid::Uuid;

/// Size given to a node that has reported no screens (no edge helper yet),
/// the same idea as the hub's placeholder link.
pub const PLACEHOLDER: (f64, f64) = (1920.0, 1080.0);

/// Sides closer than this (logical units) count as touching.
pub const TOUCH: f64 = 1.0;

/// One screen on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct Rect {
    pub node: Uuid,
    pub screen: u8,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// No screens reported: drawn grey, sized [`PLACEHOLDER`].
    pub placeholder: bool,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.w
    }
    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Native units per logical unit for `node`: the manual value, else the
/// primary screen's reported scale.
pub fn factor(screens: &[Screen], placement: Option<&Placement>) -> f64 {
    let s = placement
        .and_then(|p| p.scale)
        .or_else(|| screens.iter().min_by_key(|s| s.index).map(|s| s.scale))
        .unwrap_or(1.0) as f64;
    if s.is_finite() && s > 0.01 {
        s
    } else {
        1.0
    }
}

/// The screens of one node, placed at `p` in logical units. The block's
/// top-left corner (of its bounding box) is at `(p.x, p.y)`.
pub fn block(node: Uuid, screens: &[Screen], p: &Placement) -> Vec<Rect> {
    if screens.is_empty() {
        return vec![Rect {
            node,
            screen: 0,
            x: p.x,
            y: p.y,
            w: PLACEHOLDER.0,
            h: PLACEHOLDER.1,
            placeholder: true,
        }];
    }
    let f = factor(screens, Some(p));
    let min_x = screens.iter().map(|s| s.x).min().unwrap_or(0) as f64;
    let min_y = screens.iter().map(|s| s.y).min().unwrap_or(0) as f64;
    screens
        .iter()
        .map(|s| Rect {
            node,
            screen: s.index,
            x: p.x + (s.x as f64 - min_x) / f,
            y: p.y + (s.y as f64 - min_y) / f,
            w: s.w as f64 / f,
            h: s.h as f64 / f,
            placeholder: false,
        })
        .collect()
}

/// Every screen of every placed node.
pub fn rects(placements: &[Placement], screens: &BTreeMap<Uuid, Vec<Screen>>) -> Vec<Rect> {
    placements
        .iter()
        .flat_map(|p| {
            let s = screens.get(&p.node).map(Vec::as_slice).unwrap_or(&[]);
            block(p.node, s, p)
        })
        .collect()
}

fn ratio(v: f64, start: f64, len: f64) -> f32 {
    if len <= 0.0 {
        return 0.0;
    }
    ((v - start) / len).clamp(0.0, 1.0) as f32
}

/// Links produced by the arrangement, both directions.
pub fn links(placements: &[Placement], screens: &BTreeMap<Uuid, Vec<Screen>>) -> Vec<EdgeLink> {
    let rs = rects(placements, screens);
    let mut out = Vec::new();
    for a in &rs {
        for b in &rs {
            if a.node == b.node {
                continue;
            }
            // a's right side against b's left side.
            if (a.right() - b.x).abs() <= TOUCH {
                let (lo, hi) = (a.y.max(b.y), a.bottom().min(b.bottom()));
                if hi - lo > TOUCH {
                    out.push(link(a, Edge::Right, (lo, hi), b, Edge::Left));
                    out.push(link(b, Edge::Left, (lo, hi), a, Edge::Right));
                }
            }
            // a's bottom side against b's top side.
            if (a.bottom() - b.y).abs() <= TOUCH {
                let (lo, hi) = (a.x.max(b.x), a.right().min(b.right()));
                if hi - lo > TOUCH {
                    out.push(link(a, Edge::Bottom, (lo, hi), b, Edge::Top));
                    out.push(link(b, Edge::Top, (lo, hi), a, Edge::Bottom));
                }
            }
        }
    }
    out
}

fn link(from: &Rect, from_edge: Edge, (lo, hi): (f64, f64), to: &Rect, to_edge: Edge) -> EdgeLink {
    let range = |r: &Rect, e: Edge| match e {
        Edge::Left | Edge::Right => (ratio(lo, r.y, r.h), ratio(hi, r.y, r.h)),
        Edge::Top | Edge::Bottom => (ratio(lo, r.x, r.w), ratio(hi, r.x, r.w)),
    };
    EdgeLink {
        from_node: from.node,
        from_screen: from.screen,
        from_edge,
        from_range: range(from, from_edge),
        to_node: to.node,
        to_screen: to.screen,
        to_edge,
        to_range: range(to, to_edge),
    }
}

/// Snap a block being dragged: if one of its sides is within `tolerance`
/// of another node's opposite side, move it to touch exactly (and align
/// its top or left edge if that is within `tolerance` too).
pub fn snap(
    moving: &Placement,
    placements: &[Placement],
    screens: &BTreeMap<Uuid, Vec<Screen>>,
    tolerance: f64,
) -> Placement {
    let empty = Vec::new();
    let mine = block(
        moving.node,
        screens.get(&moving.node).unwrap_or(&empty),
        moving,
    );
    let others: Vec<Rect> = placements
        .iter()
        .filter(|p| p.node != moving.node)
        .flat_map(|p| block(p.node, screens.get(&p.node).unwrap_or(&empty), p))
        .collect();
    let mut best_dx: Option<f64> = None;
    let mut best_dy: Option<f64> = None;
    let consider = |best: &mut Option<f64>, d: f64| {
        if d.abs() <= tolerance && best.map_or(true, |b| d.abs() < b.abs()) {
            *best = Some(d);
        }
    };
    for m in &mine {
        for o in &others {
            let overlap_y = m.y < o.bottom() + tolerance && o.y < m.bottom() + tolerance;
            let overlap_x = m.x < o.right() + tolerance && o.x < m.right() + tolerance;
            if overlap_y {
                consider(&mut best_dx, o.right() - m.x); // my left to its right
                consider(&mut best_dx, o.x - m.right()); // my right to its left
            }
            if overlap_x {
                consider(&mut best_dy, o.bottom() - m.y);
                consider(&mut best_dy, o.y - m.bottom());
            }
        }
    }
    let mut p = moving.clone();
    p.x += best_dx.unwrap_or(0.0);
    p.y += best_dy.unwrap_or(0.0);
    // Side by side: also align tops if close; stacked: align lefts.
    if best_dx.is_some() && best_dy.is_none() {
        let shifted = block(p.node, screens.get(&p.node).unwrap_or(&empty), &p);
        let mut dy: Option<f64> = None;
        for m in &shifted {
            for o in &others {
                consider(&mut dy, o.y - m.y);
            }
        }
        p.y += dy.unwrap_or(0.0);
    }
    p
}

/// A starting arrangement when the hub has no canvas yet: place the nodes
/// so that the given links (the hub's current layout, e.g. from the
/// `[[layout]]` seed) touch, top-aligned; nodes not reached by any link go
/// to the right of everything, in `order`.
pub fn initial(
    order: &[Uuid],
    screens: &BTreeMap<Uuid, Vec<Screen>>,
    edges: &[EdgeLink],
) -> Vec<Placement> {
    let empty = Vec::new();
    let mut placed: Vec<Placement> = Vec::new();
    let at = |node: Uuid, x: f64, y: f64| Placement {
        node,
        x,
        y,
        scale: None,
    };
    let rect_of = |placed: &[Placement], node: Uuid, screen: u8| -> Option<Rect> {
        let p = placed.iter().find(|p| p.node == node)?;
        block(node, screens.get(&node).unwrap_or(&empty), p)
            .into_iter()
            .find(|r| r.screen == screen || r.placeholder)
    };
    let mut queue: Vec<Uuid> = Vec::new();
    for &start in order {
        if placed.iter().any(|p| p.node == start) {
            continue;
        }
        let right = placed
            .iter()
            .flat_map(|p| block(p.node, screens.get(&p.node).unwrap_or(&empty), p))
            .map(|r| r.right())
            .fold(0.0, f64::max);
        let gap = if placed.is_empty() { 0.0 } else { 200.0 };
        placed.push(at(start, right + gap, 0.0));
        queue.push(start);
        while let Some(n) = queue.pop() {
            for l in edges.iter().filter(|l| l.from_node == n) {
                if placed.iter().any(|p| p.node == l.to_node) || !order.contains(&l.to_node) {
                    continue;
                }
                let Some(fr) = rect_of(&placed, n, l.from_screen) else {
                    continue;
                };
                // Put the target block at the origin, measure where its entry
                // screen lands, then shift it against the source side.
                let probe = at(l.to_node, 0.0, 0.0);
                let tb = block(l.to_node, screens.get(&l.to_node).unwrap_or(&empty), &probe);
                let Some(tr) = tb.iter().find(|r| r.screen == l.to_screen || r.placeholder) else {
                    continue;
                };
                let (x, y) = match l.from_edge {
                    Edge::Right => (fr.right() - tr.x, fr.y - tr.y),
                    Edge::Left => (fr.x - tr.right(), fr.y - tr.y),
                    Edge::Bottom => (fr.x - tr.x, fr.bottom() - tr.y),
                    Edge::Top => (fr.x - tr.x, fr.y - tr.bottom()),
                };
                placed.push(at(l.to_node, x, y));
                queue.push(l.to_node);
            }
        }
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(node: Uuid, index: u8, x: i32, y: i32, w: u32, h: u32, scale: f32) -> Screen {
        Screen {
            node,
            index,
            x,
            y,
            w,
            h,
            scale,
        }
    }

    fn at(node: Uuid, x: f64, y: f64) -> Placement {
        Placement {
            node,
            x,
            y,
            scale: None,
        }
    }

    /// The user's desk, validated on 2026-10-22. Mac (points, scale 1 in
    /// v3): built-in 1470x956 at (0,0), external 1720x1440 at (1470,-251).
    /// Windows: two monitors; the geometry is INVENTED (the real one is in
    /// the hub log): a 2560x1440 at 125 % as primary and a 1920x1080 at
    /// 100 % to its right, in physical pixels.
    struct Desk {
        mac: Uuid,
        win: Uuid,
        screens: BTreeMap<Uuid, Vec<Screen>>,
    }

    fn desk() -> Desk {
        let (mac, win) = (Uuid::new_v4(), Uuid::new_v4());
        let mut screens = BTreeMap::new();
        screens.insert(
            mac,
            vec![
                screen(mac, 0, 0, 0, 1470, 956, 1.0),
                screen(mac, 1, 1470, -251, 1720, 1440, 1.0),
            ],
        );
        screens.insert(
            win,
            vec![
                screen(win, 0, 0, 0, 2560, 1440, 1.25),
                screen(win, 1, 2560, 0, 1920, 1080, 1.0),
            ],
        );
        Desk { mac, win, screens }
    }

    #[test]
    fn blocks_are_rigid_and_in_logical_units() {
        let d = desk();
        let mac = block(d.mac, &d.screens[&d.mac], &at(d.mac, 10.0, 20.0));
        // Bounding box top-left at the placement: the external is higher.
        assert_eq!(
            (mac[1].x, mac[1].y, mac[1].w, mac[1].h),
            (1480.0, 20.0, 1720.0, 1440.0)
        );
        assert_eq!((mac[0].x, mac[0].y), (10.0, 271.0));
        // Windows: one factor (the primary's 1.25) for the whole block.
        let win = block(d.win, &d.screens[&d.win], &at(d.win, 0.0, 0.0));
        assert_eq!((win[0].w, win[0].h), (2048.0, 1152.0));
        assert_eq!((win[1].x, win[1].w), (2048.0, 1536.0));
        // Manual correction.
        let fixed = Placement {
            scale: Some(1.0),
            ..at(d.win, 0.0, 0.0)
        };
        assert_eq!(block(d.win, &d.screens[&d.win], &fixed)[0].w, 2560.0);
        // No screens: a grey placeholder.
        let none = block(Uuid::nil(), &[], &at(Uuid::nil(), 5.0, 5.0));
        assert!(none[0].placeholder);
        assert_eq!((none[0].w, none[0].h), PLACEHOLDER);
    }

    #[test]
    fn windows_to_the_right_of_the_mac_external() {
        let d = desk();
        // The external spans x 0..1720 of its block after the built-in, so
        // the Mac block is 1470 + 1720 = 3190 wide.
        let ps = [at(d.mac, 0.0, 0.0), at(d.win, 3190.0, 100.0)];
        let ls = links(&ps, &d.screens);
        assert_eq!(ls.len(), 2, "{ls:#?}");
        let fwd = ls.iter().find(|l| l.from_node == d.mac).unwrap();
        assert_eq!(
            (
                fwd.from_screen,
                fwd.from_edge,
                fwd.to_node,
                fwd.to_screen,
                fwd.to_edge
            ),
            (1, Edge::Right, d.win, 0, Edge::Left)
        );
        // External 0..1440 tall; Windows primary 100..1252 (1152 logical).
        assert!((fwd.from_range.0 - 100.0 / 1440.0).abs() < 1e-5);
        assert!((fwd.from_range.1 - 1252.0 / 1440.0).abs() < 1e-5);
        assert_eq!(fwd.to_range, (0.0, 1.0));
        let back = ls.iter().find(|l| l.from_node == d.win).unwrap();
        assert_eq!(
            (back.from_edge, back.to_screen, back.to_edge),
            (Edge::Left, 1, Edge::Right)
        );
        assert_eq!(back.from_range, fwd.to_range);
        assert_eq!(back.to_range, fwd.from_range);
    }

    #[test]
    fn link_table() {
        let d = desk();
        let cases: &[(f64, f64, usize, &str)] = &[
            (3190.0, 0.0, 2, "touching, top aligned"),
            (3190.5, 0.0, 2, "within the touch tolerance"),
            (3195.0, 0.0, 0, "a gap"),
            (3190.0, 1500.0, 0, "below the external: no overlap"),
            (3100.0, 0.0, 0, "overlapping blocks touch nothing"),
        ];
        for &(x, y, n, why) in cases {
            let ps = [at(d.mac, 0.0, 0.0), at(d.win, x, y)];
            assert_eq!(links(&ps, &d.screens).len(), n, "{why}");
        }
        // Stacked: Windows under the Mac external -> bottom/top links.
        let ps = [at(d.mac, 0.0, 0.0), at(d.win, 1470.0, 1440.0)];
        let ls = links(&ps, &d.screens);
        assert!(ls.iter().any(|l| l.from_node == d.mac
            && l.from_edge == Edge::Bottom
            && l.to_edge == Edge::Top));
        // The Mac built-in and external never link to each other.
        let alone = [at(d.mac, 0.0, 0.0)];
        assert!(links(&alone, &d.screens).is_empty());
    }

    #[test]
    fn snapping() {
        let d = desk();
        let ps = vec![at(d.mac, 0.0, 0.0), at(d.win, 3000.0, 0.0)];
        // Dropped 12 units right of the Mac and 8 below its top: snaps to
        // touch and to align the tops.
        let p = snap(&at(d.win, 3202.0, 8.0), &ps, &d.screens, 20.0);
        assert_eq!((p.x, p.y), (3190.0, 0.0));
        // Far away: unchanged.
        let p = snap(&at(d.win, 4000.0, 300.0), &ps, &d.screens, 20.0);
        assert_eq!((p.x, p.y), (4000.0, 300.0));
    }

    #[test]
    fn initial_arrangement_follows_the_seed_links() {
        let d = desk();
        // What the hub compiled from `from = 2 (Mac), right, to = 1`.
        let edges = vec![EdgeLink {
            from_node: d.mac,
            from_screen: 1,
            from_edge: Edge::Right,
            from_range: (0.0, 1.0),
            to_node: d.win,
            to_screen: 0,
            to_edge: Edge::Left,
            to_range: (0.0, 1.0),
        }];
        let ubuntu = Uuid::new_v4(); // connected, no helper, no links
        let ps = initial(&[d.mac, d.win, ubuntu], &d.screens, &edges);
        assert_eq!(ps.len(), 3);
        let win = ps.iter().find(|p| p.node == d.win).unwrap();
        assert_eq!((win.x, win.y), (3190.0, 0.0));
        let ub = ps.iter().find(|p| p.node == ubuntu).unwrap();
        // Right of the Windows block (2048 + 1536) plus a gap.
        assert_eq!(ub.x, 3190.0 + 3584.0 + 200.0);
        // And the arrangement reproduces the seed's link.
        assert!(links(&ps, &d.screens)
            .iter()
            .any(|l| l.from_node == d.mac && l.to_node == d.win));
    }
}
