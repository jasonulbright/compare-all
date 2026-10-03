//! Where the running application keeps its state when the environment names
//! no home, and that a program folder it cannot write stays untouched.
//!
//! Each case runs this test binary again with a changed environment, so the
//! test process itself never changes a variable.

use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PROBE: &str = "COMPARE_ALL_STATE_PROBE";

#[test]
fn state_probe() -> Result<()> {
    let Some(mode) = std::env::var_os(PROBE) else {
        return Ok(());
    };
    println!("settings={}", ca_ui::paths::settings_directory().display());
    println!("notice={}", ca_ui::paths::settings_notice().unwrap_or(""));
    if mode == "store" {
        let mut handle = ca_ui::sessions::StoreHandle::open(std::sync::Arc::new(|| {}));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while handle.state() != ca_ui::sessions::StoreState::Ready
            && std::time::Instant::now() < deadline
        {
            handle.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        println!("store-notice={}", handle.notice().unwrap_or(""));
        handle.close();
    }
    if mode == "write" {
        let settings = ca_ui::paths::settings_directory();
        let outcome = ca_session::SettingsLock::acquire(&settings)?;
        ca_session::SessionStore::default().save_replacing(&settings.join("sessions.json"))?;
        ca_ui::paths::sweep_temporary()?;
        drop(outcome);
    }
    Ok(())
}

/// A copy of this test binary in a folder of its own, so no state file
/// beside the build output can select portable mode.
struct Probe {
    folder: tempfile::TempDir,
    executable: PathBuf,
}

impl Probe {
    fn new() -> Result<Self> {
        let folder = tempfile::tempdir()?;
        let program = folder.path().join("program");
        std::fs::create_dir(&program)?;
        let executable = program.join(if cfg!(windows) { "probe.exe" } else { "probe" });
        std::fs::copy(std::env::current_exe()?, &executable)?;
        Ok(Self { folder, executable })
    }

    fn program_folder(&self) -> &Path {
        self.executable.parent().unwrap_or(self.folder.path())
    }

    /// A command with every variable that names a per-user folder removed.
    fn command(&self, mode: &str) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args(["--exact", "state_probe", "--nocapture"])
            .env(PROBE, mode)
            .env_remove(ca_session::SETTINGS_DIRECTORY_VARIABLE)
            .env_remove("HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("APPDATA");
        command
    }

    fn run(command: &mut Command) -> Result<Vec<(String, String)>> {
        let child = command.output()?;
        let stdout = String::from_utf8(child.stdout)?;
        assert!(
            child.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&child.stderr)
        );
        Ok(stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect())
    }
}

fn value<'a>(lines: &'a [(String, String)], key: &str) -> &'a str {
    lines
        .iter()
        .find(|(name, _)| name == key)
        .map_or("", |(_, value)| value.as_str())
}

/// The variable the platform's temporary folder is read from.
fn temporary_variables(command: &mut Command, folder: &Path) {
    command
        .env("TMPDIR", folder)
        .env("TEMP", folder)
        .env("TMP", folder);
}

#[test]
fn without_a_home_state_goes_to_a_private_folder_made_for_the_run() -> Result<()> {
    let probe = Probe::new()?;
    let shared = probe.folder.path().join("shared-temporary");
    std::fs::create_dir(&shared)?;
    let mut command = probe.command("store");
    temporary_variables(&mut command, &shared);
    let lines = Probe::run(&mut command)?;
    let settings = PathBuf::from(value(&lines, "settings"));
    let notice = value(&lines, "notice");
    assert!(
        notice.contains("No home folder is known") && notice.contains("not kept"),
        "{notice}"
    );
    assert!(notice.contains(&settings.display().to_string()), "{notice}");
    let store_notice = value(&lines, "store-notice");
    assert!(store_notice.starts_with(notice), "{store_notice}");
    assert!(!store_notice.contains("Another instance"), "{store_notice}");
    assert_ne!(settings, shared.join("compare-all"), "a fixed shared name");
    assert!(settings.starts_with(&shared), "{}", settings.display());
    let name = settings
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert!(
        name.starts_with("compare-all-") && name.len() > "compare-all-".len(),
        "{name}"
    );
    assert!(settings.is_dir(), "the folder is made at once");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&settings)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn without_a_home_the_runtime_folder_is_preferred() -> Result<()> {
    let probe = Probe::new()?;
    let runtime = probe.folder.path().join("runtime");
    std::fs::create_dir(&runtime)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut command = probe.command("read");
    command.env("XDG_RUNTIME_DIR", &runtime);
    let lines = Probe::run(&mut command)?;
    assert_eq!(
        PathBuf::from(value(&lines, "settings")),
        runtime.join("compare-all")
    );
    let notice = value(&lines, "notice");
    assert!(notice.contains("until you log out"), "{notice}");
    Ok(())
}

