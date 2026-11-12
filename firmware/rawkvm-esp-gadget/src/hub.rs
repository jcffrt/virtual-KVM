//! Client of the hub's light link (`crates/light`): TCP to the hub, the
//! pairing token as the key, and the board as an ordinary agent.

use alloc::{string::String, vec, vec::Vec};

use embassy_futures::select::{Either3, select3};
use embassy_net::{IpAddress, IpEndpoint, Stack, tcp::TcpSocket};
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write as _;
use light::{Down, Gadget, Up};
use log::{info, warn};

use crate::usb::{self, OUTPUTS, REPORTS, Report};

const HUB_ADDR: &str = env!("HUB_ADDR");
const HUB_TOKEN: &str = env!("HUB_TOKEN");
const NODE_NAME: &str = env!("NODE_NAME");
const NODE_OS: &str = env!("NODE_OS");

/// The hub announces a new device list as each receiver interface opens,
/// about 60 ms apart: act once the gadget has been stable this long.
const SETTLE: Duration = Duration::from_millis(500);
const TICK: Duration = Duration::from_millis(100);
const PING_EVERY: u32 = 10; // ticks
const SILENCE: Duration = Duration::from_secs(5);

type Error = &'static str;

fn token() -> [u8; light::TOKEN_LEN] {
    let mut t = [0u8; light::TOKEN_LEN];
    let hex = HUB_TOKEN.as_bytes();
    let digit = |c: u8| (c as char).to_digit(16).expect("HUB_TOKEN is hex (build.rs)") as u8;
    for (i, b) in t.iter_mut().enumerate() {
        *b = digit(hex[2 * i]) << 4 | digit(hex[2 * i + 1]);
    }
    t
}

fn node_id() -> [u8; 16] {
    let mac = esp_hal::efuse::base_mac_address();
    let mut id = *b"RAWKVM-ESP\0\0\0\0\0\0";
    id[10..].copy_from_slice(mac.as_bytes());
    id
}

fn os() -> light::Os {
    match NODE_OS {
        "macos" => light::Os::MacOs,
        "linux" => light::Os::Linux,
        _ => light::Os::Windows,
    }
}

