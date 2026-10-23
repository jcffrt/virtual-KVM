//! Edge helper inside the Linux agent (docs/gnome-plan.md).
//!
//! Under Wayland only gnome-shell can read and move the pointer, so the
//! GNOME extension `rawkvm@olitec.cc` exports a minimal D-Bus object
//! (`cc.olitec.RawKvm.Shell`). This module turns it into an
//! [`EdgePlatform`] and runs the shared `agent_core::helper` session as a
//! second hub node, `EdgeHelper { for_node: <this agent> }`, with a config
//! derived from the agent's (no second pairing).
//!
//! The platform calls are synchronous, so a bridge task keeps a cache: the
//! monitors (refreshed on `MonitorsChanged`), the pointer (from the
//! `Pointer` signal, only while the helper samples it) and a queue of
//! warps. The extension samples the pointer only while watched: the bridge
//! renews `Watch(true)` while the helper keeps asking for the pointer (it
//! only does while this machine is active) and sends `Watch(false)` after.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agent_core::edge::Point;
use agent_core::helper::{run_helper, EdgePlatform};
use agent_core::{AgentConfig, AgentOptions};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use proto::{Os, Screen};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};
use uuid::Uuid;

pub const BUS_NAME: &str = "cc.olitec.RawKvm.Shell";