/// A runtime folder open to others, such as the shared temporary folder, can
/// hold an application folder another user made in advance.
#[cfg(unix)]
#[test]
fn without_a_home_an_open_runtime_folder_is_not_used() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let probe = Probe::new()?;
    let runtime = probe.folder.path().join("open-runtime");
    let planted = runtime.join("compare-all");
    std::fs::create_dir_all(&planted)?;
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o777))?;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o1777))?;
    let shared = probe.folder.path().join("shared-temporary");
    std::fs::create_dir(&shared)?;
    let mut command = probe.command("write");
    command.env("XDG_RUNTIME_DIR", &runtime);
    temporary_variables(&mut command, &shared);
    let lines = Probe::run(&mut command)?;
    let settings = PathBuf::from(value(&lines, "settings"));
    assert!(!settings.starts_with(&runtime), "{}", settings.display());
    assert!(settings.starts_with(&shared), "{}", settings.display());
    assert!(std::fs::read_dir(&planted)?.next().is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn without_a_home_relative_folders_never_reach_the_working_directory() -> Result<()> {
    let probe = Probe::new()?;
    let working = probe.folder.path().join("working");
    std::fs::create_dir(&working)?;
    let mut command = probe.command("read");
    command
        .current_dir(&working)
        .env("XDG_RUNTIME_DIR", "runtime")
        .env("TMPDIR", "temporary");
    let lines = Probe::run(&mut command)?;
    let settings = PathBuf::from(value(&lines, "settings"));
    let leftover = settings.clone();
    assert!(settings.is_absolute(), "{}", settings.display());
    assert!(!settings.starts_with(&working), "{}", settings.display());
    assert!(std::fs::read_dir(&working)?.next().is_none());
    if leftover.starts_with(std::env::temp_dir()) {
        let _ = std::fs::remove_dir_all(leftover);
    }
    Ok(())
}

/// An image mount is read-only: the program folder holds no state file, so
/// the per-user folder is chosen and nothing is written beside the program.
#[test]
fn a_read_only_program_folder_falls_to_the_per_user_folder_untouched() -> Result<()> {
    let probe = Probe::new()?;
    let home = probe.folder.path().join("home");
    std::fs::create_dir(&home)?;
    let before: Vec<_> = std::fs::read_dir(probe.program_folder())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            probe.program_folder(),
            std::fs::Permissions::from_mode(0o555),
        )?;
    }
    let mut command = probe.command("write");
    command
        .env("HOME", &home)
        .env("APPDATA", &home)
        .env("XDG_CONFIG_HOME", &home);
    let lines = Probe::run(&mut command);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            probe.program_folder(),
            std::fs::Permissions::from_mode(0o755),
        )?;
    }
    let lines = lines?;
    let expected = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/compare-all")
    } else {
        home.join("compare-all")
    };
    assert_eq!(PathBuf::from(value(&lines, "settings")), expected);
    assert_eq!(value(&lines, "notice"), "");
    assert!(expected.join("sessions.json").is_file());
    let after: Vec<_> = std::fs::read_dir(probe.program_folder())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(before, after);
    Ok(())
}
