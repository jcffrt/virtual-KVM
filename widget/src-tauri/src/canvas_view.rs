//! The Monitors tab: everything the SVG canvas draws, computed here from
//! the widget state and the draft arrangement (geometry in
//! `agent_core::canvas`). The front end only drags blocks and sends the
//! draft back.

use std::collections::BTreeMap;

use agent_core::canvas::{self, Rect};
use agent_core::widget::WidgetState;
use proto::{Edge, EdgeLink, NodeKind, Placement, Screen};
use serde::Serialize;
use uuid::Uuid;

/// Distance (logical units) within which a dropped block snaps.
pub const SNAP: f64 = 40.0;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanvasView {
    /// The arrangement shown (the draft, or the hub's, or a proposal).
    pub placements: Vec<Placement>,
    pub blocks: Vec<BlockView>,
    pub rects: Vec<RectView>,
    pub links: Vec<LinkView>,
    /// `[min_x, min_y, max_x, max_y]` of everything, in logical units.
    pub bounds: [f64; 4],
    /// The hub has a canvas saved by a widget (not the config seed).
    pub custom: bool,
    /// The arrangement differs from the hub's.
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockView {
    pub node: Uuid,
    pub name: String,
    /// Agent index (0 if not connected now).
    pub index: u8,
    pub connected: bool,
    /// No screens reported: grey placeholder.
    pub placeholder: bool,
    /// Scale the hub was told (primary screen), if any.
    pub reported_scale: Option<f32>,
    /// Manual correction in the draft, if any.
    pub scale: Option<f32>,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RectView {
    pub node: Uuid,
    pub screen: u8,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub placeholder: bool,
    /// Native size as the helper reports it, e.g. "2560×1440 ×1.25".
    pub native: String,
}

/// One touching stretch, drawn once (not per direction).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkView {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub label: String,
}

/// The nodes that belong on the canvas: every agent the hub reports, and
/// any node with screens or with a saved placement (a machine that is off).
fn nodes(st: &WidgetState) -> Vec<(Uuid, String, u8, bool)> {
    let mut out: Vec<(Uuid, String, u8, bool)> = st
        .status
        .iter()
        .flat_map(|s| s.nodes.iter())
        .filter(|n| n.kind == NodeKind::Agent)
        .map(|n| (n.node_id, n.node_name.clone(), n.assigned_index, true))
        .collect();
    let extra = st
        .screens
        .keys()
        .copied()
        .chain(st.canvas.iter().map(|p| p.node))
        .collect::<Vec<_>>();
    for id in extra {
        if !out.iter().any(|n| n.0 == id) {
            out.push((id, "desconectado".into(), 0, false));
        }
    }
    out.sort_by_key(|n| (n.2 == 0, n.2));
    out
}

/// The arrangement to show: the draft if any, else the hub's saved canvas
/// (completed with new nodes), else a proposal from the links in force.
pub fn arrangement(st: &WidgetState, draft: Option<&[Placement]>) -> Vec<Placement> {
    let ids: Vec<Uuid> = nodes(st).iter().map(|n| n.0).collect();
    let base: Vec<Placement> = match draft {
        Some(d) if !d.is_empty() => d.to_vec(),
        _ if !st.canvas.is_empty() => st.canvas.clone(),
        _ => return canvas::initial(&ids, &st.screens, &st.edges),
    };
    // Nodes not placed yet go to the right of everything.
    let mut out = base;
    let missing: Vec<Uuid> = ids
        .iter()
        .copied()
        .filter(|id| !out.iter().any(|p| p.node == *id))
        .collect();
    if !missing.is_empty() {
        let extra = canvas::initial(&missing, &st.screens, &[]);
        let right = canvas::rects(&out, &st.screens)
            .iter()
            .map(|r| r.x + r.w)
            .fold(0.0, f64::max);
        out.extend(extra.into_iter().map(|p| Placement {
            x: p.x + right + 200.0,
            ..p
        }));
    }
    out
}

fn segment(r: &Rect, edge: Edge, range: (f32, f32)) -> (f64, f64, f64, f64) {
    let (a, b) = (range.0 as f64, range.1 as f64);
    match edge {
        Edge::Left => (r.x, r.y + a * r.h, r.x, r.y + b * r.h),
        Edge::Right => (r.x + r.w, r.y + a * r.h, r.x + r.w, r.y + b * r.h),
        Edge::Top => (r.x + a * r.w, r.y, r.x + b * r.w, r.y),
        Edge::Bottom => (r.x + a * r.w, r.y + r.h, r.x + b * r.w, r.y + r.h),
    }
}

pub fn build(st: &WidgetState, draft: Option<&[Placement]>) -> CanvasView {
    let placements = arrangement(st, draft);
    let names: BTreeMap<Uuid, (String, u8, bool)> = nodes(st)
        .into_iter()
        .map(|(id, n, i, c)| (id, (n, i, c)))
        .collect();
    let empty: Vec<Screen> = Vec::new();
    let mut blocks = Vec::new();
    let mut rects = Vec::new();
    for p in &placements {
        let screens = st.screens.get(&p.node).unwrap_or(&empty);
        let rs = canvas::block(p.node, screens, p);
        let (x0, y0) = rs
            .iter()
            .fold((f64::MAX, f64::MAX), |a, r| (a.0.min(r.x), a.1.min(r.y)));
        let (x1, y1) = rs.iter().fold((f64::MIN, f64::MIN), |a, r| {
            (a.0.max(r.x + r.w), a.1.max(r.y + r.h))
        });
        let (name, index, connected) = names
            .get(&p.node)
            .cloned()
            .unwrap_or_else(|| ("desconectado".into(), 0, false));
        blocks.push(BlockView {
            node: p.node,
            name,
            index,
            connected,
            placeholder: screens.is_empty(),
            reported_scale: screens.iter().min_by_key(|s| s.index).map(|s| s.scale),
            scale: p.scale,
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        });
        for r in rs {
            let native = screens
                .iter()
                .find(|s| s.index == r.screen && !r.placeholder)
                .map_or_else(
                    || "sin helper".to_string(),
                    |s| format!("{}×{} ×{}", s.w, s.h, (s.scale * 100.0).round() / 100.0),
                );
            rects.push(RectView {
                node: r.node,
                screen: r.screen,
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                placeholder: r.placeholder,
                native,
            });
        }
    }

    let all = canvas::rects(&placements, &st.screens);
    let find = |node: Uuid, screen: u8| {
        all.iter()
            .find(|r| r.node == node && (r.screen == screen || r.placeholder))
    };
    let name = |id: &Uuid| names.get(id).map_or("?".to_string(), |n| n.0.clone());
    let links = canvas::links(&placements, &st.screens)
        .iter()
        // Both directions touch the same stretch: draw it once.
        .filter(|l| matches!(l.from_edge, Edge::Right | Edge::Bottom))
        .filter_map(|l: &EdgeLink| {
            let r = find(l.from_node, l.from_screen)?;
            let (x1, y1, x2, y2) = segment(r, l.from_edge, l.from_range);
            Some(LinkView {
                x1,
                y1,
                x2,
                y2,
                label: format!("{} ↔ {}", name(&l.from_node), name(&l.to_node)),
            })
        })
        .collect();

    let bounds = rects
        .iter()
        .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, r| {
            [
                b[0].min(r.x),
                b[1].min(r.y),
                b[2].max(r.x + r.w),
                b[3].max(r.y + r.h),
            ]
        });
    let bounds = if rects.is_empty() {
        [0.0, 0.0, 1920.0, 1080.0]
    } else {
        bounds
    };
    CanvasView {
        dirty: placements != st.canvas,
        custom: !st.canvas.is_empty(),
        placements,
        blocks,
        rects,
        links,
        bounds,
    }
}

