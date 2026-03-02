use std::sync::Arc;
use std::time::Duration;

use quinn::{ClientConfig, Connection, Endpoint};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QuicClientError {
    #[error("connect error: {0}")]
    Connect(String),
    #[error("peer certificate missing")]
    MissingPeerCertificate,
    #[error("fingerprint mismatch")]
    FingerprintMismatch,
    #[error("invalid quic addr")]
    InvalidAddr,
    #[error("invalid idle timeout: {0}")]
    InvalidIdleTimeout(String),
}

#[derive(Debug)]
struct InsecureVerifier;

impl ServerCertVerifier for InsecureVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::ED25519,
        ]
    }
}

pub fn build_insecure_pinned_client_config(
    idle_timeout_secs: u64,
) -> Result<ClientConfig, QuicClientError> {
    let mut rustls_cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(InsecureVerifier))
        .with_no_client_auth();
    rustls_cfg.alpn_protocols = vec![b"neosh/1".to_vec()];

    let mut cfg = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)
            .map_err(|e| QuicClientError::Connect(e.to_string()))?,
    ));

    let mut transport = quinn::TransportConfig::default();
    let idle_timeout = quinn::IdleTimeout::try_from(Duration::from_secs(idle_timeout_secs))
        .map_err(|e| QuicClientError::InvalidIdleTimeout(e.to_string()))?;
    transport.max_idle_timeout(Some(idle_timeout));
    cfg.transport_config(Arc::new(transport));

    Ok(cfg)
}

pub async fn connect_and_verify(
    quic_addr: &str,
    expected_fingerprint: &str,
    idle_timeout_secs: u64,
) -> Result<(Endpoint, Connection), QuicClientError> {
    let addr = quic_addr
        .parse()
        .map_err(|_| QuicClientError::InvalidAddr)?;

    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
        .map_err(|e| QuicClientError::Connect(e.to_string()))?;
    endpoint.set_default_client_config(build_insecure_pinned_client_config(idle_timeout_secs)?);

    let connection = endpoint
        .connect(addr, "localhost")
        .map_err(|e| QuicClientError::Connect(e.to_string()))?
        .await
        .map_err(|e| QuicClientError::Connect(e.to_string()))?;

    verify_peer_fingerprint(&connection, expected_fingerprint)?;
    Ok((endpoint, connection))
}

pub fn verify_peer_fingerprint(conn: &Connection, expected: &str) -> Result<(), QuicClientError> {
    let actual = peer_cert_fingerprint(conn).ok_or(QuicClientError::MissingPeerCertificate)?;
    if actual != expected {
        return Err(QuicClientError::FingerprintMismatch);
    }
    Ok(())
}

pub fn peer_cert_fingerprint(conn: &Connection) -> Option<String> {
    let identity = conn.peer_identity()?;
    let certs = identity.downcast_ref::<Vec<CertificateDer<'static>>>()?;
    let cert = certs.first()?;
    Some(sha256_fingerprint(cert.as_ref()))
}

pub fn sha256_fingerprint(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    let mut out = String::from("sha256:");
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format_is_expected() {
        let fp = sha256_fingerprint(b"abc");
        assert!(fp.starts_with("sha256:"));
        assert_eq!(fp.len(), 71);
    }
}
