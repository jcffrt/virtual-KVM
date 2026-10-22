//! Screen edge switching logic, shared by the edge helpers of every OS.
//!
//! Pure: the helper feeds pointer positions (in its own desktop
//! coordinates, the same ones it reports in `Screens`) and the hub's
//! `Layout` / `Activate` / `Deactivate`, and acts on what comes back:
//! a `SwitchTo` to send, or a point to move the pointer to. It keeps no
//! layout of its own: the hub's latest `Layout` is the only source.
//!
//! Rules (handoff §4, docs/fase2-plan.md):
//! * The pointer *touches* an edge on its last pixel row or column, inside
//!   one of this node's screens.
//! * The `corner_pct` at each end of an edge never switches.
//! * With `push_ms` > 0 the pointer must stay on the edge that long.
//! * One `SwitchTo` per contact: the pointer must leave the edge before the
//!   same contact can send again (the hub may have ignored it).
//! * On `Activate` with an edge, the pointer is placed `dead_zone_px` from
//!   that edge; the edge stays disarmed until the pointer is further away
//!   than that, so it does not bounce straight back.
//! * On `Deactivate`, the pointer is parked at the centre of its screen,
//!   away from every edge.

use std::time::{Duration, Instant};

use proto::{Edge, EdgeLink, EdgeOptions, Screen};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// What to send to the hub as `Control::SwitchTo`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Switch {
    pub target: Uuid,
    pub screen: u8,
    pub enter_edge: Edge,
    pub pos_ratio: f32,
}

#[derive(Debug)]
pub struct EdgeSwitcher {
    me: Uuid,
    screens: Vec<Screen>,
    /// Links that start on this node.
    links: Vec<EdgeLink>,
    options: EdgeOptions,
    active: bool,
    /// Link being touched and since when; `fired` once its SwitchTo went out.
    contact: Option<Contact>,
    /// Edge the pointer entered through, disarmed until it moves away.
    entered: Option<(u8, Edge)>,
}

#[derive(Debug, Clone, Copy)]
struct Contact {
    link: usize,
    since: Instant,
    fired: bool,
}

fn contains(s: &Screen, p: Point) -> bool {
    let (x, y) = (p.x as i64, p.y as i64);
    x >= s.x as i64 && x < s.x as i64 + s.w as i64 && y >= s.y as i64 && y < s.y as i64 + s.h as i64
}

/// Distance in pixels from `p` to `edge` of `s` (0 = on the edge).
fn distance(s: &Screen, edge: Edge, p: Point) -> i64 {
    match edge {
        Edge::Left => p.x as i64 - s.x as i64,
        Edge::Right => s.x as i64 + s.w as i64 - 1 - p.x as i64,
        Edge::Top => p.y as i64 - s.y as i64,
        Edge::Bottom => s.y as i64 + s.h as i64 - 1 - p.y as i64,
    }
}

/// Position of `p` along `edge` of `s`, 0.0 (top / left end) to 1.0.
fn along(s: &Screen, edge: Edge, p: Point) -> f32 {
    let (offset, len) = match edge {
        Edge::Left | Edge::Right => (p.y as i64 - s.y as i64, s.h),
        Edge::Top | Edge::Bottom => (p.x as i64 - s.x as i64, s.w),
    };
    if len <= 1 {
        return 0.5;
    }
    (offset as f32 / (len - 1) as f32).clamp(0.0, 1.0)
}

/// Map `r` from range `from` to range `to`.
fn remap(r: f32, from: (f32, f32), to: (f32, f32)) -> f32 {
    let span = from.1 - from.0;
    let t = if span.abs() < f32::EPSILON {
        0.0
    } else {
        (r - from.0) / span
    };
    (to.0 + t * (to.1 - to.0)).clamp(0.0, 1.0)
}

fn in_range(r: f32, range: (f32, f32)) -> bool {
    let (lo, hi) = if range.0 <= range.1 {
        range
    } else {
        (range.1, range.0)
    };
    r >= lo && r <= hi
}

