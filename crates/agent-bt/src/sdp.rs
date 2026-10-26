//! The SDP record of a HID device (Bluetooth HID Profile 1.1, §5.3), in the
//! XML that BlueZ's `ProfileManager1.RegisterProfile` takes as
//! `ServiceRecord`. The report descriptor travels whole in it (0x0206).

use std::fmt::Write;

/// L2CAP PSM of the HID control channel.
pub const PSM_CONTROL: u16 = 0x0011;
/// L2CAP PSM of the HID interrupt channel.
pub const PSM_INTERRUPT: u16 = 0x0013;
/// Human Interface Device service class.
pub const HID_UUID16: u16 = 0x1124;
pub const HID_UUID: &str = "00001124-0000-1000-8000-00805f9b34fb";

const UUID_L2CAP: u16 = 0x0100;
const UUID_HIDP: u16 = 0x0011;
const PUBLIC_BROWSE_GROUP: u16 = 0x1002;

/// HIDDeviceSubclass (and the minor class of device): keyboard and
/// pointing device together.
pub const SUBCLASS_COMBO: u8 = 0xC0;

#[derive(Debug, Clone, PartialEq)]
enum Element {
    U8(u8),
    U16(u16),
    Bool(bool),
    Uuid16(u16),
    Text(String),
    Hex(Vec<u8>),
    Seq(Vec<Element>),
}

use Element::*;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl Element {
    fn write(&self, out: &mut String) {
        match self {
            U8(v) => write!(out, r#"<uint8 value="0x{v:02x}"/>"#),
            U16(v) => write!(out, r#"<uint16 value="0x{v:04x}"/>"#),
            Bool(v) => write!(out, r#"<boolean value="{v}"/>"#),
            Uuid16(v) => write!(out, r#"<uuid value="0x{v:04x}"/>"#),
            Text(s) => write!(out, r#"<text value="{}"/>"#, escape(s)),
            Hex(b) => {
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                write!(out, r#"<text encoding="hex" value="{hex}"/>"#)
            }
            Seq(items) => {
                out.push_str("<sequence>");
                for i in items {
                    i.write(out);
                }
                out.push_str("</sequence>");
                Ok(())
            }
        }
        .expect("writing to a String");
    }
}

/// What varies between HID devices in the record.
#[derive(Debug, Clone)]
pub struct HidRecord {
    /// ServiceName: what the PC shows next to the device.
    pub name: String,
    pub description: String,
    pub provider: String,
    pub subclass: u8,
    /// The report descriptor, whole.
    pub descriptor: Vec<u8>,
}

impl HidRecord {
    fn attributes(&self) -> Vec<(u16, Element)> {
        vec![
            (0x0001, Seq(vec![Uuid16(HID_UUID16)])),
            (
                0x0004,
                Seq(vec![
                    Seq(vec![Uuid16(UUID_L2CAP), U16(PSM_CONTROL)]),
                    Seq(vec![Uuid16(UUID_HIDP)]),
                ]),
            ),
            (0x0005, Seq(vec![Uuid16(PUBLIC_BROWSE_GROUP)])),
            (0x0006, Seq(vec![U16(0x656e), U16(0x006a), U16(0x0100)])),
            (
                0x0009,
                Seq(vec![Seq(vec![Uuid16(HID_UUID16), U16(0x0101)])]),
            ),
            (
                0x000d,
                Seq(vec![Seq(vec![
                    Seq(vec![Uuid16(UUID_L2CAP), U16(PSM_INTERRUPT)]),
                    Seq(vec![Uuid16(UUID_HIDP)]),
                ])]),
            ),
            (0x0100, Text(self.name.clone())),
            (0x0101, Text(self.description.clone())),
            (0x0102, Text(self.provider.clone())),
            (0x0200, U16(0x0100)), // HIDDeviceReleaseNumber (1.0 hosts)
            (0x0201, U16(0x0111)), // HIDParserVersion
            (0x0202, U8(self.subclass)),
            (0x0203, U8(0x00)),   // HIDCountryCode: not localized
            (0x0204, Bool(true)), // HIDVirtualCable
            (0x0205, Bool(true)), // HIDReconnectInitiate
            (
                0x0206,
                Seq(vec![Seq(vec![U8(0x22), Hex(self.descriptor.clone())])]),
            ),
            (0x0207, Seq(vec![Seq(vec![U16(0x0409), U16(0x0100)])])),
            (0x0209, Bool(true)),  // HIDBatteryPower
            (0x020a, Bool(true)),  // HIDRemoteWake
            (0x020b, U16(0x0100)), // HIDProfileVersion
            (0x020c, U16(0x0c80)), // HIDSupervisionTimeout (2 s)
            (0x020d, Bool(true)),  // HIDNormallyConnectable
            (0x020e, Bool(false)), // HIDBootDevice
            (0x020f, U16(0x0640)), // HIDSSRHostMaxLatency
            (0x0210, U16(0x0320)), // HIDSSRHostMinTimeout
        ]
    }

    /// The `ServiceRecord` XML for BlueZ.
    pub fn to_xml(&self) -> String {
        let mut out = String::from(r#"<?xml version="1.0" encoding="UTF-8" ?><record>"#);
        for (id, e) in self.attributes() {
            write!(out, r#"<attribute id="0x{id:04x}">"#).expect("String");
            e.write(&mut out);
            out.push_str("</attribute>");
        }
        out.push_str("</record>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> HidRecord {
        HidRecord {
            name: "RawKVM <hub> & co".into(),
            description: "Keyboard and mouse".into(),
            provider: "RawKVM".into(),
            subclass: SUBCLASS_COMBO,
            descriptor: vec![0x05, 0x01, 0xC0],
        }
    }

    #[test]
    fn xml_has_the_hid_attributes_in_order() {
        let x = record().to_xml();
        assert!(x.starts_with("<?xml"));
        assert!(x.ends_with("</record>"));
        // Ascending attribute ids, as SDP requires.
        let ids: Vec<u16> = x
            .match_indices(r#"<attribute id="0x"#)
            .map(|(i, m)| u16::from_str_radix(&x[i + m.len()..i + m.len() + 4], 16).unwrap())
            .collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:x?}");
        assert!(ids.contains(&0x0206) && ids.contains(&0x000d));
        // Descriptor, in hex, as report descriptor type 0x22.
        assert!(x.contains(
            r#"<attribute id="0x0206"><sequence><sequence><uint8 value="0x22"/><text encoding="hex" value="0501c0"/></sequence></sequence></attribute>"#
        ));
        // Both channels.
        assert!(x.contains(r#"<uuid value="0x0100"/><uint16 value="0x0011"/>"#));
        assert!(x.contains(r#"<uuid value="0x0100"/><uint16 value="0x0013"/>"#));
        assert!(x.contains(r#"<attribute id="0x0202"><uint8 value="0xc0"/>"#));
        // Text is escaped.
        assert!(x.contains("RawKVM &lt;hub&gt; &amp; co"));
    }

    #[test]
    fn xml_is_balanced() {
        let x = record().to_xml();
        assert_eq!(
            x.matches("<sequence>").count(),
            x.matches("</sequence>").count()
        );
        assert_eq!(
            x.matches("<attribute ").count(),
            x.matches("</attribute>").count()
        );
    }
}
