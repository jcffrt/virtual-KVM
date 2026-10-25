//! Windows: power the hub off when this PC shuts down or restarts (the PC
//! whose USB port powers the hub's board, ADR 0025). Off by default.
//!
//! Windows asks every top-level window with `WM_QUERYENDSESSION` before it
//! ends the session; a hidden window on its own thread answers it, so the
//! wait does not depend on the Tauri event loop. Logging off
//! (`ENDSESSION_LOGOFF`) leaves the hub alone: the PC keeps powering it.
//! The time of each message goes to the log, to measure how long the board
//! keeps its power after Windows warns the apps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tracing::{info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
    TranslateMessage, ENDSESSION_LOGOFF, MSG, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
    WS_OVERLAPPED,
};

/// Called once when Windows shuts down or restarts; blocks until the hub
/// confirmed or gave up.
type Handler = Box<dyn Fn() + Send + Sync>;

static HANDLER: OnceLock<Handler> = OnceLock::new();
static DONE: AtomicBool = AtomicBool::new(false);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Start the hidden window. `on_shutdown` runs (once) on shutdown or
/// restart, not on log off.
pub fn watch(on_shutdown: impl Fn() + Send + Sync + 'static) {
    if HANDLER.set(Box::new(on_shutdown)).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("endsession".into())
        .spawn(|| {
            if let Err(e) = message_loop() {
                warn!("shutdown watch: {e}");
            }
        });
    if let Err(e) = spawned {
        warn!("shutdown watch: {e}");
    }
}

fn message_loop() -> Result<(), String> {
    let class = wide("RawKvmEndSession");
    // SAFETY: plain Win32 calls with valid, NUL-terminated strings that
    // outlive them; the window lives as long as this thread.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&wc) == 0 {
            return Err("RegisterClassW failed".into());
        }
        let title = wide("RawKVM");
        // A top-level window (never shown): message-only windows do not
        // get WM_QUERYENDSESSION.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return Err("CreateWindowExW failed".into());
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// What a session-end message means for the hub.
fn ending(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<bool> {
    match msg {
        WM_QUERYENDSESSION => {}
        // wParam FALSE: another app cancelled the shutdown.
        WM_ENDSESSION if wparam != 0 => {}
        _ => return None,
    }
    Some(lparam as u32 & ENDSESSION_LOGOFF == 0)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(shutdown) = ending(msg, wparam, lparam) else {
        // SAFETY: the default handling of a message this window received.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let name = if msg == WM_QUERYENDSESSION {
        "WM_QUERYENDSESSION"
    } else {
        "WM_ENDSESSION"
    };
    info!(lparam = format!("{:#x}", lparam as u32), "{name}");
    if !shutdown {
        info!("logging off: the hub stays on");
    } else if !DONE.swap(true, Ordering::SeqCst) {
        if let Some(h) = HANDLER.get() {
            let reason = wide("Apagando el hub de RawKVM");
            // SAFETY: valid window and NUL-terminated string.
            unsafe { ShutdownBlockReasonCreate(hwnd, reason.as_ptr()) };
            h();
            // SAFETY: the same window.
            unsafe { ShutdownBlockReasonDestroy(hwnd) };
        }
    }
    // TRUE to WM_QUERYENDSESSION (never block the shutdown); 0 otherwise.
    (msg == WM_QUERYENDSESSION) as LRESULT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_off_is_not_a_shutdown() {
        assert_eq!(ending(WM_QUERYENDSESSION, 0, 0), Some(true));
        assert_eq!(
            ending(WM_QUERYENDSESSION, 0, ENDSESSION_LOGOFF as LPARAM),
            Some(false)
        );
        assert_eq!(ending(WM_ENDSESSION, 1, 0), Some(true));
        assert_eq!(ending(WM_ENDSESSION, 0, 0), None);
        assert_eq!(ending(0x0010, 0, 0), None);
    }
}
