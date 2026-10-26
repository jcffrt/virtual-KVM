//! The bridge's `HidSink`: what agent-core hands it goes to the bridge
//! task as commands, and what the task produces (LEDs, `BtStatus`) comes
//! back through agent-core's channels.

use std::time::Duration;

use agent_core::{ControlReceiver, ControlSender, HidSink, OutputReceiver, OutputSender};
use anyhow::Result;
use proto::{Control, HidDevice};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::bridge::Cmd;

/// Changing the descriptor costs the PC a new pairing; wait for the hub's
/// list to settle, as the gadget does.
pub const SETTLE_TIME: Duration = Duration::from_millis(500);

pub struct BridgeSink {
    cmds: UnboundedSender<Cmd>,
    pending: Vec<HidDevice>,
    outputs: Option<OutputReceiver>,
    control: Option<ControlReceiver>,
}

/// The bridge task's ends of the channels.
pub struct TaskEnds {
    pub cmds: UnboundedReceiver<Cmd>,
    pub outputs: OutputSender,
    pub to_hub: ControlSender,
}

impl BridgeSink {
    pub fn new() -> (Self, TaskEnds) {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (out_tx, out_rx) = unbounded_channel();
        let (ctl_tx, ctl_rx) = unbounded_channel();
        (
            Self {
                cmds: cmd_tx,
                pending: Vec::new(),
                outputs: Some(out_rx),
                control: Some(ctl_rx),
            },
            TaskEnds {
                cmds: cmd_rx,
                outputs: out_tx,
                to_hub: ctl_tx,
            },
        )
    }

    fn send(&self, c: Cmd) {
        // The task only ends with the process.
        let _ = self.cmds.send(c);
    }
}

impl HidSink for BridgeSink {
    fn create(&mut self, dev: &HidDevice) -> Result<()> {
        self.pending.push(dev.clone());
        Ok(())
    }

    fn settle_time(&self) -> Duration {
        SETTLE_TIME
    }

    fn devices_ready(&mut self) {
        self.send(Cmd::Devices(self.pending.clone()));
    }

    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()> {
        self.send(Cmd::Input(dev_id, report.to_vec()));
        Ok(())
    }

    /// A new list is coming, or the hub went away: the PC keeps the
    /// descriptor it knows (agent-core releases the keys itself).
    fn destroy_all(&mut self) {
        self.pending.clear();
    }

    fn take_outputs(&mut self) -> Option<OutputReceiver> {
        self.outputs.take()
    }

    fn take_control(&mut self) -> Option<ControlReceiver> {
        self.control.take()
    }

    fn connected(&mut self) {
        self.send(Cmd::Hello);
    }

    fn on_control(&mut self, msg: &Control) {
        match msg {
            Control::BtDiscoverable { secs, .. } => self.send(Cmd::Discoverable(*secs)),
            Control::BtForget { .. } => self.send(Cmd::Forget),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn hub_messages_become_commands() {
        let (mut sink, mut ends) = BridgeSink::new();
        let dev = HidDevice {
            id: 0,
            name: "k".into(),
            vid: 1,
            pid: 2,
            serial: None,
            interface: 0,
            report_descriptor: vec![],
            uses_report_ids: false,
            receiver: 0,
        };
        sink.create(&dev).unwrap();
        sink.devices_ready();
        sink.input(0, &[1, 2]).unwrap();
        sink.connected();
        let node = Uuid::new_v4();
        sink.on_control(&Control::BtDiscoverable { node, secs: 60 });
        sink.on_control(&Control::BtForget { node });
        sink.on_control(&Control::Ping(1));
        sink.destroy_all();
        sink.devices_ready();
        let got: Vec<Cmd> = std::iter::from_fn(|| ends.cmds.try_recv().ok()).collect();
        assert_eq!(
            got,
            [
                Cmd::Devices(vec![dev]),
                Cmd::Input(0, vec![1, 2]),
                Cmd::Hello,
                Cmd::Discoverable(60),
                Cmd::Forget,
                Cmd::Devices(vec![]),
            ]
        );
        assert!(sink.take_outputs().is_some() && sink.take_outputs().is_none());
        assert!(sink.take_control().is_some());
    }
}
