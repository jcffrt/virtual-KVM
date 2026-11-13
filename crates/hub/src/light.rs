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
//! - Firmware update (version 2): when `{state_dir}/esp-gadget.bin` holds
//!   another version than the board runs, the hub offers it. The board
//!   restarts into its update mode, connects again with no interfaces and
//!   gets the image. The link is authenticated with the pairing token, so
//!   only the hub can update the board.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use light::{Down, Up};
use proto::datagram::{Datagram, SeqTracker};
use proto::gadget::{spec_for, GadgetSpec, Options};
use proto::{Control, HidDevice, NodeKind, Os, TOKEN_LEN};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
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
/// The board firmware the hub hands out, in the state directory.
pub const FIRMWARE_FILE: &str = "esp-gadget.bin";
/// Updates tried per board and image version before the hub gives up.
const MAX_ATTEMPTS: u32 = 3;
/// Time for the board to verify and activate the image.
const UPDATE_RESULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The firmware image for the boards, read at each connection (replacing
/// the file needs no restart).
pub struct Firmware {
    path: PathBuf,
    /// (board, image version) -> updates tried.
    attempts: Mutex<HashMap<(Uuid, String), u32>>,
}

struct Image {
    version: String,
    bytes: Vec<u8>,
}

impl Firmware {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// The image to give a board running `running`, if it needs one.
    fn for_board(&self, node: Uuid, running: &str) -> Option<Image> {
        let bytes = std::fs::read(&self.path).ok()?;
        let Some(version) = light::image_version(&bytes).map(str::to_owned) else {
            warn!(path = %self.path.display(), "light link: not an ESP app image");
            return None;
        };
        if version == running {
            return None;
        }
        let tried = *self
            .attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(node, version.clone()))
            .unwrap_or(&0);
        if tried >= MAX_ATTEMPTS {
            debug!(node = %node, %version, "light link: update given up for this board");
            return None;
        }
        Some(Image { version, bytes })
    }

    fn attempted(&self, node: Uuid, version: &str) {
        *self
            .attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((node, version.to_owned()))
            .or_default() += 1;
    }
}

fn offer(img: &Image) -> Down {
    Down::Update {
        version: img.version.clone(),
        size: img.bytes.len() as u32,
        sha256: Sha256::digest(&img.bytes).into(),
    }
}

pub async fn serve(
    addr: SocketAddr,
    token: [u8; TOKEN_LEN],
    router: Arc<Router>,
    firmware: PathBuf,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding the light link on {addr}"))?;
    info!(firmware = %firmware.display(), "light link (ESP32 gadgets) listening on {addr}");
    run(listener, token, router, Arc::new(Firmware::new(firmware))).await
}

async fn run(
    listener: TcpListener,
    token: [u8; TOKEN_LEN],
    router: Arc<Router>,
    firmware: Arc<Firmware>,
) -> Result<()> {
    loop {
        let (stream, remote) = listener.accept().await?;
        let (router, firmware) = (router.clone(), firmware.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(stream, router, token, &firmware).await {
                info!(%remote, "light link ended: {e:#}");
            }
        });
    }
}

