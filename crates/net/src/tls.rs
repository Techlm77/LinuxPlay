//! Self-signed TLS identities, identified by certificate fingerprint.
//!
//! Neither side uses a CA. The verifiers below accept any certificate but
//! still check the handshake signature, so the peer must hold the private key
//! for the certificate it shows. Whether that certificate is *trusted* is
//! decided afterwards by comparing its fingerprint with the pinned one, or by
//! pairing.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

/// SHA-256 of a DER certificate.
pub type Fingerprint = [u8; 32];

pub fn fingerprint(cert: &[u8]) -> Fingerprint {
    Sha256::digest(cert).into()
}

/// Colon-free uppercase hex, grouped in fours for reading aloud.
pub fn fingerprint_display(fp: &Fingerprint) -> String {
    let hex = hex::encode_upper(fp);
    hex.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A certificate and its key, persisted in a directory.
pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    pub fingerprint: Fingerprint,
}

impl Identity {
    /// Load `cert.der`/`key.der` from `dir`, creating them on first use.
    pub fn load_or_create(dir: &Path, common_name: &str) -> Result<Self> {
        let cert_path = dir.join("cert.der");
        let key_path = dir.join("key.der");
        if !cert_path.exists() || !key_path.exists() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            let key = rcgen::KeyPair::generate()?;
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
            params.distinguished_name.push(rcgen::DnType::CommonName, common_name);
            // Far-future expiry: trust comes from the pinned fingerprint, not dates.
            params.not_after = rcgen::date_time_ymd(2125, 1, 1);
            let cert = params.self_signed(&key)?;
            write_private(&key_path, &key.serialize_der())?;
            fs::write(&cert_path, cert.der()).with_context(|| format!("writing {}", cert_path.display()))?;
        }
        let cert = CertificateDer::from(fs::read(&cert_path)?);
        let key = PrivatePkcs8KeyDer::from(fs::read(&key_path)?);
        let fingerprint = fingerprint(&cert);
        Ok(Self { cert, key, fingerprint })
    }

    fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }

    /// TLS config for the host: TLS 1.3, client certificate required.
    pub fn server_config(&self) -> Result<rustls::ServerConfig> {
        let verifier = Arc::new(AnyCert(provider()));
        let mut cfg = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![self.cert.clone()], self.key())?;
        cfg.alpn_protocols = vec![b"linuxplay/1".to_vec()];
        Ok(cfg)
    }

    /// TLS config for the client: TLS 1.3, presents our certificate.
    pub fn client_config(&self) -> Result<rustls::ClientConfig> {
        let verifier = Arc::new(AnyCert(provider()));
        let mut cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![self.cert.clone()], self.key())?;
        cfg.alpn_protocols = vec![b"linuxplay/1".to_vec()];
        Ok(cfg)
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

fn provider() -> Arc<CryptoProvider> {
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

/// The fingerprint of the certificate the peer presented.
pub fn peer_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> Option<Fingerprint> {
    certs.and_then(|c| c.first()).map(|c| fingerprint(c))
}

/// `~/.config/linuxplay/<role>`
pub fn config_dir(role: &str) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linuxplay")
        .join(role)
}

/// Accepts any certificate whose handshake signature checks out.
#[derive(Debug)]
struct AnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for AnyCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
