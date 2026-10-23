//! Emergency hotkey: tap a modifier (Ctrl by default) twice, then press a
//! key.
//!
//! The hub reads keyboard reports for this purpose only and never alters
//! them. A tap is a press and release of the left or right modifier with
//! nothing else held. Two taps arm the hotkey; the next key decides: a digit
//! 1..4 (main row or keypad) selects that agent, 0 the previous one and Esc
//! releases every key (each if enabled in [`proto::Hotkey`]); anything else
//! disarms. The report with that key is not forwarded.
//!
//! State changes are logged at debug level (`RUST_LOG=rawkvm_hub::hotkey=debug`)
//! without key codes, so a failing hotkey can be diagnosed without logging
//! what is typed.

use std::time::{Duration, Instant};

use proto::{Hotkey, HotkeyModifier};
use tracing::debug;

/// Left (bits 0..3) and right (bits 4..7) bits of `m` in the boot modifier
/// byte: Ctrl, Shift, Alt, GUI.
fn mask(m: HotkeyModifier) -> u8 {
    match m {
        HotkeyModifier::Ctrl => 0x11,
        HotkeyModifier::Shift => 0x22,
        HotkeyModifier::Alt => 0x44,
        HotkeyModifier::Gui => 0x88,
    }
}

const KEY_ESC: u8 = 0x29;
const KEY_0: u8 = 0x27;
const KP_0: u8 = 0x62;

/// What a completed hotkey asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Switch to the agent with this index (1..=4).
    Agent(u8),
    /// Back to the agent that was active before.
    Previous,
    /// "Nothing pressed" to every agent.
    ReleaseKeys,
}
/// Longest press that still counts as a tap.
const TAP_MAX: Duration = Duration::from_millis(400);
/// Longest interval between the end of the first tap and the end of the
/// second.
const DOUBLE_WINDOW: Duration = Duration::from_millis(500);
/// How long the hotkey stays armed waiting for the digit.
const ARMED_FOR: Duration = Duration::from_millis(1500);

#[derive(Debug, Default)]
pub struct Detector {
    mod_down_at: Option<Instant>,
    /// Something else was held while the modifier was down.
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

/// The action of the key pressed while armed, if `cfg` enables it.
fn action(cfg: &Hotkey, usage: u8) -> Option<Action> {
    match usage {
        KEY_0 | KP_0 if cfg.previous => Some(Action::Previous),
        KEY_ESC if cfg.release_keys => Some(Action::ReleaseKeys),
        u => digit(u).map(Action::Agent),
    }
}

impl Detector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_armed(&self) -> bool {
        self.armed_until.is_some()
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feed one keyboard report. Returns the action when the hotkey
    /// completes; the caller should then not forward this report. A change
    /// of `cfg.modifier` takes effect from the next tap.
    pub fn feed(
        &mut self,
        now: Instant,
        cfg: &Hotkey,
        modifiers: u8,
        keys: &[u8],
    ) -> Option<Action> {
        let m = mask(cfg.modifier);
        let held = modifiers & m != 0;
        let other_mods = modifiers & !m != 0;
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
            if held {
                // A third press of the modifier keeps the hotkey armed.
                return None;
            }
            return match pressed.next() {
                None => None, // release of the second tap, or idle report
                Some(k) => {
                    self.reset();
                    let a = action(cfg, k);
                    match a {
                        Some(a) => debug!(?a, "hotkey: completed"),
                        None => debug!("hotkey: disarmed by another key"),
                    }
                    a
                }
            };
        }

        let others = other_mods || pressed.next().is_some();
        match (held, self.mod_down_at) {
            (true, None) => {
                self.mod_down_at = Some(now);
                self.dirty = others;
                debug!(with_other_keys = others, "hotkey: modifier down");
            }
            (true, Some(_)) => self.dirty |= others,
            (false, Some(down)) => {
                self.mod_down_at = None;
                let held = now.duration_since(down);
                let tap = !self.dirty && !others && held <= TAP_MAX;
                if !tap {
                    debug!(
                        held_ms = held.as_millis() as u64,
                        with_other_keys = self.dirty || others,
                        "hotkey: modifier released, not a tap"
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
        h: Detector,
        t: Instant,
        cfg: Hotkey,
    }

    impl Seq {
        fn new() -> Self {
            Self::with(Hotkey::default())
        }
        fn with(cfg: Hotkey) -> Self {
            Self {
                h: Detector::new(),
                t: Instant::now(),
                cfg,
            }
        }
        fn at(&mut self, ms: u64, mods: u8, keys: &[u8]) -> Option<Action> {
            self.t += Duration::from_millis(ms);
            let mut k = [0u8; 6];
            k[..keys.len()].copy_from_slice(keys);
            self.h.feed(self.t, &self.cfg, mods, &k)
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
        assert_eq!(s.at(200, 0, &[KEY_3]), Some(Action::Agent(3)));
        assert!(!s.h.is_armed());
    }

    #[test]
    fn mixed_ctrls_and_keypad() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.tap(RCTRL);
        assert_eq!(s.at(100, 0, &[KP_2]), Some(Action::Agent(2)));
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

    #[test]
    fn previous_and_release_keys() {
        let mut s = Seq::new();
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, 0, &[KEY_0]), Some(Action::Previous));
        s.tap(RCTRL);
        s.tap(RCTRL);
        assert_eq!(s.at(100, 0, &[KP_0]), Some(Action::Previous));
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, 0, &[KEY_ESC]), Some(Action::ReleaseKeys));

        // Disabled: 0 and Esc just disarm (and are forwarded).
        let mut s = Seq::with(Hotkey {
            previous: false,
            release_keys: false,
            ..Hotkey::default()
        });
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, 0, &[KEY_0]), None);
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, 0, &[KEY_ESC]), None);
        assert!(!s.h.is_armed());
    }

    #[test]
    fn other_modifiers() {
        const LALT: u8 = 0x04;
        const RGUI: u8 = 0x80;
        let mut s = Seq::with(Hotkey {
            modifier: HotkeyModifier::Alt,
            ..Hotkey::default()
        });
        // Ctrl no longer arms it.
        s.tap(LCTRL);
        s.tap(LCTRL);
        assert_eq!(s.at(100, 0, &[KEY_1]), None);
        s.tap(LALT);
        s.tap(LALT);
        assert_eq!(s.at(100, 0, &[KEY_1]), Some(Action::Agent(1)));
        // Alt+Tab is not a tap.
        s.at(10, LALT, &[]);
        s.at(30, LALT, &[0x2B]);
        s.at(30, 0, &[]);
        s.tap(LALT);
        assert_eq!(s.at(100, 0, &[KEY_1]), None);

        let mut s = Seq::with(Hotkey {
            modifier: HotkeyModifier::Gui,
            ..Hotkey::default()
        });
        s.tap(RGUI);
        s.tap(RGUI);
        assert_eq!(s.at(100, 0, &[KEY_3]), Some(Action::Agent(3)));
        let mut s = Seq::with(Hotkey {
            modifier: HotkeyModifier::Shift,
            ..Hotkey::default()
        });
        s.tap(LSHIFT);
        s.tap(LSHIFT);
        assert_eq!(s.at(100, 0, &[KEY_ESC]), Some(Action::ReleaseKeys));
    }
}
