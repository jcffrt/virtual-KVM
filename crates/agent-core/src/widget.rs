//! Widget session: connects as `NodeKind::Widget`, keeps a [`WidgetState`]
//! with everything the hub tells widgets (status, live screens of every
//! node, the layout in force, the receiver's devices, RTT) and sends the
//! widget's commands (`Layout`, `SwitchTo`). The Tauri app wraps this; no
//! protocol logic lives in its TypeScript.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use proto::{
    wire, Control, EdgeLink, EdgeOptions, HidDevice, HubSettings, HubStatus, NodeKind, Placement,
    Screen,
};
use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{handshake, resolve_hub, Accepted, AgentConfig, AgentOptions, End};

/// What the widget shows. Kept across reconnections (marked by
/// `connected`), so the UI does not go blank while the hub restarts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WidgetState {
    /// Hub name and address while connected.
    pub connected: Option<(String, String)>,
    /// Why the last session ended, or why the widget gave up.
    pub error: Option<String>,
    /// Retrying cannot help (token, certificate, version): fix the config.
    pub fatal: bool,
    pub status: Option<HubStatus>,
    /// Live screens of every node, as reported by agents and edge helpers.
    pub screens: BTreeMap<Uuid, Vec<Screen>>,
    /// The layout in force (both directions).
    pub edges: Vec<EdgeLink>,
    pub options: EdgeOptions,
    /// The canvas saved with the layout; empty while the hub uses its
    /// config seed (the widget then proposes `canvas::initial`).
    pub canvas: Vec<Placement>,
    pub settings: Option<HubSettings>,
    pub devices: Vec<HidDevice>,
    pub rtt_ms: Option<f64>,
}

impl WidgetState {
    /// Apply one message from the hub. Returns whether anything changed.
    pub fn apply(&mut self, msg: &Control) -> bool {
        let before = self.clone();
        match msg {
            Control::Status(s) => self.status = Some(s.clone()),
            Control::Screens { screens } => {
                if let Some(node) = screens.first().map(|s| s.node) {
                    self.screens.insert(node, screens.clone());
                }
            }
            Control::Layout {
                screens,
                edges,
                options,
                canvas,
            } => {
                let mut by_node: BTreeMap<Uuid, Vec<Screen>> = BTreeMap::new();
                for s in screens {
                    by_node.entry(s.node).or_default().push(s.clone());
                }
                // The layout carries every live screen: it is the full set.
                if !by_node.is_empty() {
                    self.screens = by_node;
                }
                self.edges = edges.clone();
                self.options = *options;
                self.canvas = canvas.clone();
            }
            Control::Settings(s) => self.settings = Some(*s),
            Control::DeviceList { devices } => self.devices = devices.clone(),
            _ => {}
        }
        *self != before
    }
}

/// Shared state plus a change callback (the Tauri app emits an event).
pub struct WidgetShared {
    pub state: Mutex<WidgetState>,
    pub on_change: Box<dyn Fn(&WidgetState) + Send + Sync>,
}

impl WidgetShared {
    pub fn new(on_change: impl Fn(&WidgetState) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(WidgetState::default()),
            on_change: Box::new(on_change),
        })
    }

    pub fn snapshot(&self) -> WidgetState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn update(&self, f: impl FnOnce(&mut WidgetState) -> bool) {
        let snap = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !f(&mut st) {
                return;
            }
            st.clone()
        };
        (self.on_change)(&snap);
    }
}

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(1);

