//! `log_panics` writes the panic to the log (its own test binary: the hook
//! is process-wide).

use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_panic_reaches_the_log() {
    let buf = Buf::default();
    let writer = buf.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();
    agent_core::supervise::log_panics();
    let r = std::thread::Builder::new()
        .name("edge".into())
        .spawn(|| panic!("boom {}", 42))
        .unwrap()
        .join();
    assert!(r.is_err());
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("ERROR"), "{log}");
    assert!(log.contains("panic at "), "{log}");
    assert!(log.contains("panic_hook.rs:"), "{log}");
    assert!(log.contains("boom 42"), "{log}");
    assert!(log.contains("thread=\"edge\""), "{log}");
}
