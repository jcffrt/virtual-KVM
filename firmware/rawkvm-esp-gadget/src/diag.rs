//! Post-mortem notes that survive a reset: the panic message, and the last
//! step of a long operation (to know where a watchdog reset hit). Logged at
//! the next boot, since there is no serial console.

use core::fmt::Write as _;

const LEN: usize = 192;
const MAGIC: u32 = 0x5241_574B; // "RAWK"

#[repr(C)]
#[derive(Clone, Copy)]
struct Note {
    magic: u32,
    len: u32,
    text: [u8; LEN],
}

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut PANIC: Note = Note { magic: 0, len: 0, text: [0; LEN] };

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut STEP: Note = Note { magic: 0, len: 0, text: [0; LEN] };

// SAFETY: plain integers and a byte array, any bit pattern is valid.
unsafe impl esp_hal::Persistable for Note {}

/// Set before a restart into the update mode (WiFi only, no USB).
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut UPDATE_MODE: u32 = 0;

pub fn request_update_mode() {
    unsafe { *core::ptr::addr_of_mut!(UPDATE_MODE) = MAGIC };
}

/// Whether this boot is in the update mode (and clears the request).
pub fn take_update_mode() -> bool {
    unsafe {
        let p = core::ptr::addr_of_mut!(UPDATE_MODE);
        let on = *p == MAGIC;
        *p = 0;
        on
    }
}

struct Writer<'a> {
    note: &'a mut Note,
}

impl core::fmt::Write for Writer<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            let n = self.note.len as usize;
            if n < LEN {
                self.note.text[n] = b;
                self.note.len += 1;
            }
        }
        Ok(())
    }
}

fn record(note: &mut Note, args: core::fmt::Arguments<'_>) {
    note.magic = 0;
    note.len = 0;
    let _ = Writer { note: &mut *note }.write_fmt(args);
    note.magic = MAGIC;
}

fn take(note: &mut Note) -> Option<alloc::string::String> {
    if note.magic != MAGIC {
        return None;
    }
    note.magic = 0;
    let n = (note.len as usize).min(LEN);
    Some(alloc::string::String::from_utf8_lossy(&note.text[..n]).into_owned())
}

/// The step a long operation is at; cleared with `step_done`.
pub fn step(args: core::fmt::Arguments<'_>) {
    // SAFETY: only core 0 (the server) records steps.
    record(unsafe { &mut *core::ptr::addr_of_mut!(STEP) }, args);
}

pub fn step_done() {
    unsafe { (*core::ptr::addr_of_mut!(STEP)).magic = 0 };
}

/// Logs what the previous run left: reset reason, panic, unfinished step.
/// True if it panicked.
pub fn report_previous_run() -> bool {
    log::info!("reset reason {:?}", esp_hal::system::reset_reason());
    let panicked = take(unsafe { &mut *core::ptr::addr_of_mut!(PANIC) });
    if let Some(p) = &panicked {
        log::warn!("previous run panicked: {p}");
    }
    if let Some(s) = take(unsafe { &mut *core::ptr::addr_of_mut!(STEP) }) {
        log::warn!("previous run stopped at: {s}");
    }
    panicked.is_some()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // SAFETY: the system restarts right after; a panic on both cores at
    // once would at worst garble the message.
    record(
        unsafe { &mut *core::ptr::addr_of_mut!(PANIC) },
        format_args!("{info}"),
    );
    esp_hal::system::software_reset()
}
