//! Platform independent part of the RawKVM agents: configuration, hub
//! discovery, the QUIC session and reconnection. Each OS crate provides a
//! [`HidSink`] that owns the virtual HID devices.

pub mod canvas;
pub mod client;
pub mod config;
pub mod discovery;
pub mod edge;
pub mod helper;
pub mod pairing;
pub mod setup;
pub mod supervise;
pub mod widget;

use std::collections::{BTreeMap, HashMap};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use proto::datagram::{Datagram, SeqTracker, FLAG_OUTPUT};
use proto::descriptor::DescriptorInfo;
use proto::{wire, Control, Edge, HidDevice, NodeKind, Os, RejectReason, PROTO_VERSION};
use tracing::{debug, info, warn};

pub use config::AgentConfig;

/// An output report the PC sent to virtual device `dev_id` (keyboard
/// LEDs), as its HID stack produced it: report ID first only if the
/// device uses report IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputReport {
    pub dev_id: u8,
    pub report: Vec<u8>,
}

pub type OutputSender = tokio::sync::mpsc::UnboundedSender<OutputReport>;
pub type OutputReceiver = tokio::sync::mpsc::UnboundedReceiver<OutputReport>;

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
    /// How long the device list must stay unchanged before
    /// [`devices_ready`](Self::devices_ready) is called. The hub announces
    /// interfaces one by one as it opens them (tens of ms apart); a sink
    /// that is expensive to rebuild (a USB gadget re-enumerates on the PC)
    /// waits for the list to settle. Zero calls it right away.
    fn settle_time(&self) -> Duration {
        Duration::ZERO
    }
    /// Called about once a second while connected (watchdogs).
    fn tick(&mut self, _now: Instant) {}
    /// This node now receives input; place the pointer at the entry edge.
    fn activate(&mut self, _enter_edge: Option<Edge>, _pos_ratio: Option<f32>) {}
    /// This node no longer receives input. The core has already sent a
    /// release report to every device but those in `keep` (exclusive
    /// receivers, ADR 0030), whose keys must stay held.
    fn deactivate(&mut self, _keep: &[u8]) {}
    /// Output reports of the virtual devices, forwarded to the hub so the
    /// real keyboard's LEDs show this PC's state. Called once; `None` if
    /// the sink has no way to see them.
    fn take_outputs(&mut self) -> Option<OutputReceiver> {
        None
    }
    /// Devices of the last list this sink could not present to its PC
    /// (a USB gadget out of HID functions, ADR 0031), by id. Asked after
    /// [`devices_ready`](Self::devices_ready).
    fn unexposed(&self) -> Vec<u8> {
        Vec::new()
    }
}

/// Delay of the redundant copy of an output report datagram (the hub drops
/// the duplicate by its sequence number).
const OUTPUT_RESEND: Duration = Duration::from_millis(30);

/// Output reports to the hub, across sessions.
struct Outputs {
    rx: Option<OutputReceiver>,
    /// Last report per device and report ID, sent again on every new
    /// session: the hub forgets nothing, but a restarted hub does, and the
    /// PC only sends its LEDs when they change.
    last: BTreeMap<(u8, Option<u8>), Vec<u8>>,
    seq: [u16; 256],
}

impl Outputs {
    fn new(rx: Option<OutputReceiver>) -> Self {
        Self {
            rx,
            last: BTreeMap::new(),
            seq: [0; 256],
        }
    }

    /// Next report from the sink; never resolves if it has none (left).
    async fn recv(&mut self) -> OutputReport {
        if let Some(rx) = &mut self.rx {
            if let Some(o) = rx.recv().await {
                return o;
            }
            self.rx = None;
        }
        std::future::pending().await
    }

    /// Remember `o` (keyed with the device's report ID use) and send it.
    fn on_report(&mut self, o: OutputReport, devices: &[HidDevice], conn: &quinn::Connection) {
        let Some(dev) = devices.iter().find(|d| d.id == o.dev_id) else {
            return;
        };
        let rid = dev
            .uses_report_ids
            .then(|| o.report.first().copied())
            .flatten();
        debug!(dev = o.dev_id, report = ?o.report, "output report");
        self.send(o.dev_id, &o.report, conn);
        self.last.insert((o.dev_id, rid), o.report);
    }

    /// Send the remembered reports of the devices in `devices`.
    fn resend_all(&mut self, devices: &[HidDevice], conn: &quinn::Connection) {
        let last: Vec<_> = self
            .last
            .iter()
            .filter(|((id, _), _)| devices.iter().any(|d| d.id == *id))
            .map(|((id, _), r)| (*id, r.clone()))
            .collect();
        for (id, r) in last {
            self.send(id, &r, conn);
        }
    }

