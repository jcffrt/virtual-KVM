//! The Bluetooth HID device side of the bridge, as the bridge task sees
//! it. Implemented with BlueZ (`bluer`) on Linux after the proof of
//! concept; faked in tests.

use bridge_core::host::{BtAddr, Radio};

/// What the stack tells the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackEvent {
    /// A PC paired (bonded) with us.
    Paired { addr: BtAddr, name: String },
    /// Control and interrupt channels open with `addr`.
    Connected(BtAddr),
    /// The channels closed.
    Disconnected,
    /// An output report from the PC (merged report ID first): LEDs.
    Output(Vec<u8>),
    /// The PC removed the virtual cable (it forgot us).
    Unplugged,
}

#[allow(async_fn_in_trait)]
pub trait HidStack: Radio {
    /// Publish the device with this report descriptor, replacing the one
    /// published before (BlueZ: the SDP record).
    async fn set_descriptor(&mut self, descriptor: &[u8]) -> anyhow::Result<()>;
    /// Send an input report (merged report ID first) to the connected PC.
    async fn send(&mut self, report: &[u8]) -> anyhow::Result<()>;
    /// Close the channels with a PC that is not the bonded one.
    async fn disconnect(&mut self) -> anyhow::Result<()>;
}
