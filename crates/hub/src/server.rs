//! QUIC endpoint, TLS identity and mDNS advertisement.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use proto::{wire, Control, RejectReason, PROTO_VERSION, TOKEN_LEN};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{self, HubConfig};
use crate::router::{Hello, Link, Router};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CERT_FILE: &str = "hub-cert.der";
const KEY_FILE: &str = "hub-key.der";

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivatePkcs8KeyDer<'static>,
    pub fingerprint: String,
}

/// Load the hub certificate from `state_dir`, creating it on first start.
/// It must persist: agents pin its fingerprint.
pub fn load_or_create_identity(state_dir: &Path) -> Result<Identity> {
    let cert_path = state_dir.join(CERT_FILE);
    let key_path = state_dir.join(KEY_FILE);
    let (cert, key) = if cert_path.exists() && key_path.exists() {
        (std::fs::read(&cert_path)?, std::fs::read(&key_path)?)
    } else {
        config::ensure_dir(state_dir)?;
        let ck = rcgen::generate_simple_self_signed(vec![proto::CERT_SERVER_NAME.to_string()])
            .context("generating the hub certificate")?;
        let cert = ck.cert.der().to_vec();
        let key = ck.signing_key.serialize_der();
        config::write_private(&key_path, &key)?;
        std::fs::write(&cert_path, &cert)?;
        info!("created hub certificate in {}", state_dir.display());
        (cert, key)
    };
    Ok(Identity {
        fingerprint: proto::cert_fingerprint(&cert),
        cert: CertificateDer::from(cert),
        key: PrivatePkcs8KeyDer::from(key),
    })
}

pub fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(1)));
    t.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(Duration::from_secs(5)).expect("valid timeout"),
    ));
    // When the send buffer fills up quinn drops the oldest datagrams, which
    // is what we want for stale mouse motion.
    t.datagram_send_buffer_size(64 * 1024);
    t
}

fn server_config(id: &Identity) -> Result<quinn::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![id.cert.clone()],
            PrivateKeyDer::Pkcs8(id.key.clone_key()),
        )?;
    tls.alpn_protocols = vec![proto::ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut sc = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    sc.transport_config(Arc::new(transport_config()));
    Ok(sc)
}

struct QuinnLink(quinn::Connection);

impl Link for QuinnLink {
    fn send_datagram(&self, data: Bytes) {
        if let Err(e) = self.0.send_datagram(data) {
            debug!("datagram not sent: {e}");
        }
    }
    fn rtt(&self) -> Option<Duration> {
        Some(self.0.rtt())
    }
}

pub async fn serve(cfg: &HubConfig, id: &Identity, router: Arc<Router>) -> Result<()> {
    let token = cfg.token_bytes()?;
    let endpoint = quinn::Endpoint::server(server_config(id)?, cfg.listen)
        .with_context(|| format!("binding {}", cfg.listen))?;
    info!(
        "listening on {} (fingerprint {})",
        endpoint.local_addr()?,
        id.fingerprint
    );
    while let Some(incoming) = endpoint.accept().await {
        let router = router.clone();
        tokio::spawn(async move {
            let remote = incoming.remote_address();
            if let Err(e) = handle(incoming, router, token).await {
                info!(%remote, "connection ended: {e:#}");
            }
        });
    }
    Ok(())
}

async fn reject(
    conn: &quinn::Connection,
    send: &mut quinn::SendStream,
    reason: RejectReason,
) -> Result<()> {
    warn!(remote = %conn.remote_address(), ?reason, "rejecting node");
    wire::write_frame(send, &Control::Reject { reason }).await?;
    let _ = send.finish();
    // Give the frame a chance to arrive before closing.
    let _ = timeout(Duration::from_secs(1), send.stopped()).await;
    conn.close(1u32.into(), b"rejected");
    Ok(())
}

/// Rate limit for rejection logs: a node rejected for good (old version,
/// wrong token) reconnects every few seconds under launchd or systemd.
#[derive(Default)]
struct Rejections(std::collections::HashMap<(Uuid, RejectReason), Instant>);

const REJECT_LOG_EVERY: Duration = Duration::from_secs(60);

impl Rejections {
    fn should_log(&mut self, node: Uuid, reason: RejectReason, now: Instant) -> bool {
        self.0
            .retain(|_, t| now.duration_since(*t) < REJECT_LOG_EVERY);
        match self.0.entry((node, reason)) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(now);
                true
            }
        }
    }
}

fn rejections() -> std::sync::MutexGuard<'static, Rejections> {
    static R: std::sync::OnceLock<std::sync::Mutex<Rejections>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

