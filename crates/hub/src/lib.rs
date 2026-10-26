//! RawKVM hub library: everything except the command line, so the router,
//! hotkey and recording logic can be tested and reused on any OS.

pub mod config;
#[cfg(target_os = "linux")]
pub mod devices;
pub mod hidpp;
#[cfg(target_os = "linux")]
pub mod hidraw;
pub mod hidrec;
pub mod hotkey;
pub mod layout;
pub mod netwatch;
pub mod nodes;
pub mod poweroff;
pub mod receiver;
pub mod receiver_ctl;
pub mod receivers;
pub mod router;
pub mod server;
pub mod settings;
