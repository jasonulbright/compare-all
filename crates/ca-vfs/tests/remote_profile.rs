//! Connection profiles: what they store, what they refuse to store, and what
//! survives a round trip through a build that does not know every field.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
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
    missing_docs
)]

use ca_vfs::remote::profile::{
    CloudProfile, FtpLogin, FtpProfile, FtpProtocol, HttpAuthScheme, ListingEncoding, PortRange,
    ProxyKind, RemoteProfile, S3Auth, S3Profile, ServiceProfile, SubversionProfile, TlsSettings,
    TransferType, WebDavProfile,
};
use ca_vfs::remote::{
    connect, KnownHosts, MemorySecretStore, RemoteContext, Secret, SecretRef, SecretStore,
};
use ca_vfs::{Cancel, VfsError};
use serde_json::Value;
use std::sync::Arc;

/// A profile with something in every field the tests care about.
fn populated_transfer() -> RemoteProfile {
    let mut settings = FtpProfile {
        login: FtpLogin {
            protocol: FtpProtocol::FtpsExplicit,
            host: "example.test".to_owned(),
            port: Some(2121),
            username: "operator".to_owned(),
            password: SecretRef::new("profile/example/password"),
            save_password: true,
            anonymous: false,
            unknown: Default::default(),
        },
        root_path: "/pub".to_owned(),
        ..FtpProfile::default()
    };
    settings.server.encoding = ListingEncoding::Detect;
    settings.server.time_zone = "Etc/UTC".to_owned();
    settings.server.time_zone_offset_minutes = -300;
    settings.server.custom_login_commands = vec!["SITE UMASK 022".to_owned()];
    settings.connection.max_connections = 4;
    settings.connection.read_timeout_seconds = 45;
    settings.connection.passive = true;
    settings.connection.active_port_range = Some(PortRange {
        first: 50_000,
        last: 50_100,
        unknown: Default::default(),
    });
    settings.connection.pinned_certificate_fingerprints = vec!["ab".repeat(32)];
    settings.connection.keep_alive_seconds = 30;
    settings.proxy.enabled = true;
    settings.proxy.kind = ProxyKind::Socks5;
    settings.proxy.host = "proxy.test".to_owned();
    settings.proxy.port = 1080;
    settings.proxy.username = "proxy-user".to_owned();
    settings.proxy.password = SecretRef::new("profile/example/proxy");
    settings.listing.show_hidden = true;
    settings.listing.complete_timestamps = true;
    settings.transfer.transfer_type = TransferType::Auto;
    settings.transfer.download_limit_kbps = 4096;
    settings.global.ssh_private_key_file = "keys/id_ed25519".to_owned();
    settings.global.ssh_private_key_passphrase = SecretRef::new("profile/example/passphrase");
    settings.global.ascii_types = vec!["*.txt".to_owned(), "*.md".to_owned()];
    settings.global.known_hosts_file = "settings/known_hosts".to_owned();

    RemoteProfile {
        name: "example".to_owned(),
        description: "a server".to_owned(),
        service: ServiceProfile::Ftp(settings),
        unknown: Default::default(),
    }
}

/// Every profile kind, so the probe walks all of them.
fn every_profile() -> Vec<RemoteProfile> {
    vec![
        populated_transfer(),
        RemoteProfile {
            name: "share".to_owned(),
            description: "a share".to_owned(),
            service: ServiceProfile::WebDav(WebDavProfile {
                url: "https://example.test/share".to_owned(),
                username: "operator".to_owned(),
                password: SecretRef::new("profile/share/password"),
                auth: HttpAuthScheme::Digest,
                recursive_listings: true,
                allow_plaintext_credentials: false,
                tls: TlsSettings {
                    accept_any_certificate: false,
                    pinned_certificate_fingerprints: vec!["cd".repeat(32)],
                    ..TlsSettings::default()
                },
                timeout_seconds: Some(30),
                unknown: Default::default(),
            }),
            unknown: Default::default(),
        },
        RemoteProfile {
            name: "store".to_owned(),
            description: "an object store".to_owned(),
            service: ServiceProfile::S3(S3Profile {
                auth: S3Auth::Saved {
                    access_key_id: "AKIAEXAMPLE".to_owned(),
                    secret_access_key: SecretRef::new("profile/store/secret"),
                    session_token: SecretRef::new("profile/store/token"),
                    unknown: Default::default(),
                },
                bucket: "bucket".to_owned(),
                region: "eu-west-1".to_owned(),
                endpoint: "https://objects.example.test".to_owned(),
                path_style: true,
                tls: TlsSettings::default(),
                timeout_seconds: Some(20),
                unknown: Default::default(),
            }),
            unknown: Default::default(),
        },
        RemoteProfile {
            name: "dropbox".to_owned(),
            description: String::new(),
            service: ServiceProfile::Dropbox(CloudProfile {
                account: "person@example.test".to_owned(),
                refresh_token: SecretRef::new("profile/dropbox/refresh"),
                client_id: "app".to_owned(),
                unknown: Default::default(),
            }),
            unknown: Default::default(),
        },
        RemoteProfile {
            name: "onedrive".to_owned(),
            description: String::new(),
            service: ServiceProfile::OneDrive(CloudProfile::default()),
            unknown: Default::default(),
        },
        RemoteProfile {
            name: "repository".to_owned(),
            description: String::new(),
            service: ServiceProfile::Subversion(SubversionProfile {
                url: "svn://example.test/trunk".to_owned(),
                revision: Some(1234),
                username: "reader".to_owned(),
                password: SecretRef::new("profile/repository/password"),
                unknown: Default::default(),
            }),
            unknown: Default::default(),
        },
    ]
}

