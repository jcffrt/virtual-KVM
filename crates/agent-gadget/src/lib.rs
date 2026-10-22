//! RawKVM USB gadget agent: receives the hub's raw reports and replays them
//! on a USB device port, presenting the receiver to a PC as the real thing
//! (same VID/PID, interfaces and report descriptors). No driver or software
//! is needed on the PC. See docs/adr/0007.

pub mod configfs;
pub mod hidg;
pub mod sink;
