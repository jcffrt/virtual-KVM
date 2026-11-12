//! The USB device, built from the hub's `Gadget`: the receiver's VID/PID
//! and strings, one HID interface per hub device, each with the receiver's
//! own report descriptor. Runs on core 1.
//!
//! Interfaces have only an interrupt IN endpoint: the PC sends output
//! reports (keyboard LEDs) with SET_REPORT on the control pipe. The S3's
//! OTG has room for four IN endpoints besides EP0, hence [`MAX`].

use alloc::boxed::Box;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_usb::{
    Builder,
    class::hid::{
        Config as HidConfig, HidBootProtocol, HidSubclass, HidWriter, ReportId, RequestHandler,
        State as HidState,
    },
    control::OutResponse,
};
use esp_hal::usb::otg::{Usb, embassy_usb_device::Driver as UsbDriver};
use light::{Gadget, MAX_REPORT};
use log::{info, warn};

/// HID interfaces the board presents at most.
pub const MAX: usize = 4;

type Usbd = UsbDriver<'static>;

#[derive(Clone, Copy)]
pub struct Report {
    len: u8,
    data: [u8; MAX_REPORT],
}

impl Report {
    pub fn new(b: &[u8]) -> Option<Self> {
        if b.len() > MAX_REPORT {
            return None;
        }
        let mut data = [0; MAX_REPORT];
        data[..b.len()].copy_from_slice(b);
        Some(Self {
            len: b.len() as u8,
            data,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

/// Input reports for each interface, from the hub task to core 1.
pub static REPORTS: [Channel<CriticalSectionRawMutex, Report, 16>; MAX] =
    [const { Channel::new() }; MAX];
/// Output reports from the PC: (hub device id, report).
pub static OUTPUTS: Channel<CriticalSectionRawMutex, (u8, Report), 8> = Channel::new();
/// Raised when the PC configures the USB device.
pub static CONFIGURED: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

fn leak<T>(v: T) -> &'static mut T {
    Box::leak(Box::new(v))
}

/// Builds the USB device and runs it.
pub async fn run(
    usb_fs: esp_hal::peripherals::USB_FS<'static>,
    dp: esp_hal::peripherals::GPIO20<'static>,
    dm: esp_hal::peripherals::GPIO19<'static>,
    g: &'static Gadget,
) {
    // The ROM enumerated as the USB-Serial-JTAG. Drop its D+ pull-up long
    // enough for the host to see a disconnect; without it the host never
    // notices the switch to the OTG controller and the device never shows up.
    disconnect_serial_jtag();
    let start = esp_hal::time::Instant::now();
    while start.elapsed() < esp_hal::time::Duration::from_millis(300) {}
    let usb = Usb::new_fs(usb_fs, dp, dm);
    let driver = UsbDriver::new(usb, leak([0u8; 1024]), Default::default());

    let mut config = embassy_usb::Config::new(g.vid, g.pid);
    config.manufacturer = Some(&g.manufacturer);
    config.product = Some(&g.product);
    config.device_release = 0x0100;
    config.max_power = 98;
    config.max_packet_size_0 = 64;
    config.supports_remote_wakeup = true;

    let mut builder = Builder::new(
        driver,
        config,
        leak([0u8; 512]),
        leak([0u8; 64]),
        &mut [],
        // Holds the longest report descriptor sent to the PC.
        leak([0u8; 1024]),
    );
    let mut writers: [Option<HidWriter<'static, Usbd, MAX_REPORT>>; MAX] = [None, None, None, None];
    for (slot, itf) in g.interfaces.iter().take(MAX).enumerate() {
        let (subclass, protocol) = match (itf.subclass, itf.protocol) {
            (1, 1) => (HidSubclass::Boot, HidBootProtocol::Keyboard),
            (1, 2) => (HidSubclass::Boot, HidBootProtocol::Mouse),
            _ => (HidSubclass::No, HidBootProtocol::None),
        };
        let packet = itf.max_input.clamp(8, MAX_REPORT as u16);
        writers[slot] = Some(HidWriter::new(
            &mut builder,
            leak(HidState::new()),
            HidConfig {
                report_descriptor: &itf.report_descriptor,
                request_handler: Some(leak(Outputs { dev_id: itf.dev_id })),
                poll_ms: 1,
                max_packet_size: packet,
                hid_subclass: subclass,
                hid_boot_protocol: protocol,
            },
        ));
        info!(
            "usb interface {slot}: hub device {}, {} byte descriptor, packet {packet}",
            itf.dev_id,
            itf.report_descriptor.len()
        );
    }
    builder.handler(leak(UsbEvents));
    let mut usb = builder.build();
    let [w0, w1, w2, w3] = writers;
    embassy_futures::join::join(
        usb.run(),
        embassy_futures::join::join4(pump(0, w0), pump(1, w1), pump(2, w2), pump(3, w3)),
    )
    .await;
}

async fn pump(slot: usize, writer: Option<HidWriter<'static, Usbd, MAX_REPORT>>) {
    let Some(mut w) = writer else {
        return core::future::pending().await;
    };
    loop {
        let r = REPORTS[slot].receive().await;
        if let Err(e) = w.write(r.bytes()).await {
            warn!("usb interface {slot}: write: {e:?}");
        }
    }
}

/// Output reports (SET_REPORT) of one interface, to the hub.
struct Outputs {
    dev_id: u8,
}

impl RequestHandler for Outputs {
    fn set_report(&mut self, _id: ReportId, data: &[u8]) -> OutResponse {
        // As hidraw hands them over: report ID first only if the interface
        // uses IDs, which is how the data stage carries them.
        if let Some(r) = Report::new(data) {
            let _ = OUTPUTS.try_send((self.dev_id, r));
        }
        OutResponse::Accepted
    }
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
            CONFIGURED.signal(());
        }
    }
    fn suspended(&mut self, suspended: bool) {
        info!("usb suspended {suspended}");
    }
}

/// Holds the USB-Serial-JTAG's D+ pull-up off (the host sees a disconnect).
fn disconnect_serial_jtag() {
    esp_hal::peripherals::USB_DEVICE::regs()
        .conf0()
        .modify(|_, w| {
            w.pad_pull_override().set_bit();
            w.dp_pullup().clear_bit();
            w.dm_pullup().clear_bit();
            w.dp_pulldown().set_bit();
            w.dm_pulldown().set_bit()
        });
}
