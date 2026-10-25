//! Leaving a trace when a program dies, and bringing it back (ADR 0027).
//!
//! [`log_panics`] sends panics to the log: a program started at logon
//! without a window has no stderr anywhere. [`supervise`] reruns a child
//! process until it ends cleanly, for platforms without a service manager
//! that does it (the Windows edge helper; launchd and systemd restart the
//! others), and logs how each run ended, which also covers the deaths no
//! panic hook sees (access violation, stack overflow, abort).

use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

/// Log every panic (message, location and thread) with `tracing`, then run
/// the default hook. Call it after setting up the log.
pub fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("(no message)");
        let at = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".into());
        let thread = std::thread::current();
        tracing::error!(
            thread = thread.name().unwrap_or("unnamed"),
            "panic at {at}: {msg}"
        );
        default(info);
    }));
}

/// Delays between restarts: `first`, doubling up to `max`, back to
/// `first` after a run of at least `healthy`.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub first: Duration,
    pub max: Duration,
    pub healthy: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            max: Duration::from_secs(60),
            healthy: Duration::from_secs(60),
        }
    }
}

impl Backoff {
    /// The delay before the next start, given the previous delay (`None`
    /// on the first restart) and how long the run that just ended lasted.
    pub fn next(&self, prev: Option<Duration>, ran: Duration) -> Duration {
        match prev {
            Some(d) if ran < self.healthy => (d * 2).min(self.max),
            _ => self.first,
        }
    }
}

/// Start the child with `spawn` and wait for it; start it again, after
/// the [`Backoff`] delay, every time it ends with a failure. Returns when a
/// run ends with success, or with the error when it cannot be started.
pub fn supervise(
    name: &str,
    backoff: Backoff,
    mut spawn: impl FnMut() -> std::io::Result<Child>,
) -> std::io::Result<()> {
    let mut delay = None;
    loop {
        let started = Instant::now();
        let status = spawn()?.wait()?;
        let ran = started.elapsed();
        if status.success() {
            tracing::info!("{name} ended after {}", secs(ran));
            return Ok(());
        }
        let next = backoff.next(delay, ran);
        tracing::error!(
            "{name} died after {} ({}); restarting in {}",
            secs(ran),
            describe(status),
            secs(next)
        );
        std::thread::sleep(next);
        delay = Some(next);
    }
}

fn secs(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f64())
}

/// How a process ended, naming the usual causes of a silent death.
pub fn describe(status: ExitStatus) -> String {
    #[cfg(unix)]
    if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return format!("signal {sig}");
    }
    match status.code() {
        None => "no exit code".into(),
        Some(101) => "exit code 101: Rust panic, see the panic line above".into(),
        Some(c) => {
            // NTSTATUS values come back as negative i32.
            let what = match c as u32 {
                0xC000_0005 => ": access violation",
                0xC000_00FD => ": stack overflow",
                0xC000_0409 => ": fail fast (abort)",
                0xC000_013A => ": Ctrl+C or console closed",
                0xFFFF_FFFF => ": killed (TerminateProcess, as Stop-Process does)",
                _ => "",
            };
            if (c as u32) >= 0x4000_0000 {
                format!("exit code 0x{:08X}{what}", c as u32)
            } else {
                format!("exit code {c}{what}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_max_and_resets_after_a_healthy_run() {
        let b = Backoff::default();
        let short = Duration::from_secs(5);
        let d1 = b.next(None, short);
        assert_eq!(d1, Duration::from_secs(1));
        let d2 = b.next(Some(d1), short);
        assert_eq!(d2, Duration::from_secs(2));
        let mut d = d2;
        for _ in 0..10 {
            d = b.next(Some(d), short);
        }
        assert_eq!(d, Duration::from_secs(60));
        assert_eq!(b.next(Some(d), Duration::from_secs(61)), b.first);
    }

    #[cfg(unix)]
    #[test]
    fn supervise_restarts_until_success() {
        use std::process::Command;
        let fast = Backoff {
            first: Duration::from_millis(1),
            max: Duration::from_millis(4),
            healthy: Duration::from_secs(60),
        };
        let mut runs = 0;
        supervise("test", fast, || {
            runs += 1;
            let code = if runs < 3 { "exit 3" } else { "exit 0" };
            Command::new("sh").args(["-c", code]).spawn()
        })
        .unwrap();
        assert_eq!(runs, 3);
    }

    #[cfg(unix)]
    #[test]
    fn describe_names_signals_and_panics() {
        use std::process::Command;
        let st = |s: &str| Command::new("sh").args(["-c", s]).status().unwrap();
        assert_eq!(describe(st("exit 3")), "exit code 3");
        assert!(describe(st("exit 101")).contains("panic"));
        assert_eq!(describe(st("kill -9 $$")), "signal 9");
    }

    #[test]
    fn supervise_reports_a_program_that_cannot_start() {
        let r = supervise("test", Backoff::default(), || {
            std::process::Command::new("/nonexistent/rawkvm-test").spawn()
        });
        assert!(r.is_err());
    }
}