#[zbus::proxy(
    interface = "cc.olitec.RawKvm.Shell1",
    default_service = "cc.olitec.RawKvm.Shell",
    default_path = "/cc/olitec/RawKvm/Shell"
)]
trait Shell {
    fn monitors(&self) -> zbus::Result<Vec<(i32, i32, i32, i32, f64, bool)>>;
    fn watch(&self, on: bool) -> zbus::Result<()>;
    fn get_pointer(&self) -> zbus::Result<(i32, i32)>;
    fn warp(&self, x: i32, y: i32) -> zbus::Result<()>;
    #[zbus(property)]
    fn version(&self) -> zbus::Result<u32>;
    #[zbus(signal)]
    fn pointer(&self, x: i32, y: i32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn monitors_changed(&self) -> zbus::Result<()>;
}

/// How long after the last `pointer()` call the extension keeps sampling.
const WANT_FOR: Duration = Duration::from_millis(1500);
/// Renewal period of `Watch(true)` (the extension's lease is 3 s).
const RENEW_EVERY: Duration = Duration::from_millis(250);
const RETRY_EVERY: Duration = Duration::from_secs(10);

/// GNOME monitors `(x, y, w, h, scale, primary)` -> screens: primary first,
/// then left to right. GNOME reports logical pixels, the same units as the
/// pointer, so `scale` is 1.0 (protocol v3).
pub fn screens_from(monitors: &[(i32, i32, i32, i32, f64, bool)]) -> Vec<Screen> {
    let mut m: Vec<_> = monitors.to_vec();
    m.sort_by_key(|&(x, y, _, _, _, primary)| (!primary, x, y));
    m.iter()
        .enumerate()
        .map(|(i, &(x, y, w, h, _, _))| Screen {
            node: Uuid::nil(),
            index: i as u8,
            x,
            y,
            w: w.max(1) as u32,
            h: h.max(1) as u32,
            scale: 1.0,
        })
        .collect()
}

/// The helper's config, derived from the agent's: its own stable node id
/// (UUID v5 of the agent's), the same hub, token and pinned certificate,
/// and `for_node` = the agent. `None` while the agent is not paired.
pub fn derive_config(agent: &AgentConfig, existing: Option<AgentConfig>) -> Option<AgentConfig> {
    if agent.token.is_empty() || agent.node_id.is_nil() {
        return None;
    }
    let mut c = existing.unwrap_or_default();
    c.node_id = Uuid::new_v5(&agent.node_id, b"rawkvm-edge");
    c.node_name = format!("{} (edge)", agent.node_name);
    c.hub = agent.hub.clone();
    c.token = agent.token.clone();
    if !agent.hub_fingerprint.is_empty() {
        c.hub_fingerprint = agent.hub_fingerprint.clone();
    }
    c.for_node = Some(agent.node_id);
    Some(c)
}

#[derive(Debug, Default)]
struct Cache {
    screens: Vec<Screen>,
    /// Valid only while the extension samples for us.
    pointer: Option<Point>,
    watching: bool,
    wanted: Option<Instant>,
}

/// [`EdgePlatform`] over the cache the bridge task keeps.
pub struct GnomeScreens {
    cache: Arc<Mutex<Cache>>,
    warps: UnboundedSender<Point>,
}

fn lock(c: &Mutex<Cache>) -> MutexGuard<'_, Cache> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

impl EdgePlatform for GnomeScreens {
    fn screens(&mut self) -> Vec<Screen> {
        lock(&self.cache).screens.clone()
    }
    fn pointer(&mut self) -> Option<Point> {
        let mut c = lock(&self.cache);
        c.wanted = Some(Instant::now());
        if c.watching {
            c.pointer
        } else {
            None
        }
    }
    fn warp(&mut self, to: Point) {
        lock(&self.cache).pointer = Some(to);
        let _ = self.warps.send(to);
    }
}

fn new_platform() -> (GnomeScreens, Arc<Mutex<Cache>>, UnboundedReceiver<Point>) {
    let cache = Arc::new(Mutex::new(Cache::default()));
    let (tx, rx) = unbounded_channel();
    (
        GnomeScreens {
            cache: cache.clone(),
            warps: tx,
        },
        cache,
        rx,
    )
}

/// Keep the cache in sync with the extension until a D-Bus error.
async fn bridge(
    shell: &ShellProxy<'_>,
    cache: &Mutex<Cache>,
    warps: &mut UnboundedReceiver<Point>,
) -> zbus::Result<()> {
    let mut pointer = shell.receive_pointer().await?;
    let mut changed = shell.receive_monitors_changed().await?;
    lock(cache).screens = screens_from(&shell.monitors().await?);
    let mut renew = tokio::time::interval(RENEW_EVERY);
    loop {
        tokio::select! {
            Some(sig) = pointer.next() => {
                let a = sig.args()?;
                let mut c = lock(cache);
                if c.watching {
                    c.pointer = Some(Point { x: a.x, y: a.y });
                }
            }
            Some(_) = changed.next() => {
                let s = screens_from(&shell.monitors().await?);
                info!(screens = s.len(), "GNOME monitors changed");
                lock(cache).screens = s;
            }
            Some(p) = warps.recv() => shell.warp(p.x, p.y).await?,
            _ = renew.tick() => {
                let (wanted, watching) = {
                    let c = lock(cache);
                    (c.wanted.is_some_and(|t| t.elapsed() < WANT_FOR), c.watching)
                };
                if wanted {
                    shell.watch(true).await?;
                    if !watching {
                        // Seed the position: the extension only signals moves.
                        let (x, y) = shell.get_pointer().await?;
                        let mut c = lock(cache);
                        c.pointer = Some(Point { x, y });
                        c.watching = true;
                    }
                } else if watching {
                    shell.watch(false).await?;
                    let mut c = lock(cache);
                    c.watching = false;
                    c.pointer = None;
                }
            }
        }
    }
}

enum Stop {
    /// The extension's bus name has no owner.
    Absent,
    Dbus(zbus::Error),
}

impl From<zbus::Error> for Stop {
    fn from(e: zbus::Error) -> Self {
        Stop::Dbus(e)
    }
}

impl From<zbus::fdo::Error> for Stop {
    fn from(e: zbus::fdo::Error) -> Self {
        Stop::Dbus(e.into())
    }
}

/// Bridge forever: wait for the extension, sync, and start over after any
/// D-Bus error (gnome-shell restarted, extension disabled).
async fn bridge_forever(cache: Arc<Mutex<Cache>>, mut warps: UnboundedReceiver<Point>) {
    let mut reported = false;
    loop {
        let r: Result<(), Stop> = async {
            let conn = zbus::Connection::session().await?;
            let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
            let name =
                zbus::names::BusName::try_from(BUS_NAME).map_err(|e| Stop::Dbus(e.into()))?;
            if !dbus.name_has_owner(name).await? {
                return Err(Stop::Absent);
            }
            let shell = ShellProxy::new(&conn).await?;
            let version = shell.version().await.unwrap_or(0);
            info!(version, "GNOME extension rawkvm@olitec.cc found");
            reported = false;
            Ok(bridge(&shell, &cache, &mut warps).await?)
        }
        .await;
        {
            let mut c = lock(&cache);
            c.watching = false;
            c.pointer = None;
        }
        match r {
            Err(Stop::Absent) if !reported => {
                warn!(
                    "GNOME extension rawkvm@olitec.cc is not running: no edge switching on this \
                     machine (gnome-extensions enable rawkvm@olitec.cc, then log out and in; \
                     docs/gnome-plan.md); retrying every {} s",
                    RETRY_EVERY.as_secs()
                );
                reported = true;
            }
            Err(Stop::Absent) => {}
            Err(Stop::Dbus(e)) => warn!("GNOME bridge: {e}"),
            Ok(()) => {}
        }
        while warps.try_recv().is_ok() {}
        tokio::time::sleep(RETRY_EVERY).await;
    }
}

/// Run the edge helper next to the agent. Returns only on a fatal error of
/// the helper session (the agent keeps running either way).
pub async fn run(agent_config: &Path) -> Result<()> {
    let helper_config: PathBuf = agent_config.with_file_name("edge.toml");
    // Wait for the agent to be paired, then derive the helper's config.
    let cfg = loop {
        let agent = AgentConfig::load_or_init(agent_config)?;
        let existing = std::fs::metadata(&helper_config)
            .is_ok()
            .then(|| AgentConfig::load_or_init(&helper_config))
            .transpose()?;
        if let Some(c) = derive_config(&agent, existing) {
            break c;
        }
        tokio::time::sleep(RETRY_EVERY).await;
    };
    cfg.save(&helper_config)
        .with_context(|| format!("writing {}", helper_config.display()))?;
    info!(node = %cfg.node_id, for_node = ?cfg.for_node, "edge helper configured");

    let (mut platform, cache, warps) = new_platform();
    tokio::spawn(bridge_forever(cache.clone(), warps));
    // Do not join the hub before the extension reported the monitors.
    while lock(&cache).screens.is_empty() {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let opts = AgentOptions {
        config_path: helper_config,
        os: Os::Linux,
    };
    run_helper(opts, &mut platform).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitors_become_screens() {
        // Two monitors, the primary on the right.
        let s = screens_from(&[
            (1920, 0, 2560, 1440, 1.25, true),
            (0, 180, 1920, 1080, 1.0, false),
        ]);
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].index, s[0].x, s[0].w, s[0].scale),
            (0, 1920, 2560, 1.0)
        );
        assert_eq!((s[1].index, s[1].x, s[1].y), (1, 0, 180));
    }

    #[test]
    fn config_is_derived_from_the_agent() {
        let agent = AgentConfig {
            node_id: Uuid::new_v4(),
            node_name: "ubuntu".into(),
            hub: "192.168.1.28".into(),
            token: "ab".repeat(32),
            hub_fingerprint: "cd".repeat(32),
            ..Default::default()
        };
        let c = derive_config(&agent, None).unwrap();
        assert_eq!(c.for_node, Some(agent.node_id));
        assert_eq!(c.node_id, Uuid::new_v5(&agent.node_id, b"rawkvm-edge"));
        assert_ne!(c.node_id, agent.node_id);
        assert_eq!(
            (c.hub.as_str(), c.node_name.as_str()),
            ("192.168.1.28", "ubuntu (edge)")
        );
        // Stable across restarts; keeps its own pin when the agent has none.
        let mut unpinned = agent.clone();
        unpinned.hub_fingerprint.clear();
        let again = derive_config(&unpinned, Some(c.clone())).unwrap();
        assert_eq!(
            (again.node_id, again.hub_fingerprint),
            (c.node_id, c.hub_fingerprint)
        );
        // Not paired yet: wait.
        assert!(derive_config(&AgentConfig::default(), None).is_none());
    }

    #[test]
    fn pointer_only_while_watching_and_warp_updates_it() {
        let (mut p, cache, mut warps) = new_platform();
        lock(&cache).pointer = Some(Point { x: 5, y: 5 });
        assert_eq!(p.pointer(), None, "stale until the extension samples");
        assert!(lock(&cache).wanted.is_some());
        lock(&cache).watching = true;
        assert_eq!(p.pointer(), Some(Point { x: 5, y: 5 }));
        p.warp(Point { x: 2, y: 300 });
        assert_eq!(p.pointer(), Some(Point { x: 2, y: 300 }));
        assert_eq!(warps.try_recv().unwrap(), Point { x: 2, y: 300 });
    }
}