#[test]
fn every_profile_round_trips_unchanged() {
    for profile in every_profile() {
        let text = serde_json::to_string(&profile).unwrap();
        let back: RemoteProfile = serde_json::from_str(&text).unwrap();
        assert_eq!(back, profile, "{text}");
    }
}

/// Collect the paths of every object inside a value.
fn object_paths(value: &Value, path: Vec<String>, out: &mut Vec<Vec<String>>) {
    if let Value::Object(map) = value {
        out.push(path.clone());
        for (key, child) in map {
            let mut next = path.clone();
            next.push(key.clone());
            object_paths(child, next, out);
        }
    }
}

/// Insert `key` into the object at `path`.
fn insert_at(value: &mut Value, path: &[String], key: &str) {
    let mut cursor = value;
    for step in path {
        cursor = match cursor {
            Value::Object(map) => map.get_mut(step).expect("the path exists"),
            _ => return,
        };
    }
    if let Value::Object(map) = cursor {
        map.insert(key.to_owned(), Value::String("kept".to_owned()));
    }
}

#[test]
fn a_field_this_build_does_not_know_survives_at_every_depth() {
    const PROBE: &str = "zz_a_field_from_a_later_build";
    for profile in every_profile() {
        let original = serde_json::to_value(&profile).unwrap();
        let mut paths = Vec::new();
        object_paths(&original, Vec::new(), &mut paths);
        assert!(paths.len() >= 2, "the profile has too few objects to probe");

        for path in paths {
            let mut probed = original.clone();
            insert_at(&mut probed, &path, PROBE);
            let text = serde_json::to_string(&probed).unwrap();
            let read: RemoteProfile = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{path:?} failed to read: {error}"));
            let written = serde_json::to_value(&read).unwrap();
            let mut found = Vec::new();
            object_paths(&written, Vec::new(), &mut found);
            let survived = written.to_string().contains(PROBE);
            assert!(
                survived,
                "an unknown field at {path:?} was dropped rather than written back"
            );
            assert!(!found.is_empty());
        }
    }
}

#[test]
fn an_arm_this_build_does_not_know_reads_as_unknown_rather_than_failing() {
    let cases = [
        (
            r#"{"name":"x","service":{"kind":"quantum_drive","anything":1}}"#,
            "/service/kind",
            "quantum_drive",
        ),
        (
            r#"{"name":"x","service":{"kind":"ftp","login":{"protocol":"telepathy"}}}"#,
            "/service/login/protocol",
            "telepathy",
        ),
        (
            r#"{"name":"x","service":{"kind":"ftp","server":{"encoding":"runes"}}}"#,
            "/service/server/encoding",
            "runes",
        ),
        (
            r#"{"name":"x","service":{"kind":"ftp","transfer":{"transfer_type":"psychic"}}}"#,
            "/service/transfer/transfer_type",
            "psychic",
        ),
        (
            r#"{"name":"x","service":{"kind":"ftp","proxy":{"kind":"carrier_pigeon"}}}"#,
            "/service/proxy/kind",
            "carrier_pigeon",
        ),
        (
            r#"{"name":"x","service":{"kind":"web_dav","auth":"secret_handshake"}}"#,
            "/service/auth",
            "secret_handshake",
        ),
        (
            r#"{"name":"x","service":{"kind":"s3","auth":{"source":"divination"}}}"#,
            "/service/auth/source",
            "divination",
        ),
    ];
    for (text, pointer, expected) in cases {
        let original: Value = serde_json::from_str(text).unwrap();
        let profile: RemoteProfile =
            serde_json::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        let again = serde_json::to_string(&profile).unwrap();
        let written: Value = serde_json::from_str(&again).unwrap();
        assert_eq!(
            written.pointer(pointer),
            original.pointer(pointer),
            "unknown value at {pointer} changed"
        );
        assert_eq!(
            written.pointer(pointer),
            Some(&Value::String(expected.to_owned()))
        );
    }
}

