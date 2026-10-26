//! The one PC the bridge is bonded with: pairing, forgetting, reconnection
//! and the status the widget shows (ADR 0033).
//!
//! The state machine does not touch BlueZ itself: it calls a [`Bluez`]
//! implementation (`bluer` on Linux, a fake in tests) and is told what
//! happened through its `on_*` methods.

use std::time::{Duration, Instant};

/// A Bluetooth device address, most significant byte first (as printed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BtAddr(pub [u8; 6]);

impl std::fmt::Display for BtAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl std::str::FromStr for BtAddr {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return Err(format!("{s:?} is not a Bluetooth address"));
        }
        let mut b = [0u8; 6];
        for (i, p) in parts.iter().enumerate() {
            b[i] = u8::from_str_radix(p, 16).map_err(|_| format!("{s:?}: bad byte {p:?}"))?;
        }
        Ok(BtAddr(b))
    }
}

/// What the state machine asks of BlueZ.
#[allow(async_fn_in_trait)]
pub trait Bluez {
    /// Discoverable and pairable for `secs`, or neither (`None`).
    async fn set_discoverable(&mut self, secs: Option<u32>) -> anyhow::Result<()>;
    /// Remove the bond with `addr` (BlueZ forgets its keys).
    async fn remove(&mut self, addr: BtAddr) -> anyhow::Result<()>;
    /// Open the control and interrupt channels to `addr` (the device
    /// reconnecting, HIDReconnectInitiate). Success is reported through
    /// [`Host::on_connected`].
    async fn connect(&mut self, addr: BtAddr) -> anyhow::Result<()>;
}

/// FNV-1a of a report descriptor: stable across builds and runs, to tell
/// whether the bonded PC saw the descriptor in force.
pub fn descriptor_hash(desc: &[u8]) -> u64 {
    desc.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// What survives a restart of the agent (`/var/lib/rawkvm/agent-bt.toml`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Saved {
    pub bonded: Option<(BtAddr, String)>,
    /// Descriptor the bonded PC paired with.
    pub paired_descriptor: Option<u64>,
}

/// Status for the hub and the widgets (`BtStatus` in the protocol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Name (or address) of the bonded PC.
    pub bonded: Option<String>,
    pub connected: bool,
    /// Seconds left of discoverability.
    pub discoverable_secs: Option<u32>,
    /// The descriptor changed since the PC paired: forget the device on the
    /// PC and pair again. Input is not sent meanwhile.
    pub repair_needed: bool,
}

/// Delays between reconnection attempts; the last one repeats.
pub const RECONNECT_BACKOFF: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

#[derive(Debug)]
pub struct Host {
    saved: Saved,
    current_descriptor: Option<u64>,
    connected: bool,
    discoverable_until: Option<Instant>,
    next_attempt: Option<Instant>,
    attempts: usize,
    /// Saved state changed: the caller writes it.
    dirty: bool,
}

impl Host {
    pub fn new(saved: Saved) -> Self {
        Self {
            saved,
            current_descriptor: None,
            connected: false,
            discoverable_until: None,
            next_attempt: None,
            attempts: 0,
            dirty: false,
        }
    }

    pub fn saved(&self) -> &Saved {
        &self.saved
    }

