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
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE};
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

    pub fn refresh_text(root: &MenuRoot) -> std::io::Result<()> {
        let hive = RegKey::predef(HKEY_CURRENT_USER);
        for class in CLASSES {
            for (verb, text) in [(SELECT_VERB, SELECT_TEXT), (COMPARE_VERB, COMPARE_TEXT)] {
                let key = match hive.open_subkey_with_flags(
                    verb_key(&root.user, class, verb),
                    KEY_READ | KEY_SET_VALUE,
                ) {
                    Ok(key) => key,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                let current: std::io::Result<String> = key.get_value("MUIVerb");
                if current.ok().as_deref() != Some(text) {
                    key.set_value("MUIVerb", &text)?;
                }
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

    pub fn refresh_text(_root: &MenuRoot) -> std::io::Result<()> {
        Ok(())
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

/// Gives each verb under the user key the menu text of this build. A verb
/// that is not there stays absent, and the icon and command stay as they are.
///
/// # Errors
///
/// Returns the first registry error.
pub fn refresh_text(root: &MenuRoot) -> std::io::Result<()> {
    platform::refresh_text(root)
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
///
/// With no `wanted`, registered user verbs first get the menu text of this
/// build. A failure there leaves the old text and is not reported.
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
                None => {
                    if installed(&root).user {
                        let _ = refresh_text(&root);
                    }
                    Ok(())
                }
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
    use super::{
        command_line, install, installed, remove, spawn, Installed, MenuRoot, Message, CLASSES,
        COMPARE_TEXT, COMPARE_VERB, SELECT_TEXT, SELECT_VERB,
    };
    use crate::cli::{COMPARE_LEFT_SWITCH, LEFT_SIDE_SWITCH};
    use std::path::Path;
    use std::time::Duration;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
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

    fn user_root(key: &Throwaway) -> MenuRoot {
        MenuRoot {
            user: key.0.clone(),
            machine: None,
        }
    }

    /// Runs the read that starts with the window and returns what it sent.
    fn read_at_start(root: &MenuRoot) -> Vec<Message> {
        let notify: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {});
        let mut job = spawn(root.clone(), None, notify);
        let messages = job.wait(Duration::from_secs(20));
        assert!(job.is_finished());
        messages
    }

    fn verb_path(root: &MenuRoot, class: &str, verb: &str) -> String {
        format!(r"{}\{class}\shell\{verb}", root.user)
    }

    fn value(path: &str, name: &str) -> String {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(path)
            .unwrap()
            .get_value(name)
            .unwrap()
    }

    fn last_write(path: &str) -> (u32, u32) {
        let info = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(path)
            .unwrap()
            .query_info()
            .unwrap();
        (
            info.last_write_time.dwLowDateTime,
            info.last_write_time.dwHighDateTime,
        )
    }

    fn texts() -> [(&'static str, &'static str); 2] {
        [(SELECT_VERB, SELECT_TEXT), (COMPARE_VERB, COMPARE_TEXT)]
    }

    /// Writes the verbs for `program`, then gives every verb `stale` as its
    /// menu text.
    fn install_with_text(root: &MenuRoot, program: &Path, stale: &str) {
        install(root, program).unwrap();
        for class in CLASSES {
            for (verb, _) in texts() {
                RegKey::predef(HKEY_CURRENT_USER)
                    .open_subkey_with_flags(verb_path(root, class, verb), KEY_SET_VALUE)
                    .unwrap()
                    .set_value("MUIVerb", &stale)
                    .unwrap();
            }
        }
    }

    #[test]
    fn the_read_at_start_gives_registered_verbs_the_current_menu_text() {
        let key = Throwaway::new("stale-text");
        let root = user_root(&key);
        let program = Path::new(r"C:\Program Files\compare-all\compare-all.exe");
        install_with_text(&root, program, "Select Left Side for compare-all");
        assert_eq!(
            read_at_start(&root),
            vec![Message::State(Installed {
                user: true,
                machine: false
            })]
        );
        for class in CLASSES {
            for (verb, text) in texts() {
                assert_eq!(value(&verb_path(&root, class, verb), "MUIVerb"), text);
            }
        }
    }

    #[test]
    fn the_read_at_start_creates_no_verbs_when_none_are_registered() {
        let key = Throwaway::new("absent");
        let root = user_root(&key);
        assert_eq!(
            read_at_start(&root),
            vec![Message::State(Installed::default())]
        );
        assert!(RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&key.0)
            .is_err());
    }

    #[test]
    fn the_read_at_start_leaves_a_partial_registration_alone() {
        let key = Throwaway::new("partial");
        let root = user_root(&key);
        let program = Path::new(r"C:\Program Files\compare-all\compare-all.exe");
        install_with_text(&root, program, "stale");
        RegKey::predef(HKEY_CURRENT_USER)
            .delete_subkey_all(verb_path(&root, "Directory", COMPARE_VERB))
            .unwrap();
        assert_eq!(
            read_at_start(&root),
            vec![Message::State(Installed::default())]
        );
        assert!(RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(verb_path(&root, "Directory", COMPARE_VERB))
            .is_err());
        assert_eq!(
            value(&verb_path(&root, "*", SELECT_VERB), "MUIVerb"),
            "stale"
        );
    }

    #[test]
    fn the_read_at_start_keeps_the_icon_and_command_of_another_program() {
        let key = Throwaway::new("other-program");
        let root = user_root(&key);
        let other = Path::new(r"D:\elsewhere\compare-all.exe");
        install_with_text(&root, other, "stale");
        read_at_start(&root);
        for class in CLASSES {
            for (verb, switch) in [
                (SELECT_VERB, LEFT_SIDE_SWITCH),
                (COMPARE_VERB, COMPARE_LEFT_SWITCH),
            ] {
                let path = verb_path(&root, class, verb);
                assert_eq!(value(&path, "Icon"), other.display().to_string());
                assert_eq!(
                    value(&format!(r"{path}\command"), ""),
                    command_line(other, switch)
                );
            }
        }
    }

    #[test]
    fn the_read_at_start_writes_nothing_when_the_text_is_current() {
        let key = Throwaway::new("current");
        let root = user_root(&key);
        let program = Path::new(r"C:\Program Files\compare-all\compare-all.exe");
        install(&root, program).unwrap();
        let paths: Vec<String> = CLASSES
            .iter()
            .flat_map(|class| texts().map(|(verb, _)| verb_path(&root, class, verb)))
            .collect();
        let before: Vec<(u32, u32)> = paths.iter().map(|path| last_write(path)).collect();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            read_at_start(&root),
            vec![Message::State(Installed {
                user: true,
                machine: false
            })]
        );
        let after: Vec<(u32, u32)> = paths.iter().map(|path| last_write(path)).collect();
        assert_eq!(before, after);
        for class in CLASSES {
            for (verb, text) in texts() {
                assert_eq!(value(&verb_path(&root, class, verb), "MUIVerb"), text);
            }
        }
    }
}