    /// One datagram now and its redundant copy shortly after.
    fn send(&mut self, dev_id: u8, report: &[u8], conn: &quinn::Connection) {
        let seq = self.seq[dev_id as usize];
        self.seq[dev_id as usize] = seq.wrapping_add(1);
        let Ok(b) = (Datagram {
            dev_id,
            flags: FLAG_OUTPUT,
            seq,
            report,
        })
        .encode() else {
            return;
        };
        if let Err(e) = conn.send_datagram(b.clone()) {
            debug!("output report not sent: {e}");
            return;
        }
        let conn = conn.clone();
        tokio::spawn(async move {
            tokio::time::sleep(OUTPUT_RESEND).await;
            let _ = conn.send_datagram(b);
        });
    }
}

pub struct AgentOptions {
    pub config_path: PathBuf,
    pub os: Os,
}

/// Why a session ended.
pub(crate) enum End {
    /// Worth retrying (network, hub restart).
    Retry(anyhow::Error),
    /// Retrying cannot help (bad token, changed certificate, version).
    Fatal(anyhow::Error),
    /// The hub said it is powering off (`ShuttingDown`): retry slowly.
    HubDown(String),
}

impl End {
    pub(crate) fn into_error(self) -> anyhow::Error {
        match self {
            End::Fatal(e) | End::Retry(e) => e,
            End::HubDown(reason) => anyhow!("the hub is powering off ({reason})"),
        }
    }
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

/// How long a service waits after a fatal error before exiting.
const FATAL_PAUSE: Duration = Duration::from_secs(60);

/// Retry pace once the hub said it is powering off: it comes back when its
/// board is powered again, maybe hours later.
pub const HUB_DOWN_RETRY: Duration = Duration::from_secs(30);

/// The reconnect pace: exponential backoff, or [`HUB_DOWN_RETRY`] from a
/// `ShuttingDown` until the hub accepts a connection again.
pub(crate) struct Reconnect {
    backoff: Duration,
    hub_down: bool,
    started: Instant,
}

impl Reconnect {
    pub(crate) fn new() -> Self {
        Self {
            backoff: BACKOFF_MIN,
            hub_down: false,
            started: Instant::now(),
        }
    }

    /// A connection attempt starts.
    pub(crate) fn attempt(&mut self) {
        self.started = Instant::now();
    }

    /// The hub accepted the connection.
    pub(crate) fn connected(&mut self) {
        self.hub_down = false;
    }

    /// Log why the session ended. `Err` for a fatal end.
    pub(crate) fn ended(&mut self, end: End) -> Result<(), anyhow::Error> {
        match end {
            End::Fatal(e) => return Err(e),
            // Expected while the hub is off: no warning every 30 s.
            End::Retry(e) if self.hub_down => debug!("{e:#}"),
            End::Retry(e) => warn!("{e:#}"),
            End::HubDown(reason) => {
                warn!("the hub is powering off ({reason})");
                self.hub_down = true;
            }
        }
        Ok(())
    }

