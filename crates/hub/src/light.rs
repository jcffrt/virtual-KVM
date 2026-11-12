//! The light link (`crates/light`): microcontroller gadgets (the ESP32-S3
//! board) that cannot speak QUIC connect here over TCP. Each one becomes an
//! ordinary `Agent` node for the router; this module translates.
//!
//! - The hub plans the board's USB interfaces from the `DeviceList` with the
//!   gadget agent's rules (ADR 0031, `proto::gadget`) and sends the result
//!   as `Down::Gadget`. Interfaces that do not fit go to the router as
//!   `Unexposed`, like the Linux gadget reports them.
//! - Report datagrams become `Down::Report`. The router sends non-mouse
//!   reports twice (ADR 0004); over TCP the copy is useless, so duplicates
//!   are dropped here by `seq`.
//! - `Up::Output` (keyboard LEDs) goes to `Router::on_output`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use light::{Down, Up};
use proto::datagram::{Datagram, SeqTracker};
use proto::gadget::{spec_for, GadgetSpec, Options};
use proto::{Control, HidDevice, NodeKind, Os, TOKEN_LEN};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::router::{Hello, Link, Router};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(1);
/// Same as the QUIC idle timeout: a board gone is noticed in 5 s.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Frames queued for one board. Full: reports are dropped, like quinn does
/// with stale datagrams.
const QUEUE: usize = 256;

pub async fn serve(addr: SocketAddr, token: [u8; TOKEN_LEN], router: Arc<Router>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding the light link on {addr}"))?;
    info!("light link (ESP32 gadgets) listening on {addr}");
    run(listener, token, router).await
}

async fn run(listener: TcpListener, token: [u8; TOKEN_LEN], router: Arc<Router>) -> Result<()> {
    loop {
        let (stream, remote) = listener.accept().await?;
        let router = router.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, router, token).await {
                info!(%remote, "light link ended: {e:#}");
            }
        });
    }
}

struct LightLink {
    tx: mpsc::Sender<Down>,
    seq: Mutex<SeqTracker>,
    rtt: Mutex<Option<Duration>>,
}

impl Link for LightLink {
    fn send_datagram(&self, data: Bytes) {
        let Ok(d) = Datagram::decode(&data) else {
            return;
        };
        if self
            .seq
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .observe(d.dev_id, d.seq)
            .is_none()
        {
            return; // the redundant copy
        }
        let msg = Down::Report {
            dev_id: d.dev_id,
            report: d.report.to_vec(),
        };
        if self.tx.try_send(msg).is_err() {
            debug!("light link queue full: report dropped");
        }
    }

