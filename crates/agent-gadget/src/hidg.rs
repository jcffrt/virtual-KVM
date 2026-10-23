//! `/dev/hidgN`: the character device of one HID gadget function.
//!
//! `f_hid` accepts one pending IN report at a time; with `O_NONBLOCK` a
//! write returns `EAGAIN` until the host has polled the previous one (every
//! 1 ms at high speed). While the host is suspended or not enumerated,
//! `EAGAIN` persists, so writers retry briefly and then drop the report.
//! Output reports from the host (LEDs, HID++ requests) queue up in the
//! driver; a reader thread drains them and forwards them to the hub, which
//! keeps only those the receiver's descriptor declares (keyboard LEDs).
//! The function has an OUT endpoint (`no_out_endpoint` stays 0), so a read
//! returns one report as the host sent it.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use agent_core::{OutputReport, OutputSender};
use tracing::trace;

/// `/dev/<DEVNAME>` for a function, from its ConfigFS `dev` attribute
/// ("major:minor") and `/sys/dev/char/<major:minor>/uevent`.
pub fn device_node(function_dir: &Path, sys_dev_char: &Path) -> io::Result<PathBuf> {
    let dev = std::fs::read_to_string(function_dir.join("dev"))?;
    let uevent = std::fs::read_to_string(sys_dev_char.join(dev.trim()).join("uevent"))?;
    let name = uevent
        .lines()
        .find_map(|l| l.strip_prefix("DEVNAME="))
        .ok_or_else(|| io::Error::other(format!("no DEVNAME for {}", dev.trim())))?;
    Ok(Path::new("/dev").join(name))
}

/// Wait for a device node to appear (udev creates it after the bind).
pub fn wait_for(path: &Path, timeout: Duration) -> io::Result<()> {
    let start = Instant::now();
    while !path.exists() {
        if start.elapsed() > timeout {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} did not appear", path.display()),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

pub struct Hidg {
    path: PathBuf,
    file: File,
    stop: Arc<AtomicBool>,
}

impl Hidg {
    /// `outputs`: where the host's output reports go, tagged with the
    /// hub's device id.
    pub fn open(path: &Path, outputs: Option<(u8, OutputSender)>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let reader = file.try_clone()?;
        let flag = stop.clone();
        let name = path.display().to_string();
        thread::Builder::new()
            .name("hidg-out".into())
            .spawn(move || drain(reader, flag, name, outputs))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            stop,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write one report, retrying on `EAGAIN` for up to `patience`.
    /// Returns `Ok(false)` if it had to be dropped.
    pub fn write_report(&self, report: &[u8], patience: Duration) -> io::Result<bool> {
        let start = Instant::now();
        loop {
            match (&self.file).write(report) {
                Ok(_) => return Ok(true),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if start.elapsed() >= patience {
                        return Ok(false);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // ESHUTDOWN: the gadget is not configured by a host.
                Err(e) if e.raw_os_error() == Some(libc::ESHUTDOWN) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for Hidg {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Read output reports until stopped and forward them.
fn drain(mut file: File, stop: Arc<AtomicBool>, name: String, outputs: Option<(u8, OutputSender)>) {
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::SeqCst) {
        let mut pfd = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd; 200 ms timeout so `stop` is noticed.
        let n = unsafe { libc::poll(&mut pfd, 1, 200) };
        if n <= 0 || pfd.revents & libc::POLLIN == 0 {
            continue;
        }
        match file.read(&mut buf) {
            Ok(n) => {
                trace!(dev = %name, report = ?&buf[..n], "output report from the host");
                if let Some((dev_id, tx)) = &outputs {
                    let _ = tx.send(OutputReport {
                        dev_id: *dev_id,
                        report: buf[..n].to_vec(),
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_node_from_sysfs() {
        let root = std::env::temp_dir().join(format!("rawkvm-hidg-{}", uuid::Uuid::new_v4()));
        let func = root.join("functions/hid.usb0");
        let sys = root.join("sys/dev/char");
        std::fs::create_dir_all(&func).unwrap();
        std::fs::create_dir_all(sys.join("240:1")).unwrap();
        std::fs::write(func.join("dev"), "240:1\n").unwrap();
        std::fs::write(
            sys.join("240:1/uevent"),
            "MAJOR=240\nMINOR=1\nDEVNAME=hidg1\n",
        )
        .unwrap();
        assert_eq!(device_node(&func, &sys).unwrap(), Path::new("/dev/hidg1"));
        std::fs::write(sys.join("240:1/uevent"), "MAJOR=240\n").unwrap();
        assert!(device_node(&func, &sys).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wait_for_times_out() {
        let p = Path::new("/nonexistent/rawkvm-hidg");
        assert!(wait_for(p, Duration::from_millis(50)).is_err());
        assert!(wait_for(Path::new("/"), Duration::ZERO).is_ok());
    }
}
