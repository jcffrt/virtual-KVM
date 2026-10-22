//! Edge helper session: connects as `NodeKind::EdgeHelper { for_node }`,
//! reports the screens, watches the pointer and asks the hub to switch at
//! linked edges (ADR 0009). The OS part only measures and moves the
//! pointer: see [`EdgePlatform`].

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use proto::{wire, Control, NodeKind, Screen};
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use crate::edge::{EdgeSwitcher, Point};
use crate::{handshake, resolve_hub, Accepted, AgentConfig, AgentOptions, End};

/// What an edge helper needs from the OS. Coordinates are the OS' global
/// desktop coordinates, the same for screens, pointer and warp.
pub trait EdgePlatform {
    /// Current screens (`node` is filled in by the hub). Index 0 should be
    /// the primary one; indices must stay stable while the arrangement does.
    fn screens(&mut self) -> Vec<Screen>;
    fn pointer(&mut self) -> Option<Point>;
    fn warp(&mut self, to: Point);
}

/// How often the pointer is sampled (about 120 Hz).
pub const POLL_EVERY: Duration = Duration::from_millis(8);
/// How often the screens are re-read to notice monitor changes.
pub const SCREENS_EVERY: Duration = Duration::from_secs(2);

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// Run the helper until a fatal error: connect, watch, reconnect.
pub async fn run_helper(opts: AgentOptions, platform: &mut dyn EdgePlatform) -> Result<()> {
    let mut backoff = BACKOFF_MIN;
    loop {
        let mut cfg = AgentConfig::load_or_init(&opts.config_path)?;
        cfg.token_bytes()?;
        let for_node = cfg.for_node.ok_or_else(|| {
            anyhow!(
                "{}: no `for_node` (the node_id of this machine's agent); \
                 set it with --for-node or --from-agent",
                opts.config_path.display()
            )
        })?;
        let started = Instant::now();
        let end = match resolve_hub(&cfg).await {
            Ok(addr) => match session(&opts, &mut cfg, addr, for_node, platform).await {
                Ok(()) => End::Retry(anyhow!("hub closed the session")),
                Err(e) => e,
            },
            Err(e) => End::Retry(e),
        };
        match end {
            End::Fatal(e) => return Err(crate::fatal_pause(e).await),
            End::Retry(e) => warn!("{e:#}"),
        }
        if started.elapsed() > BACKOFF_MAX {
            backoff = BACKOFF_MIN;
        }
        info!("reconnecting in {:.1} s", backoff.as_secs_f32());
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn session(
    opts: &AgentOptions,
    cfg: &mut AgentConfig,
    addr: std::net::SocketAddr,
    for_node: uuid::Uuid,
    platform: &mut dyn EdgePlatform,
) -> Result<(), End> {
    let Accepted {
        conn: _conn,
        mut send,
        mut recv,
    } = handshake(opts, cfg, addr, NodeKind::EdgeHelper { for_node }).await?;
    info!(%for_node, "edge helper connected");

    let mut sw = EdgeSwitcher::new(for_node);
    let mut screens = platform.screens();
    log_screens(&screens);
    sw.set_screens(screens.clone());
    wire::write_frame(
        &mut send,
        &Control::Screens {
            screens: screens.clone(),
        },
    )
    .await?;

    let mut poll = tokio::time::interval(POLL_EVERY);
    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut rescan = tokio::time::interval(SCREENS_EVERY);
    rescan.tick().await;
    loop {
        tokio::select! {
            frame = wire::read_frame(&mut recv) => {
                let Some(msg) = frame? else { return Ok(()) };
                match msg {
                    Control::Layout { edges, options, .. } => {
                        let mine = edges.iter().filter(|l| l.from_node == for_node).count();
                        info!(links = mine, ?options, "layout");
                        sw.set_layout(&edges, options);
                    }
                    Control::Activate { entry } => {
                        let to = sw.activate(
                            entry.map(|e| e.screen),
                            entry.map(|e| e.edge),
                            entry.map(|e| e.pos_ratio),
                        );
                        info!(?entry, ?to, "active");
                        if let Some(p) = to {
                            platform.warp(p);
                        }
                    }
                    Control::Deactivate => {
                        let to = sw.deactivate(platform.pointer());
                        info!(?to, "inactive; pointer parked");
                        if let Some(p) = to {
                            platform.warp(p);
                        }
                    }
                    Control::Ping(x) => wire::write_frame(&mut send, &Control::Pong(x)).await?,
                    Control::Reject { reason } => {
                        return Err(End::Fatal(anyhow!("rejected by the hub: {reason:?}")))
                    }
                    other => debug!(msg = other.name(), "ignored"),
                }
            }
            _ = poll.tick() => {
                if !sw.is_active() {
                    continue;
                }
                let Some(p) = platform.pointer() else { continue };
                if let Some(s) = sw.on_pointer(Instant::now(), p) {
                    info!(?p, target = %s.target, screen = s.screen, edge = ?s.enter_edge, pos = s.pos_ratio, "edge reached");
                    wire::write_frame(&mut send, &Control::SwitchTo {
                        target: s.target,
                        entry: Some(proto::Entry {
                            screen: s.screen,
                            edge: s.enter_edge,
                            pos_ratio: s.pos_ratio,
                        }),
                    }).await?;
                }
            }
            _ = rescan.tick() => {
                let now = platform.screens();
                if now != screens {
                    screens = now;
                    log_screens(&screens);
                    sw.set_screens(screens.clone());
                    wire::write_frame(&mut send, &Control::Screens { screens: screens.clone() }).await?;
                }
            }
        }
    }
}

fn log_screens(screens: &[Screen]) {
    for s in screens {
        info!(
            index = s.index,
            x = s.x,
            y = s.y,
            w = s.w,
            h = s.h,
            scale = s.scale,
            "screen"
        );
    }
    if screens.is_empty() {
        warn!("no screen found");
    }
}
