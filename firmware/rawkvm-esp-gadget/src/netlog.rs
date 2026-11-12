//! Log lines kept in memory and sent to the TCP client: the USB port is the
//! HID device, so there is no serial console.

use alloc::{collections::VecDeque, string::String};
use core::{cell::RefCell, fmt::Write as _};

use critical_section::Mutex;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

const MAX_LINES: usize = 128;

struct Ring {
    /// Sequence number of `lines[0]`.
    first: u32,
    lines: VecDeque<String>,
}

static RING: Mutex<RefCell<Ring>> = Mutex::new(RefCell::new(Ring {
    first: 0,
    lines: VecDeque::new(),
}));

/// Raised on every new line.
pub static NEW_LINE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

struct NetLogger;

impl log::Log for NetLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut line = String::new();
        let ms = esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_millis();
        let _ = write!(line, "{ms:>8} {:<5} {}", record.level(), record.args());
        esp_println::println!("{line}");
        critical_section::with(|cs| {
            let mut ring = RING.borrow_ref_mut(cs);
            if ring.lines.len() == MAX_LINES {
                ring.lines.pop_front();
                ring.first = ring.first.wrapping_add(1);
            }
            ring.lines.push_back(line);
        });
        NEW_LINE.signal(());
    }

    fn flush(&self) {}
}

pub fn init() {
    static LOGGER: NetLogger = NetLogger;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}

/// The next line from sequence `*next` on (skipping lines already dropped),
/// advancing `*next`.
pub fn next_line(next: &mut u32) -> Option<String> {
    critical_section::with(|cs| {
        let ring = RING.borrow_ref(cs);
        let behind = next.wrapping_sub(ring.first);
        let index = if behind as i32 > 0 { behind as usize } else { 0 };
        let line = ring.lines.get(index)?.clone();
        *next = ring.first.wrapping_add(index as u32 + 1);
        Some(line)
    })
}
