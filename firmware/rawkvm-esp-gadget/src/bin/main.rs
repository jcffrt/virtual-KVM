//! RawKVM USB gadget on an ESP32-S3.
//!
//! The board joins the WiFi, connects to the hub over the light link
//! (`crates/light`, hub.rs) and presents the hub's receiver interfaces on
//! the PC's USB port (usb.rs), with the receiver's own descriptors. The
//! interface set comes from the hub and is kept across resets (store.rs).
//!
//! Service protocol (TCP port 7790, one client at a time), for the test
//! client `firmware/tools/gadget_test.py`. Each frame is
//! `[kind u8][len u8][payload; len]`:
//! - client -> board: 3 ping (echoed back), 0xF1/0xF2/0xF3 firmware update
//!   (begin with the u32 LE size, data, end).
//! - board -> client: 5 text (log lines).
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{Runner, Stack, StackResources, tcp::TcpSocket};
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as _;
use esp_hal::{
    clock::CpuClock,
    rng::Rng,
    rtc_cntl::{Rtc, RwdtStage, RwdtStageAction},
    system::{AppCoreGuard, CpuControl},
    timer::timg::TimerGroup,
};
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, Interface, WifiController, sta::StationConfig,
};
use log::{info, warn};
use static_cell::StaticCell;

extern crate alloc;

#[path = "../netlog.rs"]
mod netlog;
#[path = "../ota.rs"]
mod ota;
#[path = "../diag.rs"]
mod diag;
#[path = "../hub.rs"]
mod hub;
#[path = "../store.rs"]
mod store;
#[path = "../usb.rs"]
mod usb;

esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const PORT: u16 = 7790;

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    // The logger allocates its lines: after the heap.
    netlog::init();
    diag::report_previous_run();
    let update_mode = diag::take_update_mode();
    // Before core 1 starts: this may write the flash.
    let gadget: Option<&'static light::Gadget> = if update_mode {
        None
    } else {
        store::load().map(|g| &*alloc::boxed::Box::leak(alloc::boxed::Box::new(g)))
    };

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // ---- USB device on the second core ---------------------------------------
    // With the WiFi driver on the same core the PC never configured the
    // device and no IN report went out. Core 1 runs only the USB, in a busy
    // polling loop without esp-rtos: the WiFi threads stay on core 0, and
    // the firmware update can stop core 1 safely (see ota.rs).
    if update_mode {
        info!("update mode: WiFi only, no USB");
        spawner.spawn(update_mode_timeout().unwrap());
    } else if gadget.is_none() {
        info!("no gadget yet: USB off until the hub sends one");
    } else if let Some(g) = gadget {
        let (usb_fs, dp, dm) = (peripherals.USB_FS, peripherals.GPIO20, peripherals.GPIO19);
        static APP_CORE_STACK: static_cell::ConstStaticCell<esp_hal::system::Stack<16384>> =
            static_cell::ConstStaticCell::new(esp_hal::system::Stack::new());
        let cpu = mk_static!(CpuControl<'static>, CpuControl::new(peripherals.CPU_CTRL));
        let guard = cpu
            .start_app_core(APP_CORE_STACK.take(), move || {
                embassy_futures::block_on(usb::run(usb_fs, dp, dm, g))
            })
            .expect("start core 1");
        // Dropping the guard would stop core 1.
        mk_static!(AppCoreGuard<'static>, guard);
    }

    // Restart when core 0 hangs (fed by the server loop's ticker).
    let mut rtc = Rtc::new(peripherals.RTC_TIMER);
    rtc.rwdt.set_stage_action(RwdtStage::Stage0, RwdtStageAction::ResetCore);
    rtc.rwdt.set_timeout(RwdtStage::Stage0, esp_hal::time::Duration::from_secs(10));
    rtc.rwdt.enable();
    spawner.spawn(watchdog(mk_static!(Rtc<'static>, rtc)).unwrap());

    // ---- WiFi ----------------------------------------------------------------
    // Experiment: the PC never configured the device when the WiFi came up
    // during enumeration. Start the WiFi once configured (6 s at most).
    let configured = gadget.is_some()
        && embassy_time::with_timeout(Duration::from_secs(6), usb::CONFIGURED.wait())
            .await
            .is_ok();
    info!("starting wifi (usb configured: {configured})");
    let controller =
        WifiController::new(peripherals.WIFI, Default::default()).expect("Wi-Fi controller");
    let interface = Interface::station();
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, runner) = embassy_net::new(
        interface,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    );
    spawner.spawn(wifi_task(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());

    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        info!("ip {}", cfg.address);
    }
    if !update_mode {
        spawner.spawn(hub::task(stack, gadget).unwrap());
    }
    server(stack, ota::Ota::new(peripherals.FLASH, update_mode)).await
}

// ---- network tasks -----------------------------------------------------------

/// Leaves the update mode when no update finishes in time.
#[embassy_executor::task]
async fn update_mode_timeout() {
    Timer::after(Duration::from_secs(120)).await;
    warn!("update mode: no update, restarting");
    Timer::after(Duration::from_millis(200)).await;
    esp_hal::system::software_reset();
}

#[embassy_executor::task]
async fn watchdog(rtc: &'static mut Rtc<'static>) -> ! {
    loop {
        rtc.rwdt.feed();
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>) -> ! {
    let config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(SSID.try_into().expect("SSID"))
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                PASSWORD.try_into().expect("password"),
            )),
    );
    controller.set_config(&config).expect("Wi-Fi config");
    // Modem sleep adds tens of ms to every packet: keep the radio awake.
    if let Err(e) = controller.set_power_saving(esp_radio::wifi::PowerSaveMode::None) {
        warn!("wifi power saving: {e:?}");
    }
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                info!("wifi connected to {SSID}");
                let _ = controller.wait_for_disconnect_async().await;
                warn!("wifi disconnected");
            }
            Err(e) => warn!("wifi connect: {e:?}"),
        }
        Timer::after(Duration::from_secs(2)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

/// Service server (ping, log, firmware update): one client at a time.
async fn server(stack: Stack<'static>, mut ota: ota::Ota) -> ! {
    let rx = mk_static!([u8; 1024], [0; 1024]);
    let tx = mk_static!([u8; 1024], [0; 1024]);
    loop {
        let mut sock = TcpSocket::new(stack, &mut rx[..], &mut tx[..]);
        sock.set_timeout(Some(Duration::from_secs(30)));
        if let Err(e) = sock.accept(PORT).await {
            warn!("accept: {e:?}");
            continue;
        }
        info!("client {:?}", sock.remote_endpoint());
        let mut hello = alloc::string::String::new();
        let _ = write!(hello, "rawkvm-esp-gadget {}", env!("CARGO_PKG_VERSION"));
        let _ = send(&mut sock, 5, hello.as_bytes()).await;
        if let Err(e) = session(&mut sock, &mut ota).await {
            warn!("session: {e:?}");
        }
        sock.close();
        let _ = sock.flush().await;
        sock.abort();
    }
}

async fn send(
    sock: &mut TcpSocket<'_>,
    kind: u8,
    payload: &[u8],
) -> Result<(), embassy_net::tcp::Error> {
    sock.write_all(&[kind, payload.len() as u8]).await?;
    sock.write_all(payload).await?;
    sock.flush().await
}

async fn read_exact(
    sock: &mut TcpSocket<'_>,
    buf: &mut [u8],
) -> Result<bool, embassy_net::tcp::Error> {
    let mut n = 0;
    while n < buf.len() {
        let got = sock.read(&mut buf[n..]).await?;
        if got == 0 {
            return Ok(false);
        }
        n += got;
    }
    Ok(true)
}

async fn session(
    sock: &mut TcpSocket<'_>,
    ota: &mut ota::Ota,
) -> Result<(), embassy_net::tcp::Error> {
    let mut head = [0u8; 2];
    let mut body = [0u8; 255];
    let mut log_next = 0u32;
    loop {
        // A frame header or a new log line, whichever comes first.
        // Only the header read races: the body is read to the end.
        while let Some(line) = netlog::next_line(&mut log_next) {
            let n = line.len().min(255);
            send(sock, 5, &line.as_bytes()[..n]).await?;
        }
        match select(sock.read(&mut head[..1]), netlog::NEW_LINE.wait()).await {
            Either::Second(()) => continue,
            Either::First(n) => {
                if n? == 0 {
                    return Ok(());
                }
            }
        }
        if !read_exact(sock, &mut head[1..]).await? {
            return Ok(());
        }
        let (kind, len) = (head[0], head[1] as usize);
        if !read_exact(sock, &mut body[..len]).await? {
            return Ok(());
        }
        let payload = &body[..len];
        match kind {
            3 => send(sock, 3, payload).await?,
            // Firmware update: begin (u32 LE size), data, end.
            0xF1 if len == 4 => {
                let size = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                match ota.begin(size) {
                    Ok(msg) => info!("{msg}"),
                    Err(e) if e == ota::RESTART => {
                        info!("{e}");
                        while let Some(line) = netlog::next_line(&mut log_next) {
                            let n = line.len().min(255);
                            send(sock, 5, &line.as_bytes()[..n]).await?;
                        }
                        Timer::after(Duration::from_millis(200)).await;
                        diag::request_update_mode();
                        esp_hal::system::software_reset();
                    }
                    Err(e) => warn!("{e}"),
                }
            }
            0xF2 => {
                if let Err(e) = ota.data(payload) {
                    warn!("{e}");
                }
            }
            0xF3 => match ota.end() {
                Ok(msg) => {
                    info!("{msg}");
                    // Let the log line reach the client before the restart.
                    while let Some(line) = netlog::next_line(&mut log_next) {
                        let n = line.len().min(255);
                        send(sock, 5, &line.as_bytes()[..n]).await?;
                    }
                    Timer::after(Duration::from_millis(200)).await;
                    esp_hal::system::software_reset();
                }
                Err(e) => warn!("{e}"),
            },
            _ => warn!("bad frame kind {kind} len {len}"),
        }
    }
}