/// Talks to the hub for ever. `current` is the gadget the USB was built
/// with (`None`: no USB yet, waiting for the first one).
#[embassy_executor::task]
pub async fn task(stack: Stack<'static>, current: Option<&'static Gadget>) -> ! {
    let rx = alloc::boxed::Box::leak(alloc::boxed::Box::new([0u8; 4096]));
    let tx = alloc::boxed::Box::leak(alloc::boxed::Box::new([0u8; 1024]));
    let hub: core::net::Ipv4Addr = HUB_ADDR.parse().expect("HUB_ADDR is an IPv4 address");
    let mut backoff = Duration::from_secs(1);
    loop {
        let mut sock = TcpSocket::new(stack, &mut rx[..], &mut tx[..]);
        sock.set_timeout(Some(Duration::from_secs(10)));
        let started = Instant::now();
        match session(&mut sock, hub, current).await {
            Ok(()) => info!("hub: session ended"),
            Err(e) => warn!("hub: {e}"),
        }
        sock.abort();
        let _ = sock.flush().await;
        release_all(current);
        if started.elapsed() > Duration::from_secs(30) {
            backoff = Duration::from_secs(1);
        }
        Timer::after(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

/// "Nothing pressed" on every interface: the hub is gone.
fn release_all(current: Option<&Gadget>) {
    let Some(g) = current else { return };
    for (slot, itf) in g.interfaces.iter().take(usb::MAX).enumerate() {
        for r in itf.release.iter().filter_map(|r| Report::new(r)) {
            let _ = REPORTS[slot].try_send(r);
        }
    }
}

async fn read_exact(sock: &mut TcpSocket<'_>, buf: &mut [u8]) -> Result<(), Error> {
    let mut n = 0;
    while n < buf.len() {
        match sock.read(&mut buf[n..]).await {
            Ok(0) => return Err("the hub closed the connection"),
            Ok(k) => n += k,
            Err(_) => return Err("read failed"),
        }
    }
    Ok(())
}

async fn send(sock: &mut TcpSocket<'_>, c: &mut light::Cipher, m: &Up) -> Result<(), Error> {
    let f = c.seal(m).map_err(|_| "frame too long")?;
    sock.write_all(&f).await.map_err(|_| "write failed")?;
    sock.flush().await.map_err(|_| "flush failed")
}

async fn session(
    sock: &mut TcpSocket<'_>,
    hub: core::net::Ipv4Addr,
    current: Option<&'static Gadget>,
) -> Result<(), Error> {
    sock.connect(IpEndpoint::new(IpAddress::Ipv4(hub), light::DEFAULT_PORT))
        .await
        .map_err(|_| "cannot connect")?;
    let rng = esp_hal::rng::Rng::new();
    let mut nonce = [0u8; light::NONCE_LEN];
    rng.read(&mut nonce);
    sock.write_all(&light::prelude(&nonce))
        .await
        .map_err(|_| "write failed")?;
    let mut p = [0u8; light::PRELUDE_LEN];
    read_exact(sock, &mut p).await?;
    let hub_nonce = light::parse_prelude(&p).ok_or("not a RawKVM hub")?;
    let (mut tx, mut rx) = light::node_ciphers(&token(), &nonce, &hub_nonce);
    send(
        sock,
        &mut tx,
        &Up::Hello {
            version: light::VERSION,
            node_id: node_id(),
            name: String::from(NODE_NAME),
            os: os(),
            firmware: String::from(env!("CARGO_PKG_VERSION")),
            max_interfaces: usb::MAX as u8,
        },
    )
    .await?;

    let slots: Vec<u8> = current
        .map(|g| g.interfaces.iter().take(usb::MAX).map(|i| i.dev_id).collect())
        .unwrap_or_default();
    let mut pending: Option<(Gadget, Instant)> = None;
    let mut last_rx = Instant::now();
    let mut ticks = 0u32;
    let start = Instant::now();
    let mut len = [0u8; 2];
    let mut welcomed = false;
    loop {
        let tick = Timer::after(TICK);
        match select3(sock.read(&mut len[..1]), OUTPUTS.receive(), tick).await {
            Either3::First(n) => {
                match n {
                    Ok(0) => {
                        return Err(if welcomed {
                            "the hub closed the connection"
                        } else {
                            "the hub refused the board (wrong HUB_TOKEN?)"
                        });
                    }
                    Ok(_) => {}
                    Err(_) => return Err("read failed"),
                }
                read_exact(sock, &mut len[1..]).await?;
                let n = light::frame_len(len).map_err(|_| "bad frame length")?;
                let mut body = vec![0u8; n];
                read_exact(sock, &mut body).await?;
                let msg: Down = rx.open(&body).map_err(|_| "bad frame from the hub")?;
                last_rx = Instant::now();
                match msg {
                    Down::Welcome { hub_name, index } => {
                        welcomed = true;
                        info!("hub: joined {hub_name} as agent {index}");
                    }
                    Down::Reject(r) => {
                        warn!("hub: rejected: {r:?}");
                        return Err("rejected");
                    }
                    Down::Gadget(g) => {
                        if g.interfaces.is_empty() {
                            info!("hub: no devices; keeping the current USB device");
                        } else if current == Some(&g) {
                            pending = None;
                        } else {
                            info!("hub: new gadget with {} interfaces", g.interfaces.len());
                            pending = Some((g, Instant::now()));
                        }
                    }
                    Down::Report { dev_id, report } => {
                        if let Some(slot) = slots.iter().position(|&d| d == dev_id) {
                            match Report::new(&report) {
                                Some(r) => {
                                    if REPORTS[slot].try_send(r).is_err() {
                                        warn!("usb interface {slot}: queue full, report dropped");
                                    }
                                }
                                None => warn!("report of {} bytes dropped", report.len()),
                            }
                        }
                    }
                    Down::Ping(t) => send(sock, &mut tx, &Up::Pong(t)).await?,
                    Down::Pong(_) => {}
                }
            }
            Either3::Second((dev_id, r)) => {
                send(
                    sock,
                    &mut tx,
                    &Up::Output {
                        dev_id,
                        report: r.bytes().to_vec(),
                    },
                )
                .await?;
            }
            Either3::Third(()) => {
                ticks += 1;
                if last_rx.elapsed() > SILENCE {
                    return Err("the hub went silent");
                }
                if ticks % PING_EVERY == 0 {
                    let t = start.elapsed().as_millis() as u32;
                    send(sock, &mut tx, &Up::Ping(t)).await?;
                }
                if let Some((g, since)) = &pending {
                    if since.elapsed() >= SETTLE {
                        if crate::store::stage(g) {
                            info!("hub: restarting to present the new gadget");
                            Timer::after(Duration::from_millis(200)).await;
                            esp_hal::system::software_reset();
                        }
                        warn!("hub: the gadget is too large to keep");
                        pending = None;
                    }
                }
            }
        }
    }
}