#[test]
fn nothing_a_profile_writes_carries_a_secret() {
    let store = MemorySecretStore::new();
    store.insert("profile/example/password", "hunter2");
    store.insert("profile/example/passphrase", "correct horse");
    store.insert("profile/store/secret", "wJalrXUtnFEMI");

    for profile in every_profile() {
        let text = serde_json::to_string(&profile).unwrap();
        for value in ["hunter2", "correct horse", "wJalrXUtnFEMI"] {
            assert!(!text.contains(value), "{text}");
        }
        let debug = format!("{profile:?}");
        for value in ["hunter2", "correct horse", "wJalrXUtnFEMI"] {
            assert!(!debug.contains(value));
        }
    }

    let secret = store
        .secret(&SecretRef::new("profile/example/password"))
        .unwrap();
    assert_eq!(secret.expose(), "hunter2");
    assert_eq!(format!("{secret:?}"), "***");
    assert_eq!(format!("{secret}"), "***");
}

#[test]
fn a_context_reads_a_secret_only_through_the_store() {
    let store = Arc::new(MemorySecretStore::new());
    store.insert("a", "value");
    let context = RemoteContext::with_secrets(store);
    assert_eq!(
        context
            .secret(&SecretRef::new("a"))
            .map(|s| s.expose().to_owned()),
        Some("value".to_owned())
    );
    assert!(context.secret(&SecretRef::new("b")).is_none());
    assert!(context.secret(&SecretRef::default()).is_none());
    assert!(!format!("{context:?}").contains("value"));
}

#[test]
fn cloud_services_and_refused_svn_credentials_say_why() {
    let context = RemoteContext::default();
    let cancel = Cancel::new();
    for profile in every_profile() {
        match &profile.service {
            ServiceProfile::Dropbox(_) | ServiceProfile::OneDrive(_) => {
                let Err(error) = connect(&profile, &context, &cancel) else {
                    panic!("{} must not connect yet", profile.name);
                };
                let VfsError::Unsupported { what } = error else {
                    panic!("{} reported the wrong error", profile.name);
                };
                assert!(what.len() > 40, "{what}");
            }
            ServiceProfile::Subversion(_) => {
                let Err(error) = connect(&profile, &context, &cancel) else {
                    panic!("a password reference must not reach svn's command line");
                };
                assert!(matches!(error, VfsError::Unsupported { .. }));
                assert!(error.to_string().contains("passwords are not passed"));
            }
            _ => {}
        }
    }
}

#[test]
fn a_host_key_store_reports_an_unknown_host_and_refuses_a_changed_one() {
    let directory = tempfile::tempdir().unwrap();
    let store = KnownHosts::new(directory.path().join("known_hosts"));

    let Err(error) = store.verify("server.test", 22, "ssh-ed25519", b"first") else {
        panic!("an unrecorded host must not verify");
    };
    let VfsError::UnknownHostKey { host, fingerprint } = error else {
        panic!("the wrong error was reported");
    };
    assert_eq!(host, "server.test");
    assert!(fingerprint.starts_with("SHA256:"));

    store
        .record("server.test", 22, "ssh-ed25519", b"first")
        .unwrap();
    store
        .verify("server.test", 22, "ssh-ed25519", b"first")
        .unwrap();

    let Err(error) = store.verify("server.test", 22, "ssh-ed25519", b"second") else {
        panic!("a changed key must not verify");
    };
    assert!(matches!(error, VfsError::HostKeyChanged { .. }));
}

#[test]
fn a_secret_never_reaches_a_message_built_from_one() {
    let secret = Secret::new("hunter2");
    let error = VfsError::auth_failed(format!("the server refused the account, password {secret}"));
    assert!(!error.to_string().contains("hunter2"));
    assert!(error.to_string().contains("***"));
}
