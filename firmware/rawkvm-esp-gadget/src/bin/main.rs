//! RawKVM USB gadget on an ESP32-S3: phase 1 (bring-up).
//!
//! The board joins the WiFi and shows up on the PC's USB port as a keyboard
//! and a mouse. A test client sends reports over TCP; the hub protocol comes
//! in a later phase.
//!
//! Test protocol (TCP port 7790, one client at a time). Each frame is
//! `[kind u8][len u8][payload; len]`:
//! - client -> board: 1 keyboard report (8 bytes, boot layout), 2 mouse report
//!   (7 bytes: buttons, x i16 LE, y i16 LE, wheel i8, pan i8), 3 ping (echoed
//!   back as is), 0xF0 restart into the ROM download mode (to flash again
//!   over USB without touching the BOOT button).
//! - board -> client: 4 keyboard LEDs (1 byte), 5 text (status lines).
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, select3};
use embassy_net::{Runner, Stack, StackResources, tcp::TcpSocket};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use embassy_usb::{
    Builder,
    class::hid::{
        Config as HidConfig, HidBootProtocol, HidReaderWriter, HidSubclass, HidWriter,
        ReportId, RequestHandler, State as HidState,
    },
    control::OutResponse,
};
use embedded_io_async::Write as _;
use esp_hal::{
    clock::CpuClock,
    peripherals::{LPWR, USB_DEVICE, USB_WRAP},
    rtc_cntl::{Rtc, RwdtStage, RwdtStageAction},
    system::{AppCoreGuard, CpuControl},
    rng::Rng,
    timer::timg::TimerGroup,
    usb::otg::{Usb, embassy_usb_device::Driver as UsbDriver},
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

esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const PORT: u16 = 7790;

/// Boot keyboard (HID 1.11 appendix B.1) with the LED output report.
const KEYBOARD_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, // Generic Desktop, Keyboard, Application
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08,
    0x81, 0x02, // modifiers
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01, // reserved
    0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, // LEDs
    0x95, 0x01, 0x75, 0x03, 0x91, 0x01, // LED padding
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x05, 0x07, 0x19, 0x00, 0x2A,
    0xFF, 0x00, 0x81, 0x00, // keys
    0xC0,
];

/// Mouse: 5 buttons, 16-bit X/Y, wheel, horizontal pan. 7 bytes, no report ID.
const MOUSE_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, // Generic Desktop, Mouse, Application
    0x09, 0x01, 0xA1, 0x00, // Pointer, Physical
    0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x81, 0x02, // buttons
    0x95, 0x01, 0x75, 0x03, 0x81, 0x01, // padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10,
    0x95, 0x02, 0x81, 0x06, // X, Y relative
    0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, // wheel
    0x05, 0x0C, 0x0A, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06, // AC Pan
    0xC0, 0xC0,
];

const KB_LEN: usize = 8;
const MOUSE_LEN: usize = 7;

static KB_REPORTS: Channel<CriticalSectionRawMutex, [u8; KB_LEN], 16> = Channel::new();
static MOUSE_REPORTS: Channel<CriticalSectionRawMutex, [u8; MOUSE_LEN], 32> = Channel::new();
/// Raised when the PC configures the USB device.
static USB_CONFIGURED: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();
static LEDS: Channel<CriticalSectionRawMutex, u8, 4> = Channel::new();