async fn handle(
    incoming: quinn::Incoming,
    router: Arc<Router>,
    token: [u8; TOKEN_LEN],
) -> Result<()> {
    let conn = timeout(HANDSHAKE_TIMEOUT, incoming)
        .await
        .context("TLS handshake timed out")??;
    let (mut send, mut recv) = timeout(HANDSHAKE_TIMEOUT, conn.accept_bi())
        .await
        .context("no control stream")??;
    let first = timeout(HANDSHAKE_TIMEOUT, wire::read_frame(&mut recv))
        .await
        .context("no Hello")??;
    let Some(Control::Hello {
        proto_ver,
        node_id,
        node_name,
        os,
        kind,
        token: presented,
    }) = first
    else {
        conn.close(2u32.into(), b"expected Hello");
        anyhow::bail!("first message was not Hello");
    };
    if proto_ver != PROTO_VERSION {
        if rejections().should_log(node_id, RejectReason::VersionMismatch, Instant::now()) {
            warn!(
                node = %node_id,
                name = %node_name,
                "rejected: the node speaks protocol {proto_ver}, this hub {PROTO_VERSION}; update it \
                 (repeats from this node are not logged for a minute)"
            );
        }
        return reject(&conn, &mut send, RejectReason::VersionMismatch).await;
    }
    if !proto::token_eq(&presented, &token) {
        if rejections().should_log(node_id, RejectReason::BadToken, Instant::now()) {
            warn!(node = %node_id, name = %node_name, "rejected: wrong pairing token");
        }
        return reject(&conn, &mut send, RejectReason::BadToken).await;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Control>();
    let link = Arc::new(QuinnLink(conn.clone()));
    let hello = Hello {
        node_id,
        node_name,
        os,
        kind,
    };
    let reg = match router.register(hello, link, tx) {
        Ok(r) => r,
        Err(reason) => return reject(&conn, &mut send, reason).await,
    };

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = wire::write_frame(&mut send, &msg).await {
                debug!("control write failed: {e}");
                break;
            }
        }
    });

    let result = loop {
        tokio::select! {
            msg = wire::read_frame(&mut recv) => match msg {
                Ok(Some(m)) => router.on_control(node_id, m),
                Ok(None) => break Ok(()),
                Err(e) => break Err(anyhow::Error::from(e)),
            },
            dg = conn.read_datagram() => match dg {
                Ok(b) => on_datagram(&router, node_id, &b),
                Err(e) => break Err(e.into()),
            },
            e = conn.closed() => break Err(e.into()),
        }
    };
    router.unregister(node_id, reg);
    writer.abort();
    conn.close(0u32.into(), b"bye");
    result
}

/// A datagram from a node: only output reports travel that way (v4).
fn on_datagram(router: &Router, node: Uuid, bytes: &[u8]) {
    match proto::datagram::Datagram::decode(bytes) {
        Ok(d) if d.is_output() => router.on_output(node, d.dev_id, d.seq, d.report),
        Ok(d) => debug!(%node, flags = d.flags, "datagram without FLAG_OUTPUT ignored"),
        Err(e) => debug!(%node, "bad datagram: {e}"),
    }
}

/// Advertise `_rawkvm._udp.local.`. Keep the daemon alive while serving.
pub fn advertise(cfg: &HubConfig, fingerprint: &str) -> Result<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let label: String = cfg
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // A distinct host name so we do not fight avahi over `<hostname>.local`.
    let host = format!("rawkvm-{label}.local.");
    let props = [
        ("ver", PROTO_VERSION.to_string()),
        ("name", cfg.name.clone()),
        ("fp", fingerprint.to_string()),
        ("id", cfg.hub_id.to_string()),
    ];
    let info = mdns_sd::ServiceInfo::new(
        proto::SERVICE_TYPE,
        &cfg.name,
        &host,
        "",
        cfg.listen.port(),
        &props[..],
    )?
    .enable_addr_auto();
    daemon.register(info)?;
    info!("advertising {} as {:?}", proto::SERVICE_TYPE, cfg.name);
    Ok(daemon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_rejections_are_logged_once_a_minute() {
        let mut r = Rejections::default();
        let (n, t) = (Uuid::new_v4(), Instant::now());
        let v = RejectReason::VersionMismatch;
        assert!(r.should_log(n, v, t));
        assert!(!r.should_log(n, v, t + Duration::from_secs(2)));
        assert!(r.should_log(n, RejectReason::BadToken, t));
        assert!(r.should_log(Uuid::new_v4(), v, t));
        assert!(r.should_log(n, v, t + Duration::from_secs(61)));
    }
}
