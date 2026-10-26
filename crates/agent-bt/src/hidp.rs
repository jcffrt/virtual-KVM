//! HIDP transactions (Bluetooth HID Profile 1.1, §3.1): the one-byte
//! header of every message on the control and interrupt channels, and the
//! device's answers.

/// Header type nibble.
const HANDSHAKE: u8 = 0x0;
const HID_CONTROL: u8 = 0x1;
const GET_REPORT: u8 = 0x4;
const SET_REPORT: u8 = 0x5;
const GET_PROTOCOL: u8 = 0x6;
const SET_PROTOCOL: u8 = 0x7;
const DATA: u8 = 0xA;

/// Report type in the parameter nibble.
pub const REPORT_INPUT: u8 = 0x1;
pub const REPORT_OUTPUT: u8 = 0x2;
pub const REPORT_FEATURE: u8 = 0x3;

/// HANDSHAKE result codes.
pub const RESULT_SUCCESSFUL: u8 = 0x0;
pub const RESULT_ERR_INVALID_REPORT_ID: u8 = 0x2;
pub const RESULT_ERR_UNSUPPORTED_REQUEST: u8 = 0x3;

/// HID_CONTROL operations.
pub const CONTROL_SUSPEND: u8 = 0x3;
pub const CONTROL_EXIT_SUSPEND: u8 = 0x4;
pub const CONTROL_VIRTUAL_CABLE_UNPLUG: u8 = 0x5;

/// An input report on the interrupt channel: `DATA | Input`, then the
/// report with its ID byte.
pub fn input(report: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(report.len() + 1);
    v.push(DATA << 4 | REPORT_INPUT);
    v.extend_from_slice(report);
    v
}

/// A message from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMsg {
    Control(u8),
    /// `size`: the host's buffer size, when it sent one.
    GetReport {
        rtype: u8,
        id: Option<u8>,
        size: Option<u16>,
    },
    SetReport {
        rtype: u8,
        data: Vec<u8>,
    },
    GetProtocol,
    /// 0 boot, 1 report.
    SetProtocol(u8),
    Data {
        rtype: u8,
        data: Vec<u8>,
    },
    Other(u8),
}

/// Decode a message. `with_report_ids`: whether GET_REPORT carries a
/// report ID byte (it does when the descriptor declares IDs).
pub fn decode(msg: &[u8], with_report_ids: bool) -> Option<HostMsg> {
    let (&h, rest) = msg.split_first()?;
    let (kind, param) = (h >> 4, h & 0x0F);
    Some(match kind {
        HID_CONTROL => HostMsg::Control(param),
        GET_REPORT => {
            let rtype = param & 0x3;
            let (id, rest) = if with_report_ids {
                match rest.split_first() {
                    Some((&id, r)) => (Some(id), r),
                    None => (None, rest),
                }
            } else {
                (None, rest)
            };
            let size = (param & 0x8 != 0)
                .then(|| rest.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]])))
                .flatten();
            HostMsg::GetReport { rtype, id, size }
        }
        SET_REPORT => HostMsg::SetReport {
            rtype: param & 0x3,
            data: rest.to_vec(),
        },
        GET_PROTOCOL => HostMsg::GetProtocol,
        SET_PROTOCOL => HostMsg::SetProtocol(param & 0x1),
        DATA => HostMsg::Data {
            rtype: param & 0x3,
            data: rest.to_vec(),
        },
        _ => HostMsg::Other(h),
    })
}

pub fn handshake(result: u8) -> Vec<u8> {
    vec![HANDSHAKE << 4 | result]
}

/// What the device does about a message on the control channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    /// Answer on the control channel.
    pub reply: Option<Vec<u8>>,
    /// An output report (LEDs) for the hub, with its ID byte.
    pub output: Option<Vec<u8>>,
    /// The host removed the virtual cable: forget it.
    pub unplug: bool,
}

