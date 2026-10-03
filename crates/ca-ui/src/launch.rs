//! Starting another program over the files a view holds.
//!
//! An entry of the Open With list is a program and a list of arguments, never a
//! shell string, so a path holding a space or a quotation mark cannot start a
//! second command. Starting one touches the operating system, so it runs on a
//! worker and the view is told what happened through a message.
//!
//! The spawner is a trait, so a test states what starting a program does
//! without starting one.

use crate::worker::{Job, Terminal};
use ca_session::options::{LaunchCommand, LaunchContext, OpenWithEntry, OpenWithOptions};
use std::sync::Arc;

/// What an entry is offered over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// One or more files.
    Files,
    /// One or more folders.
    Folders,
}

/// What a view would run an Open With entry over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTarget {
    /// Whether the named items are files or folders.
    pub selection: Selection,
    /// The items themselves.
    pub context: LaunchContext,
}

impl LaunchTarget {
    /// A target over two files.
    #[must_use]
    pub fn files(context: LaunchContext) -> Self {
        Self {
            selection: Selection::Files,
            context,
        }
    }

    /// A target over two folders.
    #[must_use]
    pub fn folders(context: LaunchContext) -> Self {
        Self {
            selection: Selection::Folders,
            context,
        }
    }
}

/// What starting a program reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMessage {
    /// The program was started.
    Started {
        /// The label of the entry that started it.
        description: String,
    },
    /// The program could not be started.
    Failed {
        /// What to tell the user.
        reason: String,
    },
}

impl Terminal for LaunchMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        LaunchMessage::Failed {
            reason: "Starting the program stopped.".to_owned(),
        }
    }

    fn panicked(detail: String) -> Self {
        LaunchMessage::Failed { reason: detail }
    }
}

/// Starts a built command.
///
/// The trait exists so a test can assert on the program and the arguments a
/// menu entry produced without a process being created.
pub trait Spawner: Send + Sync + 'static {
    /// Start `command` without waiting for it to end.
    ///
    /// # Errors
    /// Returns what to tell the user when the program cannot be started.
    fn spawn(&self, command: &LaunchCommand) -> Result<(), String>;
}

/// The spawner that starts a real process.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSpawner;

impl Spawner for SystemSpawner {
    fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
        let mut process = ca_io::host_command::host_command(&command.program);
        process
            .args(&command.arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(directory) = &command.working_directory {
            process.current_dir(directory);
        }
        // The child is not waited for, so the program keeps running after the
        // comparison it was started from is closed.
        process.spawn().map(|_| ()).map_err(|error| {
            format!(
                "{} could not be started: {error}",
                command.program.display()
            )
        })
    }
}

/// The entries offered over `selection`, with the index each one sits at.
#[must_use]
pub fn offered(options: &OpenWithOptions, selection: Selection) -> Vec<(usize, &'_ OpenWithEntry)> {
    options
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| match selection {
            Selection::Files => entry.accepts_files,
            Selection::Folders => entry.accepts_folders,
        })
        .collect()
}

/// The label a menu line shows for one entry.
#[must_use]
pub fn label_of(entry: &OpenWithEntry) -> String {
    if entry.description.is_empty() {
        entry.program.as_path().display().to_string()
    } else {
        entry.description.clone()
    }
}

/// The commands one entry runs over `context`.
///
/// An entry that asks for one run per selected item produces one command per
/// side; every other entry produces one command that names both sides.
#[must_use]
pub fn commands_of(entry: &OpenWithEntry, context: &LaunchContext) -> Vec<LaunchCommand> {
    if !entry.multiple_instances {
        return vec![entry.command(context)];
    }
    let mut built = vec![entry.command(&LaunchContext {
        first: context.first.clone(),
        second: None,
    })];
    if let Some(second) = context.second.clone() {
        built.push(entry.command(&LaunchContext {
            first: second,
            second: None,
        }));
    }
    built
}

/// The command that hands `path` to whatever the platform opens it with.
///
/// Every platform reaches its handler through a program and arguments, never
/// through a shell string, so a name holding a space or a quotation mark cannot
/// start a second command.
#[must_use]
pub fn system_open_command(path: &std::path::Path) -> LaunchCommand {
    let text = path.display().to_string();
    #[cfg(target_os = "windows")]
    let (program, arguments) = (
        std::path::PathBuf::from("rundll32.exe"),
        vec!["url.dll,FileProtocolHandler".to_owned(), text],
    );
    #[cfg(target_os = "macos")]
    let (program, arguments) = (std::path::PathBuf::from("/usr/bin/open"), vec![text]);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let (program, arguments) = (std::path::PathBuf::from("xdg-open"), vec![text]);
    LaunchCommand {
        program,
        arguments,
        working_directory: path.parent().map(std::path::Path::to_path_buf),
    }
}

