//! Clean power-off of the hub's board when a widget asks (`Shutdown`,
//! ADR 0025): the board is often powered by a PC's USB port, and pulling
//! the power under a running system has already damaged an SD card.
//!
//! The router has already released every key and told every node
//! (`ShuttingDown`). Here: give those messages time to leave, stop the USB
//! gadget (the PC on the cable sees the receiver go away cleanly), flush
//! the file systems and ask systemd to power off. systemd then stops this
//! service, which lets go of the receiver. If `systemctl` fails the hub
//! stays up and says why.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, error, info, warn};

use crate::router::Router;

/// Time for `ShuttingDown` to reach the nodes before the gadget goes.
const FLUSH: Duration = Duration::from_millis(500);
/// Longest wait for the gadget to stop (its unit's timeout is longer).
const GADGET_STOP: Duration = Duration::from_secs(3);
const GADGET_UNIT: &str = "rawkvm-agent-gadget.service";

/// Serve the power-off requests from `rx` (see [`Router::with_poweroff`]).
pub async fn serve(router: Arc<Router>, mut rx: UnboundedReceiver<String>) {
    while let Some(reason) = rx.recv().await {
        tokio::time::sleep(FLUSH).await;
        let stop = tokio::task::spawn_blocking(|| systemctl(&["stop", GADGET_UNIT]));
        match tokio::time::timeout(GADGET_STOP, stop).await {
            Ok(Ok(Ok(()))) => info!("USB gadget stopped"),
            // No gadget on this board: nothing to stop.
            Ok(Ok(Err(e))) => debug!("{e}"),
            Ok(Err(e)) => warn!("{e}"),
            Err(_) => warn!(
                "the USB gadget did not stop in {} s; powering off anyway",
                GADGET_STOP.as_secs()
            ),
        }
        sync();
        warn!(%reason, "powering off");
        match tokio::task::spawn_blocking(|| systemctl(&["poweroff"])).await {
            // systemd stops this service next.
            Ok(Ok(())) => return,
            Ok(Err(e)) => error!("{e}; the hub stays up"),
            Err(e) => error!("{e}; the hub stays up"),
        }
        router.poweroff_failed();
    }
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl {}: {e}", args.join(" ")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "systemctl {}: {} {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn sync() {
    #[cfg(target_os = "linux")]
    // SAFETY: sync(2) takes no arguments and cannot fail.
    unsafe {
        libc::sync()
    };
}