impl EdgeSwitcher {
    /// `me` is the agent node this helper acts for.
    pub fn new(me: Uuid) -> Self {
        Self {
            me,
            screens: Vec::new(),
            links: Vec::new(),
            options: EdgeOptions::default(),
            active: false,
            contact: None,
            entered: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// This node's screens, as measured locally (the helper also sends them
    /// to the hub in `Screens`).
    pub fn set_screens(&mut self, screens: Vec<Screen>) {
        self.screens = screens;
        self.contact = None;
    }

    /// Apply the hub's `Layout`: keep the links that start on this node.
    pub fn set_layout(&mut self, edges: &[EdgeLink], options: EdgeOptions) {
        self.links = edges
            .iter()
            .filter(|l| l.from_node == self.me)
            .cloned()
            .collect();
        self.options = options;
        self.contact = None;
    }

    fn screen(&self, index: u8) -> Option<&Screen> {
        self.screens.iter().find(|s| s.index == index)
    }

    /// The hub made this node active. Returns where to place the pointer,
    /// if the activation came through an edge.
    pub fn activate(
        &mut self,
        screen: Option<u8>,
        edge: Option<Edge>,
        pos_ratio: Option<f32>,
    ) -> Option<Point> {
        self.active = true;
        self.contact = None;
        self.entered = None;
        let edge = edge?;
        let s = screen
            .and_then(|i| self.screen(i))
            .or_else(|| self.screens.first())?
            .clone();
        let r = pos_ratio.unwrap_or(0.5).clamp(0.0, 1.0);
        let dz = (self.options.dead_zone_px as i64)
            .min(s.w as i64 / 2)
            .min(s.h as i64 / 2);
        let at =
            |origin: i32, len: u32| origin as i64 + (r * (len.max(1) - 1) as f32).round() as i64;
        let (x, y) = match edge {
            Edge::Left => (s.x as i64 + dz, at(s.y, s.h)),
            Edge::Right => (s.x as i64 + s.w as i64 - 1 - dz, at(s.y, s.h)),
            Edge::Top => (at(s.x, s.w), s.y as i64 + dz),
            Edge::Bottom => (at(s.x, s.w), s.y as i64 + s.h as i64 - 1 - dz),
        };
        self.entered = Some((s.index, edge));
        Some(Point {
            x: x as i32,
            y: y as i32,
        })
    }

    /// The hub made another node active. Returns where to park the pointer
    /// (the centre of the screen it is on, or of the first screen).
    pub fn deactivate(&mut self, pointer: Option<Point>) -> Option<Point> {
        self.active = false;
        self.contact = None;
        self.entered = None;
        let s = pointer
            .and_then(|p| self.screens.iter().find(|s| contains(s, p)))
            .or_else(|| self.screens.first())?;
        Some(Point {
            x: (s.x as i64 + s.w as i64 / 2) as i32,
            y: (s.y as i64 + s.h as i64 / 2) as i32,
        })
    }

    /// Feed one pointer position. Returns the switch to request, if any.
    pub fn on_pointer(&mut self, now: Instant, p: Point) -> Option<Switch> {
        if !self.active {
            return None;
        }
        if let Some((index, edge)) = self.entered {
            let away = self.screen(index).map_or(true, |s| {
                distance(s, edge, p) > self.options.dead_zone_px as i64
            });
            if away {
                self.entered = None;
            }
        }

        let touching = self.links.iter().enumerate().find_map(|(i, l)| {
            if self.entered == Some((l.from_screen, l.from_edge)) {
                return None;
            }
            let s = self.screen(l.from_screen)?;
            if !contains(s, p) || distance(s, l.from_edge, p) > 0 {
                return None;
            }
            let r = along(s, l.from_edge, p);
            let corner = self.options.corner_pct.min(49) as f32 / 100.0;
            if r < corner || r > 1.0 - corner || !in_range(r, l.from_range) {
                return None;
            }
            Some((i, r))
        });
        let Some((link, r)) = touching else {
            self.contact = None;
            return None;
        };

        let c = match self.contact {
            Some(c) if c.link == link => c,
            _ => Contact {
                link,
                since: now,
                fired: false,
            },
        };
        let pushed =
            now.duration_since(c.since) >= Duration::from_millis(self.options.push_ms as u64);
        if c.fired || !pushed {
            self.contact = Some(c);
            return None;
        }
        self.contact = Some(Contact { fired: true, ..c });
        let l = &self.links[link];
        Some(Switch {
            target: l.to_node,
            screen: l.to_screen,
            enter_edge: l.to_edge,
            pos_ratio: remap(r, l.from_range, l.to_range),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The user's desk (2026-10-22), left to right: Mac built-in, Mac
    // external 2560x1440, Windows (gadget), Ubuntu later. The geometries
    // are INVENTED for the tests (a 14" MacBook at its default 1512x982
    // points, the external's top 229 points above it, Windows at
    // 1920x1080); the helpers report the real ones at run time.
    struct Desk {
        mac: Uuid,
        win: Uuid,
        mac_screens: Vec<Screen>,
        win_screens: Vec<Screen>,
        links: Vec<EdgeLink>,
    }

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

    fn link(from: (Uuid, u8, Edge), to: (Uuid, u8, Edge)) -> EdgeLink {
        EdgeLink {
            from_node: from.0,
            from_screen: from.1,
            from_edge: from.2,
            from_range: (0.0, 1.0),
            to_node: to.0,
            to_screen: to.1,
            to_edge: to.2,
            to_range: (0.0, 1.0),
        }
    }

    fn desk() -> Desk {
        let (mac, win) = (Uuid::new_v4(), Uuid::new_v4());
        Desk {
            mac,
            win,
            mac_screens: vec![
                screen(mac, 0, 0, 0, 1512, 982),
                screen(mac, 1, 1512, -229, 2560, 1440),
            ],
            win_screens: vec![screen(win, 0, 0, 0, 1920, 1080)],
            // What the hub compiles from `from = 2, edge = "right", to = 1`.
            links: vec![
                link((mac, 1, Edge::Right), (win, 0, Edge::Left)),
                link((win, 0, Edge::Left), (mac, 1, Edge::Right)),
            ],
        }
    }

    fn switcher(
        me: Uuid,
        screens: &[Screen],
        links: &[EdgeLink],
        opts: EdgeOptions,
    ) -> EdgeSwitcher {
        let mut s = EdgeSwitcher::new(me);
        s.set_screens(screens.to_vec());
        s.set_layout(links, opts);
        s.activate(None, None, None);
        s
    }

    fn pt(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    /// Mac right edge of the external screen: x = 1512 + 2560 - 1.
    const MAC_RIGHT: i32 = 4071;

    #[test]
    fn mac_edge_table() {
        let d = desk();
        let t = Instant::now();
        let mid_ext = -229 + 720;
        // (pointer, expected pos_ratio on Windows' left edge or no switch)
        let cases: &[(Point, Option<f32>, &str)] = &[
            (
                pt(1511, 400),
                None,
                "built-in -> external is macOS' business",
            ),
            (pt(0, 400), None, "left edge of the built-in has no link"),
            (
                pt(MAC_RIGHT - 1, mid_ext),
                None,
                "one pixel short of the edge",
            ),
            (
                pt(MAC_RIGHT, mid_ext),
                Some(720.0 / 1439.0),
                "middle of the edge",
            ),
            (pt(MAC_RIGHT, -229), None, "top corner"),
            (pt(MAC_RIGHT, -229 + 1439), None, "bottom corner"),
            (
                pt(MAC_RIGHT, -229 + 100),
                Some(100.0 / 1439.0),
                "outside the 3% corner",
            ),
            (pt(MAC_RIGHT, -229 + 30), None, "inside the 3% corner"),
            (pt(MAC_RIGHT + 5, mid_ext), None, "off every screen"),
        ];
        for (p, want, why) in cases {
            let mut s = switcher(d.mac, &d.mac_screens, &d.links, EdgeOptions::default());
            let got = s.on_pointer(t, *p);
            match (got, want) {
                (None, None) => {}
                (Some(sw), Some(r)) => {
                    assert_eq!(
                        (sw.target, sw.screen, sw.enter_edge),
                        (d.win, 0, Edge::Left),
                        "{why}"
                    );
                    assert!((sw.pos_ratio - r).abs() < 1e-4, "{why}: {}", sw.pos_ratio);
                }
                _ => panic!("{why}: got {got:?}, want {want:?}"),
            }
        }
    }

    #[test]
    fn round_trip_mac_windows_mac_without_bouncing() {
        let d = desk();
        let t = Instant::now();
        let mut mac = switcher(d.mac, &d.mac_screens, &d.links, EdgeOptions::default());
        let mut win = EdgeSwitcher::new(d.win);
        win.set_screens(d.win_screens.clone());
        win.set_layout(&d.links, EdgeOptions::default());

        // Mac -> Windows at a quarter of the external screen's height.
        let y = -229 + 360;
        let sw = mac.on_pointer(t, pt(MAC_RIGHT, y)).unwrap();
        // The hub answers: Deactivate to the Mac (park), Activate to Windows.
        assert_eq!(
            mac.deactivate(Some(pt(MAC_RIGHT, y))),
            Some(pt(1512 + 1280, -229 + 720))
        );
        let entry = win
            .activate(Some(sw.screen), Some(sw.enter_edge), Some(sw.pos_ratio))
            .unwrap();
        assert_eq!(entry, pt(2, 270)); // 360/1439 of 1079, 2 px in
        assert!(mac.on_pointer(t, pt(MAC_RIGHT, y)).is_none(), "inactive");

        // The trackball keeps moving left: still in the dead zone.
        assert!(win.on_pointer(t, pt(1, 270)).is_none());
        assert!(win.on_pointer(t, pt(0, 270)).is_none());
        // Out of the dead zone and back to the edge: back to the Mac.
        assert!(win.on_pointer(t, pt(3, 270)).is_none());
        let back = win.on_pointer(t, pt(0, 270)).unwrap();
        assert_eq!(
            (back.target, back.screen, back.enter_edge),
            (d.mac, 1, Edge::Right)
        );
        assert!((back.pos_ratio - 270.0 / 1079.0).abs() < 1e-4);
        assert_eq!(win.deactivate(Some(pt(0, 270))), Some(pt(960, 540)));
        let entry = mac
            .activate(
                Some(back.screen),
                Some(back.enter_edge),
                Some(back.pos_ratio),
            )
            .unwrap();
        assert_eq!(entry, pt(MAC_RIGHT - 2, -229 + 360));
    }

    #[test]
    fn one_switch_per_contact() {
        let d = desk();
        let t = Instant::now();
        let mut mac = switcher(d.mac, &d.mac_screens, &d.links, EdgeOptions::default());
        let edge = pt(MAC_RIGHT, 500);
        assert!(mac.on_pointer(t, edge).is_some());
        // The hub ignored it (target gone): no flood while pressed on the edge.
        for ms in 1..50 {
            assert!(mac
                .on_pointer(t + Duration::from_millis(ms), edge)
                .is_none());
        }
        assert!(mac.on_pointer(t, pt(MAC_RIGHT - 10, 500)).is_none());
        assert!(
            mac.on_pointer(t, edge).is_some(),
            "a new contact sends again"
        );
    }

    #[test]
    fn push_threshold() {
        let d = desk();
        let t = Instant::now();
        let opts = EdgeOptions {
            push_ms: 100,
            ..Default::default()
        };
        let mut mac = switcher(d.mac, &d.mac_screens, &d.links, opts);
        let edge = pt(MAC_RIGHT, 500);
        let at = |ms| t + Duration::from_millis(ms);
        assert!(mac.on_pointer(at(0), edge).is_none());
        assert!(mac.on_pointer(at(60), edge).is_none());
        // Leaving restarts the count.
        assert!(mac.on_pointer(at(70), pt(MAC_RIGHT - 1, 500)).is_none());
        assert!(mac.on_pointer(at(80), edge).is_none());
        assert!(mac.on_pointer(at(170), edge).is_none());
        assert!(mac.on_pointer(at(180), edge).is_some());
        // Moving along the same edge is the same contact.
        let mut mac = switcher(d.mac, &d.mac_screens, &d.links, opts);
        assert!(mac.on_pointer(at(0), pt(MAC_RIGHT, 300)).is_none());
        assert!(mac.on_pointer(at(100), pt(MAC_RIGHT, 700)).is_some());
    }

    #[test]
    fn partial_ranges_are_remapped() {
        let d = desk();
        let mut links = d.links.clone();
        // Only the lower half of the Mac edge leads to the whole Windows edge.
        links[0].from_range = (0.5, 1.0);
        let mut mac = switcher(d.mac, &d.mac_screens, &links, EdgeOptions::default());
        let t = Instant::now();
        assert!(mac.on_pointer(t, pt(MAC_RIGHT, -229 + 400)).is_none());
        let sw = mac.on_pointer(t, pt(MAC_RIGHT, -229 + 1079)).unwrap();
        // 1079/1439 = 0.75 of the edge -> half way through 0.5..1.0.
        assert!((sw.pos_ratio - 0.4996).abs() < 1e-3, "{}", sw.pos_ratio);
    }

    #[test]
    fn layout_and_screens_changes() {
        let d = desk();
        let t = Instant::now();
        let mut mac = switcher(d.mac, &d.mac_screens, &[], EdgeOptions::default());
        assert!(
            mac.on_pointer(t, pt(MAC_RIGHT, 500)).is_none(),
            "no layout yet"
        );
        mac.set_layout(&d.links, EdgeOptions::default());
        assert!(mac.on_pointer(t, pt(MAC_RIGHT, 500)).is_some());
        // The external monitor is unplugged: its link no longer applies.
        mac.set_screens(vec![d.mac_screens[0].clone()]);
        assert!(mac.on_pointer(t, pt(1511, 500)).is_none());
        // Activate through a screen that is gone falls back to the first one.
        assert_eq!(
            mac.activate(Some(1), Some(Edge::Right), Some(0.5)),
            Some(pt(1509, 491))
        );
        // Activate without an edge (hotkey) does not move the pointer.
        assert_eq!(mac.activate(None, None, None), None);
        assert!(mac.is_active());
    }
}