/// The command that shows `path` in the platform's file manager.
///
/// Files are selected where the platform offers that operation. Folders are
/// opened directly. The path is always one process argument; it is never
/// interpolated into a shell command.
#[must_use]
pub fn system_explorer_command(path: &std::path::Path, selection: Selection) -> LaunchCommand {
    if std::env::consts::OS == "windows" {
        return windows_explorer_command(path, selection);
    }
    let (program, arguments) = match (std::env::consts::OS, selection) {
        ("macos", Selection::Files) => (
            std::path::PathBuf::from("/usr/bin/open"),
            vec!["-R".to_owned(), path.display().to_string()],
        ),
        ("macos", Selection::Folders) => (
            std::path::PathBuf::from("/usr/bin/open"),
            vec![path.display().to_string()],
        ),
        (_, Selection::Files) => (
            std::path::PathBuf::from("xdg-open"),
            vec![path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .display()
                .to_string()],
        ),
        (_, Selection::Folders) => (
            std::path::PathBuf::from("xdg-open"),
            vec![path.display().to_string()],
        ),
    };
    LaunchCommand {
        program,
        arguments,
        working_directory: None,
    }
}

fn windows_explorer_command(path: &std::path::Path, selection: Selection) -> LaunchCommand {
    let arguments = match selection {
        Selection::Files => vec!["/select,".to_owned(), path.display().to_string()],
        Selection::Folders => vec![path.display().to_string()],
    };
    LaunchCommand {
        program: std::path::PathBuf::from("explorer.exe"),
        arguments,
        working_directory: None,
    }
}

