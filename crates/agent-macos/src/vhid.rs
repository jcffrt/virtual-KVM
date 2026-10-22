//! Client of the Karabiner-VirtualHIDDevice-Daemon socket.
//!
//! Writes happen on the caller's thread (a local socket write takes
//! microseconds). A reader thread answers the daemon's own requests (it
//! pushes status updates as requests and expects a response), records the
//! status, and sends a heartbeat when the socket has been quiet for a while
//! (the daemon drops peers silent for 30 s). If the connection breaks, the
//! next post reconnects, at most once per second, and initializes the
//! virtual keyboard and pointing device again.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::karabiner::{
    frame, keyboard_parameters, request_payload, FrameReader, MessageType, Request, Status,
    StatusKind,
};

const HEARTBEAT_EVERY: Duration = Duration::from_secs(3);
const RECONNECT_EVERY: Duration = Duration::from_secs(1);

/// Identity of the virtual keyboard (`virtual_hid_keyboard_parameters`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardParams {
    pub vendor_id: u16,
    pub product_id: u16,
    pub country_code: u8,
}

impl Default for KeyboardParams {
    /// Karabiner's own defaults.
    fn default() -> Self {
        Self {
            vendor_id: 0x16c0,
            product_id: 0x27db,
            country_code: 0,
        }
    }
}

struct Conn {
    stream: UnixStream,
    generation: u64,
}

struct Shared {
    path: PathBuf,
    conn: Mutex<Option<Conn>>,
    status: Mutex<Status>,
    next_id: AtomicU64,
    generation: AtomicU64,
    params: Mutex<KeyboardParams>,
    last_attempt: Mutex<Option<Instant>>,
}

#[derive(Clone)]
pub struct VirtualHid {
    shared: Arc<Shared>,
}

impl VirtualHid {
    pub fn new(path: impl Into<PathBuf>, params: KeyboardParams) -> Self {
        Self {
            shared: Arc::new(Shared {
                path: path.into(),
                conn: Mutex::new(None),
                status: Mutex::new(Status::default()),
                next_id: AtomicU64::new(1),
                generation: AtomicU64::new(0),
                params: Mutex::new(params),
                last_attempt: Mutex::new(None),
            }),
        }
    }

    #[cfg(test)]
    pub fn status(&self) -> Status {
        lock(&self.shared.status).clone()
    }

    /// Connect now (instead of on the first post) and report errors.
    pub fn connect(&self) -> Result<()> {
        let mut conn = lock(&self.shared.conn);
        if conn.is_none() {
            *conn = Some(self.open()?);
        }
        Ok(())
    }

    /// Change the keyboard identity; re-initializes the keyboard if needed.
    pub fn set_keyboard_params(&self, params: KeyboardParams) {
        let changed = {
            let mut p = lock(&self.shared.params);
            let changed = *p != params;
            *p = params;
            changed
        };
        if changed {
            info!(
                "virtual keyboard identity {:04x}:{:04x}",
                params.vendor_id, params.product_id
            );
            // Not connected: the next connection initializes with these.
            if lock(&self.shared.conn).is_some() {
                let _ = self.send(Request::KeyboardInitialize, &self.params_bytes());
            }
        }
    }

    fn params_bytes(&self) -> [u8; 24] {
        let p = *lock(&self.shared.params);
        keyboard_parameters(
            p.vendor_id.into(),
            p.product_id.into(),
            p.country_code.into(),
        )
    }

    fn open(&self) -> Result<Conn> {
        let path = &self.shared.path;
        let stream = UnixStream::connect(path).with_context(|| {
            format!(
                "connecting to {} (is Karabiner-VirtualHIDDevice-Daemon running, and are we root?)",
                path.display()
            )
        })?;
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let generation = self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let reader = stream.try_clone()?;
        let shared = self.shared.clone();
        thread::Builder::new()
            .name("karabiner-reader".into())
            .spawn(move || reader_loop(shared, reader, generation))?;
        info!("connected to {}", path.display());

        let mut conn = Conn { stream, generation };
        for (req, data) in [
            (Request::KeyboardInitialize, self.params_bytes().to_vec()),
            (Request::PointingInitialize, Vec::new()),
        ] {
            let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
            conn.stream.write_all(&frame(
                MessageType::Request,
                Some(id),
                &request_payload(req, &data),
            ))?;
        }
        Ok(conn)
    }

    /// Send one request. Reconnects if needed (rate limited); errors are
    /// returned but the caller may ignore them: input is best effort.
    pub fn send(&self, req: Request, data: &[u8]) -> Result<()> {
        let mut conn = lock(&self.shared.conn);
        if conn.is_none() {
            let mut last = lock(&self.shared.last_attempt);
            if last.is_some_and(|t| t.elapsed() < RECONNECT_EVERY) {
                anyhow::bail!("not connected to the Karabiner daemon");
            }
            *last = Some(Instant::now());
            drop(last);
            *conn = Some(self.open()?);
        }
        let c = conn.as_mut().expect("connected above");
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let bytes = frame(MessageType::Request, Some(id), &request_payload(req, data));
        if let Err(e) = c.stream.write_all(&bytes) {
            warn!("Karabiner daemon write failed: {e}");
            *conn = None;
            return Err(e.into());
        }
        Ok(())
    }

