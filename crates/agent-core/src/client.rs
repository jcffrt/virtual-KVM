//! QUIC client with certificate pinning.
//!
//! The hub uses a self-signed certificate, so there is no CA to check it
//! against. Instead the agent pins the certificate's SHA-256 fingerprint:
//! the first connection accepts and records it (trust on first use, together
//! with the pairing token), later connections refuse any other certificate.
//! The TLS 1.3 handshake signature is still verified, so presenting a copied
//! certificate without its private key does not work.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::config::Trust;

/// What the verifier saw during the handshake.
#[derive(Debug, Default, Clone)]
pub struct Observed {
    pub fingerprint: Option<String>,
    pub mismatch: bool,
}

#[derive(Debug)]
pub struct PinnedVerifier {
    pinned: String,
    observed: Arc<Mutex<Observed>>,
    provider: Arc<CryptoProvider>,
}

impl PinnedVerifier {
    pub fn new(pinned: &str, provider: Arc<CryptoProvider>) -> (Self, Arc<Mutex<Observed>>) {
        let observed = Arc::new(Mutex::new(Observed::default()));
        (
            Self {
                pinned: pinned.to_string(),
                observed: observed.clone(),
                provider,
            },
            observed,
        )
    }

    fn trust(&self, fp: &str) -> Trust {
        crate::config::AgentConfig {
            hub_fingerprint: self.pinned.clone(),
            ..Default::default()
        }
        .trust(fp)
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = proto::cert_fingerprint(end_entity.as_ref());
        let trust = self.trust(&fp);
        {
            let mut o = self.observed.lock().unwrap_or_else(|e| e.into_inner());
            o.fingerprint = Some(fp.clone());
            o.mismatch = matches!(trust, Trust::Mismatch { .. });
        }
        match trust {
            Trust::FirstUse | Trust::Known => Ok(ServerCertVerified::assertion()),
            Trust::Mismatch { pinned } => Err(rustls::Error::General(format!(
                "hub certificate changed: pinned {pinned}, presented {fp}"
            ))),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not allowed".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(1)));
    t.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(Duration::from_secs(5)).expect("valid timeout"),
    ));
    t
}

pub struct Connected {
    /// Keep alive as long as the connection is used.
    pub endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
}

/// Connect to `addr`, pinning `pinned` (empty: first use). `observed`
/// receives the fingerprint the hub presented, also on failure.
pub async fn connect(addr: SocketAddr, pinned: &str) -> (Result<Connected>, Arc<Mutex<Observed>>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let (verifier, observed) = PinnedVerifier::new(pinned, provider.clone());
    let result = async {
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        tls.alpn_protocols = vec![proto::ALPN.to_vec()];
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
        let mut cc = quinn::ClientConfig::new(Arc::new(crypto));
        cc.transport_config(Arc::new(transport_config()));
        let bind: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0".parse()?
        } else {
            "[::]:0".parse()?
        };
        let mut endpoint = quinn::Endpoint::client(bind)?;
        endpoint.set_default_client_config(cc);
        let conn = tokio::time::timeout(
            Duration::from_secs(5),
            endpoint.connect(addr, proto::CERT_SERVER_NAME)?,
        )
        .await
        .context("connection timed out")?
        .with_context(|| format!("connecting to {addr}"))?;
        Ok(Connected { endpoint, conn })
    }
    .await;
    (result, observed)
}
