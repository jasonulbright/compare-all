//! Where a script run keeps its journals when the environment names no home.
//!
//! The case runs this test binary again with a changed environment, so the
//! test process itself never changes a variable.

use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PROBE: &str = "COMPARE_ALL_STATE_PROBE";

#[test]
fn state_probe() {
    if std::env::var_os(PROBE).is_some() {
        println!(
            "settings={}",
            ca_script::paths::settings_directory().display()
        );
        println!(
            "journals={}",
            ca_script::paths::journal_directory().display()
        );
    }
}

#[test]
fn without_a_home_the_journals_never_go_to_the_working_directory() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let working = folder.path().join("working");
    let temporary = folder.path().join("temporary");
    std::fs::create_dir(&working)?;
    std::fs::create_dir(&temporary)?;
    let child = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "state_probe", "--nocapture"])
        .current_dir(&working)
        .env(PROBE, "1")
        .env_remove(ca_session::SETTINGS_DIRECTORY_VARIABLE)
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("APPDATA")
        .env("TMPDIR", &temporary)
        .env("TEMP", &temporary)
        .env("TMP", &temporary)
        .output()?;
    let stdout = String::from_utf8(child.stdout)?;
    assert!(child.status.success(), "{stdout}");
    let read = |key: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    let settings = read("settings");
    let journals = read("journals");
    for path in [&settings, &journals] {
        assert!(path.is_absolute(), "{}", path.display());
        assert!(!path.starts_with(&working), "{}", path.display());
    }
    assert_ne!(
        settings,
        temporary.join("compare-all"),
        "a fixed shared name"
    );
    assert!(journals.starts_with(&settings));
    assert!(std::fs::read_dir(&working)?.next().is_none());
    Ok(())
}