    /// Remove the virtual devices (on exit).
    pub fn terminate(&self) {
        if lock(&self.shared.conn).is_some() {
            let _ = self.send(Request::KeyboardTerminate, &[]);
            let _ = self.send(Request::PointingTerminate, &[]);
        }
        *lock(&self.shared.conn) = None;
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write on the current connection if it is still `generation`.
fn write_if_current(shared: &Shared, generation: u64, bytes: &[u8]) -> io::Result<()> {
    let mut conn = lock(&shared.conn);
    match conn.as_mut() {
        Some(c) if c.generation == generation => c.stream.write_all(bytes),
        _ => Err(io::ErrorKind::NotConnected.into()),
    }
}

fn reader_loop(shared: Arc<Shared>, mut stream: UnixStream, generation: u64) {
    let _ = stream.set_read_timeout(Some(HEARTBEAT_EVERY));
    let mut reader = FrameReader::default();
    let mut buf = [0u8; 4096];
    let mut last_write = Instant::now();
    let end = loop {
        if last_write.elapsed() >= HEARTBEAT_EVERY {
            if let Err(e) = write_if_current(
                &shared,
                generation,
                &frame(MessageType::Heartbeat, None, &[]),
            ) {
                break e;
            }
            last_write = Instant::now();
        }
        let n = match stream.read(&mut buf) {
            Ok(0) => break io::ErrorKind::UnexpectedEof.into(),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break e,
        };
        reader.push(&buf[..n]);
        loop {
            let f = match reader.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => {
                    warn!("bad frame from the Karabiner daemon: {e:?}");
                    return close(&shared, generation);
                }
            };
            match f.kind {
                MessageType::Request => {
                    // Status push from the daemon: acknowledge it.
                    let id = f.request_id.unwrap_or(0);
                    if let Err(e) = write_if_current(
                        &shared,
                        generation,
                        &frame(MessageType::Response, Some(id), &[]),
                    ) {
                        debug!("ack failed: {e}");
                    }
                    apply_status(&shared, &f.payload);
                }
                MessageType::Response => apply_status(&shared, &f.payload),
                _ => {}
            }
        }
    };
    debug!("Karabiner reader ended: {end}");
    close(&shared, generation);
}

fn close(shared: &Shared, generation: u64) {
    let mut conn = lock(&shared.conn);
    if conn.as_ref().is_some_and(|c| c.generation == generation) {
        *conn = None;
        warn!("disconnected from the Karabiner daemon");
    }
    *lock(&shared.status) = Status::default();
}

fn apply_status(shared: &Shared, payload: &[u8]) {
    for (kind, value) in lock(&shared.status).apply(payload) {
        match (kind, value) {
            (StatusKind::DriverVersionMismatched, true) => {
                warn!("Karabiner driver version mismatch: update Karabiner-DriverKit-VirtualHIDDevice")
            }
            (StatusKind::DriverActivated, false) => {
                warn!("Karabiner driver extension not activated (System Settings > General > Login Items & Extensions)")
            }
            (kind, value) => info!(?kind, value, "Karabiner status"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A fake daemon: records requests, pushes a status request, answers.
    #[test]
    fn talks_to_a_fake_daemon() {
        let dir = std::env::temp_dir().join(format!("rawkvm-vhid-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();

        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            // Push a status update as the daemon does.
            s.write_all(&frame(MessageType::Request, Some(77), &[1, 1, 4, 1, 5, 1]))
                .unwrap();
            let mut r = FrameReader::default();
            let mut got = Vec::new();
            let mut buf = [0u8; 1024];
            while got.len() < 4 {
                let n = s.read(&mut buf).unwrap();
                assert!(n > 0);
                r.push(&buf[..n]);
                while let Some(f) = r.next_frame().unwrap() {
                    got.push(f);
                }
            }
            // Keep the socket open: closing it resets the client's status.
            (got, s)
        });

        let v = VirtualHid::new(&path, KeyboardParams::default());
        v.connect().unwrap();
        v.send(Request::PostPointing, &[0; 8]).unwrap();
        let (got, _keep_open) = server.join().unwrap();

        let reqs: Vec<(MessageType, Option<u8>)> = got
            .iter()
            .map(|f| (f.kind, f.payload.get(2).copied()))
            .collect();
        assert!(reqs.contains(&(
            MessageType::Request,
            Some(Request::KeyboardInitialize as u8)
        )));
        assert!(reqs.contains(&(
            MessageType::Request,
            Some(Request::PointingInitialize as u8)
        )));
        assert!(reqs.contains(&(MessageType::Request, Some(Request::PostPointing as u8))));
        let ack = got
            .iter()
            .find(|f| f.kind == MessageType::Response)
            .unwrap();
        assert_eq!(ack.request_id, Some(77));
        // Every request carries protocol version 7.
        assert!(got
            .iter()
            .filter(|f| f.kind == MessageType::Request)
            .all(|f| f.payload[..2] == [7, 0]));
        let init = got.iter().find(|f| f.payload.get(2) == Some(&0)).unwrap();
        assert_eq!(init.payload.len(), 3 + 24);

        // Status is applied by the reader thread.
        for _ in 0..100 {
            if v.status().pointing_ready == Some(true) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(v.status().keyboard_ready, Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_socket_is_an_error_and_rate_limited() {
        let v = VirtualHid::new("/nonexistent/rawkvm.sock", KeyboardParams::default());
        assert!(v.connect().is_err());
        let first = v.send(Request::PostPointing, &[0; 8]).unwrap_err();
        assert!(format!("{first:#}").contains("Karabiner-VirtualHIDDevice-Daemon"));
        let second = v.send(Request::PostPointing, &[0; 8]).unwrap_err();
        assert!(format!("{second:#}").contains("not connected"));
    }
}
