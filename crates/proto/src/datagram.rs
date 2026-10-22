//! HID report datagram layout.
//!
//! ```text
//! +--------+--------+--------+--------+-----------------------------+
//! | dev_id | flags  |   seq (u16 LE)  | report bytes (incl. ID)     |
//! +--------+--------+--------+--------+-----------------------------+
//! ```
//!
//! `seq` is per device and wraps. Agents use it only to count losses.

use bytes::{BufMut, Bytes, BytesMut};

/// Header size in bytes.
pub const HEADER_LEN: usize = 4;

/// The report is a synthetic "all keys / buttons released" report generated
/// by the hub when it switches away from the receiving node.
pub const FLAG_RELEASE_ALL: u8 = 0x01;

/// Largest report the hub forwards. hidraw reads of a Unifying receiver never
/// exceed 64 bytes; QUIC datagrams on a typical MTU allow ~1200.
pub const MAX_REPORT: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram<'a> {
    pub dev_id: u8,
    pub flags: u8,
    pub seq: u16,
    pub report: &'a [u8],
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum DatagramError {
    #[error("datagram shorter than header")]
    TooShort,
    #[error("report of {0} bytes exceeds the maximum")]
    TooLong(usize),
}

impl<'a> Datagram<'a> {
    pub fn encode(&self) -> Result<Bytes, DatagramError> {
        if self.report.len() > MAX_REPORT {
            return Err(DatagramError::TooLong(self.report.len()));
        }
        let mut b = BytesMut::with_capacity(HEADER_LEN + self.report.len());
        b.put_u8(self.dev_id);
        b.put_u8(self.flags);
        b.put_u16_le(self.seq);
        b.put_slice(self.report);
        Ok(b.freeze())
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, DatagramError> {
        if bytes.len() < HEADER_LEN {
            return Err(DatagramError::TooShort);
        }
        let report = &bytes[HEADER_LEN..];
        if report.len() > MAX_REPORT {
            return Err(DatagramError::TooLong(report.len()));
        }
        Ok(Datagram {
            dev_id: bytes[0],
            flags: bytes[1],
            seq: u16::from_le_bytes([bytes[2], bytes[3]]),
            report,
        })
    }

    pub fn is_release_all(&self) -> bool {
        self.flags & FLAG_RELEASE_ALL != 0
    }
}

/// Tracks per-device sequence numbers on the receiving side and reports gaps.
#[derive(Debug, Clone)]
pub struct SeqTracker {
    last: [Option<u16>; 256],
    pub received: u64,
    pub lost: u64,
    pub reordered: u64,
}

impl Default for SeqTracker {
    fn default() -> Self {
        Self {
            last: [None; 256],
            received: 0,
            lost: 0,
            reordered: 0,
        }
    }
}

impl SeqTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a datagram. Returns the number of datagrams presumed lost
    /// before this one (0 if in order), or `None` if it is a duplicate or
    /// arrived after a newer one. Such datagrams must be dropped: the hub
    /// sends some reports twice, and replaying an older keyboard state after
    /// a newer one would leave keys pressed.
    pub fn observe(&mut self, dev_id: u8, seq: u16) -> Option<u16> {
        self.received += 1;
        let slot = &mut self.last[dev_id as usize];
        let gap = match *slot {
            None => 0,
            Some(prev) => {
                let delta = seq.wrapping_sub(prev);
                if delta == 0 || delta > u16::MAX / 2 {
                    self.reordered += 1;
                    return None;
                }
                delta - 1
            }
        };
        *slot = Some(seq);
        self.lost += gap as u64;
        Some(gap)
    }

    /// Forget the last sequence number of every device (new connection).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let d = Datagram {
            dev_id: 2,
            flags: FLAG_RELEASE_ALL,
            seq: 0xBEEF,
            report: &[0x02, 0x00, 0x10, 0xF0, 0x00],
        };
        let b = d.encode().unwrap();
        assert_eq!(b.len(), HEADER_LEN + 5);
        let back = Datagram::decode(&b).unwrap();
        assert_eq!(back, d);
        assert!(back.is_release_all());
        assert_eq!(Datagram::decode(&b[..3]), Err(DatagramError::TooShort));
    }

    #[test]
    fn seq_tracker_counts_gaps_and_wraps() {
        let mut t = SeqTracker::new();
        assert_eq!(t.observe(0, 1), Some(0));
        assert_eq!(t.observe(0, 2), Some(0));
        assert_eq!(t.observe(0, 5), Some(2));
        assert_eq!(t.observe(0, 4), None); // late
        assert_eq!(t.observe(0, 5), None); // duplicate
        assert_eq!(t.reordered, 2);
        assert_eq!(t.observe(1, 100), Some(0)); // other device independent
                                                // wrap-around on another device
        assert_eq!(t.observe(2, u16::MAX - 1), Some(0));
        assert_eq!(t.observe(2, u16::MAX), Some(0));
        assert_eq!(t.observe(2, 0), Some(0));
        assert_eq!(t.observe(2, 3), Some(2));
        assert_eq!(t.received, 10);
        assert_eq!(t.lost, 4);
    }
}
