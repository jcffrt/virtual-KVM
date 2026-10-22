//! Install the RawKVM VHF driver on its root-enumerated device, the way
//! `devcon install` does: create `Root\RawKvmVhf` with SetupAPI if it does
//! not exist yet, then install the INF on it. pnputil only stages drivers;
//! it cannot create a root device.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiSetDeviceRegistryPropertyW,
    UpdateDriverForPlugAndPlayDevicesW, DICD_GENERATE_ID, DIF_REGISTERDEVICE, HDEVINFO,
    INSTALLFLAG_FORCE, SPDRP_HARDWAREID, SP_DEVINFO_DATA,
};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

pub const HARDWARE_ID: &str = r"Root\RawKvmVhf";

/// `GUID_DEVCLASS_SYSTEM` {4d36e97d-e325-11ce-bfc1-08002be10318}.
const GUID_DEVCLASS_SYSTEM: GUID = GUID::from_u128(0x4d36e97d_e325_11ce_bfc1_08002be10318);

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// REG_MULTI_SZ with one string.
fn multi_sz(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain([0, 0]).collect()
}

struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: created by SetupDi*, destroyed once.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn check(ok: i32, what: &str) -> Result<()> {
    if ok == 0 {
        return Err(io::Error::last_os_error()).context(what.to_string());
    }
    Ok(())
}

/// Does a System-class device with our hardware id exist (present or not)?
fn device_exists() -> Result<bool> {
    // SAFETY: plain SetupAPI enumeration with owned buffers.
    unsafe {
        let set = SetupDiGetClassDevsW(
            &GUID_DEVCLASS_SYSTEM,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
        if set as isize == INVALID_HANDLE_VALUE as isize {
            return Err(io::Error::last_os_error()).context("SetupDiGetClassDevsW");
        }
        let set = DevInfo(set);
        let mut index = 0;
        loop {
            let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set.0, index, &mut data) == 0 {
                return Ok(false);
            }
            index += 1;
            let mut buf = [0u16; 512];
            let ok = SetupDiGetDeviceRegistryPropertyW(
                set.0,
                &data,
                SPDRP_HARDWAREID,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut u8,
                (buf.len() * 2) as u32,
                std::ptr::null_mut(),
            );
            if ok == 0 {
                continue;
            }
            let ids = String::from_utf16_lossy(&buf);
            if ids
                .split('\0')
                .any(|id| id.eq_ignore_ascii_case(HARDWARE_ID))
            {
                return Ok(true);
            }
        }
    }
}

fn create_root_device() -> Result<()> {
    // SAFETY: standard devcon-style sequence with owned buffers.
    unsafe {
        let set = SetupDiCreateDeviceInfoList(&GUID_DEVCLASS_SYSTEM, std::ptr::null_mut());
        if set as isize == INVALID_HANDLE_VALUE as isize {
            return Err(io::Error::last_os_error()).context("SetupDiCreateDeviceInfoList");
        }
        let set = DevInfo(set);
        let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
        let class_name = wide(OsStr::new("System"));
        check(
            SetupDiCreateDeviceInfoW(
                set.0,
                class_name.as_ptr(),
                &GUID_DEVCLASS_SYSTEM,
                std::ptr::null(),
                std::ptr::null_mut(),
                DICD_GENERATE_ID,
                &mut data,
            ),
            "SetupDiCreateDeviceInfoW",
        )?;
        let ids = multi_sz(HARDWARE_ID);
        check(
            SetupDiSetDeviceRegistryPropertyW(
                set.0,
                &mut data,
                SPDRP_HARDWAREID,
                ids.as_ptr() as *const u8,
                (ids.len() * 2) as u32,
            ),
            "SetupDiSetDeviceRegistryPropertyW",
        )?;
        check(
            SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &data),
            "SetupDiCallClassInstaller(DIF_REGISTERDEVICE)",
        )?;
    }
    Ok(())
}

/// Create the root device if needed and install `inf` on it. Returns true
/// if Windows asks for a reboot.
pub fn install(inf: &Path) -> Result<bool> {
    let inf = std::fs::canonicalize(inf).with_context(|| format!("{} not found", inf.display()))?;
    // canonicalize returns \\?\C:\...; SetupAPI wants a plain path.
    let inf_str = inf.to_string_lossy();
    let inf_plain = inf_str.strip_prefix(r"\\?\").unwrap_or(&inf_str);
    if !inf_plain.to_ascii_lowercase().ends_with(".inf") {
        bail!("{inf_plain} is not an .inf file");
    }

    if device_exists()? {
        println!("{HARDWARE_ID} already exists");
    } else {
        create_root_device()?;
        println!("created {HARDWARE_ID}");
    }

    let hwid = wide(OsStr::new(HARDWARE_ID));
    let path = wide(OsStr::new(inf_plain));
    let mut reboot = 0i32;
    // SAFETY: NUL-terminated strings, valid out pointer.
    let ok = unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            std::ptr::null_mut(),
            hwid.as_ptr(),
            path.as_ptr(),
            INSTALLFLAG_FORCE,
            &mut reboot,
        )
    };
    check(ok, "UpdateDriverForPlugAndPlayDevicesW")?;
    println!("driver installed from {inf_plain}");
    Ok(reboot != 0)
}
