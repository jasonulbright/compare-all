//! Test support: the servers a remote test runs against, in this process.
//!
//! This module is not product code. It is behind the `testing` feature, which
//! is off by default and is meant for a dev-dependency of a crate whose tests
//! need a server. Nothing in the shipped binaries reaches it.
//!
//! Every server binds `127.0.0.1` on a port the operating system picks, serves
//! a temporary folder, and stops when the handle is dropped. Nothing here
//! reaches the network and nothing needs a credential from the machine it runs
//! on. Each server implements only what the clients of this crate send.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    dead_code,
    clippy::default_trait_access,
    clippy::assigning_clones,
    clippy::format_push_string,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::needless_pass_by_value,
    clippy::ref_option,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::return_self_not_must_use,
    clippy::unused_self,
    clippy::doc_markdown,
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::uninlined_format_args,
    clippy::redundant_closure_for_method_calls,
    clippy::missing_const_for_fn,
    missing_docs
)]

#[cfg(feature = "ftp")]
pub mod ftp_server;
#[cfg(feature = "http")]
pub mod http_server;
#[cfg(feature = "s3")]
pub mod s3_server;
#[cfg(feature = "sftp")]
pub mod ssh_server;
#[cfg(feature = "webdav")]
pub mod webdav_server;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A flag a server thread polls to know it should stop.
#[derive(Debug, Clone, Default)]
pub struct Stop(Arc<AtomicBool>);

impl Stop {
    /// A flag that has not been raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise the flag.
    pub fn raise(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// True once the flag is raised.
    pub fn raised(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A certificate and key for a server that speaks transport security.
pub struct TestCertificate {
    /// The certificate, in the binary encoding.
    pub der: Vec<u8>,
    /// The private key, in the binary encoding.
    pub key: Vec<u8>,
}

impl TestCertificate {
    /// A self-signed certificate for the loopback name.
    pub fn new() -> Self {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        Self {
            der: generated.cert.der().to_vec(),
            key: generated.signing_key.serialize_der(),
        }
    }

    /// The fingerprint a profile pins.
    pub fn fingerprint(&self) -> String {
        crate::remote::tls::fingerprint(&self.der)
    }

    /// A server configuration that offers this certificate.
    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let certificate = rustls::pki_types::CertificateDer::from(self.der.clone());
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(self.key.clone()),
        );
        Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate], key)
                .unwrap(),
        )
    }
}

impl Default for TestCertificate {
    fn default() -> Self {
        Self::new()
    }
}
