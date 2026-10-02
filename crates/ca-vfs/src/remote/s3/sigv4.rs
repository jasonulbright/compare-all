//! Version 4 request signing for the object store protocol.
//!
//! The algorithm is written out here rather than taken from a service
//! library, because the only part of it this crate needs is the signature of
//! one request. It follows the published steps: build a canonical request,
//! hash it into a string to sign, derive a key from the secret, the date, the
//! region and the service, and sign.

use hmac::{Hmac, KeyInit as _, Mac};
use sha2::{Digest, Sha256};

use crate::remote::secret::Secret;
use crate::remote::timestamp::{format_basic_date, format_basic_iso8601};
use crate::remote::tls::hex;

/// The algorithm name that appears in the header and in the string to sign.
pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// Credentials one request is signed with.
pub struct Credentials {
    /// Access key identifier.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: Secret,
    /// Session token, for time-limited credentials.
    pub session_token: Option<Secret>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &self.secret_access_key)
            .field("session_token", &self.session_token)
            .finish()
    }
}

/// Everything one signature covers.
#[derive(Debug)]
pub struct Request<'a> {
    /// Request verb.
    pub method: &'a str,
    /// Path, already percent-encoded, starting with a separator.
    pub path: &'a str,
    /// Query parameters, unencoded.
    pub query: &'a [(String, String)],
    /// Headers other than the ones this module adds.
    pub headers: &'a [(String, String)],
    /// The body.
    pub payload: &'a [u8],
    /// Value of the host header.
    pub host: &'a str,
    /// Region the request is signed for.
    pub region: &'a str,
    /// Service the request is signed for.
    pub service: &'a str,
    /// Seconds from the Unix epoch the request is stamped with.
    pub now: i64,
}

/// The headers a signed request carries, including the signature.
#[must_use]
pub fn sign(request: &Request<'_>, credentials: &Credentials) -> Vec<(String, String)> {
    let stamp = format_basic_iso8601(request.now);
    let date = format_basic_date(request.now);
    let payload_hash = hex(&Sha256::digest(request.payload));

    let mut headers: Vec<(String, String)> = vec![
        ("host".to_owned(), request.host.to_owned()),
        ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
        ("x-amz-date".to_owned(), stamp.clone()),
    ];
    if let Some(token) = &credentials.session_token {
        headers.push(("x-amz-security-token".to_owned(), token.expose().to_owned()));
    }
    for (name, value) in request.headers {
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    headers.sort_by(|left, right| left.0.cmp(&right.0));
    headers.dedup_by(|left, right| left.0 == right.0);

    let signed_names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    let signed = signed_names.join(";");
    let canonical_headers = headers
        .iter()
        .fold(String::new(), |mut out, (name, value)| {
            use std::fmt::Write;
            let _ = writeln!(out, "{name}:{}", collapse(value));
            out
        });

    let canonical_query = canonical_query(request.query);
    let canonical = format!(
        "{}\n{}\n{canonical_query}\n{canonical_headers}\n{signed}\n{payload_hash}",
        request.method, request.path
    );

    let scope = format!("{date}/{}/{}/aws4_request", request.region, request.service);
    let to_sign = format!(
        "{ALGORITHM}\n{stamp}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );

    let key = signing_key(credentials, &date, request.region, request.service);
    let signature = hex(&mac(&key, to_sign.as_bytes()));
    let authorization = format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        credentials.access_key_id
    );

    let mut out = headers;
    out.push(("authorization".to_owned(), authorization));
    out
}

/// The derived key one signature uses.
fn signing_key(credentials: &Credentials, date: &str, region: &str, service: &str) -> Vec<u8> {
    let mut key = format!("AWS4{}", credentials.secret_access_key.expose()).into_bytes();
    for step in [date, region, service, "aws4_request"] {
        key = mac(&key, step.as_bytes());
    }
    key
}

/// One keyed hash step.
fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(mut hasher) = Hmac::<Sha256>::new_from_slice(key) else {
        // The construction accepts a key of any length, so this branch is
        // unreachable; an empty result would fail the signature rather than
        // sign the wrong thing.
        return Vec::new();
    };
    hasher.update(data);
    hasher.finalize().into_bytes().to_vec()
}

/// Query parameters in the order and spelling the signature covers.
#[must_use]
pub fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| (encode(key), encode(value)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode a value, keeping only the characters the algorithm names.
#[must_use]
pub fn encode(value: &str) -> String {
    /// Characters the algorithm leaves as they are.
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(value, KEEP).to_string()
}

/// Percent-encode a path, keeping the separators.
#[must_use]
pub fn encode_path(path: &str) -> String {
    let encoded: Vec<String> = path.split('/').map(encode).collect();
    encoded.join("/")
}

/// Collapse the runs of spaces inside a header value.
fn collapse(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_space = false;
    for ch in value.trim().chars() {
        if ch == ' ' {
            if !last_space {
                out.push(ch);
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// The worked example from the published description of the algorithm.
    #[test]
    fn the_published_example_signs_to_the_published_value() {
        let credentials = Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: Secret::new("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
            session_token: None,
        };
        let now = crate::remote::timestamp::civil_to_unix(2013, 5, 24, 0, 0, 0);
        let headers = sign(
            &Request {
                method: "GET",
                path: "/test.txt",
                query: &[],
                headers: &[("range".to_owned(), "bytes=0-9".to_owned())],
                payload: b"",
                host: "examplebucket.s3.amazonaws.com",
                region: "us-east-1",
                service: "s3",
                now,
            },
            &credentials,
        );
        let authorization = headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert!(authorization.contains(
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        ));
        assert!(authorization
            .contains("Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"));
    }

    #[test]
    fn a_signature_never_carries_the_secret() {
        let credentials = Credentials {
            access_key_id: "AKIA".to_owned(),
            secret_access_key: Secret::new("topsecret"),
            session_token: Some(Secret::new("tokenvalue")),
        };
        assert!(!format!("{credentials:?}").contains("topsecret"));
        assert!(!format!("{credentials:?}").contains("tokenvalue"));
        let headers = sign(
            &Request {
                method: "GET",
                path: "/a",
                query: &[],
                headers: &[],
                payload: b"",
                host: "h",
                region: "r",
                service: "s3",
                now: 0,
            },
            &credentials,
        );
        let joined = format!("{headers:?}");
        assert!(!joined.contains("topsecret"));
    }

    #[test]
    fn the_canonical_forms_follow_the_algorithm() {
        assert_eq!(
            canonical_query(&[
                ("list-type".to_owned(), "2".to_owned()),
                ("delimiter".to_owned(), "/".to_owned()),
            ]),
            "delimiter=%2F&list-type=2"
        );
        assert_eq!(encode_path("/a b/c+d"), "/a%20b/c%2Bd");
        assert_eq!(collapse("  a   b  "), "a b");
    }
}
