//! Platform independent part of the RawKVM agents: configuration, hub
//! discovery, the QUIC session and reconnection. Each OS crate provides a
//! [`HidSink`] that owns the virtual HID devices.

pub mod client;
pub mod config;
pub mod discovery;

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use proto::datagram::{Datagram, SeqTracker};
use proto::descriptor::DescriptorInfo;
use proto::{wire, Control, Edge, HidDevice, NodeKind, Os, RejectReason, PROTO_VERSION};
use tracing::{debug, info, warn};

pub use config::AgentConfig;

/// The OS side of an agent: the virtual HID devices.
pub trait HidSink {
    /// Create a virtual device that is an exact copy of `dev`.
    fn create(&mut self, dev: &HidDevice) -> Result<()>;
    /// Deliver one raw input report to device `dev_id`.
    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()>;
    /// Remove every virtual device.
    fn destroy_all(&mut self);
    /// Every device of a `DeviceList` has been passed to `create`. Sinks
    /// that must see the whole set at once (a USB gadget) build it here.
    fn devices_ready(&mut self) {}
    /// This node now receives input; place the pointer at the entry edge.
    fn activate(&mut self, _enter_edge: Option<Edge>, _pos_ratio: Option<f32>) {}
    /// This node no longer receives input.
    fn deactivate(&mut self) {}
}

pub struct AgentOptions {
    pub config_path: PathBuf,
    pub os: Os,
}