/// A board in its update mode: send the image, wait for the verdict.
async fn push_update(
    rd: &mut OwnedReadHalf,
    wr: &mut OwnedWriteHalf,
    tx_c: &mut light::Cipher,
    rx_c: &mut light::Cipher,
    img: &Image,
) -> Result<()> {
    let seal = |c: &mut light::Cipher, m: &Down| {
        c.seal(m).map_err(|e| anyhow::anyhow!("sealing: {e:?}"))
    };
    wr.write_all(&seal(tx_c, &offer(img))?).await?;
    // Several chunks per write: fewer packets for the board.
    let mut out = Vec::with_capacity(8 * (light::UPDATE_CHUNK + 64));
    for chunks in img.bytes.chunks(8 * light::UPDATE_CHUNK) {
        out.clear();
        for c in chunks.chunks(light::UPDATE_CHUNK) {
            out.extend_from_slice(&seal(tx_c, &Down::UpdateData(c.to_vec()))?);
        }
        wr.write_all(&out).await?;
    }
    wr.write_all(&seal(tx_c, &Down::UpdateEnd)?).await?;
    let deadline = Instant::now() + UPDATE_RESULT_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let body = timeout(left, read_frame(rd))
            .await
            .context("no result from the board")??;
        match rx_c.open::<Up>(&body) {
            Ok(Up::UpdateResult(Ok(()))) => return Ok(()),
            Ok(Up::UpdateResult(Err(e))) => bail!("the board refused the image: {e}"),
            Ok(_) => {}
            Err(e) => bail!("bad frame: {e:?}"),
        }
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

async fn handle(
    stream: TcpStream,
    router: Arc<Router>,
    token: [u8; TOKEN_LEN],
    fw: &Firmware,
) -> Result<()> {
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
    if !(light::MIN_VERSION..=light::VERSION).contains(&version) {
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
    let update = if version >= 2 {
        fw.for_board(node_id, &firmware)
    } else {
        None
    };

    if max_interfaces == 0 {
        // The update mode: no USB, not a node for the router.
        let Some(img) = update else {
            info!(node = %node_id, "light link: board in its update mode, no update for it");
            return Ok(());
        };
        fw.attempted(node_id, &img.version);
        info!(node = %node_id, from = %firmware, to = %img.version, bytes = img.bytes.len(), "light link: updating the board");
        let started = Instant::now();
        push_update(&mut rd, &mut wr, &mut tx_c, &mut rx_c, &img)
            .await
            .with_context(|| format!("update of {name} to {}", img.version))?;
        info!(node = %node_id, version = %img.version, secs = started.elapsed().as_secs_f32(), "light link: board updated, restarting");
        return Ok(());
    }

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

    // Everything queued goes out in one write: with TCP_NODELAY each write
    // is a WiFi packet, and every packet costs the board a buffer.
    let writer = tokio::spawn(async move {
        let mut out = Vec::with_capacity(4096);
        while let Some(msg) = down_rx.recv().await {
            out.clear();
            let mut next = Some(msg);
            while let Some(msg) = next {
                match tx_c.seal(&msg) {
                    Ok(f) => out.extend_from_slice(&f),
                    Err(e) => warn!("light link: frame not sent: {e:?}"),
                }
                next = if out.len() < 1400 { down_rx.try_recv().ok() } else { None };
            }
            if let Err(e) = wr.write_all(&out).await {
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
                    if let Some(img) = &update {
                        info!(node = %node_id, from = %firmware, to = %img.version, "light link: offering a firmware update");
                        let _ = down_tx.try_send(offer(img));
                    }
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
                    Some(Ok(Up::UpdateResult(r))) => debug!(node = %node_id, ?r, "update result outside an update"),
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
            Self::connect_as(addr, token, "test", 4).await
        }
        async fn connect_as(
            addr: SocketAddr,
            token: [u8; 32],
            firmware: &str,
            max_interfaces: u8,
        ) -> Board {
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
                firmware: firmware.into(),
                max_interfaces,
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
        hub_with(token, PathBuf::from("/nonexistent/esp-gadget.bin")).await
    }

    async fn hub_with(token: [u8; 32], firmware: PathBuf) -> (SocketAddr, Arc<Router>) {
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
        tokio::spawn(run(
            listener,
            token,
            router.clone(),
            Arc::new(Firmware::new(firmware)),
        ));
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

    fn image(version: &str, size: usize) -> Vec<u8> {
        let mut img: Vec<u8> = (0..size).map(|i| i as u8).collect();
        img[0] = 0xE9;
        img[32..36].copy_from_slice(&0xABCD_5432u32.to_le_bytes());
        img[48..80].fill(0);
        img[48..48 + version.len()].copy_from_slice(version.as_bytes());
        img
    }

    #[tokio::test]
    async fn a_board_with_another_firmware_is_offered_and_sent_the_image() {
        let token = [5u8; 32];
        let dir = std::env::temp_dir().join(format!("rawkvm-light-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FIRMWARE_FILE);
        let img = image("0.3.0", 5000);
        std::fs::write(&path, &img).unwrap();
        let (addr, _router) = hub_with(token, path).await;

        // The same version: no offer.
        let mut b = Board::connect_as(addr, token, "0.3.0", 4).await;
        assert!(matches!(b.recv().await, Some(Down::Welcome { .. })));
        assert!(matches!(b.recv().await, Some(Down::Gadget(_))));
        drop(b);

        // Another version: offered after the welcome.
        let mut b = Board::connect_as(addr, token, "0.2.2", 4).await;
        assert!(matches!(b.recv().await, Some(Down::Welcome { .. })));
        let Some(Down::Update { version, size, sha256 }) = b.recv().await else {
            panic!("expected the offer")
        };
        assert_eq!((version.as_str(), size), ("0.3.0", 5000));
        assert_eq!(sha256, <[u8; 32]>::from(Sha256::digest(&img)));
        drop(b);

        // In the update mode: the image, then the end.
        let mut b = Board::connect_as(addr, token, "0.2.2", 0).await;
        assert!(matches!(b.recv().await, Some(Down::Update { .. })));
        let mut got = Vec::new();
        loop {
            match b.recv().await {
                Some(Down::UpdateData(d)) => {
                    assert!(d.len() <= light::UPDATE_CHUNK);
                    got.extend_from_slice(&d);
                }
                Some(Down::UpdateEnd) => break,
                m => panic!("unexpected {m:?}"),
            }
        }
        assert_eq!(got, img);
        b.send(&Up::UpdateResult(Ok(()))).await;
        assert_eq!(b.recv().await, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_update_is_given_up_after_three_attempts() {
        let token = [5u8; 32];
        let dir = std::env::temp_dir().join(format!("rawkvm-light-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FIRMWARE_FILE);
        std::fs::write(&path, image("0.3.0", 100)).unwrap();
        let (addr, _router) = hub_with(token, path).await;
        for _ in 0..MAX_ATTEMPTS {
            let mut b = Board::connect_as(addr, token, "0.2.2", 0).await;
            assert!(matches!(b.recv().await, Some(Down::Update { .. })));
            b.send(&Up::UpdateResult(Err("bad sha256".into()))).await;
        }
        let mut b = Board::connect_as(addr, token, "0.2.2", 0).await;
        assert_eq!(b.recv().await, None);
        std::fs::remove_dir_all(&dir).unwrap();
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
