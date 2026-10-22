//! RawKVM hub library: everything except the command line, so the router,
//! hotkey and recording logic can be tested and reused on any OS.

pub mod config;
#[cfg(target_os = "linux")]
pub mod devices;
#[cfg(target_os = "linux")]
pub mod hidraw;
pub mod hidrec;
pub mod hotkey;
pub mod layout;
pub mod nodes;
pub mod receiver;
pub mod router;
pub mod server;
pub mod settings;
