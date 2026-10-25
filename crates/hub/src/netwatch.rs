//! Wi-Fi watchdog for boards whose radio firmware hangs (ADR 0029).
//!
//! The Orange Pi Zero 2W's uwe5622 (driver sprdwl) sometimes stops
//! transmitting: `sprdwl:[WIFI_CMD_TX_DATA]timeout`, then `NETDEV
//! WATCHDOG: wlan0 ... transmit queue 0 timed out`. The board keeps
//! running, only the radio is gone, and every network agent loses its
//! keyboard. `rawkvm-netwatch` pings the default gateway every few seconds
//! and, when it stops answering (at once if the kernel logs the hang),
//! escalates: restart the Wi-Fi, reload the driver, reboot, with a limit
//! on reboots. This module is the decision logic, without I/O.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Kernel log lines that mean the radio firmware hung.
pub const HANG_SIGNATURES: &[&str] = &["WIFI_CMD_TX_DATA]timeout", "transmit queue 0 timed out"];

/// Whether a kernel log line shows the firmware hang.
pub fn is_hang(line: &str) -> bool {
    HANG_SIGNATURES.iter().any(|s| line.contains(s))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// No action this long after start (the Wi-Fi coming up at boot).
    pub grace: Duration,
    /// The gateway silent this long starts the escalation.
    pub fail_after: Duration,
    /// Wait after each step before the next one.
    pub step_wait: Duration,
    /// Driver modules to reload (step 2); skipped when false.
    pub reload_modules: bool,
    /// At most this many reboots per `reboot_window`; then only log.
    pub max_reboots: usize,
    pub reboot_window: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(120),
            fail_after: Duration::from_secs(60),
            step_wait: Duration::from_secs(30),
            reload_modules: true,
            max_reboots: 3,
            reboot_window: Duration::from_secs(3600),
        }
    }
}

/// What the default route says about the watched interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Default route through the watched interface; `None`: no gateway
    /// address to ping (a point-to-point link).
    Wifi { gateway: Option<String> },
    /// Another interface has a default route (Ethernet): nothing to do.
    Other { dev: String },
    /// No default route at all.
    Missing,
    /// The watched interface does not exist.
    NoInterface,
}

/// The default route of `ip route show default`, seen from `iface`.
pub fn parse_route(ip_route: &str, iface: &str) -> Route {
    let mut wifi = None;
    for line in ip_route.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() != Some(&"default") {
            continue;
        }
        let after = |key: &str| {
            words
                .iter()
                .position(|w| *w == key)
                .and_then(|i| words.get(i + 1))
                .map(|s| s.to_string())
        };
        match after("dev") {
            Some(dev) if dev == iface => {
                wifi.get_or_insert(Route::Wifi {
                    gateway: after("via"),
                });
            }
            Some(dev) => return Route::Other { dev },
            None => {}
        }
    }
    wifi.unwrap_or(Route::Missing)
}

/// One look at the network.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// Time since the watchdog started.
    pub at: Duration,
    pub route: Route,
    /// The gateway answered (meaningful with `Route::Wifi`).
    pub ping_ok: bool,
    /// The kernel logged the firmware hang since the last observation.
    pub hang: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Turn the Wi-Fi off and on (NetworkManager), or the link down and up.
    RestartWifi,
    /// Unload and load the driver modules.
    ReloadModules,
    Reboot,
    /// A reboot was due but the limit is reached: only logged, and the
    /// escalation starts over.
    RebootLimited,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Act {
        action: Action,
        why: String,
    },
    /// The gateway answers again after `down_for`; `after` is the last
    /// step taken, if any.
    Recovered {
        down_for: Duration,
        after: Option<Action>,
    },
}

/// The escalation state.
#[derive(Debug)]
pub struct Watch {
    settings: Settings,
    failing_since: Option<Duration>,
    last: Option<(Action, Duration)>,
    hang_at: Option<Duration>,
    seen_iface: bool,
}

/// A hang signature counts for this long (it shows up seconds before the
/// pings start failing).
const HANG_MEMORY: Duration = Duration::from_secs(120);