    /// Whether the saved state changed since the last call.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn status(&self, now: Instant) -> Status {
        Status {
            bonded: self.saved.bonded.as_ref().map(|(a, n)| {
                if n.is_empty() {
                    a.to_string()
                } else {
                    n.clone()
                }
            }),
            connected: self.connected,
            discoverable_secs: self
                .discoverable_until
                .filter(|t| *t > now)
                .map(|t| (t - now).as_secs_f64().ceil() as u32),
            repair_needed: self.repair_needed(),
        }
    }

    fn repair_needed(&self) -> bool {
        self.saved.bonded.is_some()
            && self.current_descriptor.is_some()
            && self.saved.paired_descriptor != self.current_descriptor
    }

    /// Input may be sent: a connected PC that knows the descriptor.
    pub fn can_send(&self) -> bool {
        self.connected && !self.repair_needed()
    }

    /// The descriptor in force (a new `DeviceList` or exposure).
    pub fn set_descriptor(&mut self, hash: u64) {
        self.current_descriptor = Some(hash);
    }

    /// The widget asked for pairing (`secs` > 0) or to stop it (0).
    pub async fn discoverable(
        &mut self,
        bz: &mut impl Bluez,
        secs: u8,
        now: Instant,
    ) -> anyhow::Result<()> {
        if secs == 0 {
            bz.set_discoverable(None).await?;
            self.discoverable_until = None;
        } else {
            bz.set_discoverable(Some(secs.into())).await?;
            self.discoverable_until = Some(now + Duration::from_secs(secs.into()));
        }
        Ok(())
    }

    /// The widget asked to forget the PC.
    pub async fn forget(&mut self, bz: &mut impl Bluez) -> anyhow::Result<()> {
        if let Some((addr, _)) = self.saved.bonded.take() {
            self.saved.paired_descriptor = None;
            self.connected = false;
            self.next_attempt = None;
            self.dirty = true;
            bz.remove(addr).await?;
        }
        Ok(())
    }

    /// A PC paired. One host at a time: an earlier one is forgotten.
    pub async fn on_paired(
        &mut self,
        bz: &mut impl Bluez,
        addr: BtAddr,
        name: String,
    ) -> anyhow::Result<()> {
        let old = self.saved.bonded.replace((addr, name));
        self.saved.paired_descriptor = self.current_descriptor;
        self.dirty = true;
        self.discoverable_until = None;
        bz.set_discoverable(None).await?;
        if let Some((old, _)) = old.filter(|(a, _)| *a != addr) {
            bz.remove(old).await?;
        }
        Ok(())
    }

    /// Both channels are open with `addr`. Returns false (refuse) if it is
    /// not the bonded PC.
    pub fn on_connected(&mut self, addr: BtAddr) -> bool {
        if self.saved.bonded.as_ref().map(|(a, _)| *a) != Some(addr) {
            return false;
        }
        self.connected = true;
        self.next_attempt = None;
        self.attempts = 0;
        true
    }

    /// The channels closed (PC asleep, off, out of range).
    pub fn on_disconnected(&mut self, now: Instant) {
        self.connected = false;
        self.attempts = 0;
        if self.saved.bonded.is_some() {
            self.next_attempt = Some(now + RECONNECT_BACKOFF[0]);
        }
    }

    /// The PC removed the virtual cable: it forgot us, so we forget it.
    pub async fn on_unplugged(&mut self, bz: &mut impl Bluez) -> anyhow::Result<()> {
        self.connected = false;
        self.forget(bz).await
    }

    /// Periodic: expire discoverability, try to reconnect when due.
    pub async fn tick(&mut self, bz: &mut impl Bluez, now: Instant) {
        if self.discoverable_until.is_some_and(|t| t <= now) {
            self.discoverable_until = None;
        }
        let Some((addr, _)) = self.saved.bonded.clone() else {
            return;
        };
        if self.connected || self.discoverable_until.is_some() {
            return;
        }
        let due = *self.next_attempt.get_or_insert(now);
        if now < due {
            return;
        }
        let delay = RECONNECT_BACKOFF[self.attempts.min(RECONNECT_BACKOFF.len() - 1)];
        self.attempts += 1;
        self.next_attempt = Some(now + delay);
        if let Err(e) = bz.connect(addr).await {
            tracing::debug!(%addr, "reconnection attempt failed: {e:#}");
        }
    }

    /// When the next reconnection attempt is due, if any.
    pub fn next_attempt(&self) -> Option<Instant> {
        self.next_attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBluez {
        calls: Vec<String>,
        fail_connect: bool,
    }

    impl Bluez for FakeBluez {
        async fn set_discoverable(&mut self, secs: Option<u32>) -> anyhow::Result<()> {
            self.calls.push(format!("discoverable {secs:?}"));
            Ok(())
        }
        async fn remove(&mut self, addr: BtAddr) -> anyhow::Result<()> {
            self.calls.push(format!("remove {addr}"));
            Ok(())
        }
        async fn connect(&mut self, addr: BtAddr) -> anyhow::Result<()> {
            self.calls.push(format!("connect {addr}"));
            if self.fail_connect {
                anyhow::bail!("page timeout");
            }
            Ok(())
        }
    }

    const PC: BtAddr = BtAddr([0xAA, 0xBB, 0xCC, 0x00, 0x11, 0x22]);
    const MAC: BtAddr = BtAddr([0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn addresses_print_and_parse() {
        assert_eq!(PC.to_string(), "AA:BB:CC:00:11:22");
        assert_eq!("aa:bb:cc:00:11:22".parse::<BtAddr>(), Ok(PC));
        assert!("aa:bb".parse::<BtAddr>().is_err());
    }

    #[test]
    fn pairing_a_second_pc_forgets_the_first() {
        block(async {
            let mut bz = FakeBluez::default();
            let mut h = Host::new(Saved::default());
            let t0 = Instant::now();
            h.set_descriptor(1);
            h.discoverable(&mut bz, 60, t0).await.unwrap();
            assert_eq!(
                h.status(t0 + Duration::from_secs(10)).discoverable_secs,
                Some(50)
            );
            h.on_paired(&mut bz, PC, "GAMER".into()).await.unwrap();
            assert!(h.take_dirty());
            assert!(h.on_connected(PC));
            let st = h.status(t0);
            assert_eq!(st.bonded.as_deref(), Some("GAMER"));
            assert!(st.connected && !st.repair_needed && st.discoverable_secs.is_none());
            assert!(h.can_send());

            h.discoverable(&mut bz, 60, t0).await.unwrap();
            h.on_paired(&mut bz, MAC, String::new()).await.unwrap();
            assert_eq!(
                bz.calls,
                [
                    "discoverable Some(60)",
                    "discoverable None",
                    "discoverable Some(60)",
                    "discoverable None",
                    "remove AA:BB:CC:00:11:22"
                ]
            );
            // A connection from the forgotten PC is refused.
            assert!(!h.on_connected(PC));
            assert_eq!(h.status(t0).bonded.as_deref(), Some("10:20:30:40:50:60"));
        });
    }

    #[test]
    fn a_new_descriptor_needs_pairing_again() {
        block(async {
            let mut bz = FakeBluez::default();
            let mut h = Host::new(Saved::default());
            h.set_descriptor(descriptor_hash(&[1, 2, 3]));
            h.on_paired(&mut bz, PC, "GAMER".into()).await.unwrap();
            h.on_connected(PC);
            assert!(h.can_send());
            h.set_descriptor(descriptor_hash(&[1, 2, 3, 4]));
            assert!(h.status(Instant::now()).repair_needed);
            assert!(!h.can_send());
            // Back to the old one (the receiver came back as it was).
            h.set_descriptor(descriptor_hash(&[1, 2, 3]));
            assert!(h.can_send());
            // After a restart: the saved hash is compared again.
            let saved = h.saved().clone();
            let mut h = Host::new(saved);
            h.set_descriptor(descriptor_hash(&[9]));
            assert!(h.status(Instant::now()).repair_needed);
        });
    }

    #[test]
    fn reconnection_backs_off_and_stops_when_connected() {
        block(async {
            let mut bz = FakeBluez {
                fail_connect: true,
                ..Default::default()
            };
            let mut h = Host::new(Saved {
                bonded: Some((PC, "GAMER".into())),
                paired_descriptor: Some(1),
            });
            h.set_descriptor(1);
            let t0 = Instant::now();
            // Not connected after a restart: try at once.
            h.tick(&mut bz, t0).await;
            assert_eq!(bz.calls.len(), 1);
            // Not again before the delay.
            h.tick(&mut bz, t0 + Duration::from_millis(500)).await;
            assert_eq!(bz.calls.len(), 1);
            let mut t = t0;
            let mut gaps = Vec::new();
            for _ in 0..7 {
                let due = h.next_attempt().unwrap();
                gaps.push((due - t).as_secs());
                t = due;
                h.tick(&mut bz, t).await;
            }
            assert_eq!(gaps, [1, 2, 5, 10, 30, 60, 60]);

            assert!(h.on_connected(PC));
            h.tick(&mut bz, t + Duration::from_secs(600)).await;
            assert_eq!(bz.calls.len(), 8);
            assert!(h.next_attempt().is_none());

            // The PC goes to sleep: reconnect after a second.
            h.on_disconnected(t);
            assert_eq!(h.next_attempt(), Some(t + Duration::from_secs(1)));
            // Not while discoverable for another PC.
            h.discoverable(&mut bz, 30, t).await.unwrap();
            h.tick(&mut bz, t + Duration::from_secs(5)).await;
            assert!(!bz.calls.last().unwrap().starts_with("connect"));
        });
    }

    #[test]
    fn forget_and_unplug_clear_the_bond() {
        block(async {
            let mut bz = FakeBluez::default();
            let mut h = Host::new(Saved {
                bonded: Some((PC, "GAMER".into())),
                paired_descriptor: Some(1),
            });
            h.forget(&mut bz).await.unwrap();
            assert_eq!(bz.calls, ["remove AA:BB:CC:00:11:22"]);
            assert_eq!(h.saved(), &Saved::default());
            assert!(h.take_dirty());
            h.tick(&mut bz, Instant::now()).await;
            assert_eq!(bz.calls.len(), 1); // nothing to reconnect to

            let mut h = Host::new(Saved {
                bonded: Some((PC, String::new())),
                paired_descriptor: Some(1),
            });
            h.on_connected(PC);
            h.on_unplugged(&mut bz).await.unwrap();
            assert!(h.status(Instant::now()).bonded.is_none());
            assert_eq!(bz.calls.len(), 2);
        });
    }
}