/// Why a session ended.
enum End {
    /// Worth retrying (network, hub restart).
    Retry(anyhow::Error),
    /// Retrying cannot help (bad token, changed certificate, version).
    Fatal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for End {
    fn from(e: E) -> Self {
        End::Retry(e.into())
    }
}

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(1);
const STATS_EVERY: Duration = Duration::from_secs(10);

/// Run the agent until a fatal error: connect, serve, reconnect.
pub async fn run(opts: AgentOptions, sink: &mut dyn HidSink) -> Result<()> {
    let mut backoff = BACKOFF_MIN;
    loop {
        let mut cfg = AgentConfig::load_or_init(&opts.config_path)?;
        cfg.token_bytes()?;
        let started = Instant::now();
        let end = match resolve_hub(&cfg).await {
            Ok(addr) => session(&opts, &mut cfg, addr, sink).await,
            Err(e) => End::Retry(e),
        };
        sink.destroy_all();
        match end {
            End::Fatal(e) => return Err(e),
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

async fn resolve_hub(cfg: &AgentConfig) -> Result<SocketAddr> {
    if !cfg.hub.is_empty() {
        let hub = if cfg.hub.contains(':') {
            cfg.hub.clone()
        } else {
            format!("{}:{}", cfg.hub, proto::DEFAULT_PORT)
        };
        let addrs: Vec<SocketAddr> = tokio::task::spawn_blocking(move || {
            hub.to_socket_addrs().map(|a| a.collect::<Vec<_>>())
        })
        .await??;
        return addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or(addrs.first())
            .copied()
            .ok_or_else(|| anyhow!("{} does not resolve", cfg.hub));
    }
    let hubs = discovery::discover(Duration::from_secs(3)).await?;
    let hub = discovery::pick(&hubs, &cfg.hub_fingerprint).ok_or_else(|| {
        if hubs.is_empty() {
            anyhow!("no hub found with mDNS (set `hub` in the config or use --hub)")
        } else {
            anyhow!(
                "{} hubs found, none pinned; choose one with --hub: {:?}",
                hubs.len(),
                hubs.iter().map(|h| (&h.name, &h.addrs)).collect::<Vec<_>>()
            )
        }
    })?;
    info!("found hub {:?} via mDNS", hub.name);
    hub.addrs
        .first()
        .copied()
        .ok_or_else(|| anyhow!("hub {:?} advertised no address", hub.name))
}

/// Per-session receive state.
struct Session {
    devices: Vec<HidDevice>,
    release: HashMap<u8, Vec<Vec<u8>>>,
    seq: SeqTracker,
    rtt_ms: Option<f64>,
    active: bool,
    t0: Instant,
}

impl Session {
    fn new() -> Self {
        Self {
            devices: Vec::new(),
            release: HashMap::new(),
            seq: SeqTracker::new(),
            rtt_ms: None,
            active: false,
            t0: Instant::now(),
        }
    }

    fn set_devices(&mut self, devices: Vec<HidDevice>, sink: &mut dyn HidSink) {
        if devices == self.devices {
            return;
        }
        sink.destroy_all();
        self.release.clear();
        for d in &devices {
            match sink.create(d) {
                Ok(()) => {
                    info!(id = d.id, iface = d.interface, name = %d.name, "virtual device created")
                }
                Err(e) => warn!(id = d.id, "creating virtual device: {e:#}"),
            }
            self.release.insert(
                d.id,
                DescriptorInfo::parse(&d.report_descriptor).release_reports(),
            );
        }
        sink.devices_ready();
        self.devices = devices;
    }

    fn release_all(&self, sink: &mut dyn HidSink) {
        for (id, reports) in &self.release {
            for r in reports {
                let _ = sink.input(*id, r);
            }
        }
    }

    fn on_datagram(&mut self, bytes: &[u8], sink: &mut dyn HidSink) {
        let d = match Datagram::decode(bytes) {
            Ok(d) => d,
            Err(e) => {
                debug!("bad datagram: {e}");
                return;
            }
        };
        if self.seq.observe(d.dev_id, d.seq).is_none() {
            return; // duplicate (redundant resend) or stale
        }
        if !self.release.contains_key(&d.dev_id) {
            return;
        }
        if let Err(e) = sink.input(d.dev_id, d.report) {
            debug!(dev = d.dev_id, "input: {e:#}");
        }
    }

    fn on_pong(&mut self, sent_us: u64) {
        let now = self.t0.elapsed().as_micros() as u64;
        let sample = now.saturating_sub(sent_us) as f64 / 1000.0;
        self.rtt_ms = Some(match self.rtt_ms {
            None => sample,
            Some(avg) => avg * 0.8 + sample * 0.2,
        });
    }
}

async fn session(
    opts: &AgentOptions,
    cfg: &mut AgentConfig,
    addr: SocketAddr,
    sink: &mut dyn HidSink,
) -> End {
    match session_inner(opts, cfg, addr, sink).await {
        Ok(()) => End::Retry(anyhow!("hub closed the session")),
        Err(e) => e,
    }
}

async fn session_inner(
    opts: &AgentOptions,
    cfg: &mut AgentConfig,
    addr: SocketAddr,
    sink: &mut dyn HidSink,
) -> Result<(), End> {
    let (connected, observed) = client::connect(addr, &cfg.hub_fingerprint).await;
    let connected = match connected {
        Ok(c) => c,
        Err(e) => {
            let o = observed.lock().unwrap_or_else(|p| p.into_inner()).clone();
            return Err(if o.mismatch {
                End::Fatal(e.context(
                    "the hub certificate is not the pinned one; if the hub was reinstalled, \
                     remove `hub_fingerprint` from the agent config",
                ))
            } else {
                End::Retry(e)
            });
        }
    };
    let conn = connected.conn;
    let (mut send, mut recv) = conn.open_bi().await?;
    wire::write_frame(
        &mut send,
        &Control::Hello {
            proto_ver: PROTO_VERSION,
            node_id: cfg.node_id,
            node_name: cfg.node_name.clone(),
            os: opts.os,
            kind: NodeKind::Agent,
            token: cfg.token_bytes().map_err(End::Fatal)?,
        },
    )
    .await?;
    let first = tokio::time::timeout(Duration::from_secs(5), wire::read_frame(&mut recv))
        .await
        .context("no reply to Hello")??;
    match first {
        Some(Control::HelloAck {
            hub_name,
            assigned_index,
            ..
        }) => info!(%addr, hub = %hub_name, index = assigned_index, "connected"),
        Some(Control::Reject { reason }) => {
            return Err(End::Fatal(match reason {
                RejectReason::BadToken => anyhow!("the hub rejected the pairing token"),
                RejectReason::VersionMismatch => {
                    anyhow!("protocol version mismatch (agent speaks {PROTO_VERSION})")
                }
                // Another machine may leave; keep trying.
                RejectReason::TooManyNodes => {
                    return Err(End::Retry(anyhow!("the hub has no free agent slot")))
                }
            }));
        }
        Some(other) => return Err(anyhow!("unexpected first message {}", other.name()).into()),
        None => return Err(anyhow!("hub closed the control stream").into()),
    }

    // Pin the certificate only once the hub accepted our token.
    if cfg.hub_fingerprint.is_empty() {
        let fp = observed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .fingerprint
            .clone();
        if let Some(fp) = fp {
            cfg.hub_fingerprint = fp;
            cfg.save(&opts.config_path).map_err(End::Fatal)?;
            info!(fingerprint = %cfg.hub_fingerprint, "hub certificate pinned");
        }
    }

    let mut s = Session::new();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut stats = tokio::time::interval(STATS_EVERY);
    stats.tick().await;
    loop {
        tokio::select! {
            frame = wire::read_frame(&mut recv) => {
                let Some(msg) = frame? else { return Ok(()) };
                match msg {
                    Control::DeviceList { devices } => s.set_devices(devices, sink),
                    Control::Activate { enter_edge, pos_ratio } => {
                        info!(?enter_edge, ?pos_ratio, "active");
                        s.active = true;
                        sink.activate(enter_edge, pos_ratio);
                    }
                    Control::Deactivate => {
                        info!("inactive");
                        s.active = false;
                        s.release_all(sink);
                        sink.deactivate();
                    }
                    Control::Ping(x) => wire::write_frame(&mut send, &Control::Pong(x)).await?,
                    Control::Pong(x) => s.on_pong(x),
                    Control::Status(st) => debug!(active = ?st.active, nodes = st.nodes.len(), "hub status"),
                    Control::Layout { .. } | Control::Screens { .. } => {}
                    Control::Reject { reason } => {
                        return Err(End::Fatal(anyhow!("rejected by the hub: {reason:?}")))
                    }
                    other => debug!(msg = other.name(), "ignored"),
                }
            }
            dg = conn.read_datagram() => s.on_datagram(&dg?, sink),
            _ = ping.tick() => {
                let t = s.t0.elapsed().as_micros() as u64;
                wire::write_frame(&mut send, &Control::Ping(t)).await?;
            }
            _ = stats.tick() => {
                info!(
                    rtt_ms = s.rtt_ms.map(|r| (r * 10.0).round() / 10.0),
                    quic_rtt_ms = conn.rtt().as_secs_f32() * 1000.0,
                    received = s.seq.received,
                    lost = s.seq.lost,
                    dup_or_late = s.seq.reordered,
                    active = s.active,
                    "stats"
                );
            }
        }
    }
}

/// Wait for Ctrl+C or SIGTERM (what launchd and systemd send on stop), so
/// the agent can release every key before exiting.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests;