impl Watch {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            failing_since: None,
            last: None,
            hang_at: None,
            seen_iface: false,
        }
    }

    /// Whether the network is being treated as down.
    pub fn failing(&self) -> bool {
        self.failing_since.is_some()
    }

    /// Take one observation; `recent_reboots` is how many reboots this
    /// watchdog made within `reboot_window` (from its state file).
    pub fn observe(&mut self, obs: &Observation, recent_reboots: usize) -> Option<Event> {
        let at = obs.at;
        if obs.hang {
            self.hang_at = Some(at);
        }
        let down = match &obs.route {
            Route::Wifi { gateway } => {
                self.seen_iface = true;
                gateway.is_some() && !obs.ping_ok
            }
            Route::Missing => {
                self.seen_iface = true;
                true
            }
            // Gone in the middle of a driver reload: still down.
            Route::NoInterface => self.seen_iface && self.failing(),
            Route::Other { .. } => false,
        };
        if !down {
            let since = self.failing_since.take()?;
            let after = self.last.take().map(|(a, _)| a);
            self.hang_at = None;
            return Some(Event::Recovered {
                down_for: at.saturating_sub(since),
                after,
            });
        }
        let since = *self.failing_since.get_or_insert(at);
        if at < self.settings.grace {
            return None;
        }
        let hang = self
            .hang_at
            .is_some_and(|t| at.saturating_sub(t) < HANG_MEMORY);
        let silent = at.saturating_sub(since);
        let waited = |t: Duration| at.saturating_sub(t) >= self.settings.step_wait;
        let (action, why) = match self.last {
            None if hang => (
                Action::RestartWifi,
                "the kernel logged the Wi-Fi firmware hang".into(),
            ),
            None if silent >= self.settings.fail_after => (
                Action::RestartWifi,
                format!("no answer from the gateway for {} s", silent.as_secs()),
            ),
            None => return None,
            Some((Action::RestartWifi, t)) if waited(t) && self.settings.reload_modules => (
                Action::ReloadModules,
                format!(
                    "still no network {} s after restarting the Wi-Fi",
                    self.settings.step_wait.as_secs()
                ),
            ),
            Some((Action::RestartWifi | Action::ReloadModules, t)) if waited(t) => (
                Action::Reboot,
                format!("still no network after {} s down", silent.as_secs()),
            ),
            _ => return None,
        };
        let action = if action == Action::Reboot && recent_reboots >= self.settings.max_reboots {
            Action::RebootLimited
        } else {
            action
        };
        self.last = Some((action, at));
        if action == Action::RebootLimited {
            // Start over: restart the Wi-Fi again after fail_after.
            self.last = None;
            self.failing_since = Some(at);
            self.hang_at = None;
        }
        Some(Event::Act { action, why })
    }
}

