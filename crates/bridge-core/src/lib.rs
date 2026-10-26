//! What every RawKVM Bluetooth bridge does the same way, whatever its
//! Bluetooth HID stack (BlueZ in `rawkvm-agent-bt`, Android's
//! BluetoothHidDevice in a possible app): merge the receiver's descriptors
//! into one (`merge`) and keep the one bonded PC (`host`). No I/O here; the
//! rules are also written down in docs/adr/0033 for other implementations.

pub mod host;
pub mod merge;