/// Show a path in the platform's file manager on a worker.
pub fn reveal(
    path: &std::path::Path,
    selection: Selection,
    spawner: Arc<dyn Spawner>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<LaunchMessage> {
    let command = system_explorer_command(path, selection);
    let description = path.display().to_string();
    Job::spawn_notifying(
        move |emitter, _| {
            emitter.send(match spawner.spawn(&command) {
                Ok(()) => LaunchMessage::Started { description },
                Err(reason) => LaunchMessage::Failed { reason },
            });
        },
        notify,
    )
}

/// Hand `path` to the platform's handler on a worker.
pub fn open_with_system(
    path: &std::path::Path,
    spawner: Arc<dyn Spawner>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<LaunchMessage> {
    let command = system_open_command(path);
    let description = path.display().to_string();
    Job::spawn_notifying(
        move |emitter, _| {
            emitter.send(match spawner.spawn(&command) {
                Ok(()) => LaunchMessage::Started { description },
                Err(reason) => LaunchMessage::Failed { reason },
            });
        },
        notify,
    )
}

/// The command that hands a web address to the platform's browser.
///
/// The address is one argument, never part of a shell string. No working
/// folder is set, because the parent of an address is no folder.
#[must_use]
pub fn system_url_command(url: &str) -> LaunchCommand {
    let mut command = system_open_command(std::path::Path::new(url));
    command.working_directory = None;
    command
}

/// Hand a web address to the platform's browser on a worker.
pub fn open_url_with_system(
    url: &str,
    spawner: Arc<dyn Spawner>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<LaunchMessage> {
    let command = system_url_command(url);
    let description = url.to_owned();
    Job::spawn_notifying(
        move |emitter, _| {
            emitter.send(match spawner.spawn(&command) {
                Ok(()) => LaunchMessage::Started { description },
                Err(reason) => LaunchMessage::Failed { reason },
            });
        },
        notify,
    )
}

/// Start one entry over `context` on a worker.
pub fn spawn(
    entry: &OpenWithEntry,
    context: &LaunchContext,
    spawner: Arc<dyn Spawner>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<LaunchMessage> {
    let description = label_of(entry);
    let commands = commands_of(entry, context);
    Job::spawn_notifying(
        move |emitter, _| {
            for command in &commands {
                if let Err(reason) = spawner.spawn(command) {
                    emitter.send(LaunchMessage::Failed { reason });
                    return;
                }
            }
            emitter.send(LaunchMessage::Started { description });
        },
        notify,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{
        commands_of, label_of, offered, spawn, system_explorer_command, windows_explorer_command,
        LaunchMessage, Selection, Spawner,
    };
    use ca_session::options::{
        LaunchCommand, LaunchContext, LaunchSide, OpenWithEntry, OpenWithOptions,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A spawner that records what it was asked to start.
    #[derive(Default)]
    struct Recorder {
        started: Mutex<Vec<LaunchCommand>>,
        refuse: Option<String>,
    }

    impl Spawner for Arc<Recorder> {
        fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
            if let Some(reason) = &self.refuse {
                return Err(reason.clone());
            }
            self.started.lock().unwrap().push(command.clone());
            Ok(())
        }
    }

    fn entry() -> OpenWithEntry {
        OpenWithEntry {
            description: "Editor".to_owned(),
            program: PathBuf::from("editor").into(),
            arguments: vec!["--line".to_owned(), "%l".to_owned(), "%f".to_owned()],
            ..OpenWithEntry::default()
        }
    }

    #[test]
    fn explorer_commands_keep_paths_as_arguments() {
        let file = std::path::Path::new("/tmp/a file's name.txt");
        let command = system_explorer_command(file, Selection::Files);
        #[cfg(target_os = "windows")]
        assert_eq!(command.arguments, ["/select,", "/tmp/a file's name.txt"]);
        #[cfg(target_os = "macos")]
        assert_eq!(command.arguments, ["-R", "/tmp/a file's name.txt"]);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert_eq!(command.arguments, ["/tmp"]);
        assert!(command.working_directory.is_none());

        let comma_path = std::path::Path::new(r"C:\work tree\a,b.txt");
        let command = windows_explorer_command(comma_path, Selection::Files);
        assert_eq!(command.program, PathBuf::from("explorer.exe"));
        assert_eq!(command.arguments, ["/select,", r"C:\work tree\a,b.txt"]);

        let folder = std::path::Path::new("/tmp/a folder");
        let command = system_explorer_command(folder, Selection::Folders);
        assert_eq!(
            command.arguments.last().map(String::as_str),
            Some("/tmp/a folder")
        );
    }

    fn context() -> LaunchContext {
        LaunchContext {
            first: LaunchSide {
                path: PathBuf::from("/base/one file.txt"),
                base: Some(PathBuf::from("/base")),
                line: Some(9),
            },
            second: Some(LaunchSide {
                path: PathBuf::from("/base/two.txt"),
                base: Some(PathBuf::from("/base")),
                line: Some(3),
            }),
        }
    }

    fn settle(job: &mut crate::worker::Job<LaunchMessage>) -> LaunchMessage {
        let mut seen = None;
        assert!(
            crate::testing::wait_until(Duration::from_secs(10), || {
                for message in job.drain() {
                    seen = Some(message);
                }
                seen.is_some()
            }),
            "the launcher never answered"
        );
        seen.unwrap()
    }

    #[test]
    fn a_web_address_is_one_argument_with_no_working_folder() {
        let url = "https://example.invalid/releases/tag/v1 &x=\"y\"";
        let command = super::system_url_command(url);
        assert_eq!(command.arguments.last().map(String::as_str), Some(url));
        assert_eq!(command.working_directory, None);
    }

    #[test]
    fn only_the_entries_that_accept_the_selection_are_offered() {
        let mut files = entry();
        files.description = "Over files".to_owned();
        let mut folders = entry();
        folders.description = "Over folders".to_owned();
        folders.accepts_files = false;
        folders.accepts_folders = true;
        let options = OpenWithOptions {
            entries: vec![files, folders],
            ..OpenWithOptions::default()
        };
        let listed: Vec<String> = offered(&options, Selection::Files)
            .into_iter()
            .map(|(_, entry)| label_of(entry))
            .collect();
        assert_eq!(listed, vec!["Over files".to_owned()]);
        let listed: Vec<String> = offered(&options, Selection::Folders)
            .into_iter()
            .map(|(_, entry)| label_of(entry))
            .collect();
        assert_eq!(listed, vec!["Over folders".to_owned()]);
    }

    /// The command reaches the spawner as a program and separate arguments, so
    /// nothing a path holds can be read as a second command.
    #[test]
    fn starting_an_entry_hands_the_spawner_a_program_and_its_arguments() {
        let recorder = Arc::new(Recorder::default());
        let mut job = spawn(
            &entry(),
            &context(),
            Arc::new(Arc::clone(&recorder)),
            Arc::new(|| {}),
        );
        assert_eq!(
            settle(&mut job),
            LaunchMessage::Started {
                description: "Editor".to_owned()
            }
        );
        let started = recorder.started.lock().unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].program, PathBuf::from("editor"));
        assert_eq!(started[0].arguments.len(), 3);
        assert_eq!(started[0].arguments[1], "9");
        assert!(started[0].arguments[2].contains("one file.txt"));
    }

    #[test]
    fn a_failure_to_start_is_reported_rather_than_dropped() {
        let recorder = Arc::new(Recorder {
            refuse: Some("no such program".to_owned()),
            ..Recorder::default()
        });
        let mut job = spawn(
            &entry(),
            &context(),
            Arc::new(Arc::clone(&recorder)),
            Arc::new(|| {}),
        );
        assert_eq!(
            settle(&mut job),
            LaunchMessage::Failed {
                reason: "no such program".to_owned()
            }
        );
        assert!(recorder.started.lock().unwrap().is_empty());
    }

    #[test]
    fn one_run_per_item_names_each_side_on_its_own() {
        let mut entry = entry();
        entry.multiple_instances = true;
        entry.arguments = vec!["%n".to_owned()];
        let built = commands_of(&entry, &context());
        assert_eq!(built.len(), 2);
        assert_eq!(built[0].arguments, vec!["one file.txt".to_owned()]);
        assert_eq!(built[1].arguments, vec!["two.txt".to_owned()]);

        entry.multiple_instances = false;
        entry.arguments = vec!["%n1".to_owned(), "%n2".to_owned()];
        let built = commands_of(&entry, &context());
        assert_eq!(built.len(), 1);
        assert_eq!(
            built[0].arguments,
            vec!["one file.txt".to_owned(), "two.txt".to_owned()]
        );
    }

    #[test]
    fn an_entry_with_no_description_is_labeled_by_its_program() {
        let mut entry = entry();
        entry.description.clear();
        assert_eq!(
            label_of(&entry),
            PathBuf::from("editor").display().to_string()
        );
    }
}
