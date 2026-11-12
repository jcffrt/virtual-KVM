//! Light RawKVM link: what a microcontroller node (the ESP32-S3 USB
//! gadget) speaks with the hub instead of QUIC.
//!
//! Transport: one TCP connection, hub port [`DEFAULT_PORT`].
//!
//! ```text
//! node -> hub  MAGIC + node nonce (16 bytes)      plaintext
//! hub  -> node MAGIC + hub nonce (16 bytes)       plaintext
//! then, both ways, frames: u16 LE length + ChaCha20-Poly1305(postcard msg)
//! ```
//!
//! Keys: HKDF-SHA256 over the hub's pairing token (the same 32 bytes every
//! agent presents), salted with both nonces, one key per direction. The
//! AEAD nonce is a per-direction frame counter. Fresh nonces on both sides
//! make every session's keys new, so a recorded session cannot be replayed;
//! a node without the token cannot produce a frame the hub accepts. The
//! first node frame must be [`Up::Hello`].
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Version of this protocol. The hub rejects a node with another one.
pub const VERSION: u16 = 1;
pub const DEFAULT_PORT: u16 = 4434;
pub const MAGIC: [u8; 4] = *b"RKL1";
pub const NONCE_LEN: usize = 16;
pub const PRELUDE_LEN: usize = MAGIC.len() + NONCE_LEN;
pub const TOKEN_LEN: usize = 32;
/// Largest encrypted frame body (ciphertext + tag).
pub const MAX_FRAME: usize = 8192;
pub const TAG_LEN: usize = 16;
/// Largest report the board writes to one USB interface.
pub const MAX_REPORT: usize = 64;

/// The PC's operating system, as the hub's keyboard profile needs it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

/// Node -> hub.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Up {
    /// First frame of every session.
    Hello {
        version: u16,
        node_id: [u8; 16],
        name: String,
        os: Os,
        firmware: String,
        /// HID interfaces the board can present.
        max_interfaces: u8,
    },
    Ping(u32),
    Pong(u32),
    /// An output report the PC sent to interface `dev_id` (keyboard LEDs),
    /// report ID first only if the interface uses IDs.
    Output { dev_id: u8, report: Vec<u8> },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Version,
    TooManyNodes,
}

/// Hub -> node.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Down {
    Welcome { hub_name: String, index: u8 },
    /// Followed by the hub closing the connection.
    Reject(Reject),
    /// What to present on USB. Sent on connect and when it changes.
    Gadget(Gadget),
    /// An input report for interface `dev_id`, as the receiver produced it.
    Report { dev_id: u8, report: Vec<u8> },
    Ping(u32),
    Pong(u32),
}

/// The USB device the board presents: the receiver's identity and the
/// interfaces the hub chose for it (ADR 0031 priorities).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Gadget {
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: String,
    pub product: String,
    pub interfaces: Vec<Interface>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// Hub device id; reports for it carry the same id.
    pub dev_id: u8,
    pub report_descriptor: Vec<u8>,
    /// Boot subclass and protocol: 1/1 keyboard, 1/2 mouse, 0/0 otherwise.
    pub subclass: u8,
    pub protocol: u8,
    /// Largest input report, report ID included.
    pub max_input: u16,
    /// "Nothing pressed" reports, written when the hub link drops.
    pub release: Vec<Vec<u8>>,
}

impl Gadget {
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("serializing to a Vec cannot fail")
    }
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        postcard::from_bytes(b).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Wrong key (the token differs) or a tampered or replayed frame.
    Auth,
    Decode,
    TooLong,
}

pub fn prelude(nonce: &[u8; NONCE_LEN]) -> [u8; PRELUDE_LEN] {
    let mut p = [0; PRELUDE_LEN];
    p[..4].copy_from_slice(&MAGIC);
    p[4..].copy_from_slice(nonce);
    p
}

/// The peer's nonce, or `None` if this is not a light-link prelude.
pub fn parse_prelude(p: &[u8; PRELUDE_LEN]) -> Option<[u8; NONCE_LEN]> {
    (p[..4] == MAGIC).then(|| p[4..].try_into().expect("16 bytes"))
}

/// One direction of a session.
pub struct Cipher {
    aead: ChaCha20Poly1305,
    counter: u64,
}

impl Cipher {
    fn new(key: &[u8]) -> Self {
        Self {
            aead: ChaCha20Poly1305::new(Key::from_slice(key)),
            counter: 0,
        }
    }

    fn nonce(&mut self) -> [u8; 12] {
        let mut n = [0; 12];
        n[..8].copy_from_slice(&self.counter.to_le_bytes());
        self.counter += 1;
        n
    }

