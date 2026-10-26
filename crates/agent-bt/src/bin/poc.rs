//! Phase 7 milestone 1: proof of concept of the Bluetooth bridge.
//!
//! Advertises a classic Bluetooth HID keyboard and mouse with a fixed
//! descriptor (BlueZ, `bluer`), lets a PC pair, then:
//! 1. sends mouse reports as fast as the link takes them, to measure the
//!    report rate;
//! 2. taps Caps Lock and times the LED output report the PC sends back
//!    (round trip host <-> device), an even number of times so the state
//!    ends as it began;
//! 3. moves the pointer in a circle so the user sees it.
//!
//! Needs root (PSMs 0x11 and 0x13) and `bluetoothd --noplugin=input`.
//! See docs/STATUS.md (phase 7, milestone 1) for the steps.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("rawkvm-bt-poc needs Linux with BlueZ");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use bluer::l2cap::{Security, SecurityLevel, SeqPacket, SeqPacketListener, Socket, SocketAddr};
    use bluer::{Address, AddressType};
    use clap::Parser;
    use rawkvm_agent_bt::hidp::{self, HostMsg};
    use rawkvm_agent_bt::sdp::{self, HidRecord};
    use rawkvm_agent_bt::{POC_DESCRIPTOR, POC_KEYBOARD_ID, POC_MOUSE_ID};
    use tokio::sync::mpsc;
    use tracing::{debug, info, warn};

    const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RAWKVM_COMMIT"), ")");
    const CAPS_LOCK: u8 = 0x39;
    const LED_CAPS: u8 = 0x02;

    #[derive(Parser, Debug)]
    #[command(version = VERSION, about = "Bluetooth HID proof of concept (phase 7)")]
    struct Args {
        /// Name the PC shows.
        #[arg(long, default_value = "RawKVM PoC")]
        name: String,
        /// Seconds the adapter stays discoverable for pairing.
        #[arg(long, default_value_t = 180)]
        discoverable_secs: u32,
        /// Seconds of the report rate test (0: skip).
        #[arg(long, default_value_t = 10)]
        rate_secs: u64,
        /// Caps Lock round trips to time (rounded up to even; 0: skip).
        #[arg(long, default_value_t = 10)]
        latency_rounds: u32,
        /// Seconds of the visible circle (0: skip).
        #[arg(long, default_value_t = 15)]
        circle_secs: u64,
    }

    pub fn main() -> Result<()> {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .init();
        let args = Args::parse();
        info!(version = VERSION, "rawkvm-bt-poc starting");
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(run(args))
    }

    fn listen(psm: u16) -> Result<SeqPacketListener> {
        let socket = Socket::<SeqPacket>::new_seq_packet()?;
        // Encrypted link: the PC sends keyboard LEDs and gets keystrokes.
        socket.set_security(Security {
            level: SecurityLevel::Medium,
            key_size: 0,
        })?;
        socket
            .bind(SocketAddr::new(Address::any(), AddressType::BrEdr, psm))
            .with_context(|| {
                format!(
                    "binding L2CAP PSM {psm:#x}: run as root, and start bluetoothd with \
                     --noplugin=input (its HID plugin holds the PSM)"
                )
            })?;
        Ok(socket.listen(1)?)
    }

    async fn run(args: Args) -> Result<()> {
        let session = bluer::Session::new().await.context("D-Bus / bluetoothd")?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;
        adapter.set_alias(args.name.clone()).await?;
        let class = adapter.class().await?;
        info!(
            adapter = adapter.name(),
            address = %adapter.address().await?,
            class = format!("{class:#08x}"),
            "Bluetooth adapter"
        );
        if class & 0x1FFC != 0x05C0 {
            warn!(
                "class of device {class:#08x} is not keyboard + pointer (0x0005c0): set \
                 `Class = 0x0005C0` in [General] of /etc/bluetooth/main.conf and restart \
                 bluetooth. Pairing may still work, but the PC may show another icon"
            );
        }

        // No callbacks: BlueZ registers it as NoInputNoOutput, and the
        // kernel accepts Just Works pairing by itself.
        let _agent = session
            .register_agent(bluer::agent::Agent {
                request_default: true,
                ..Default::default()
            })
            .await?;
        let record = HidRecord {
            name: args.name.clone(),
            description: "Keyboard and mouse".into(),
            provider: "RawKVM".into(),
            subclass: sdp::SUBCLASS_COMBO,
            descriptor: POC_DESCRIPTOR.to_vec(),
        };
        let _profile = session
            .register_profile(bluer::rfcomm::Profile {
                uuid: sdp::HID_UUID.parse()?,
                name: Some(args.name.clone()),
                role: Some(bluer::rfcomm::Role::Server),
                require_authentication: Some(false),
                require_authorization: Some(false),
                service_record: Some(record.to_xml()),
                ..Default::default()
            })
            .await
            .context("registering the HID SDP record")?;
        let control = listen(sdp::PSM_CONTROL)?;
        let interrupt = listen(sdp::PSM_INTERRUPT)?;

        adapter.set_pairable(true).await?;
        adapter.set_pairable_timeout(0).await?;
        adapter
            .set_discoverable_timeout(args.discoverable_secs)
            .await?;
        adapter.set_discoverable(true).await?;
        info!(
            "discoverable for {} s as {:?}: pair it from the PC (Windows: Settings > \
             Bluetooth > Add device; macOS: System Settings > Bluetooth)",
            args.discoverable_secs, args.name
        );

        loop {
            let (ctl, peer) = control.accept().await?;
            info!(peer = %peer.addr, "control channel connected");
            let (intr, ipeer) = tokio::time::timeout(Duration::from_secs(10), interrupt.accept())
                .await
                .context("no interrupt channel 10 s after the control channel")??;
            if ipeer.addr != peer.addr {
                warn!(control = %peer.addr, interrupt = %ipeer.addr, "channels from two hosts");
                continue;
            }
            info!(
                peer = %peer.addr,
                send_mtu = intr.send_mtu().unwrap_or(0),
                "interrupt channel connected"
            );
            if let Ok(dev) = adapter.device(peer.addr) {
                let _ = dev.set_trusted(true).await;
            }
            if let Err(e) = serve(&args, ctl, intr).await {
                warn!("{e:#}");
            }
            info!("disconnected; waiting for the PC to connect again");
        }
    }

    /// Last input report per report ID, for GET_REPORT.
    type Current = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

    async fn serve(args: &Args, ctl: SeqPacket, intr: SeqPacket) -> Result<()> {
        let ctl = Arc::new(ctl);
        let intr = Arc::new(intr);
        let current: Current = Arc::new(Mutex::new(vec![
            (
                POC_KEYBOARD_ID,
                vec![POC_KEYBOARD_ID, 0, 0, 0, 0, 0, 0, 0, 0],
            ),
            (POC_MOUSE_ID, vec![POC_MOUSE_ID, 0, 0, 0, 0]),
        ]));
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
        let (gone_tx, mut gone_rx) = mpsc::channel::<String>(2);

        // Control channel: answer the host, collect SET_REPORT outputs.
        {
            let (ctl, current, out_tx, gone_tx) = (
                ctl.clone(),
                current.clone(),
                out_tx.clone(),
                gone_tx.clone(),
            );
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                loop {
                    let n = match ctl.recv(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let Some(msg) = hidp::decode(&buf[..n], true) else {
                        continue;
                    };
                    debug!(?msg, "control");
                    let r = hidp::on_control(&msg, |id| {
                        let cur = current.lock().unwrap_or_else(|e| e.into_inner());
                        id.and_then(|id| cur.iter().find(|(i, _)| *i == id))
                            .map(|(_, r)| r.clone())
                    });
                    if let Some(o) = r.output {
                        let _ = out_tx.send((Instant::now(), o));
                    }
                    if let Some(reply) = r.reply {
                        if ctl.send(&reply).await.is_err() {
                            break;
                        }
                    }
                    if r.unplug {
                        info!("the PC removed the device (virtual cable unplug)");
                        break;
                    }
                    if matches!(msg, HostMsg::Other(_)) {
                        info!(?msg, "unsupported control message");
                    }
                }
                let _ = gone_tx.send("control channel closed".into()).await;
            });
        }
        // Interrupt channel: DATA(Output) reports.
        {
            let (intr, out_tx, gone_tx) = (intr.clone(), out_tx.clone(), gone_tx.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                loop {
                    let n = match intr.recv(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if let Some(o) = hidp::interrupt_output(&buf[..n]) {
                        let _ = out_tx.send((Instant::now(), o));
                    }
                }
                let _ = gone_tx.send("interrupt channel closed".into()).await;
            });
        }

        let send = |report: Vec<u8>| {
            let intr = intr.clone();
            let current = current.clone();
            async move {
                {
                    let mut cur = current.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(slot) = cur.iter_mut().find(|(i, _)| *i == report[0]) {
                        slot.1 = report.clone();
                    }
                }
                intr.send(&hidp::input(&report)).await
            }
        };
        let mouse = |dx: i8, dy: i8| vec![POC_MOUSE_ID, 0, dx as u8, dy as u8, 0];

        // Let the host finish its setup (SET_PROTOCOL, LEDs).
        tokio::time::sleep(Duration::from_secs(2)).await;
        while out_rx.try_recv().is_ok() {}

        let tests = async {
            if args.rate_secs > 0 {
                info!("rate test: mouse reports as fast as the link takes them");
                let start = Instant::now();
                let (mut sent, mut slowest) = (0u64, Duration::ZERO);
                let mut i = 0usize;
                while start.elapsed() < Duration::from_secs(args.rate_secs) {
                    // Back and forth: the pointer ends where it started.
                    let dx = if (i / 50) % 2 == 0 { 2 } else { -2 };
                    let t = Instant::now();
                    send(mouse(dx, 0)).await?;
                    slowest = slowest.max(t.elapsed());
                    sent += 1;
                    i += 1;
                }
                let secs = start.elapsed().as_secs_f64();
                info!(
                    reports = sent,
                    secs = format!("{secs:.1}"),
                    rate_per_s = format!("{:.0}", sent as f64 / secs),
                    slowest_send_ms = format!("{:.1}", slowest.as_secs_f64() * 1000.0),
                    "RESULT rate"
                );
            }

            let rounds = args.latency_rounds.div_ceil(2) * 2;
            let mut rtts = Vec::new();
            let mut leds: Option<u8> = None;
            for _ in 0..rounds {
                while out_rx.try_recv().is_ok() {}
                let t0 = Instant::now();
                send(vec![POC_KEYBOARD_ID, 0, 0, CAPS_LOCK, 0, 0, 0, 0, 0]).await?;
                let deadline = t0 + Duration::from_secs(1);
                let mut got = None;
                while let Ok(Some((t, o))) =
                    tokio::time::timeout_at(deadline.into(), out_rx.recv()).await
                {
                    if o.first() == Some(&POC_KEYBOARD_ID) && o.len() >= 2 {
                        let caps = o[1] & LED_CAPS;
                        if leds.map_or(true, |l| l & LED_CAPS != caps) {
                            leds = Some(o[1]);
                            got = Some(t - t0);
                            break;
                        }
                    }
                }
                send(vec![POC_KEYBOARD_ID, 0, 0, 0, 0, 0, 0, 0, 0]).await?;
                match got {
                    Some(d) => rtts.push(d),
                    None => warn!("no Caps Lock LED report within 1 s"),
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            if rounds > 0 {
                rtts.sort();
                let ms = |d: Duration| format!("{:.1}", d.as_secs_f64() * 1000.0);
                if rtts.is_empty() {
                    warn!(
                        "RESULT latency: the PC sent no LED report (it may not send LEDs to \
                         this keyboard, or Caps Lock is remapped)"
                    );
                } else {
                    info!(
                        answered = rtts.len(),
                        rounds,
                        min_ms = ms(rtts[0]),
                        median_ms = ms(rtts[rtts.len() / 2]),
                        max_ms = ms(rtts[rtts.len() - 1]),
                        "RESULT latency: Caps Lock press -> LED report (round trip)"
                    );
                }
            }

            if args.circle_secs > 0 {
                info!("moving the pointer in a circle for {} s", args.circle_secs);
                let start = Instant::now();
                let mut tick = tokio::time::interval(Duration::from_millis(8));
                while start.elapsed() < Duration::from_secs(args.circle_secs) {
                    tick.tick().await;
                    let a = start.elapsed().as_secs_f64() * 2.0 * std::f64::consts::PI / 2.0;
                    let (dx, dy) = ((a.cos() * 6.0) as i8, (a.sin() * 6.0) as i8);
                    send(mouse(dx, dy)).await?;
                }
            }
            info!("tests done; staying connected (Ctrl+C to stop)");
            std::future::pending::<()>().await;
            Ok::<(), anyhow::Error>(())
        };

        tokio::select! {
            r = tests => r,
            why = gone_rx.recv() => bail!("{}", why.unwrap_or_default()),
            _ = tokio::signal::ctrl_c() => std::process::exit(0),
        }
    }
}
