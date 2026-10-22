//! Emergency hotkey: tap Ctrl twice, then press 1..4.
//!
//! The hub reads keyboard reports for this purpose only and never alters
//! them. A tap is a press and release of Left or Right Ctrl with nothing else
//! held. Two taps arm the hotkey; the next key decides: a digit 1..4 (main
//! row or keypad) selects that agent, anything else disarms.
//!
//! State changes are logged at debug level (`RUST_LOG=rawkvm_hub::hotkey=debug`)
//! without key codes, so a failing hotkey can be diagnosed without logging
//! what is typed.

use std::time::{Duration, Instant};

use tracing::debug;

/// Left Ctrl (bit 0) and Right Ctrl (bit 4) of the modifier byte.
const CTRL_MASK: u8 = 0x11;
/// Longest press that still counts as a tap.
const TAP_MAX: Duration = Duration::from_millis(400);
/// Longest interval between the end of the first tap and the end of the
/// second.
const DOUBLE_WINDOW: Duration = Duration::from_millis(500);
/// How long the hotkey stays armed waiting for the digit.
const ARMED_FOR: Duration = Duration::from_millis(1500);

#[derive(Debug, Default)]
pub struct Hotkey {
    ctrl_down_at: Option<Instant>,
    /// Something else was held while Ctrl was down.
    dirty: bool,
    last_tap: Option<Instant>,
    armed_until: Option<Instant>,
}

/// Map a keyboard usage to an agent index.
fn digit(usage: u8) -> Option<u8> {
    match usage {
        0x1E..=0x21 => Some(usage - 0x1E + 1), // 1..4
        0x59..=0x5C => Some(usage - 0x59 + 1), // keypad 1..4
        _ => None,
    }
}

impl Hotkey {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_armed(&self) -> bool {
        self.armed_until.is_some()
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feed one keyboard report. Returns the selected agent index (1..=4)
    /// when the hotkey completes; the caller should then not forward this
    /// report.
    pub fn feed(&mut self, now: Instant, modifiers: u8, keys: &[u8]) -> Option<u8> {
        let ctrl = modifiers & CTRL_MASK != 0;
        let other_mods = modifiers & !CTRL_MASK != 0;
        let mut pressed = keys.iter().copied().filter(|&k| k != 0);

        if let Some(until) = self.armed_until {
            if now > until {
                debug!("hotkey: armed state expired");
                self.reset();
                return None;
            }
            if other_mods {
                debug!("hotkey: disarmed by another modifier");
                self.reset();
                return None;
            }
            if ctrl {
                // A third Ctrl press keeps the hotkey armed.
                return None;
            }
            return match pressed.next() {
                None => None, // release of the second tap, or idle report
                Some(k) => {
                    self.reset();
                    let d = digit(k);
                    match d {
                        Some(i) => debug!(index = i, "hotkey: digit"),
                        None => debug!("hotkey: disarmed by a non-digit key"),
                    }
                    d
                }
            };
        }

        let others = other_mods || pressed.next().is_some();
        match (ctrl, self.ctrl_down_at) {
            (true, None) => {
                self.ctrl_down_at = Some(now);
                self.dirty = others;
                debug!(with_other_keys = others, "hotkey: ctrl down");
            }
            (true, Some(_)) => self.dirty |= others,
            (false, Some(down)) => {
                self.ctrl_down_at = None;
                let held = now.duration_since(down);
                let tap = !self.dirty && !others && held <= TAP_MAX;
                if !tap {
                    debug!(
                        held_ms = held.as_millis() as u64,
                        with_other_keys = self.dirty || others,
                        "hotkey: ctrl released, not a tap"
                    );
                    self.last_tap = None;
                } else if let Some(gap) = self
                    .last_tap
                    .map(|t| now.duration_since(t))
                    .filter(|g| *g <= DOUBLE_WINDOW)
                {
                    debug!(gap_ms = gap.as_millis() as u64, "hotkey: second tap, armed");
                    self.last_tap = None;
                    self.armed_until = Some(now + ARMED_FOR);
                } else {
                    debug!(
                        held_ms = held.as_millis() as u64,
                        since_last_tap_ms = self
                            .last_tap
                            .map(|t| now.duration_since(t).as_millis() as u64),
                        "hotkey: first tap"
                    );
                    self.last_tap = Some(now);
                }
            }
            (false, None) => {
                if others && self.last_tap.take().is_some() {
                    debug!("hotkey: tap cancelled by another key");
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LCTRL: u8 = 0x01;
    const RCTRL: u8 = 0x10;
    const LSHIFT: u8 = 0x02;
    const KEY_1: u8 = 0x1E;
    const KEY_3: u8 = 0x20;
    const KEY_A: u8 = 0x04;
    const KP_2: u8 = 0x5A;

    struct Seq {
        h: Hotkey,
        t: Instant,
    }

    impl Seq {
        fn new() -> Self {
            Self {
                h: Hotkey::new(),
                t: Instant::now(),
            }
        }
        fn at(&mut self, ms: u64, mods: u8, keys: &[u8]) -> Option<u8> {
            self.t += Duration::from_millis(ms);
            let mut k = [0u8; 6];
            k[..keys.len()].copy_from_slice(keys);
            self.h.feed(self.t, mods, &k)
        }
        fn tap(&mut self, ctrl: u8) {
            assert_eq!(self.at(50, ctrl, &[]), None);
            assert_eq!(self.at(80, 0, &[]), None);
        }
    }

    #[test]
    fn double_ctrl_then_digit() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert!(s.h.is_armed());
        assert_eq!(s.at(200, 0, &[KEY_3]), Some(3));
        assert!(!s.h.is_armed());
    }

    #[test]
    fn mixed_ctrls_and_keypad() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.tap(RCTRL);
        assert_eq!(s.at(100, 0, &[KP_2]), Some(2));
    }

    #[test]
    fn ctrl_shortcut_is_not_a_tap() {
        let mut s = Seq::new();
        // Ctrl+A, then a clean tap: only one tap counted.
        s.at(10, LCTRL, &[]);
        s.at(30, LCTRL, &[KEY_A]);
        s.at(30, LCTRL, &[]);
        s.at(30, 0, &[]);
        s.tap(LCTRL);
        assert!(!s.h.is_armed());
        assert_eq!(s.at(100, 0, &[KEY_1]), None);
    }

    #[test]
    fn slow_taps_do_not_arm() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.at(700, 0, &[]);
        s.tap(LCTRL);
        assert!(!s.h.is_armed());
    }

    #[test]
    fn long_press_is_not_a_tap() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.at(50, LCTRL, &[]);
        s.at(450, 0, &[]);
        assert!(!s.h.is_armed());
    }

    #[test]
    fn other_key_or_timeout_disarms() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, 0, &[KEY_A]), None);
        assert!(!s.h.is_armed());

        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(2000, 0, &[KEY_1]), None);

        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, LSHIFT, &[KEY_1]), None);
    }

    #[test]
    fn digits_out_of_range_are_ignored() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, 0, &[0x22]), None); // '5'
        assert!(!s.h.is_armed());
    }
}