    /// A whole frame (length prefix included) carrying `msg`.
    pub fn seal<T: Serialize>(&mut self, msg: &T) -> Result<Vec<u8>, Error> {
        let plain = postcard::to_allocvec(msg).map_err(|_| Error::Decode)?;
        let n = self.nonce();
        let body = self
            .aead
            .encrypt(Nonce::from_slice(&n), plain.as_slice())
            .map_err(|_| Error::Auth)?;
        if body.len() > MAX_FRAME {
            return Err(Error::TooLong);
        }
        let mut out = Vec::with_capacity(2 + body.len());
        out.extend_from_slice(&(body.len() as u16).to_le_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decrypt a frame body (after its length prefix).
    pub fn open<T: DeserializeOwned>(&mut self, body: &[u8]) -> Result<T, Error> {
        let n = self.nonce();
        let plain = self
            .aead
            .decrypt(Nonce::from_slice(&n), body)
            .map_err(|_| Error::Auth)?;
        postcard::from_bytes(&plain).map_err(|_| Error::Decode)
    }
}

/// Frame length from its 2-byte prefix, if acceptable.
pub fn frame_len(prefix: [u8; 2]) -> Result<usize, Error> {
    let n = u16::from_le_bytes(prefix) as usize;
    if n < TAG_LEN || n > MAX_FRAME {
        return Err(Error::TooLong);
    }
    Ok(n)
}

fn keys(
    token: &[u8; TOKEN_LEN],
    node_nonce: &[u8; NONCE_LEN],
    hub_nonce: &[u8; NONCE_LEN],
) -> ([u8; 32], [u8; 32]) {
    let mut salt = [0u8; 2 * NONCE_LEN];
    salt[..NONCE_LEN].copy_from_slice(node_nonce);
    salt[NONCE_LEN..].copy_from_slice(hub_nonce);
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), token);
    let mut okm = [0u8; 64];
    hk.expand(b"rawkvm light v1", &mut okm)
        .expect("64 bytes is a valid HKDF-SHA256 length");
    (
        okm[..32].try_into().expect("32"),
        okm[32..].try_into().expect("32"),
    )
}

/// The node's (send, receive) ciphers.
pub fn node_ciphers(
    token: &[u8; TOKEN_LEN],
    node_nonce: &[u8; NONCE_LEN],
    hub_nonce: &[u8; NONCE_LEN],
) -> (Cipher, Cipher) {
    let (up, down) = keys(token, node_nonce, hub_nonce);
    (Cipher::new(&up), Cipher::new(&down))
}

/// The hub's (send, receive) ciphers.
pub fn hub_ciphers(
    token: &[u8; TOKEN_LEN],
    node_nonce: &[u8; NONCE_LEN],
    hub_nonce: &[u8; NONCE_LEN],
) -> (Cipher, Cipher) {
    let (up, down) = keys(token, node_nonce, hub_nonce);
    (Cipher::new(&down), Cipher::new(&up))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::ToString, vec};

    fn hello() -> Up {
        Up::Hello {
            version: VERSION,
            node_id: [9; 16],
            name: "GAMER".to_string(),
            os: Os::Windows,
            firmware: "0.2.0".to_string(),
            max_interfaces: 4,
        }
    }

    fn body(frame: &[u8]) -> &[u8] {
        assert_eq!(frame_len([frame[0], frame[1]]).unwrap(), frame.len() - 2);
        &frame[2..]
    }

    #[test]
    fn both_directions_round_trip() {
        let t = [7u8; 32];
        let (nn, hn) = ([1u8; 16], [2u8; 16]);
        let (mut node_tx, mut node_rx) = node_ciphers(&t, &nn, &hn);
        let (mut hub_tx, mut hub_rx) = hub_ciphers(&t, &nn, &hn);
        for _ in 0..3 {
            let f = node_tx.seal(&hello()).unwrap();
            assert_eq!(hub_rx.open::<Up>(body(&f)).unwrap(), hello());
            let r = Down::Report {
                dev_id: 1,
                report: vec![2, 0, 0, 5, 0, 0, 0],
            };
            let f = hub_tx.seal(&r).unwrap();
            assert_eq!(node_rx.open::<Down>(body(&f)).unwrap(), r);
        }
    }

    #[test]
    fn wrong_token_replay_and_reflection_fail() {
        let (nn, hn) = ([1u8; 16], [2u8; 16]);
        let (mut node_tx, _) = node_ciphers(&[7; 32], &nn, &hn);
        let f = node_tx.seal(&hello()).unwrap();

        let (_, mut bad) = hub_ciphers(&[8; 32], &nn, &hn);
        assert_eq!(bad.open::<Up>(body(&f)), Err(Error::Auth));

        // The same frame in a session with another hub nonce.
        let (_, mut other) = hub_ciphers(&[7; 32], &nn, &[3; 16]);
        assert_eq!(other.open::<Up>(body(&f)), Err(Error::Auth));

        // Replayed within the session: the counter has moved on.
        let (_, mut rx) = hub_ciphers(&[7; 32], &nn, &hn);
        rx.open::<Up>(body(&f)).unwrap();
        assert_eq!(rx.open::<Up>(body(&f)), Err(Error::Auth));

        // Reflected back to the node: the directions use different keys.
        let (_, mut node_rx) = node_ciphers(&[7; 32], &nn, &hn);
        assert_eq!(node_rx.open::<Down>(body(&f)), Err(Error::Auth));
    }

    #[test]
    fn prelude_and_lengths() {
        let p = prelude(&[5; 16]);
        assert_eq!(parse_prelude(&p), Some([5; 16]));
        let mut q = p;
        q[0] = b'X';
        assert_eq!(parse_prelude(&q), None);
        assert!(frame_len([0, 0]).is_err());
        assert!(frame_len(((MAX_FRAME + 1) as u16).to_le_bytes()).is_err());
    }

    #[test]
    fn gadget_bytes_round_trip() {
        let g = Gadget {
            vid: 0x046D,
            pid: 0xC52B,
            manufacturer: "Logitech".to_string(),
            product: "USB Receiver".to_string(),
            interfaces: vec![Interface {
                dev_id: 0,
                report_descriptor: vec![5, 1, 9, 6],
                subclass: 1,
                protocol: 1,
                max_input: 8,
                release: vec![vec![0; 8]],
            }],
        };
        assert_eq!(Gadget::from_bytes(&g.to_bytes()), Some(g));
        assert_eq!(Gadget::from_bytes(&[0xFF]), None);
    }
}
