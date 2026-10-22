//! The real `\\.\RawKvmVhf` handle.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::ioctl;
use crate::sink::{Control, Opener};

pub struct Driver {
    handle: HANDLE,
}

// The handle is only used from one thread at a time (the sink owns it).
unsafe impl Send for Driver {}

impl Driver {
    pub fn open() -> io::Result<Self> {
        let path: Vec<u16> = std::ffi::OsStr::new(ioctl::DEVICE_PATH)
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: valid NUL-terminated path; no template, default security.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<u32> {
        let mut returned = 0u32;
        // SAFETY: buffers are valid for the given lengths; synchronous call.
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                code,
                input.as_ptr() as *const c_void,
                input.len() as u32,
                output.as_mut_ptr() as *mut c_void,
                output.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(returned)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // SAFETY: handle came from CreateFileW and is closed once.
        unsafe { CloseHandle(self.handle) };
    }
}

impl Control for Driver {
    fn create(&mut self, request: &[u8]) -> io::Result<u32> {
        let mut out = [0u8; 4];
        let n = self.ioctl(ioctl::IOCTL_CREATE, request, &mut out)?;
        if n != 4 {
            return Err(io::Error::other("IOCTL_RAWKVM_CREATE returned no handle"));
        }
        Ok(u32::from_le_bytes(out))
    }

    fn report(&mut self, request: &[u8]) -> io::Result<()> {
        self.ioctl(ioctl::IOCTL_REPORT, request, &mut [])
            .map(|_| ())
    }

    fn destroy(&mut self, handle: u32) -> io::Result<()> {
        self.ioctl(ioctl::IOCTL_DESTROY, &ioctl::encode_handle(handle), &mut [])
            .map(|_| ())
    }
}

pub struct DriverOpener;

impl Opener for DriverOpener {
    fn open(&mut self) -> io::Result<Box<dyn Control>> {
        Ok(Box::new(Driver::open()?))
    }
}
