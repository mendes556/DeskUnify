// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use crate::crypto;
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
use webrtc_dtls::crypto::Certificate;

pub(crate) type Authorized = Arc<RwLock<HashMap<String, String>>>;
pub(super) const ALPN: &[u8] = b"lan-bridge-clipboard/1";

#[derive(Debug)]
struct PairedVerifier(Authorized, bool);

impl PairedVerifier {
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), Error> {
        if authorized(&self.0, cert.as_ref()) {
            Ok(())
        } else {
            Err(Error::General("unpaired DeskUnify peer".into()))
        }
    }

    fn signature12(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn signature13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(crate) fn authorized(keys: &Authorized, cert: &[u8]) -> bool {
    keys.read()
        .is_ok_and(|keys| keys.contains_key(&crypto::generate_fingerprint(cert)))
}

// Fingerprints establish identity; rustls still verifies possession of the
// private key through CertificateVerify. Self-signed certificates have no CA.
impl ServerCertVerifier for PairedVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.check(cert)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.signature12(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.signature13(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for PairedVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        if !self.1 {
            self.check(cert)?;
        }
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.signature12(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.signature13(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

#[derive(Clone)]
pub(crate) struct TlsConfig {
    pub client: Arc<ClientConfig>,
    pub server: Arc<ServerConfig>,
}

impl TlsConfig {
    pub fn new(cert: &Certificate, keys: Authorized) -> Result<Self, Error> {
        Self::with_alpn(cert, keys, ALPN)
    }

    pub fn with_alpn(cert: &Certificate, keys: Authorized, alpn: &[u8]) -> Result<Self, Error> {
        Self::build(cert, keys, alpn, false)
    }

    pub fn control(cert: &Certificate, keys: Authorized, alpn: &[u8]) -> Result<Self, Error> {
        Self::build(cert, keys, alpn, true)
    }

    fn build(
        cert: &Certificate,
        keys: Authorized,
        alpn: &[u8],
        allow_pair_request: bool,
    ) -> Result<Self, Error> {
        let verifier = Arc::new(PairedVerifier(keys, allow_pair_request));
        let key = || {
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                cert.private_key.serialized_der.clone(),
            ))
        };
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut client = ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_client_auth_cert(cert.certificate.clone(), key())?;
        client.alpn_protocols = vec![alpn.to_vec()];
        client.resumption = rustls::client::Resumption::disabled();
        let mut server = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(cert.certificate.clone(), key())?;
        server.alpn_protocols = vec![alpn.to_vec()];
        server.send_tls13_tickets = 0;
        Ok(Self {
            client: Arc::new(client),
            server: Arc::new(server),
        })
    }
}