type Usbd = UsbDriver<'static>;

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
    } else {
        let (usb_fs, dp, dm) = (peripherals.USB_FS, peripherals.GPIO20, peripherals.GPIO19);
        static APP_CORE_STACK: static_cell::ConstStaticCell<esp_hal::system::Stack<16384>> =
            static_cell::ConstStaticCell::new(esp_hal::system::Stack::new());
        let cpu = mk_static!(CpuControl<'static>, CpuControl::new(peripherals.CPU_CTRL));
        let guard = cpu
            .start_app_core(APP_CORE_STACK.take(), move || {
                embassy_futures::block_on(usb_main(usb_fs, dp, dm))
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
    let configured = !update_mode
        && embassy_time::with_timeout(Duration::from_secs(6), USB_CONFIGURED.wait())
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
    server(stack, ota::Ota::new(peripherals.FLASH, update_mode)).await
}

// ---- USB tasks ---------------------------------------------------------------

/// Builds the USB device and runs it, on core 1.
async fn usb_main(
    usb_fs: esp_hal::peripherals::USB_FS<'static>,
    dp: esp_hal::peripherals::GPIO20<'static>,
    dm: esp_hal::peripherals::GPIO19<'static>,
) {
    // The ROM enumerated as the USB-Serial-JTAG. Drop its D+ pull-up long
    // enough for the host to see a disconnect; without it the host never
    // notices the switch to the OTG controller and the device never shows up.
    disconnect_serial_jtag();
    let start = esp_hal::time::Instant::now();
    while start.elapsed() < esp_hal::time::Duration::from_millis(300) {}
    let usb = Usb::new_fs(usb_fs, dp, dm);
    let ep_out = mk_static!([u8; 1024], [0; 1024]);
    let driver = UsbDriver::new(usb, ep_out, Default::default());

    let mut config = embassy_usb::Config::new(0x303A, 0x4001);
    config.manufacturer = Some("RawKVM");
    config.product = Some("RawKVM ESP32 gadget");
    config.serial_number = Some("0001");
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    config.supports_remote_wakeup = true;

    let mut builder = Builder::new(
        driver,
        config,
        mk_static!([u8; 256], [0; 256]),
        mk_static!([u8; 64], [0; 64]),
        &mut [],
        mk_static!([u8; 64], [0; 64]),
    );
    let keyboard = HidReaderWriter::<_, 1, KB_LEN>::new(
        &mut builder,
        mk_static!(HidState<'static>, HidState::new()),
        HidConfig {
            report_descriptor: KEYBOARD_DESC,
            request_handler: None,
            poll_ms: 1,
            max_packet_size: 8,
            hid_subclass: HidSubclass::Boot,
            hid_boot_protocol: HidBootProtocol::Keyboard,
        },
    );
    let mouse = HidWriter::<_, MOUSE_LEN>::new(
        &mut builder,
        mk_static!(HidState<'static>, HidState::new()),
        HidConfig {
            report_descriptor: MOUSE_DESC,
            request_handler: None,
            poll_ms: 1,
            max_packet_size: 8,
            hid_subclass: HidSubclass::No,
            hid_boot_protocol: HidBootProtocol::None,
        },
    );
    builder.handler(mk_static!(UsbEvents, UsbEvents));
    let usb = builder.build();
    let mut usb = usb;
    let (kb_reader, kb_writer) = keyboard.split();
    embassy_futures::join::join4(
        usb.run(),
        kb_reader.run(false, &mut KeyboardLeds),
        keyboard_in(kb_writer),
        mouse_in(mouse),
    )
    .await;
}

/// Logs the USB device state changes.
struct UsbEvents;

impl embassy_usb::Handler for UsbEvents {
    fn enabled(&mut self, enabled: bool) {
        info!("usb enabled {enabled}");
    }
    fn reset(&mut self) {
        info!("usb bus reset");
    }
    fn addressed(&mut self, addr: u8) {
        info!("usb address {addr}");
    }
    fn configured(&mut self, configured: bool) {
        info!("usb configured {configured}");
        if configured {
            USB_CONFIGURED.signal(());
        }
    }
    fn suspended(&mut self, suspended: bool) {
        info!("usb suspended {suspended}");
    }
}

/// Keyboard LEDs (output report) from the PC.
struct KeyboardLeds;

impl RequestHandler for KeyboardLeds {
    fn set_report(&mut self, _id: ReportId, data: &[u8]) -> OutResponse {
        if let Some(&leds) = data.first() {
            let _ = LEDS.try_send(leds);
        }
        OutResponse::Accepted
    }
}

async fn keyboard_in(mut writer: HidWriter<'static, Usbd, KB_LEN>) -> ! {
    loop {
        let r = KB_REPORTS.receive().await;
        if let Err(e) = writer.write(&r).await {
            warn!("keyboard write: {e:?}");
        }
    }
}

async fn mouse_in(mut writer: HidWriter<'static, Usbd, MOUSE_LEN>) -> ! {
    loop {
        let r = MOUSE_REPORTS.receive().await;
        if let Err(e) = writer.write(&r).await {
            warn!("mouse write: {e:?}");
        }
    }
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

/// Test server: one client at a time.
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
        // A frame header, or keyboard LEDs from the PC, whichever comes first.
        // Only the header read races: the body is read to the end.
        while let Some(line) = netlog::next_line(&mut log_next) {
            let n = line.len().min(255);
            send(sock, 5, &line.as_bytes()[..n]).await?;
        }
        match select3(sock.read(&mut head[..1]), LEDS.receive(), netlog::NEW_LINE.wait()).await {
            Either3::Second(leds) => {
                send(sock, 4, &[leds]).await?;
                continue;
            }
            Either3::Third(()) => continue,
            Either3::First(n) => {
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
            1 if len == KB_LEN => {
                let mut r = [0u8; KB_LEN];
                r.copy_from_slice(payload);
                KB_REPORTS.send(r).await;
            }
            2 if len == MOUSE_LEN => {
                let mut r = [0u8; MOUSE_LEN];
                r.copy_from_slice(payload);
                MOUSE_REPORTS.send(r).await;
            }
            3 => send(sock, 3, payload).await?,
            0xF0 => {
                info!("restarting into the download mode");
                let _ = send(sock, 5, b"download mode").await;
                Timer::after(Duration::from_millis(100)).await;
                download_mode();
            }
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

/// Holds the USB-Serial-JTAG's D+ pull-up off (the host sees a disconnect).
fn disconnect_serial_jtag() {
    USB_DEVICE::regs().conf0().modify(|_, w| {
        w.pad_pull_override().set_bit();
        w.dp_pullup().clear_bit();
        w.dm_pullup().clear_bit();
        w.dp_pulldown().set_bit();
        w.dm_pulldown().set_bit()
    });
}

/// Restarts into the ROM download mode on the USB-Serial-JTAG, so espflash
/// can flash again without the BOOT button: give the USB PHY back to the
/// USB-Serial-JTAG and set the force-download flag, which survives the reset.
fn download_mode() -> ! {
    // Disconnect the OTG device first, so the host sees the USB-Serial-JTAG
    // arrive as a new device.
    USB_WRAP::regs().otg_conf().modify(|_, w| {
        w.pad_pull_override().set_bit();
        w.dp_pullup().clear_bit();
        w.dm_pullup().clear_bit();
        w.dp_pulldown().set_bit();
        w.dm_pulldown().set_bit()
    });
    let start = esp_hal::time::Instant::now();
    while start.elapsed() < esp_hal::time::Duration::from_millis(300) {}
    USB_DEVICE::regs()
        .conf0()
        .modify(|_, w| w.pad_pull_override().clear_bit());
    let rtc = LPWR::regs();
    rtc.usb_conf().modify(|_, w| {
        w.sw_hw_usb_phy_sel().clear_bit();
        w.sw_usb_phy_sel().clear_bit()
    });
    rtc.option1().modify(|_, w| w.force_download_boot().set_bit());
    esp_hal::system::software_reset()
}
