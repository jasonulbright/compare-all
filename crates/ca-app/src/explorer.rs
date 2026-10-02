//! The classic file manager context menu on Windows: two verbs written as
//! registry keys under `Software\Classes`, with no shell extension.
//!
//! A verb of this kind starts the program once for each selected item, so a
//! comparison takes two steps: the first verb remembers the left side, and the
//! second compares the selected item against it.

use std::path::{Path, PathBuf};

/// Key name of the verb that remembers the left side.
pub const SELECT_VERB: &str = "compare-all.selectleft";
/// Key name of the verb that compares against the remembered left side.
pub const COMPARE_VERB: &str = "compare-all.compare";
/// Menu text of the verb that remembers the left side.
pub const SELECT_TEXT: &str = "Select Left Side for Compare All";
/// Menu text of the verb that compares against the remembered left side.
pub const COMPARE_TEXT: &str = "Compare with Compare All";
/// Class keys the verbs are written under: every file, and every folder.
pub const CLASSES: [&str; 2] = ["*", "Directory"];

/// Where the verbs live.
///
/// The user key is written and removed. The machine key is only read: the
/// installer writes it for a per-machine install, and only the installer
/// removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRoot {
    /// Path under `HKEY_CURRENT_USER` that stands for `Software\Classes`.
    pub user: String,
    /// Path under `HKEY_LOCAL_MACHINE` that stands for `Software\Classes`.
    pub machine: Option<String>,
}

impl MenuRoot {
    /// The keys the file manager reads.
    #[must_use]
    pub fn system() -> Self {
        Self {
            user: r"Software\Classes".to_owned(),
            machine: Some(r"Software\Classes".to_owned()),
        }
    }
}

/// Which keys hold the verbs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Installed {
    /// The verbs are under the user key.
    pub user: bool,
    /// The verbs are under the machine key.
    pub machine: bool,
}

impl Installed {
    /// True when the menu shows the verbs.
    #[must_use]
    pub const fn any(self) -> bool {
        self.user || self.machine
    }
}

/// The command a verb runs for the selected item.
#[must_use]
pub fn command_line(program: &Path, switch: &str) -> String {
    format!("\"{}\" {switch} \"%1\"", program.display())
}

/// The program the verbs start: this executable.
///
/// # Errors
///
/// Returns the reason when the path of the running program is not known.
pub fn this_program() -> std::io::Result<PathBuf> {
    std::env::current_exe()
}

#[cfg(windows)]
mod platform {
    use super::{
        command_line, Installed, MenuRoot, CLASSES, COMPARE_TEXT, COMPARE_VERB, SELECT_TEXT,
        SELECT_VERB,
    };
    use crate::cli::{COMPARE_LEFT_SWITCH, LEFT_SIDE_SWITCH};
    use std::path::Path;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    fn verb_key(base: &str, class: &str, verb: &str) -> String {
        format!(r"{base}\{class}\shell\{verb}")
    }

    fn holds_verbs(hive: &RegKey, base: &str) -> bool {
        CLASSES.iter().all(|class| {
            [SELECT_VERB, COMPARE_VERB].iter().all(|verb| {
                hive.open_subkey_with_flags(
                    format!(r"{}\command", verb_key(base, class, verb)),
                    KEY_READ,
                )
                .is_ok()
            })
        })
    }

    pub fn installed(root: &MenuRoot) -> Installed {
        let user = holds_verbs(&RegKey::predef(HKEY_CURRENT_USER), &root.user);
        let machine = root
            .machine
            .as_deref()
            .is_some_and(|base| holds_verbs(&RegKey::predef(HKEY_LOCAL_MACHINE), base));
        Installed { user, machine }
    }

    pub fn install(root: &MenuRoot, program: &Path) -> std::io::Result<()> {
        let hive = RegKey::predef(HKEY_CURRENT_USER);
        let icon = program.display().to_string();
        for class in CLASSES {
            for (verb, text, switch) in [
                (SELECT_VERB, SELECT_TEXT, LEFT_SIDE_SWITCH),
                (COMPARE_VERB, COMPARE_TEXT, COMPARE_LEFT_SWITCH),
            ] {
                let (key, _) = hive.create_subkey(verb_key(&root.user, class, verb))?;
                key.set_value("MUIVerb", &text)?;
                key.set_value("Icon", &icon)?;
                let (command, _) = key.create_subkey("command")?;
                command.set_value("", &command_line(program, switch))?;
            }
        }
        Ok(())
    }