/// Unix seconds of `t` (negative before 1970, which a board without a
/// real-time clock never shows, but keep the arithmetic total).
pub fn unix(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// The reboot times of the state file (one Unix time per line) within
/// `window` of `now`. A time in the future counts too: the clock of a
/// board without a battery starts in the past until NTP sets it, and a
/// reboot loop must not escape the limit that way.
pub fn recent_reboots(state: &str, now: i64, window: Duration) -> Vec<i64> {
    state
        .lines()
        .filter_map(|l| l.trim().parse::<i64>().ok())
        .filter(|t| now - t < window.as_secs() as i64)
        .collect()
}

/// Watches the wall clock against the monotonic one, to log when NTP
/// moves it: a board without a battery starts with a wrong date, and the
/// log lines before the sync carry it.
#[derive(Debug)]
pub struct ClockJump {
    offset: i64,
}

/// Smaller differences are drift or scheduling, not a jump.
const JUMP: i64 = 30;

impl ClockJump {
    /// `wall` and `mono` (any monotonic seconds) taken at the same moment.
    pub fn new(wall: i64, mono: i64) -> Self {
        Self {
            offset: wall - mono,
        }
    }

    /// The jump in seconds since the last call, if the wall clock moved
    /// more than [`JUMP`] against the monotonic one.
    pub fn check(&mut self, wall: i64, mono: i64) -> Option<i64> {
        let offset = wall - mono;
        let jump = offset - self.offset;
        self.offset = offset;
        (jump.abs() > JUMP).then_some(jump)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn wifi(at: u64, ping_ok: bool) -> Observation {
        Observation {
            at: s(at),
            route: Route::Wifi {
                gateway: Some("192.168.1.1".into()),
            },
            ping_ok,
            hang: false,
        }
    }

    /// Run observations every 10 s from `from` to `to` with the gateway
    /// silent; the actions taken, with their times.
    fn run_down(w: &mut Watch, from: u64, to: u64, reboots: usize) -> Vec<(u64, Action)> {
        (from..=to)
            .step_by(10)
            .filter_map(|t| match w.observe(&wifi(t, false), reboots) {
                Some(Event::Act { action, .. }) => Some((t, action)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_the_default_route() {
        let wlan = "default via 192.168.1.1 dev wlan0 proto dhcp src 192.168.1.28 metric 600\n";
        assert_eq!(
            parse_route(wlan, "wlan0"),
            Route::Wifi {
                gateway: Some("192.168.1.1".into())
            }
        );
        let both = "default via 192.168.1.1 dev end0 proto dhcp metric 100\n\
                    default via 192.168.1.1 dev wlan0 proto dhcp metric 600\n";
        assert_eq!(
            parse_route(both, "wlan0"),
            Route::Other { dev: "end0".into() }
        );
        assert_eq!(parse_route("", "wlan0"), Route::Missing);
        assert_eq!(
            parse_route("default dev wlan0 scope link\n", "wlan0"),
            Route::Wifi { gateway: None }
        );
    }

    #[test]
    fn recognises_the_kernel_lines() {
        assert!(is_hang("6,1234,5678,-;sprdwl:[WIFI_CMD_TX_DATA]timeout"));
        assert!(is_hang(
            "4,99,1,-;NETDEV WATCHDOG: wlan0 (unisoc_wifi): transmit queue 0 timed out 5000 ms"
        ));
        assert!(!is_hang("6,1,1,-;wlan0: associated"));
    }

    #[test]
    fn escalates_restart_reload_reboot() {
        let mut w = Watch::new(Settings::default());
        assert_eq!(w.observe(&wifi(130, true), 0), None);
        let acts = run_down(&mut w, 140, 300, 0);
        assert_eq!(
            acts,
            vec![
                (200, Action::RestartWifi),
                (230, Action::ReloadModules),
                (260, Action::Reboot)
            ]
        );
    }

    #[test]
    fn skips_the_module_step_when_not_configured() {
        let mut w = Watch::new(Settings {
            reload_modules: false,
            ..Default::default()
        });
        let acts = run_down(&mut w, 140, 300, 0);
        assert_eq!(
            acts,
            vec![(200, Action::RestartWifi), (230, Action::Reboot)]
        );
    }

    #[test]
    fn waits_out_the_grace_period() {
        let mut w = Watch::new(Settings::default());
        // Down from the start: nothing until 120 s, then at once (the
        // gateway has been silent for more than 60 s by then).
        let acts = run_down(&mut w, 0, 130, 0);
        assert_eq!(acts, vec![(120, Action::RestartWifi)]);
    }

    #[test]
    fn the_hang_signature_acts_at_once_once_pings_fail() {
        let mut w = Watch::new(Settings::default());
        let mut o = wifi(200, true);
        o.hang = true;
        // Logged while the gateway still answers: nothing yet.
        assert_eq!(w.observe(&o, 0), None);
        match w.observe(&wifi(210, false), 0) {
            Some(Event::Act {
                action: Action::RestartWifi,
                why,
            }) => assert!(why.contains("firmware hang"), "{why}"),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn recovery_resets_the_escalation() {
        let mut w = Watch::new(Settings::default());
        run_down(&mut w, 140, 200, 0);
        assert_eq!(
            w.observe(&wifi(210, true), 0),
            Some(Event::Recovered {
                down_for: s(70),
                after: Some(Action::RestartWifi)
            })
        );
        assert!(!w.failing());
        // A new outage starts from the first step again.
        let acts = run_down(&mut w, 300, 360, 0);
        assert_eq!(acts, vec![(360, Action::RestartWifi)]);
    }

    #[test]
    fn the_reboot_limit_only_logs_and_starts_over() {
        let mut w = Watch::new(Settings::default());
        let acts = run_down(&mut w, 140, 330, 3);
        assert_eq!(
            acts,
            vec![
                (200, Action::RestartWifi),
                (230, Action::ReloadModules),
                (260, Action::RebootLimited),
                (320, Action::RestartWifi)
            ]
        );
    }

    #[test]
    fn ethernet_with_a_route_is_left_alone() {
        let mut w = Watch::new(Settings::default());
        for t in (140..400).step_by(10) {
            let o = Observation {
                at: s(t),
                route: Route::Other { dev: "end0".into() },
                ping_ok: false,
                hang: false,
            };
            assert_eq!(w.observe(&o, 0), None);
        }
    }

    #[test]
    fn no_route_counts_as_down_and_no_interface_only_mid_outage() {
        let mut w = Watch::new(Settings::default());
        let at = |t, route| Observation {
            at: s(t),
            route,
            ping_ok: false,
            hang: false,
        };
        // A board without wlan0: nothing ever.
        assert_eq!(w.observe(&at(500, Route::NoInterface), 0), None);
        assert!(!w.failing());
        assert_eq!(w.observe(&at(510, Route::Missing), 0), None);
        assert!(w.failing());
        // wlan0 gone while the driver reloads: still down.
        assert_eq!(w.observe(&at(520, Route::NoInterface), 0), None);
        assert!(w.failing());
        assert!(matches!(
            w.observe(&at(570, Route::Missing), 0),
            Some(Event::Act {
                action: Action::RestartWifi,
                ..
            })
        ));
    }

    #[test]
    fn counts_recent_reboots_including_future_ones() {
        let state = "1000\n4000\n\ngarbage\n9000\n";
        let w = s(3600);
        // At 5000: 4000 (1000 s ago) and 9000 (in the future) count.
        assert_eq!(recent_reboots(state, 5000, w), vec![4000, 9000]);
        assert_eq!(recent_reboots(state, 20000, w), Vec::<i64>::new());
    }

    #[test]
    fn notices_a_clock_jump() {
        let mut c = ClockJump::new(1_000, 10);
        assert_eq!(c.check(1_010, 20), None);
        assert_eq!(c.check(1_025, 30), None, "5 s of drift");
        // NTP sets the real date: +1 day.
        assert_eq!(c.check(87_440, 40), Some(86_405));
        assert_eq!(c.check(87_450, 50), None);
    }
}