/// Move `node` to (x, y) in the draft and snap it to its neighbours.
pub fn drop_block(
    st: &WidgetState,
    draft: &[Placement],
    node: Uuid,
    x: f64,
    y: f64,
) -> Vec<Placement> {
    let mut out = arrangement(st, Some(draft));
    if let Some(i) = out.iter().position(|p| p.node == node) {
        let moved = Placement {
            x,
            y,
            ..out[i].clone()
        };
        out[i] = canvas::snap(&moved, &out, &st.screens, SNAP);
    }
    out
}

/// Set (or clear, with `None` or a value <= 0) the manual scale of `node`.
pub fn set_scale(
    st: &WidgetState,
    draft: &[Placement],
    node: Uuid,
    scale: Option<f32>,
) -> Vec<Placement> {
    let mut out = arrangement(st, Some(draft));
    if let Some(p) = out.iter_mut().find(|p| p.node == node) {
        p.scale = scale.filter(|s| s.is_finite() && *s > 0.05 && *s < 10.0);
    }
    out
}

/// The `Layout` to send for an arrangement: its links, the hub's current
/// edge options, and the arrangement itself.
pub fn layout_for(st: &WidgetState, placements: &[Placement]) -> proto::Control {
    proto::Control::Layout {
        screens: Vec::new(),
        edges: canvas::links(placements, &st.screens),
        options: st.options,
        canvas: placements.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{HubStatus, NodeInfo, Os};

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

    fn agent(id: Uuid, name: &str, index: u8) -> NodeInfo {
        NodeInfo {
            node_id: id,
            node_name: name.into(),
            os: Os::Other,
            kind: NodeKind::Agent,
            assigned_index: index,
            rtt_ms: None,
            screens: 0,
        }
    }

    /// The real desk (Mac geometry real; the Windows one INVENTED: two
    /// monitors, primary 2560x1440 at 125 %), with the seed link Mac ->
    /// Windows and an Ubuntu agent without helper.
    fn desk() -> (WidgetState, Uuid, Uuid, Uuid) {
        let (win, mac, ubuntu) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut st = WidgetState {
            status: Some(HubStatus {
                hub_name: "orangepi".into(),
                active: Some(mac),
                nodes: vec![
                    agent(win, "GAMER", 1),
                    agent(mac, "MacBook", 2),
                    agent(ubuntu, "ubuntu", 3),
                ],
                device_count: 3,
                uptime_s: 1,
                version: String::new(),
                held: Vec::new(),
            }),
            ..Default::default()
        };
        st.screens.insert(
            mac,
            vec![
                screen(mac, 0, 0, 0, 1470, 956, 1.0),
                screen(mac, 1, 1470, -251, 1720, 1440, 1.0),
            ],
        );
        st.screens.insert(
            win,
            vec![
                screen(win, 0, 0, 0, 2560, 1440, 1.25),
                screen(win, 1, 2560, 0, 1920, 1080, 1.0),
            ],
        );
        // What the hub sends for the seed: both directions.
        let fwd = EdgeLink {
            from_node: mac,
            from_screen: 1,
            from_edge: Edge::Right,
            from_range: (0.0, 1.0),
            to_node: win,
            to_screen: 0,
            to_edge: Edge::Left,
            to_range: (0.0, 1.0),
        };
        let back = EdgeLink {
            from_node: win,
            from_screen: 0,
            from_edge: Edge::Left,
            to_node: mac,
            to_screen: 1,
            to_edge: Edge::Right,
            ..fwd.clone()
        };
        st.edges = vec![back, fwd];
        (st, win, mac, ubuntu)
    }

    #[test]
    fn proposal_from_the_seed_and_blocks() {
        let (st, win, mac, ubuntu) = desk();
        let v = build(&st, None);
        assert!(!v.custom && v.dirty, "seed: nothing saved yet");
        // Order: by agent index; the Mac is placed left of the Windows
        // block, as the seed link says.
        let order: Vec<_> = v.blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(order, ["GAMER", "MacBook", "ubuntu"]);
        let w = v.blocks.iter().find(|b| b.node == win).unwrap();
        assert_eq!((w.x, w.y, w.w, w.h), (0.0, 0.0, 3584.0, 1152.0));
        let m = v.blocks.iter().find(|b| b.node == mac).unwrap();
        assert_eq!((m.x, m.y, m.w), (-3190.0, 0.0, 3190.0));
        assert_eq!(w.reported_scale, Some(1.25));
        let u = v.blocks.iter().find(|b| b.node == ubuntu).unwrap();
        assert!(u.placeholder);
        assert_eq!(v.rects.iter().filter(|r| r.node == mac).count(), 2);
        assert_eq!(
            v.rects
                .iter()
                .find(|r| r.node == win && r.screen == 0)
                .unwrap()
                .native,
            "2560×1440 ×1.25"
        );
        // One stretch drawn for the Mac <-> Windows pair.
        assert_eq!(v.links.len(), 1);
        assert_eq!(v.links[0].label, "MacBook ↔ GAMER");
        assert_eq!((v.links[0].x1, v.links[0].x2), (0.0, 0.0));
        assert_eq!(v.bounds[0], -3190.0);
    }

    #[test]
    fn drag_snap_scale_apply_and_saved() {
        let (mut st, win, mac, ubuntu) = desk();
        let draft = arrangement(&st, None);
        // Drop Ubuntu near the right of the Windows block: it snaps.
        let draft = drop_block(&st, &draft, ubuntu, 3584.0 + 25.0, 10.0);
        let u = draft.iter().find(|p| p.node == ubuntu).unwrap();
        assert_eq!((u.x, u.y), (3584.0, 0.0));
        let v = build(&st, Some(&draft));
        assert_eq!(v.links.len(), 2, "Windows <-> Ubuntu too");

        // Correct the Windows scale to 1.0: its block grows.
        let draft = set_scale(&st, &draft, win, Some(1.0));
        let v = build(&st, Some(&draft));
        assert_eq!(v.blocks.iter().find(|b| b.node == win).unwrap().w, 4480.0);
        let draft = set_scale(&st, &draft, win, Some(0.0));
        assert_eq!(
            draft.iter().find(|p| p.node == win).unwrap().scale,
            None,
            "invalid clears"
        );

        // Apply: the Layout carries the links and the canvas.
        let proto::Control::Layout { edges, canvas, .. } = layout_for(&st, &draft) else {
            panic!()
        };
        assert_eq!(canvas, draft);
        assert!(edges.iter().any(|l| l.from_node == mac && l.to_node == win));
        // Once the hub echoes it, the view is clean and custom.
        st.canvas = canvas;
        let v = build(&st, None);
        assert!(v.custom && !v.dirty);
    }

    #[test]
    fn saved_canvas_gets_new_nodes_appended() {
        let (mut st, win, mac, ubuntu) = desk();
        st.canvas = vec![
            Placement {
                node: mac,
                x: 0.0,
                y: 0.0,
                scale: None,
            },
            Placement {
                node: win,
                x: 3190.0,
                y: 0.0,
                scale: None,
            },
        ];
        let a = arrangement(&st, None);
        assert_eq!(a.len(), 3);
        let u = a.iter().find(|p| p.node == ubuntu).unwrap();
        assert!(u.x > 3190.0 + 3584.0);
        // A saved node that is off now still shows.
        let gone = Uuid::new_v4();
        st.canvas.push(Placement {
            node: gone,
            x: -3000.0,
            y: 0.0,
            scale: None,
        });
        let v = build(&st, None);
        let g = v.blocks.iter().find(|b| b.node == gone).unwrap();
        assert!(!g.connected && g.placeholder);
    }
}
