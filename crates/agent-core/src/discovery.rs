//! Find hubs on the LAN with mDNS / DNS-SD.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::debug;

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredHub {
    pub name: String,
    pub hub_id: Option<String>,
    /// Certificate fingerprint advertised in TXT `fp`. Only a hint: the
    /// fingerprint that counts is the one checked during the TLS handshake.
    pub fingerprint: Option<String>,
    pub proto_ver: Option<u16>,
    /// IPv4 first.
    pub addrs: Vec<SocketAddr>,
}

/// Browse for `wait` and return every hub that resolved.
pub async fn discover(wait: Duration) -> Result<Vec<DiscoveredHub>> {
    tokio::task::spawn_blocking(move || discover_blocking(wait)).await?
}

fn discover_blocking(wait: Duration) -> Result<Vec<DiscoveredHub>> {
    let daemon = ServiceDaemon::new()?;
    let rx = daemon.browse(proto::SERVICE_TYPE)?;
    let deadline = Instant::now() + wait;
    let mut found: Vec<DiscoveredHub> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = rx.recv_timeout(left) else {
            break;
        };
        if let ServiceEvent::ServiceResolved(info) = event {
            let port = info.get_port();
            let mut addrs: Vec<SocketAddr> = info
                .get_addresses()
                .iter()
                .map(|ip| SocketAddr::new(ip.to_ip_addr(), port))
                .collect();
            addrs.sort_by_key(|a| (!a.is_ipv4(), is_link_local(a.ip())));
            addrs.dedup();
            let hub = DiscoveredHub {
                name: info
                    .get_property_val_str("name")
                    .unwrap_or(info.get_fullname())
                    .to_string(),
                hub_id: info.get_property_val_str("id").map(str::to_string),
                fingerprint: info.get_property_val_str("fp").map(str::to_string),
                proto_ver: info
                    .get_property_val_str("ver")
                    .and_then(|v| v.parse().ok()),
                addrs,
            };
            debug!(?hub, "hub resolved");
            match found
                .iter_mut()
                .find(|h| h.hub_id == hub.hub_id && h.name == hub.name)
            {
                Some(h) => *h = hub,
                None => found.push(hub),
            }
        }
    }
    let _ = daemon.shutdown();
    Ok(found)
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Choose the hub to connect to: the one whose advertised fingerprint is
/// pinned, else the only one found.
pub fn pick<'a>(hubs: &'a [DiscoveredHub], pinned: &str) -> Option<&'a DiscoveredHub> {
    if !pinned.is_empty() {
        return hubs.iter().find(|h| {
            h.fingerprint
                .as_deref()
                .is_some_and(|f| f.eq_ignore_ascii_case(pinned))
        });
    }
    match hubs {
        [only] => Some(only),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(name: &str, fp: &str) -> DiscoveredHub {
        DiscoveredHub {
            name: name.into(),
            hub_id: None,
            fingerprint: Some(fp.into()),
            proto_ver: Some(1),
            addrs: vec![],
        }
    }

    #[test]
    fn pick_prefers_pinned_and_refuses_ambiguity() {
        let hubs = [hub("a", "AA"), hub("b", "bb")];
        assert_eq!(pick(&hubs, "aa").unwrap().name, "a");
        assert!(pick(&hubs, "cc").is_none());
        assert!(pick(&hubs, "").is_none());
        assert_eq!(pick(&hubs[1..], "").unwrap().name, "b");
    }
}
