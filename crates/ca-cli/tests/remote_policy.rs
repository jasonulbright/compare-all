//! A stored remote profile and the remote profile policy in a script run.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::AdminPolicies;

fn first_failure_of_remote_load(policies: AdminPolicies) -> String {
    let settings = tempfile::tempdir().expect("temporary folder");
    std::fs::write(
        settings.path().join("shelf.json"),
        r#"{"name":"shelf","service":{"kind":"web_dav","url":"http://127.0.0.1:9/"}}"#,
    )
    .expect("write profile");
    let options = ca_script::SessionOptions {
        profiles: std::sync::Arc::new(
            ca_cli::StoredProfiles::in_folder(settings.path()).with_policies(&policies),
        ),
        ..ca_script::SessionOptions::default()
    };
    let mut session = ca_script::Session::new(options);
    let source = "load dav://shelf/a dav://shelf/b\n";
    match ca_script::run_script(source, &ca_script::Substitution::none(), &mut session) {
        Ok(outcome) => outcome
            .steps
            .iter()
            .filter_map(|step| step.error.as_ref().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_stored_profile_loads_without_the_policy_and_is_refused_with_it() {
    let allowed = first_failure_of_remote_load(AdminPolicies::default());
    assert!(!allowed.contains("no stored profile matches"), "{allowed}");
    assert!(!allowed.contains("DisableRemoteProfiles"), "{allowed}");

    let refused = first_failure_of_remote_load(AdminPolicies::all_restricted());
    assert!(refused.contains("DisableRemoteProfiles"), "{refused}");
}
