//! The running application resolves its settings beside a portable executable.

use std::path::{Path, PathBuf};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn portable_settings_are_selected_by_the_running_application() -> Result<()> {
    if std::env::var_os("COMPARE_ALL_PATH_PROBE").is_some() {
        println!("settings={}", ca_ui::paths::settings_directory().display());
        println!("journals={}", ca_ui::paths::journal_directory().display());
        println!("temporary={}", ca_ui::paths::temporary_root().display());
        return Ok(());
    }
    let directory = tempfile::tempdir()?;
    let executable = directory
        .path()
        .join(if cfg!(windows) { "probe.exe" } else { "probe" });
    std::fs::copy(std::env::current_exe()?, &executable)?;
    let marker = directory.path().join(ca_session::store::PROGRAM_STATE_FILE);
    std::fs::write(&marker, "{}")?;
    let per_user = directory.path().join("per-user");
    let probe = |override_path: Option<&Path>, expected: &Path| -> Result<()> {
        let mut command = std::process::Command::new(&executable);
        command
            .args([
                "--exact",
                "portable_settings_are_selected_by_the_running_application",
                "--nocapture",
            ])
            .env("COMPARE_ALL_PATH_PROBE", "1")
            .env_remove(ca_session::SETTINGS_DIRECTORY_VARIABLE)
            .env("APPDATA", &per_user)
            .env("HOME", &per_user)
            .env("XDG_CONFIG_HOME", &per_user);
        if let Some(path) = override_path {
            command.env(ca_session::SETTINGS_DIRECTORY_VARIABLE, path);
        }
        let child = command.output()?;
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        let output = String::from_utf8(child.stdout)?;
        for (label, path) in [
            ("settings", PathBuf::from(expected)),
            ("journals", expected.join("journals")),
            ("temporary", expected.join("temporary")),
        ] {
            assert!(
                output.contains(&format!("{label}={}", path.display())),
                "{output}"
            );
        }
        Ok(())
    };
    probe(None, directory.path())?;
    let override_path = directory.path().join("explicit-settings");
    probe(Some(&override_path), &override_path)?;
    std::fs::remove_file(marker)?;
    let platform = if cfg!(target_os = "macos") {
        per_user.join("Library/Application Support/compare-all")
    } else {
        per_user.join("compare-all")
    };
    probe(None, &platform)?;
    Ok(())
}