    /// How long to wait before the next attempt (logged).
    pub(crate) fn delay(&mut self) -> Duration {
        if self.hub_down {
            info!(
                "hub off; reconnecting in {:.1} s",
                HUB_DOWN_RETRY.as_secs_f32()
            );
            return HUB_DOWN_RETRY;
        }
        if self.started.elapsed() > BACKOFF_MAX {
            self.backoff = BACKOFF_MIN;
        }
        let d = self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        info!("reconnecting in {:.1} s", d.as_secs_f32());
        d
    }
}

/// Log a fatal error once and, when running as a service (no terminal),
/// wait before exiting: launchd and systemd restart the process at once,
/// and retrying cannot fix a bad token, a changed certificate or a
/// protocol mismatch.
pub(crate) async fn fatal_pause(e: anyhow::Error) -> anyhow::Error {
    use std::io::IsTerminal;
    tracing::error!("{e:#}");
    if !cfg!(test) && !std::io::stderr().is_terminal() {
        tracing::error!(
            "waiting {} s before exiting, so the service is not restarted in a loop",
            FATAL_PAUSE.as_secs()
        );
        tokio::time::sleep(FATAL_PAUSE).await;
    }
    e
}

/// Run the agent until a fatal error: connect, serve, reconnect.
pub async fn run(opts: AgentOptions, sink: &mut dyn HidSink) -> Result<()> {
    let mut retry = Reconnect::new();
    let mut outputs = Outputs::new(sink.take_outputs());
    loop {
        let mut cfg = AgentConfig::load_or_init(&opts.config_path)?;
        cfg.token_bytes()?;
        retry.attempt();
        let end = match resolve_hub(&cfg).await {
            Ok(addr) => session(&opts, &mut cfg, addr, sink, &mut outputs, &mut retry).await,
            Err(e) => End::Retry(e),
        };
        sink.destroy_all();
        if let Err(e) = retry.ended(end) {
            return Err(fatal_pause(e).await);
        }
        tokio::time::sleep(retry.delay()).await;
    }
}

pub(crate) async fn resolve_hub(cfg: &AgentConfig) -> Result<SocketAddr> {
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
    /// When to call `devices_ready` for a list still settling.
    ready_at: Option<Instant>,
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
            ready_at: None,
        }
    }

    fn set_devices(&mut self, devices: Vec<HidDevice>, sink: &mut dyn HidSink, now: Instant) {
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
        self.devices = devices;
        let settle = sink.settle_time();
        if settle.is_zero() {
            sink.devices_ready();
        } else {
            self.ready_at = Some(now + settle);
        }
    }

    /// No longer active: release everything but the devices in `keep`.
    fn deactivate(&mut self, keep: &[u8], sink: &mut dyn HidSink) {
        self.active = false;
        self.release_except(keep, sink);
        sink.deactivate(keep);
    }

    /// Call `devices_ready` if the list has settled by `now`.
    fn poll_ready(&mut self, sink: &mut dyn HidSink, now: Instant) {
        if self.ready_at.is_some_and(|t| now >= t) {
            self.ready_at = None;
            sink.devices_ready();
        }
    }

    fn release_all(&self, sink: &mut dyn HidSink) {
        self.release_except(&[], sink);
    }

    /// Release every device but those in `keep`.
    fn release_except(&self, keep: &[u8], sink: &mut dyn HidSink) {
        for (id, reports) in self.release.iter().filter(|(id, _)| !keep.contains(id)) {
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
    outputs: &mut Outputs,
    retry: &mut Reconnect,
) -> End {
    match session_inner(opts, cfg, addr, sink, outputs, retry).await {
        Ok(()) => End::Retry(anyhow!("hub closed the session")),
        Err(e) => e,
    }
}

/// An accepted session: the connection and its control stream.
pub(crate) struct Accepted {
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

/// Connect, present `kind` in `Hello`, and pin the certificate once the hub
/// accepted us.
pub(crate) async fn handshake(
    opts: &AgentOptions,
    cfg: &mut AgentConfig,
    addr: SocketAddr,
    kind: NodeKind,
) -> Result<Accepted, End> {
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
            kind,
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
                    anyhow!(
                        "the hub speaks another protocol version (this agent speaks {PROTO_VERSION}); \
                         update the hub and every agent to the same release"
                    )
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
    Ok(Accepted { conn, send, recv })
}

async fn session_inner(
    opts: &AgentOptions,
    cfg: &mut AgentConfig,
    addr: SocketAddr,
    sink: &mut dyn HidSink,
    outputs: &mut Outputs,
    retry: &mut Reconnect,
) -> Result<(), End> {
    let Accepted {
        conn,
        mut send,
        mut recv,
    } = handshake(opts, cfg, addr, NodeKind::Agent).await?;
    retry.connected();
    let mut s = Session::new();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut stats = tokio::time::interval(STATS_EVERY);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    stats.tick().await;
    loop {
        tokio::select! {
            frame = wire::read_frame(&mut recv) => {
                let Some(msg) = frame? else { return Ok(()) };
                match msg {
                    Control::DeviceList { devices } => {
                        let first = s.devices.is_empty();
                        s.set_devices(devices, sink, Instant::now());
                        if first {
                            outputs.resend_all(&s.devices, &conn);
                        }
                    }
                    Control::Activate { entry } => {
                        let (enter_edge, pos_ratio) = (entry.map(|e| e.edge), entry.map(|e| e.pos_ratio));
                        info!("active");
                        s.active = true;
                        sink.activate(enter_edge, pos_ratio);
                    }
                    Control::Deactivate { keep } => {
                        info!(?keep, "inactive");
                        s.deactivate(&keep, sink);
                    }
                    Control::Ping(x) => wire::write_frame(&mut send, &Control::Pong(x)).await?,
                    Control::Pong(x) => s.on_pong(x),
                    Control::Status(st) => debug!(active = ?st.active, nodes = st.nodes.len(), "hub status"),
                    Control::Layout { .. } | Control::Screens { .. } => {}
                    Control::Reject { reason } => {
                        return Err(End::Fatal(anyhow!("rejected by the hub: {reason:?}")))
                    }
                    Control::ShuttingDown { reason } => {
                        s.release_all(sink);
                        return Err(End::HubDown(reason));
                    }
                    other => debug!(msg = other.name(), "ignored"),
                }
            }
            dg = conn.read_datagram() => s.on_datagram(&dg?, sink),
            o = outputs.recv() => outputs.on_report(o, &s.devices, &conn),
            _ = sleep_until(s.ready_at) => s.poll_ready(sink, Instant::now()),
            _ = ping.tick() => {
                let t = s.t0.elapsed().as_micros() as u64;
                wire::write_frame(&mut send, &Control::Ping(t)).await?;
            }
            _ = tick.tick() => sink.tick(Instant::now()),
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

/// Sleep until `t`, or forever if there is nothing to wait for.
async fn sleep_until(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => std::future::pending().await,
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