/// Run the widget session until a fatal error. `commands` are sent to the
/// hub as they come (while disconnected they are dropped, with a warning).
pub async fn run_widget(
    opts: AgentOptions,
    shared: Arc<WidgetShared>,
    mut commands: UnboundedReceiver<Control>,
) -> Result<()> {
    let mut backoff = BACKOFF_MIN;
    loop {
        let mut cfg = AgentConfig::load_or_init(&opts.config_path)?;
        if let Err(e) = cfg.token_bytes() {
            shared.update(|st| {
                st.error = Some(format!("{e:#}"));
                st.fatal = true;
                true
            });
            return Err(e);
        }
        let started = Instant::now();
        let end = match resolve_hub(&cfg).await {
            Ok(addr) => match session(&opts, &mut cfg, addr, &shared, &mut commands).await {
                Ok(()) => End::Retry(anyhow!("hub closed the session")),
                Err(e) => e,
            },
            Err(e) => End::Retry(e),
        };
        let (fatal, e) = match end {
            End::Fatal(e) => (true, e),
            End::Retry(e) => (false, e),
        };
        warn!("{e:#}");
        shared.update(|st| {
            st.connected = None;
            st.rtt_ms = None;
            st.error = Some(format!("{e:#}"));
            st.fatal = fatal;
            true
        });
        if fatal {
            return Err(e);
        }
        if started.elapsed() > BACKOFF_MAX {
            backoff = BACKOFF_MIN;
        }
        // Drop commands queued while disconnected.
        while let Ok(c) = commands.try_recv() {
            warn!(msg = c.name(), "not connected; command dropped");
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn session(
    opts: &AgentOptions,
    cfg: &mut AgentConfig,
    addr: std::net::SocketAddr,
    shared: &WidgetShared,
    commands: &mut UnboundedReceiver<Control>,
) -> Result<(), End> {
    let Accepted {
        conn: _conn,
        mut send,
        mut recv,
    } = handshake(opts, cfg, addr, NodeKind::Widget).await?;
    let hub_name = String::new();
    shared.update(|st| {
        st.connected = Some((hub_name, addr.to_string()));
        st.error = None;
        st.fatal = false;
        true
    });
    info!(%addr, "widget connected");

    let t0 = Instant::now();
    let mut ping = tokio::time::interval(PING_EVERY);
    loop {
        tokio::select! {
            frame = wire::read_frame(&mut recv) => {
                let Some(msg) = frame? else { return Ok(()) };
                match msg {
                    Control::Ping(x) => wire::write_frame(&mut send, &Control::Pong(x)).await?,
                    Control::Pong(sent) => {
                        let ms = (t0.elapsed().as_micros() as u64).saturating_sub(sent) as f64 / 1000.0;
                        shared.update(|st| {
                            let avg = st.rtt_ms.map_or(ms, |a| a * 0.8 + ms * 0.2);
                            st.rtt_ms = Some((avg * 10.0).round() / 10.0);
                            true
                        });
                    }
                    Control::Status(s) => shared.update(|st| {
                        let named = st.connected.as_mut().map(|c| {
                            let changed = c.0 != s.hub_name;
                            c.0 = s.hub_name.clone();
                            changed
                        });
                        st.apply(&Control::Status(s)) | named.unwrap_or(false)
                    }),
                    Control::Reject { reason } => {
                        return Err(End::Fatal(anyhow!("rejected by the hub: {reason:?}")))
                    }
                    other => {
                        debug!(msg = other.name(), "from hub");
                        shared.update(|st| st.apply(&other));
                    }
                }
            }
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                info!(msg = cmd.name(), "to hub");
                wire::write_frame(&mut send, &cmd).await?;
            }
            _ = ping.tick() => {
                wire::write_frame(&mut send, &Control::Ping(t0.elapsed().as_micros() as u64)).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::Edge;

    fn screen(node: Uuid, index: u8) -> Screen {
        Screen {
            node,
            index,
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
            scale: 1.0,
        }
    }

    #[test]
    fn state_follows_the_hub() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut st = WidgetState::default();
        assert!(st.apply(&Control::Screens {
            screens: vec![screen(a, 0), screen(a, 1)]
        }));
        assert!(!st.apply(&Control::Screens {
            screens: vec![screen(a, 0), screen(a, 1)]
        }));
        let link = EdgeLink {
            from_node: a,
            from_screen: 1,
            from_edge: Edge::Right,
            from_range: (0.0, 1.0),
            to_node: b,
            to_screen: 0,
            to_edge: Edge::Left,
            to_range: (0.0, 1.0),
        };
        assert!(st.apply(&Control::Layout {
            screens: vec![screen(a, 0), screen(a, 1), screen(b, 0)],
            edges: vec![link.clone()],
            options: EdgeOptions::default(),
            canvas: vec![],
        }));
        assert_eq!(st.screens.len(), 2);
        assert_eq!(st.screens[&a].len(), 2);
        assert_eq!(st.edges, [link]);
        // An empty-screens layout (going back to no layout) keeps the
        // screens and clears the links.
        assert!(st.apply(&Control::Layout {
            screens: vec![],
            edges: vec![],
            options: EdgeOptions::default(),
            canvas: vec![],
        }));
        assert_eq!(st.screens.len(), 2);
        assert!(st.edges.is_empty());
        assert!(!st.apply(&Control::Ping(1)));
    }
}