/// React to a control channel message. `current`: the last input report of
/// a report ID (with its ID byte), for GET_REPORT; `None` answers
/// "invalid report ID".
pub fn on_control(msg: &HostMsg, current: impl Fn(Option<u8>) -> Option<Vec<u8>>) -> Reaction {
    let mut r = Reaction {
        reply: None,
        output: None,
        unplug: false,
    };
    match msg {
        HostMsg::Control(CONTROL_VIRTUAL_CABLE_UNPLUG) => r.unplug = true,
        HostMsg::Control(_) => {} // suspend / exit suspend: no answer
        HostMsg::SetReport { rtype, data } => {
            if *rtype == REPORT_OUTPUT {
                r.output = Some(data.clone());
            }
            r.reply = Some(handshake(RESULT_SUCCESSFUL));
        }
        HostMsg::SetProtocol(_) => r.reply = Some(handshake(RESULT_SUCCESSFUL)),
        // Always the report protocol: HIDBootDevice is false.
        HostMsg::GetProtocol => r.reply = Some(vec![DATA << 4, 0x01]),
        HostMsg::GetReport { rtype, id, size } if *rtype == REPORT_INPUT => {
            r.reply = Some(match current(*id) {
                Some(mut report) => {
                    if let Some(s) = size {
                        report.truncate(*s as usize);
                    }
                    let mut v = vec![DATA << 4 | REPORT_INPUT];
                    v.append(&mut report);
                    v
                }
                None => handshake(RESULT_ERR_INVALID_REPORT_ID),
            })
        }
        HostMsg::GetReport { .. } | HostMsg::Other(_) => {
            r.reply = Some(handshake(RESULT_ERR_UNSUPPORTED_REQUEST))
        }
        // DATA on the control channel is deprecated; take outputs anyway.
        HostMsg::Data { rtype, data } => {
            if *rtype == REPORT_OUTPUT {
                r.output = Some(data.clone());
            }
        }
    }
    r
}

/// An output report on the interrupt channel (`DATA | Output`).
pub fn interrupt_output(msg: &[u8]) -> Option<Vec<u8>> {
    match decode(msg, false)? {
        HostMsg::Data {
            rtype: REPORT_OUTPUT,
            data,
        } => Some(data),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_reports_get_the_data_header() {
        assert_eq!(input(&[2, 0, 5, 0xFB, 0]), [0xA1, 2, 0, 5, 0xFB, 0]);
    }

    #[test]
    fn led_output_from_either_channel() {
        // Windows: SET_REPORT(Output) on the control channel.
        let m = decode(&[0x52, 0x01, 0x02], true).unwrap();
        let r = on_control(&m, |_| None);
        assert_eq!(r.output, Some(vec![0x01, 0x02]));
        assert_eq!(r.reply, Some(vec![0x00]));
        // Linux, macOS: DATA(Output) on the interrupt channel.
        assert_eq!(
            interrupt_output(&[0xA2, 0x01, 0x02]),
            Some(vec![0x01, 0x02])
        );
        assert_eq!(interrupt_output(&[0xA1, 0x01, 0x02]), None);
    }

    #[test]
    fn get_report_answers_the_current_state_or_an_error() {
        let m = decode(&[0x49, 0x02, 0x03, 0x00], true).unwrap();
        assert_eq!(
            m,
            HostMsg::GetReport {
                rtype: REPORT_INPUT,
                id: Some(2),
                size: Some(3)
            }
        );
        let r = on_control(&m, |id| (id == Some(2)).then(|| vec![2, 0, 0, 0, 0]));
        assert_eq!(r.reply, Some(vec![0xA1, 2, 0, 0])); // truncated to 3
        let m = decode(&[0x41, 0x07], true).unwrap();
        assert_eq!(on_control(&m, |_| None).reply, Some(vec![0x02]));
        let m = decode(&[0x43, 0x01], true).unwrap(); // feature
        assert_eq!(on_control(&m, |_| None).reply, Some(vec![0x03]));
    }

    #[test]
    fn protocol_and_control() {
        let r = on_control(&decode(&[0x70], true).unwrap(), |_| None);
        assert_eq!(r.reply, Some(vec![0x00]));
        let r = on_control(&decode(&[0x60], true).unwrap(), |_| None);
        assert_eq!(r.reply, Some(vec![0xA0, 0x01]));
        let r = on_control(&decode(&[0x15], true).unwrap(), |_| None);
        assert!(r.unplug && r.reply.is_none());
        let r = on_control(&decode(&[0x13], true).unwrap(), |_| None);
        assert!(!r.unplug && r.reply.is_none());
        assert_eq!(
            on_control(&decode(&[0x90], true).unwrap(), |_| None).reply,
            Some(vec![0x03])
        );
    }
}