    fn rtt(&self) -> Option<Duration> {
        *self.rtt.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The board's interfaces for a device list.
pub fn plan(devices: &[HidDevice], max_interfaces: u8, os: Os) -> (light::Gadget, Vec<u8>) {
    let opts = Options {
        // A Linux PC would load hid-logitech-dj for the HID++ interface
        // and ignore the others (ADR 0007).
        expose_hidpp: os != Os::Linux,
        serial: String::new(),
        max_functions: max_interfaces as usize,
    };
    match spec_for(devices, &opts) {
        Some(spec) => (gadget_of(&spec, devices), spec.left_out),
        None => {
            let mut all: Vec<u8> = devices.iter().map(|d| d.id).collect();
            all.sort_unstable();
            (light::Gadget::default(), all)
        }
    }
}

fn gadget_of(spec: &GadgetSpec, devices: &[HidDevice]) -> light::Gadget {
    light::Gadget {
        vid: spec.vendor_id,
        pid: spec.product_id,
        manufacturer: spec.manufacturer.clone(),
        product: spec.product.clone(),
        interfaces: spec
            .functions
            .iter()
            .map(|f| {
                let release = devices
                    .iter()
                    .find(|d| d.id == f.dev_id)
                    .map(|d| {
                        proto::descriptor::DescriptorInfo::parse(&d.report_descriptor)
                            .release_reports()
                    })
                    .unwrap_or_default();
                light::Interface {
                    dev_id: f.dev_id,
                    report_descriptor: f.report_desc.clone(),
                    subclass: f.subclass,
                    protocol: f.protocol,
                    max_input: f.report_length,
                    release,
                }
            })
            .collect(),
    }
}

fn os_of(os: light::Os) -> Os {
    match os {
        light::Os::Windows => Os::Windows,
        light::Os::MacOs => Os::MacOs,
        light::Os::Linux => Os::Linux,
    }
}

async fn read_frame(rd: &mut OwnedReadHalf) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    rd.read_exact(&mut len).await?;
    let n = light::frame_len(len).map_err(|e| anyhow::anyhow!("bad frame length: {e:?}"))?;
    let mut body = vec![0u8; n];
    rd.read_exact(&mut body).await?;
    Ok(body)
}

async fn handle(stream: TcpStream, router: Arc<Router>, token: [u8; TOKEN_LEN]) -> Result<()> {
    let remote = stream.peer_addr()?;
    stream.set_nodelay(true)?;
    let (mut rd, mut wr) = stream.into_split();

    let mut p = [0u8; light::PRELUDE_LEN];
    timeout(HANDSHAKE_TIMEOUT, rd.read_exact(&mut p))
        .await
        .context("no prelude")??;
    let Some(node_nonce) = light::parse_prelude(&p) else {
        bail!("not a light-link client");
    };
    let hub_nonce: [u8; light::NONCE_LEN] = rand::random();
    wr.write_all(&light::prelude(&hub_nonce)).await?;
    let (mut tx_c, mut rx_c) = light::hub_ciphers(&token, &node_nonce, &hub_nonce);

    let first = timeout(HANDSHAKE_TIMEOUT, read_frame(&mut rd))
        .await
        .context("no Hello")??;
    let hello = match rx_c.open::<Up>(&first) {
        Ok(h) => h,
        Err(light::Error::Auth) => {
            warn!(%remote, "light link rejected: wrong pairing token");
            return Ok(());
        }
        Err(e) => bail!("bad Hello: {e:?}"),
    };
    let Up::Hello {
        version,
        node_id,
        name,
        os,
        firmware,
        max_interfaces,
    } = hello
    else {
        bail!("first frame was not Hello");
    };
    if version != light::VERSION {
        warn!(%remote, %name, "light link rejected: the board speaks version {version}, this hub {}", light::VERSION);
        let f = tx_c.seal(&Down::Reject(light::Reject::Version)).ok();
        if let Some(f) = f {
            let _ = wr.write_all(&f).await;
        }
        return Ok(());
    }
    let node_id = Uuid::from_bytes(node_id);
    let os = os_of(os);
    info!(%remote, node = %node_id, %name, %firmware, max_interfaces, "light link: board connected");

    let (down_tx, mut down_rx) = mpsc::channel::<Down>(QUEUE);
    let link = Arc::new(LightLink {
        tx: down_tx.clone(),
        seq: Mutex::new(SeqTracker::new()),
        rtt: Mutex::new(None),
    });
    let (ctl_tx, mut ctl_rx) = mpsc::unbounded_channel::<Control>();
    let reg = match router.register(
        Hello {
            node_id,
            node_name: name,
            os,
            kind: NodeKind::Agent,
        },
        link.clone(),
        ctl_tx,
    ) {
        Ok(r) => r,
        Err(reason) => {
            warn!(node = %node_id, ?reason, "light link: node refused");
            if let Ok(f) = tx_c.seal(&Down::Reject(light::Reject::TooManyNodes)) {
                let _ = wr.write_all(&f).await;
            }
            return Ok(());
        }
    };

    let writer = tokio::spawn(async move {
        while let Some(msg) = down_rx.recv().await {
            let frame = match tx_c.seal(&msg) {
                Ok(f) => f,
                Err(e) => {
                    warn!("light link: frame not sent: {e:?}");
                    continue;
                }
            };
            if let Err(e) = wr.write_all(&frame).await {
                debug!("light link write failed: {e}");
                break;
            }
        }
    });
    // Reads are not cancel-safe: a task of their own, frames by channel.
    let (up_tx, mut up_rx) = mpsc::channel::<Result<Up>>(64);
    let reader = tokio::spawn(async move {
        loop {
            let msg = match read_frame(&mut rd).await {
                Ok(body) => rx_c
                    .open::<Up>(&body)
                    .map_err(|e| anyhow::anyhow!("bad frame: {e:?}")),
                Err(e) => Err(e),
            };
            let end = msg.is_err();
            if up_tx.send(msg).await.is_err() || end {
                break;
            }
        }
    });

    let start = Instant::now();
    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut gadget: Option<light::Gadget> = None;
    let mut left_out: Option<Vec<u8>> = None;
    let mut out_seq = [0u16; 256];
    let result: Result<()> = loop {
        tokio::select! {
            ctl = ctl_rx.recv() => match ctl {
                Some(Control::HelloAck { hub_name, assigned_index, .. }) => {
                    let _ = down_tx.try_send(Down::Welcome { hub_name, index: assigned_index });
                }
                Some(Control::DeviceList { devices }) => {
                    let (g, lo) = plan(&devices, max_interfaces, os);
                    if gadget.as_ref() != Some(&g) {
                        info!(node = %node_id, interfaces = ?g.interfaces.iter().map(|i| i.dev_id).collect::<Vec<_>>(), "light link: gadget");
                        // Never dropped: a board without its gadget is useless.
                        if down_tx.send(Down::Gadget(g.clone())).await.is_err() {
                            break Ok(());
                        }
                        gadget = Some(g);
                    }
                    // First time only if not empty, then on every change (ADR 0031).
                    let changed = match &left_out {
                        None => !lo.is_empty(),
                        Some(prev) => *prev != lo,
                    };
                    if changed {
                        router.on_control(node_id, Control::Unexposed { devices: lo.clone() });
                    }
                    left_out = Some(lo);
                }
                Some(_) => {}
                None => break Ok(()),
            },
            up = up_rx.recv() => {
                last_rx = Instant::now();
                match up {
                    Some(Ok(Up::Ping(t))) => { let _ = down_tx.try_send(Down::Pong(t)); }
                    Some(Ok(Up::Pong(t))) => {
                        let sent = Duration::from_millis(t as u64);
                        if let Some(rtt) = start.elapsed().checked_sub(sent) {
                            *link.rtt.lock().unwrap_or_else(|e| e.into_inner()) = Some(rtt);
                        }
                    }
                    Some(Ok(Up::Output { dev_id, report })) => {
                        let seq = &mut out_seq[dev_id as usize];
                        *seq = seq.wrapping_add(1);
                        router.on_output(node_id, dev_id, *seq, &report);
                    }
                    Some(Ok(Up::Hello { .. })) => debug!(node = %node_id, "second Hello ignored"),
                    Some(Err(e)) => break Err(e),
                    None => break Ok(()),
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > IDLE_TIMEOUT {
                    break Err(anyhow::anyhow!("no frame for {IDLE_TIMEOUT:?}"));
                }
                let _ = down_tx.try_send(Down::Ping(start.elapsed().as_millis() as u32));
            }
        }
    };
    router.unregister(node_id, reg);
    writer.abort();
    reader.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::tests as fx;

    /// A board, as the firmware does it.
    struct Board {
        s: TcpStream,
        tx: light::Cipher,
        rx: light::Cipher,
    }

    impl Board {
        async fn connect(addr: SocketAddr, token: [u8; 32]) -> Board {
            let mut s = TcpStream::connect(addr).await.unwrap();
            let nn = [3u8; 16];
            s.write_all(&light::prelude(&nn)).await.unwrap();
            let mut p = [0u8; light::PRELUDE_LEN];
            s.read_exact(&mut p).await.unwrap();
            let hn = light::parse_prelude(&p).unwrap();
            let (tx, rx) = light::node_ciphers(&token, &nn, &hn);
            let mut b = Board { s, tx, rx };
            b.send(&Up::Hello {
                version: light::VERSION,
                node_id: [0x42; 16],
                name: "GAMER".into(),
                os: light::Os::Windows,
                firmware: "test".into(),
                max_interfaces: 4,
            })
            .await;
            b
        }
        async fn send(&mut self, m: &Up) {
            let f = self.tx.seal(m).unwrap();
            self.s.write_all(&f).await.unwrap();
        }
        /// Next message other than a ping; `None` if the hub closed.
        async fn recv(&mut self) -> Option<Down> {
            loop {
                let mut len = [0u8; 2];
                timeout(Duration::from_secs(3), self.s.read_exact(&mut len))
                    .await
                    .expect("a frame in time")
                    .ok()?;
                let mut body = vec![0u8; light::frame_len(len).unwrap()];
                self.s.read_exact(&mut body).await.unwrap();
                match self.rx.open::<Down>(&body).unwrap() {
                    Down::Ping(_) | Down::Pong(_) => {}
                    m => return Some(m),
                }
            }
        }
    }

    async fn hub(token: [u8; 32]) -> (SocketAddr, Arc<Router>) {
        let router = Arc::new(Router::new(
            Uuid::new_v4(),
            "hub".into(),
            crate::router::Options {
                resend: Some(Duration::from_millis(5)),
                ..Default::default()
            },
        ));
        router.add_device(dev(0, fx::BOOT_KEYBOARD));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(run(listener, token, router.clone()));
        (addr, router)
    }

    #[tokio::test]
    async fn a_board_joins_gets_its_gadget_and_reports_once() {
        let token = [5u8; 32];
        let (addr, router) = hub(token).await;
        let mut b = Board::connect(addr, token).await;
        assert!(matches!(
            b.recv().await,
            Some(Down::Welcome { index: 1, .. })
        ));
        let Some(Down::Gadget(g)) = b.recv().await else {
            panic!("expected the gadget")
        };
        assert_eq!(g.interfaces.len(), 1);
        let id = Uuid::from_bytes([0x42; 16]);
        assert_eq!(router.active(), Some(id));

        // Twice through the router (redundant copy), once to the board.
        let down = [0, 0, 4, 0, 0, 0, 0, 0];
        router.on_report(0, &down);
        let up = [0u8; 8];
        router.on_report(0, &up);
        assert_eq!(
            b.recv().await,
            Some(Down::Report {
                dev_id: 0,
                report: down.to_vec()
            })
        );
        assert_eq!(
            b.recv().await,
            Some(Down::Report {
                dev_id: 0,
                report: up.to_vec()
            })
        );
        // The copies 5 ms later are not forwarded.
        tokio::time::sleep(Duration::from_millis(30)).await;
        b.send(&Up::Ping(1)).await;
        let mut len = [0u8; 2];
        b.s.read_exact(&mut len).await.unwrap();
        let mut body = vec![0u8; light::frame_len(len).unwrap()];
        b.s.read_exact(&mut body).await.unwrap();
        assert!(matches!(
            b.rx.open::<Down>(&body).unwrap(),
            Down::Pong(_) | Down::Ping(_)
        ));

        // LEDs back to the hub: accepted without closing the link.
        b.send(&Up::Output {
            dev_id: 0,
            report: vec![2],
        })
        .await;
        b.send(&Up::Ping(7)).await;
        let mut len = [0u8; 2];
        b.s.read_exact(&mut len).await.unwrap();
    }

    #[tokio::test]
    async fn a_wrong_token_is_dropped_without_a_word() {
        let (addr, router) = hub([5u8; 32]).await;
        let mut b = Board::connect(addr, [6u8; 32]).await;
        assert_eq!(b.recv().await, None);
        assert_eq!(router.active(), None);
    }

    fn dev(id: u8, desc: &[u8]) -> HidDevice {
        HidDevice {
            id,
            name: "USB Receiver".into(),
            vid: 0x046d,
            pid: 0xc52b,
            serial: None,
            interface: id % 16,
            report_descriptor: desc.to_vec(),
            uses_report_ids: proto::descriptor::DescriptorInfo::parse(desc).uses_report_ids,
            receiver: id / 16,
        }
    }

    #[test]
    fn plan_keeps_keyboards_and_mice_and_reports_the_rest() {
        let devices = vec![
            dev(0, fx::BOOT_KEYBOARD),
            dev(1, fx::UNIFYING_IFACE1),
            dev(2, fx::UNIFYING_IFACE2),
            dev(16, fx::BOOT_KEYBOARD),
            dev(17, fx::UNIFYING_IFACE1),
        ];
        let (g, left_out) = plan(&devices, 4, Os::Windows);
        let ids: Vec<u8> = g.interfaces.iter().map(|i| i.dev_id).collect();
        assert_eq!(ids, [0, 1, 16, 17]);
        assert_eq!(left_out, [2]);
        assert_eq!((g.vid, g.pid), (0x046d, 0xc52b));
        let kb = &g.interfaces[0];
        assert_eq!((kb.subclass, kb.protocol, kb.max_input), (1, 1, 8));
        assert_eq!(kb.release, [vec![0u8; 8]]);

        // A Linux PC never gets the HID++ interface, and it does not count
        // as left out.
        let (g, left_out) = plan(&devices[..3], 4, Os::Linux);
        assert_eq!(g.interfaces.len(), 2);
        assert!(left_out.is_empty());

        let (g, left_out) = plan(&[], 4, Os::Windows);
        assert!(g.interfaces.is_empty() && left_out.is_empty());
    }
}