    pub fn remove(root: &MenuRoot) -> std::io::Result<()> {
        let hive = RegKey::predef(HKEY_CURRENT_USER);
        for class in CLASSES {
            for verb in [SELECT_VERB, COMPARE_VERB] {
                match hive.delete_subkey_all(verb_key(&root.user, class, verb)) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(error)
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{Installed, MenuRoot};
    use std::path::Path;

    pub fn installed(_root: &MenuRoot) -> Installed {
        Installed::default()
    }

    pub fn install(_root: &MenuRoot, _program: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            super::UNSUPPORTED,
        ))
    }

    pub fn remove(_root: &MenuRoot) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            super::UNSUPPORTED,
        ))
    }
}

/// Why the menu cannot be changed on this platform.
pub const UNSUPPORTED: &str = "The file manager menu exists on Windows only.";

/// True when this platform has the menu.
pub const AVAILABLE: bool = cfg!(windows);

/// Which keys hold the verbs. A key that cannot be read counts as empty.
#[must_use]
pub fn installed(root: &MenuRoot) -> Installed {
    platform::installed(root)
}

/// Writes both verbs for every file and folder under the user key.
///
/// # Errors
///
/// Returns the first registry error.
pub fn install(root: &MenuRoot, program: &Path) -> std::io::Result<()> {
    platform::install(root, program)
}

/// Removes both verbs from the user key. A verb that is not there is not an
/// error.
///
/// # Errors
///
/// Returns the first registry error.
pub fn remove(root: &MenuRoot) -> std::io::Result<()> {
    platform::remove(root)
}

/// What a change of the menu produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// The state after the change, or after a read.
    State(Installed),
    /// The change failed; the state is read again after it.
    Failed(String, Installed),
    /// The worker stopped before it reported.
    Cancelled,
}

impl ca_ui::worker::Terminal for Message {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail, Installed::default())
    }
}

/// Reads the state on a worker, and first writes or removes the user verbs
/// when `wanted` says so.
#[must_use]
pub fn spawn(
    root: MenuRoot,
    wanted: Option<bool>,
    notify: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> ca_ui::worker::Job<Message> {
    ca_ui::worker::Job::spawn_notifying(
        move |emitter: &ca_ui::worker::Emitter<Message>, _cancel| {
            let changed = match wanted {
                Some(true) => this_program().and_then(|program| install(&root, &program)),
                Some(false) => remove(&root),
                None => Ok(()),
            };
            let state = installed(&root);
            emitter.send(match changed {
                Ok(()) => Message::State(state),
                Err(error) => Message::Failed(error.to_string(), state),
            });
        },
        notify,
    )
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{command_line, install, installed, remove, MenuRoot, COMPARE_VERB};
    use std::path::Path;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    /// Parent of every key these tests create. Nothing outside it is written.
    const TEST_ROOT: &str = r"Software\compare-all-tests";

    struct Throwaway(String);

    impl Throwaway {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            Self(format!(
                r"{TEST_ROOT}\explorer-{name}-{}-{nanos}",
                std::process::id()
            ))
        }
    }

    impl Drop for Throwaway {
        fn drop(&mut self) {
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
        }
    }

    #[test]
    fn the_verbs_are_written_read_and_removed_under_the_given_root() {
        let key = Throwaway::new("roundtrip");
        let root = MenuRoot {
            user: key.0.clone(),
            machine: None,
        };
        assert!(!installed(&root).any());
        let program = Path::new(r"C:\Program Files\compare-all\compare-all.exe");
        install(&root, program).unwrap();
        let state = installed(&root);
        assert!(state.user && !state.machine);
        let command: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!(r"{}\Directory\shell\{COMPARE_VERB}\command", key.0))
            .unwrap()
            .get_value("")
            .unwrap();
        assert_eq!(command, command_line(program, "/compareleft"));
        assert_eq!(
            command,
            r#""C:\Program Files\compare-all\compare-all.exe" /compareleft "%1""#
        );
        remove(&root).unwrap();
        assert!(!installed(&root).any());
        remove(&root).unwrap();
    }
}
