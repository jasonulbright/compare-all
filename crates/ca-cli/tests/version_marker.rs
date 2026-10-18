//! The console program carries its build version where the updater reads it.

#![allow(clippy::unwrap_used)]

#[test]
fn the_program_file_carries_its_version_for_the_updater() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_ca")).unwrap();
    assert_eq!(
        ca_update::verify::embedded_version(&bytes).unwrap(),
        ca_update::Version::parse(env!("CA_VERSION")).unwrap()
    );
}
