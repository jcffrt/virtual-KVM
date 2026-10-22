//! CoreGraphics: screens, pointer position and pointer warp.
//!
//! Everything used here works without Input Monitoring or Accessibility:
//! reading the cursor location from a null `CGEvent` and warping the
//! cursor are allowed for any process in the user's session. Coordinates
//! are global display points, origin at the top-left of the main display,
//! y growing downwards (the same space as `CGDisplayBounds`).

use std::ffi::c_void;

use agent_core::edge::Point;
use proto::Screen;
use uuid::Uuid;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CGDirectDisplayID = u32;
type CGError = i32;

/// `kCGEventSourceStateCombinedSessionState`.
const COMBINED_SESSION_STATE: i32 = 0;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetActiveDisplayList(
        max: u32,
        displays: *mut CGDirectDisplayID,
        count: *mut u32,
    ) -> CGError;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
    fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut c_void);
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *const c_void) -> CGPoint;
    fn CGEventSourceCreate(state: i32) -> *mut c_void;
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: *mut c_void, seconds: f64);
    fn CGWarpMouseCursorPosition(to: CGPoint) -> CGError;
    fn CGAssociateMouseAndMouseCursorPosition(connected: i32) -> CGError;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// One display as macOS reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    pub id: u32,
    pub main: bool,
    pub screen: Screen,
}

/// Active displays: the main one gets index 0, the others follow left to
/// right (then top to bottom), so indices only change when the arrangement
/// does.
pub fn displays() -> Vec<Display> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    // SAFETY: the buffer holds `ids.len()` entries; `count` is written.
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err != 0 {
        return Vec::new();
    }
    // SAFETY: plain queries on display ids macOS just returned.
    let main = unsafe { CGMainDisplayID() };
    let mut list: Vec<(u32, CGRect, usize)> = ids[..count as usize]
        .iter()
        .map(|&id| unsafe { (id, CGDisplayBounds(id), backing_width(id)) })
        .collect();
    list.sort_by(|a, b| {
        (a.0 != main)
            .cmp(&(b.0 != main))
            .then(a.1.origin.x.total_cmp(&b.1.origin.x))
            .then(a.1.origin.y.total_cmp(&b.1.origin.y))
    });
    list.into_iter()
        .enumerate()
        .map(|(i, (id, r, pixels))| Display {
            id,
            main: id == main,
            screen: Screen {
                node: Uuid::nil(),
                index: i as u8,
                x: r.origin.x.round() as i32,
                y: r.origin.y.round() as i32,
                w: r.size.width.round().max(1.0) as u32,
                h: r.size.height.round().max(1.0) as u32,
                scale: if r.size.width > 0.0 {
                    (pixels as f64 / r.size.width) as f32
                } else {
                    1.0
                },
            },
        })
        .collect()
}

/// Width in device pixels (2x the points on a Retina display).
///
/// # Safety
/// `id` must be a display id macOS returned.
unsafe fn backing_width(id: CGDirectDisplayID) -> usize {
    let mode = CGDisplayCopyDisplayMode(id);
    if mode.is_null() {
        return CGDisplayPixelsWide(id);
    }
    let w = CGDisplayModeGetPixelWidth(mode);
    CGDisplayModeRelease(mode);
    w
}

/// Cursor location in global display points, rounded down so the last
/// column of a screen reads as `x + w - 1`.
pub fn pointer() -> Option<Point> {
    // SAFETY: a null-source event only carries the current location; it is
    // released right after.
    unsafe {
        let ev = CGEventCreate(std::ptr::null());
        if ev.is_null() {
            return None;
        }
        let p = CGEventGetLocation(ev);
        CFRelease(ev);
        Some(Point {
            x: p.x.floor() as i32,
            y: p.y.floor() as i32,
        })
    }
}

/// Moves the cursor without generating events. The pointer stays coupled
/// to the mouse and local events are not suppressed afterwards (macOS
/// otherwise ignores pointer input for about 250 ms after a warp).
pub struct Warper {
    source: *mut c_void,
}

impl Warper {
    pub fn new() -> Self {
        // SAFETY: creates an event source we own and release in Drop.
        let source = unsafe { CGEventSourceCreate(COMBINED_SESSION_STATE) };
        if !source.is_null() {
            // SAFETY: valid source.
            unsafe { CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0) };
        }
        Self { source }
    }

    pub fn warp(&self, to: Point) -> Result<(), i32> {
        // SAFETY: plain CoreGraphics calls with values.
        let err = unsafe {
            CGWarpMouseCursorPosition(CGPoint {
                x: to.x as f64 + 0.5,
                y: to.y as f64 + 0.5,
            })
        };
        // SAFETY: as above.
        unsafe { CGAssociateMouseAndMouseCursorPosition(1) };
        if err == 0 {
            Ok(())
        } else {
            Err(err)
        }
    }
}

impl Drop for Warper {
    fn drop(&mut self) {
        if !self.source.is_null() {
            // SAFETY: created by CGEventSourceCreate, released once.
            unsafe { CFRelease(self.source) };
        }
    }
}
