//! Win32: monitors, pointer position and pointer placement, without a
//! driver or a hook.
//!
//! The process is per-monitor DPI aware (v2), so every coordinate is in
//! physical pixels of the virtual desktop (the primary monitor's top-left
//! corner is (0, 0)) and `SetCursorPos` lands where `GetCursorPos` reads.

use agent_core::edge::Point;
use proto::Screen;
use uuid::Uuid;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, SetCursorPos, MONITORINFOF_PRIMARY,
};

/// Put this process in a job that kills every process in it when its last
/// handle closes, that is, when this process ends: the helper it starts
/// (ADR 0027) then never outlives it, and a new logon or setup.ps1 never
/// finds two helpers for the same node.
pub fn kill_children_with_me() -> std::io::Result<()> {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain Win32 calls on handles created here; the job handle is
    // deliberately never closed, so it lives as long as the process.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&info) as u32,
        ) == 0
            || AssignProcessToJobObject(job, GetCurrentProcess()) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Call once at start, before reading any coordinate.
pub fn dpi_aware() -> bool {
    // SAFETY: plain call with a predefined context.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 }
}

/// Make `println!` reach the console the program was started from (it is
/// a GUI-subsystem program, so it has none of its own).
pub fn attach_console() {
    // SAFETY: fails harmlessly when there is no parent console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[derive(Debug, Clone, Copy)]
pub struct Monitor {
    pub primary: bool,
    pub dpi: u32,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let out = &mut *(data as *mut Vec<Monitor>);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(m, &mut info) == 0 {
        return 1;
    }
    let (mut dx, mut dy) = (96u32, 96u32);
    if GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 {
        dx = 96;
    }
    out.push(Monitor {
        primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
        dpi: dx,
        left: info.rcMonitor.left,
        top: info.rcMonitor.top,
        right: info.rcMonitor.right,
        bottom: info.rcMonitor.bottom,
    });
    1
}

/// Monitors: the primary gets index 0, the others follow left to right
/// (then top to bottom).
pub fn monitors() -> Vec<Monitor> {
    let mut out: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only runs during this call and writes to `out`.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut out as *mut Vec<Monitor> as LPARAM,
        );
    }
    out.sort_by_key(|m| (!m.primary, m.left, m.top));
    out
}

pub fn screens() -> Vec<Screen> {
    monitors()
        .into_iter()
        .enumerate()
        .map(|(i, m)| Screen {
            node: Uuid::nil(),
            index: i as u8,
            x: m.left,
            y: m.top,
            w: (m.right - m.left).max(1) as u32,
            h: (m.bottom - m.top).max(1) as u32,
            scale: m.dpi as f32 / 96.0,
        })
        .collect()
}

/// `None` while the pointer cannot be read (secure desktop: lock screen,
/// UAC prompt, Ctrl+Alt+Del).
pub fn pointer() -> Option<Point> {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: valid out pointer.
    (unsafe { GetCursorPos(&mut p) } != 0).then_some(Point { x: p.x, y: p.y })
}

pub fn set_pointer(to: Point) -> bool {
    // SAFETY: plain call.
    unsafe { SetCursorPos(to.x, to.y) != 0 }
}
