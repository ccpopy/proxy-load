//! Target TLS only. Never used by the update client or release verification.
use std::sync::{Arc, OnceLock};
use tokio_rustls::rustls::{
    self,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

#[derive(Debug)]
struct SkipChainVerification(rustls::crypto::WebPkiSupportedAlgorithms);
impl ServerCertVerifier for SkipChainVerification {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

pub(crate) fn config(skip_chain: bool) -> Arc<rustls::ClientConfig> {
    static VERIFIED: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    static SKIPPED: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    let slot = if skip_chain { &SKIPPED } else { &VERIFIED };
    slot.get_or_init(|| {
        build(
            skip_chain,
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        )
    })
    .clone()
}

pub(crate) fn build(skip_chain: bool, roots: rustls::RootCertStore) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let algorithms = provider.signature_verification_algorithms;
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports TLS 1.2/1.3");
    let mut config = if skip_chain {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipChainVerification(algorithms)))
            .with_no_client_auth()
    } else {
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    // A health probe measures a fresh authenticated connection, not a resumed TLS session.
    config.resumption = rustls::client::Resumption::disabled();
    Arc::new(config)
}
