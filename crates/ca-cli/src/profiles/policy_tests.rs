#![allow(clippy::unwrap_used)]

use super::{StoredProfiles, REMOTE_PROFILES_DISABLED};
use ca_script::ProfileLookup;
use ca_session::{AdminPolicies, InMemoryPolicySource, PolicyKey, PolicyLoader};

fn policies(key: PolicyKey) -> AdminPolicies {
    InMemoryPolicySource::new().with(key, true).load().unwrap()
}

fn folder_with_profile() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("shelf.json"),
        r#"{"name":"shelf","service":{"kind":"web_dav","password":"shelf/password"}}"#,
    )
    .unwrap();
    dir
}

#[test]
fn disabled_remote_profiles_are_not_read_and_the_connection_is_refused() {
    let dir = folder_with_profile();
    let lookup = StoredProfiles::in_folder(dir.path())
        .with_policies(&policies(PolicyKey::DisableRemoteProfiles));
    assert!(lookup.profile("dav://shelf/files").is_none());
    let error = ca_script::profiles::connect("dav://shelf/files", &lookup, &ca_vfs::Cancel::new())
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(error.contains(REMOTE_PROFILES_DISABLED), "{error}");
}

#[test]
fn without_the_policy_the_profile_is_found() {
    let dir = folder_with_profile();
    let lookup = StoredProfiles::in_folder(dir.path()).with_policies(&AdminPolicies::default());
    assert!(lookup.profile("dav://shelf/files").is_some());
    assert!(lookup.refusal().is_none());
}

#[test]
fn disabled_saved_passwords_read_nothing_from_the_secret_store() {
    let dir = folder_with_profile();
    let store = ca_vfs::MemorySecretStore::new();
    store.insert("shelf/password", "hunter2");
    let lookup = StoredProfiles::in_folder(dir.path())
        .with_secrets(std::sync::Arc::new(store))
        .with_policies(&policies(PolicyKey::DisableSavedPasswords));
    assert!(lookup
        .secrets()
        .secret(&ca_vfs::SecretRef::new("shelf/password"))
        .is_none());
    assert!(lookup.profile("dav://shelf/files").is_some());
}
