//! Transport security for the protocols that carry their own TLS.
//!
//! Certificate validation is on. A profile can add the fingerprint of one
//! certificate it accepts in addition to the trusted roots, which is what a
//! server with a private certificate authority needs. Turning validation off
//! altogether needs [`TlsOptions::accept_any_certificate`], which a profile
//! sets on its own and which the interface presents as unsafe.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::error::{VfsError, VfsResult};

/// Lowest and highest TLS versions a connection may negotiate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsVersion {
    /// Whatever the library offers.
    #[default]
    Any,
    /// TLS 1.2.
    Tls12,
    /// TLS 1.3.
    Tls13,
}

/// How one connection treats the server's certificate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsOptions {
    /// Lowest version the connection may negotiate.
    pub min_version: TlsVersion,
    /// Highest version the connection may negotiate.
    pub max_version: TlsVersion,
    /// SHA-256 fingerprints of certificates accepted in addition to the
    /// trusted roots, written as lowercase hexadecimal with no separators.
    pub pinned_fingerprints: Vec<String>,
    /// Accept any certificate, including one that does not verify and one
    /// whose name does not match.
    ///
    /// This removes the only defence against a machine in the middle of the
    /// connection. Nothing sets it implicitly.
    pub accept_any_certificate: bool,
}

/// The SHA-256 fingerprint of a certificate, as lowercase hexadecimal.
#[must_use]
pub fn fingerprint(certificate: &[u8]) -> String {
    hex(&Sha256::digest(certificate))
}

/// Lowercase hexadecimal for `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The crypto backend every connection in this crate uses.
#[must_use]
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Build a client configuration from `options`.
///
/// # Errors
/// Returns [`VfsError::Tls`] when the version range is empty or the backend
/// refuses the configuration.
pub fn client_config(options: &TlsOptions) -> VfsResult<Arc<ClientConfig>> {
    let versions = version_range(options)?;
    let builder = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&versions)
        .map_err(|error| VfsError::tls(error.to_string()))?;

    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = if options.accept_any_certificate {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny))
            .with_no_client_auth()
    } else if options.pinned_fingerprints.is_empty() {
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            provider(),
        )
        .build()
        .map_err(|error| VfsError::tls(error.to_string()))?;
        let pinned: Vec<String> = options
            .pinned_fingerprints
            .iter()
            .map(|value| value.trim().replace([':', ' '], "").to_ascii_lowercase())
            .collect();
        // A short or misspelled pin would never match, and the connection
        // would fall back to the trusted roots while the profile reads as
        // pinned. It is refused instead.
        for value in &pinned {
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(VfsError::tls(
                    "a pinned fingerprint must be the 64 hexadecimal digits of a SHA-256 digest"
                        .to_owned(),
                ));
            }
        }
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedOrTrusted { inner, pinned }))
            .with_no_client_auth()
    };
    let mut config = config;
    // One configuration serves every connection of one file system, so the
    // session store is shared between them. A protocol whose data connection
    // must resume the session of its control connection depends on that.
    config.resumption = rustls::client::Resumption::in_memory_sessions(SESSION_CACHE_ENTRIES);
    Ok(Arc::new(config))
}

/// Sessions one configuration remembers for resumption.
const SESSION_CACHE_ENTRIES: usize = 32;

/// The protocol versions `options` allows.
fn version_range(
    options: &TlsOptions,
) -> VfsResult<Vec<&'static rustls::SupportedProtocolVersion>> {
    let allows = |version: TlsVersion| -> bool {
        let rank = |value: TlsVersion| match value {
            TlsVersion::Tls12 => 2u8,
            TlsVersion::Tls13 => 3,
            TlsVersion::Any => 0,
        };
        let wanted = rank(version);
        let low = rank(options.min_version);
        let high = rank(options.max_version);
        (low == 0 || wanted >= low) && (high == 0 || wanted <= high)
    };
    let mut out = Vec::new();
    if allows(TlsVersion::Tls12) {
        out.push(&rustls::version::TLS12);
    }
    if allows(TlsVersion::Tls13) {
        out.push(&rustls::version::TLS13);
    }
    if out.is_empty() {
        return Err(VfsError::tls(
            "the configured minimum TLS version is above the configured maximum",
        ));
    }
    Ok(out)
}

/// Compare two byte strings in a time that does not depend on where they
/// first differ.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Accepts the trusted roots, and one certificate named by its fingerprint.
#[derive(Debug)]
struct PinnedOrTrusted {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    pinned: Vec<String>,
}

impl ServerCertVerifier for PinnedOrTrusted {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let offered = fingerprint(end_entity.as_ref());
        // The comparison runs over every pin and reports one answer, so the
        // time it takes does not say which pin was close.
        let mut matched = 0u8;
        for pin in &self.pinned {
            matched |= u8::from(constant_time_eq(pin.as_bytes(), offered.as_bytes()));
        }
        if matched != 0 {
            return Ok(ServerCertVerified::assertion());
        }
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Accepts every certificate. Reached only through
/// [`TlsOptions::accept_any_certificate`].
#[derive(Debug)]
struct AcceptAny;

impl ServerCertVerifier for AcceptAny {
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
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn validation_is_on_unless_a_profile_turns_it_off() {
        let options = TlsOptions::default();
        assert!(!options.accept_any_certificate);
        assert!(options.pinned_fingerprints.is_empty());
        assert!(client_config(&options).is_ok());
    }

    #[test]
    fn an_empty_version_range_is_refused() {
        let options = TlsOptions {
            min_version: TlsVersion::Tls13,
            max_version: TlsVersion::Tls12,
            ..TlsOptions::default()
        };
        assert!(matches!(client_config(&options), Err(VfsError::Tls { .. })));
    }

    #[test]
    fn the_fingerprint_is_lowercase_hexadecimal() {
        let value = fingerprint(b"");
        assert_eq!(value.len(), 64);
        assert!(value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_uppercase()));
    }
}
