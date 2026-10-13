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

    /// Start `command`, a program that ends as soon as it has passed its
    /// request on, and wait for it to end. The call blocks, so it runs on a
    /// worker.
    ///
    /// # Errors
    /// Returns what to tell the user when the program cannot be started or
    /// ends with a failure status.
    fn run(&self, command: &LaunchCommand) -> Result<(), String> {
        self.spawn(command)
    }
}

/// The spawner that starts a real process.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSpawner;

/// How long [`Spawner::run`] waits for the program to end.
const RUN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

impl SystemSpawner {
    fn start(command: &LaunchCommand) -> Result<std::process::Child, String> {
        let mut process = ca_io::host_command::host_command(&command.program);
        add_arguments(&mut process, command);
        process
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(directory) = &command.working_directory {
            process.current_dir(directory);
        }
        process.spawn().map_err(|error| {
            format!(
                "{} could not be started: {error}",
                command.program.display()
            )
        })
    }
}

impl Spawner for SystemSpawner {
    fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
        // The child is not waited for, so the program keeps running after the
        // comparison it was started from is closed.
        Self::start(command).map(|_| ())
    }

    fn run(&self, command: &LaunchCommand) -> Result<(), String> {
        let mut child = Self::start(command)?;
        let deadline = std::time::Instant::now() + RUN_WAIT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(format!("{} ended with {status}", command.program.display()));
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                // A program that still runs has reported no failure. It is
                // left to end by itself.
                Ok(None) => return Ok(()),
                Err(error) => {
                    return Err(format!(
                        "{} could not be waited for: {error}",
                        command.program.display()
                    ));
                }
            }
        }
    }
}

/// Give `process` the arguments of `command`.
///
/// The standard library puts quotation marks around an argument only when
/// it holds a space or a tab. A program that splits its command line at
/// another character then reads one argument as two, so a command whose
/// builder quoted the arguments reaches Windows as written.
fn add_arguments(process: &mut std::process::Command, command: &LaunchCommand) {
    #[cfg(windows)]
    if command.arguments_as_written {
        use std::os::windows::process::CommandExt;
        for argument in &command.arguments {
            process.raw_arg(argument);
        }
        return;
    }
    process.args(&command.arguments);
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

/// `path` as one argument that a program reads as a path, never as an
/// option: a relative path that starts with a hyphen gets the current folder
/// in front of it.
fn path_argument(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    if text.starts_with('-') {
        format!(".{}{text}", std::path::MAIN_SEPARATOR)
    } else {
        text
    }
}

/// The command that hands `path` to whatever the platform opens it with.
///
/// Every platform reaches its handler through a program and arguments, never
/// through a shell string, so a name holding a space or a quotation mark cannot
/// start a second command.
#[must_use]
pub fn system_open_command(path: &std::path::Path) -> LaunchCommand {
    let text = path_argument(path);
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
        arguments_as_written: false,
    }
}

/// The command that shows `path` in the platform's file manager.
///
/// A folder is opened directly, and only a path that is a folder when this
/// call runs is opened: a request to open a file starts the program that
/// handles the file. Every other item is selected in its folder, or its
/// folder is opened where the platform has no selection. On macOS a folder
/// that the system takes for a package is shown from the inside without a
/// request to open it, because that request starts a program. The path is
/// always one process argument; it is never interpolated into a shell
/// command.
///
/// The call reads the file system, so a caller on the interface thread hands
/// it to a worker.
#[must_use]
pub fn system_explorer_command(path: &std::path::Path, selection: Selection) -> LaunchCommand {
    explorer_command_on(
        std::env::consts::OS,
        path,
        selection,
        std::path::Path::is_dir,
        mac_folder,
    )
}

/// How macOS treats a folder that it is asked to open.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum MacFolder {
    /// The file manager opens a window on the folder.
    Plain,
    /// The system takes the folder for a package: an application bundle, a
    /// document package or a folder with the bundle flag. A request to open
    /// it starts an application. `inside` is one item of the folder, or
    /// `None` when the folder is empty or cannot be read.
    Package { inside: Option<std::path::PathBuf> },
}

/// [`system_explorer_command`] for the platform named `os`. `is_folder` says
/// whether a path is a folder now, through its links. `mac_folder` answers
/// for a folder on macOS and is not called otherwise.
fn explorer_command_on(
    os: &str,
    path: &std::path::Path,
    selection: Selection,
    is_folder: impl Fn(&std::path::Path) -> bool,
    mac_folder: impl FnOnce(&std::path::Path) -> MacFolder,
) -> LaunchCommand {
    // The kind a view names can be the kind of the other side of its row, or
    // of an item that is replaced after the scan. Only a folder is opened.
    let selection = match selection {
        Selection::Folders if is_folder(path) => Selection::Folders,
        _ => Selection::Files,
    };
    if os == "windows" {
        return windows_explorer_command(path, selection);
    }
    let open = |arguments: Vec<String>| (std::path::PathBuf::from("/usr/bin/open"), arguments);
    let select = |item: &std::path::Path| open(vec!["-R".to_owned(), path_argument(item)]);
    let (program, arguments) = match (os, selection) {
        ("macos", Selection::Folders) => match mac_folder(path) {
            MacFolder::Plain => open(vec![path_argument(path)]),
            // Selecting an item of the package opens a window on the inside
            // of the package and starts nothing.
            MacFolder::Package {
                inside: Some(inside),
            } => select(&inside),
            MacFolder::Package { inside: None } => select(path),
        },
        ("macos", Selection::Files) => select(path),
        // The parent of a path that is gone can be a file.
        (_, Selection::Files) => (
            std::path::PathBuf::from("xdg-open"),
            vec![path_argument(
                path.ancestors()
                    .skip(1)
                    .find(|above| is_folder(above))
                    .unwrap_or_else(|| std::path::Path::new(".")),
            )],
        ),
        (_, Selection::Folders) => (
            std::path::PathBuf::from("xdg-open"),
            vec![path_argument(path)],
        ),
    };
    LaunchCommand {
        program,
        arguments,
        working_directory: None,
        arguments_as_written: false,
    }
}

/// What the system says about the folder `path`, through its links.
///
/// The answer comes from the service that decides what a request to open the
/// folder starts, so it also covers a package type that an installed
/// application declares and a folder with the bundle flag.
#[cfg(target_os = "macos")]
fn mac_folder(path: &std::path::Path) -> MacFolder {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;

    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // A name that is no Unicode text cannot be asked about. It is shown the
    // way that starts nothing.
    let package = resolved.to_str().is_none_or(|text| {
        NSWorkspace::sharedWorkspace().isFilePackageAtPath(&NSString::from_str(text))
    });
    if !package {
        return MacFolder::Plain;
    }
    let mut items: Vec<std::path::PathBuf> = std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    items.sort();
    MacFolder::Package {
        inside: items.into_iter().next(),
    }
}

#[cfg(not(target_os = "macos"))]
fn mac_folder(_path: &std::path::Path) -> MacFolder {
    MacFolder::Plain
}

/// The Windows file manager splits its command line at a comma outside
/// quotation marks and reads the first part as an item to open. The path
/// therefore stands between quotation marks in both forms, and the command
/// reaches the program as written. A Windows name holds no quotation mark;
/// one in the text would end the quoted part, so it is taken out, and the
/// text then names no item.
fn windows_explorer_command(path: &std::path::Path, selection: Selection) -> LaunchCommand {
    let quoted = format!("\"{}\"", path_argument(path).replace('"', ""));
    let arguments = match selection {
        Selection::Files => vec![format!("/select,{quoted}")],
        Selection::Folders => vec![quoted],
    };
    LaunchCommand {
        program: std::path::PathBuf::from("explorer.exe"),
        arguments,
        working_directory: None,
        arguments_as_written: true,
    }
}

/// Show a path in the platform's file manager on a worker.
pub fn reveal(
    path: &std::path::Path,
    selection: Selection,
    spawner: Arc<dyn Spawner>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<LaunchMessage> {
    let path = path.to_path_buf();
    Job::spawn_notifying(
        move |emitter, _| {
            // The command reads the file system and can wait for a program,
            // so it is built and started on this worker.
            emitter.send(show_on(
                std::env::consts::OS,
                &path,
                selection,
                spawner.as_ref(),
                mac_folder,
            ));
        },
        notify,
    )
}

/// Show `path` in the file manager of the platform named `os` and say what
/// happened.
///
/// The file managers open another folder for an item that is gone and
/// report nothing, so the item is looked for first. `open` on macOS ends
/// when it has passed the item on, with a failure status when it could not,
/// so it is waited for. The Windows file manager ends with the same status
/// for every outcome, and `xdg-open` can run as long as the file manager it
/// starts; neither is waited for.
fn show_on(
    os: &str,
    path: &std::path::Path,
    selection: Selection,
    spawner: &dyn Spawner,
    mac_folder: impl FnOnce(&std::path::Path) -> MacFolder,
) -> LaunchMessage {
    let shown = path.display();
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        // Linux and macOS give the second kind for a path under a file.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return LaunchMessage::Failed {
                reason: format!("{shown} no longer exists."),
            };
        }
        Err(error) => {
            return LaunchMessage::Failed {
                reason: format!("{shown} cannot be shown: {error}"),
            };
        }
    }
    let command = explorer_command_on(os, path, selection, std::path::Path::is_dir, mac_folder);
    let started = if os == "macos" {
        spawner
            .run(&command)
            .map_err(|reason| format!("{shown} could not be shown: {reason}"))
    } else {
        spawner.spawn(&command)
    };
    match started {
        Ok(()) => LaunchMessage::Started {
            description: shown.to_string(),
        },
        Err(reason) => LaunchMessage::Failed { reason },
    }
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
        commands_of, explorer_command_on, label_of, offered, spawn, system_explorer_command,
        windows_explorer_command, LaunchMessage, MacFolder, Selection, Spawner,
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
        assert_eq!(command.arguments, ["/select,\"/tmp/a file's name.txt\""]);
        #[cfg(target_os = "macos")]
        assert_eq!(command.arguments, ["-R", "/tmp/a file's name.txt"]);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert_eq!(command.arguments, ["/tmp"]);
        assert!(command.working_directory.is_none());

        let comma_path = std::path::Path::new(r"C:\work tree\a,b.txt");
        let command = windows_explorer_command(comma_path, Selection::Files);
        assert_eq!(command.program, PathBuf::from("explorer.exe"));
        assert_eq!(command.arguments, [r#"/select,"C:\work tree\a,b.txt""#]);

        let base = tempfile::tempdir().unwrap();
        let folder = base.path().join("a folder");
        std::fs::create_dir(&folder).unwrap();
        let command = system_explorer_command(&folder, Selection::Folders);
        #[cfg(target_os = "windows")]
        assert_eq!(command.arguments, [format!("\"{}\"", folder.display())]);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(command.arguments, [folder.display().to_string()]);
    }

    #[test]
    fn no_platform_is_asked_to_open_an_item_that_is_no_folder() {
        let base = tempfile::tempdir().unwrap();
        let file = base.path().join("tool.command");
        std::fs::write(&file, b"#!/bin/sh\n").unwrap();
        let gone = file.join("inside");
        let text = |path: &std::path::Path| path.display().to_string();
        let arguments = |os: &str, path: &std::path::Path| {
            explorer_command_on(
                os,
                path,
                Selection::Folders,
                std::path::Path::is_dir,
                |_| MacFolder::Plain,
            )
            .arguments
        };

        assert_eq!(
            arguments("windows", &file),
            [format!("/select,\"{}\"", text(&file))]
        );
        assert_eq!(arguments("macos", &file), ["-R".to_owned(), text(&file)]);
        assert_eq!(arguments("linux", &file), [text(base.path())]);
        // The parent of the path that is gone is the file.
        assert_eq!(
            arguments("windows", &gone),
            [format!("/select,\"{}\"", text(&gone))]
        );
        assert_eq!(arguments("macos", &gone), ["-R".to_owned(), text(&gone)]);
        assert_eq!(arguments("linux", &gone), [text(base.path())]);

        assert_eq!(
            arguments("windows", base.path()),
            [format!("\"{}\"", text(base.path()))]
        );
        for os in ["macos", "linux"] {
            assert_eq!(arguments(os, base.path()), [text(base.path())], "{os}");
        }
    }

    #[test]
    fn showing_a_file_that_the_view_names_as_a_folder_selects_the_file() {
        let recorder = Arc::new(Recorder::default());
        let base = tempfile::tempdir().unwrap();
        let file = base.path().join("note.txt");
        std::fs::write(&file, b"text").unwrap();
        let mut job = super::reveal(
            &file,
            Selection::Folders,
            Arc::new(Arc::clone(&recorder)),
            Arc::new(|| {}),
        );
        assert!(matches!(settle(&mut job), LaunchMessage::Started { .. }));
        let started = recorder.started.lock().unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0], system_explorer_command(&file, Selection::Files));
        #[cfg(target_os = "windows")]
        assert!(started[0].arguments[0].starts_with("/select,\""));
        #[cfg(target_os = "macos")]
        assert_eq!(started[0].arguments[0], "-R");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert_eq!(started[0].arguments, [base.path().display().to_string()]);
    }

    #[test]
    fn a_path_that_starts_with_a_hyphen_reaches_the_platform_tool_as_a_path() {
        let here = |rest: &str| format!(".{}{rest}", std::path::MAIN_SEPARATOR);
        let file = std::path::Path::new("--output=evil.txt");
        let opened = super::system_open_command(file);
        assert_eq!(
            opened.arguments.last().map(String::as_str),
            Some(here("--output=evil.txt").as_str())
        );
        for os in ["windows", "macos", "linux"] {
            for selection in [Selection::Files, Selection::Folders] {
                let command =
                    explorer_command_on(os, file, selection, |_| true, |_| MacFolder::Plain);
                assert!(
                    command
                        .arguments
                        .iter()
                        .all(|argument| argument != "--output=evil.txt"),
                    "{os}: {:?}",
                    command.arguments
                );
            }
        }
    }

    #[test]
    fn the_mac_file_manager_opens_a_plain_folder_and_never_asks_to_open_a_package() {
        let arguments = |folder: &str, kind: MacFolder| {
            let command = explorer_command_on(
                "macos",
                std::path::Path::new(folder),
                Selection::Folders,
                |_| true,
                |_| kind,
            );
            assert_eq!(command.program, PathBuf::from("/usr/bin/open"));
            command.arguments
        };
        // The name does not decide: a folder with an extension that is no
        // package opens, and a package with a plain name does not.
        for plain in [
            "/tmp/left/plain folder",
            "/tmp/left/release.v2",
            "/tmp/left",
        ] {
            assert_eq!(arguments(plain, MacFolder::Plain), [plain]);
        }
        for package in ["/tmp/left/Probe.app", "/tmp/left/link to a bundle"] {
            let inside = PathBuf::from(package).join("Contents");
            let shown = arguments(
                package,
                MacFolder::Package {
                    inside: Some(inside.clone()),
                },
            );
            assert_eq!(shown, ["-R".to_owned(), inside.display().to_string()]);
            let empty = arguments(package, MacFolder::Package { inside: None });
            assert_eq!(empty, ["-R", package]);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_says_which_folder_is_a_package_also_through_a_link() {
        let base = tempfile::tempdir().unwrap();
        let folder = |name: &str| {
            let path = base.path().join(name);
            std::fs::create_dir(&path).unwrap();
            path
        };
        let plain = folder("plain");
        let dotted = folder("release.v2");
        let bundle = folder("Probe.app");
        let empty = folder("Empty.app");
        std::fs::create_dir(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Zebra.txt"), b"x").unwrap();
        let link_to_bundle = base.path().join("link to a bundle");
        std::os::unix::fs::symlink(&bundle, &link_to_bundle).unwrap();
        let link_to_plain = base.path().join("link.app");
        std::os::unix::fs::symlink(&plain, &link_to_plain).unwrap();

        for path in [&plain, &dotted, &link_to_plain] {
            assert_eq!(super::mac_folder(path), MacFolder::Plain, "{path:?}");
        }
        assert_eq!(
            super::mac_folder(&bundle),
            MacFolder::Package {
                inside: Some(bundle.join("Contents"))
            }
        );
        assert_eq!(
            super::mac_folder(&link_to_bundle),
            MacFolder::Package {
                inside: Some(link_to_bundle.join("Contents"))
            }
        );
        assert_eq!(
            super::mac_folder(&empty),
            MacFolder::Package { inside: None }
        );

        // The whole command for the link asks for no open of the bundle.
        let command = system_explorer_command(&link_to_bundle, Selection::Folders);
        assert_eq!(
            command.arguments,
            [
                "-R".to_owned(),
                link_to_bundle.join("Contents").display().to_string()
            ]
        );
    }

    #[test]
    fn showing_a_folder_builds_its_command_on_the_worker_and_starts_it() {
        let recorder = Arc::new(Recorder::default());
        let base = tempfile::tempdir().unwrap();
        let mut job = super::reveal(
            base.path(),
            Selection::Folders,
            Arc::new(Arc::clone(&recorder)),
            Arc::new(|| {}),
        );
        assert!(matches!(settle(&mut job), LaunchMessage::Started { .. }));
        let started = recorder.started.lock().unwrap();
        assert_eq!(started.len(), 1);
        let folder = base.path().display().to_string();
        #[cfg(target_os = "windows")]
        let folder = format!("\"{folder}\"");
        assert_eq!(
            started[0].arguments.last().map(String::as_str),
            Some(folder.as_str())
        );
    }

    /// A spawner whose program starts and then ends with a failure status.
    #[derive(Default)]
    struct EndsWithFailure {
        not_waited_for: Mutex<Vec<LaunchCommand>>,
        waited_for: Mutex<Vec<LaunchCommand>>,
    }

    impl Spawner for EndsWithFailure {
        fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
            self.not_waited_for.lock().unwrap().push(command.clone());
            Ok(())
        }

        fn run(&self, command: &LaunchCommand) -> Result<(), String> {
            self.waited_for.lock().unwrap().push(command.clone());
            Err("the program ended with exit status: 1".to_owned())
        }
    }

    #[test]
    fn an_item_that_is_gone_gets_a_message_that_names_it_and_no_program_starts() {
        let base = tempfile::tempdir().unwrap();
        let gone = base.path().join("gone, moved.txt");
        let under_a_file = base.path().join("note.txt").join("inside");
        std::fs::write(base.path().join("note.txt"), b"text").unwrap();
        for path in [&gone, &under_a_file] {
            let expected = LaunchMessage::Failed {
                reason: format!("{} no longer exists.", path.display()),
            };
            for os in ["windows", "macos", "linux"] {
                for selection in [Selection::Files, Selection::Folders] {
                    let recorder = Arc::new(Recorder::default());
                    let message =
                        super::show_on(os, path, selection, &recorder, |_| MacFolder::Plain);
                    assert_eq!(message, expected, "{os}");
                    assert!(recorder.started.lock().unwrap().is_empty(), "{os}");
                }
            }
            let recorder = Arc::new(Recorder::default());
            let mut job = super::reveal(
                path,
                Selection::Folders,
                Arc::new(Arc::clone(&recorder)),
                Arc::new(|| {}),
            );
            assert_eq!(settle(&mut job), expected);
            assert!(recorder.started.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn a_failure_status_of_the_mac_command_is_no_start_and_names_the_item() {
        let base = tempfile::tempdir().unwrap();
        let file = base.path().join("note.txt");
        std::fs::write(&file, b"text").unwrap();
        for (path, selection) in [
            (file.as_path(), Selection::Files),
            (base.path(), Selection::Folders),
        ] {
            let spawner = EndsWithFailure::default();
            let message = super::show_on("macos", path, selection, &spawner, |_| MacFolder::Plain);
            let LaunchMessage::Failed { reason } = message else {
                panic!("{message:?}");
            };
            assert!(reason.contains(&path.display().to_string()), "{reason}");
            assert!(reason.contains("ended with exit status: 1"), "{reason}");
            assert_eq!(spawner.waited_for.lock().unwrap().len(), 1);
            assert!(spawner.not_waited_for.lock().unwrap().is_empty());

            // The other file managers give no status to read.
            for os in ["windows", "linux"] {
                let spawner = EndsWithFailure::default();
                let message = super::show_on(os, path, selection, &spawner, |_| MacFolder::Plain);
                assert!(matches!(message, LaunchMessage::Started { .. }), "{os}");
                assert!(spawner.waited_for.lock().unwrap().is_empty(), "{os}");
                assert_eq!(spawner.not_waited_for.lock().unwrap().len(), 1, "{os}");
            }
        }
    }

    #[test]
    fn the_system_spawner_reports_the_failure_status_of_a_program_it_waits_for() {
        let ending_with = |code: &str| {
            let (program, option) = if cfg!(target_os = "windows") {
                ("cmd.exe", "/c")
            } else {
                ("/bin/sh", "-c")
            };
            LaunchCommand {
                program: PathBuf::from(program),
                arguments: vec![option.to_owned(), format!("exit {code}")],
                working_directory: None,
                arguments_as_written: false,
            }
        };
        assert_eq!(super::SystemSpawner.run(&ending_with("0")), Ok(()));
        let reason = super::SystemSpawner.run(&ending_with("3")).unwrap_err();
        assert!(reason.contains("ended with"), "{reason}");
        assert!(reason.contains('3'), "{reason}");
        // A start that is not waited for reports the start only.
        assert_eq!(super::SystemSpawner.spawn(&ending_with("3")), Ok(()));
        let missing = LaunchCommand {
            program: PathBuf::from("compare-all-no-such-program"),
            ..LaunchCommand::default()
        };
        assert!(super::SystemSpawner.run(&missing).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_link_whose_target_is_gone_is_reported_and_named_on_macos() {
        let base = tempfile::tempdir().unwrap();
        let link = base.path().join("broken link");
        std::os::unix::fs::symlink(base.path().join("gone"), &link).unwrap();
        let message = super::show_on(
            "macos",
            &link,
            Selection::Files,
            &super::SystemSpawner,
            super::mac_folder,
        );
        let LaunchMessage::Failed { reason } = message else {
            panic!("{message:?}");
        };
        assert!(reason.contains(&link.display().to_string()), "{reason}");
        assert!(reason.contains("could not be shown"), "{reason}");
    }

    #[test]
    fn the_windows_file_manager_gets_each_path_between_quotation_marks_in_both_forms() {
        for name in [
            "tool.cmd,x",
            "tool.cmd x",
            "tool.cmd, x",
            ",x",
            "tool.cmd,",
            "tool.cmd^x",
            "tool.cmd%x",
            "%windir%",
            "tool.cmd&x",
            "tool.cmd;x",
            "tool.cmd=x",
            "plain",
        ] {
            for path in [
                format!(r"C:\work\{name}"),
                format!(r"C:\work\{name}\inside.txt"),
                format!(r"\\server\share\{name}"),
                name.to_owned(),
            ] {
                let open = windows_explorer_command(path.as_ref(), Selection::Folders);
                assert_eq!(open.program, PathBuf::from("explorer.exe"));
                assert_eq!(open.arguments, [format!("\"{path}\"")]);
                assert!(open.arguments_as_written, "{path}");
                let select = windows_explorer_command(path.as_ref(), Selection::Files);
                assert_eq!(select.arguments, [format!("/select,\"{path}\"")]);
                assert!(select.arguments_as_written, "{path}");
            }
        }
        // The command of every other platform and of every other builder
        // leaves the quotation marks to the system.
        for os in ["macos", "linux"] {
            let command = explorer_command_on(
                os,
                "/work/a,b".as_ref(),
                Selection::Folders,
                |_| true,
                |_| MacFolder::Plain,
            );
            assert_eq!(command.arguments, ["/work/a,b"]);
            assert!(!command.arguments_as_written, "{os}");
        }
        assert!(!super::system_open_command("/work/a,b".as_ref()).arguments_as_written);
        assert!(!entry().command(&context()).arguments_as_written);
    }

    #[test]
    fn a_quotation_mark_in_the_text_of_a_path_ends_no_quoted_argument() {
        for selection in [Selection::Folders, Selection::Files] {
            let command = windows_explorer_command(r#"C:\work\a" "tool.cmd"#.as_ref(), selection);
            let argument = command.arguments.last().unwrap();
            let quoted = argument.strip_prefix("/select,").unwrap_or(argument);
            assert_eq!(quoted, r#""C:\work\a tool.cmd""#);
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_command_with_its_own_quotation_marks_reaches_the_program_as_written() {
        let line_of = |arguments: &[&str], as_written: bool| {
            let command = LaunchCommand {
                program: PathBuf::from("cmd.exe"),
                arguments: ["/d", "/c", "echo"]
                    .iter()
                    .chain(arguments)
                    .map(|argument| (*argument).to_owned())
                    .collect(),
                working_directory: None,
                arguments_as_written: as_written,
            };
            let mut process = ca_io::host_command::host_command(&command.program);
            super::add_arguments(&mut process, &command);
            let output = process.output().unwrap();
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        assert_eq!(
            line_of(&[r#""C:\work\tool.cmd,x""#], true),
            r#""C:\work\tool.cmd,x""#
        );
        assert_eq!(
            line_of(&[r#"/select,"C:\work\a b,c""#], true),
            r#"/select,"C:\work\a b,c""#
        );
        // The standard quoting adds no quotation mark for a comma.
        assert_eq!(
            line_of(&[r"C:\work\tool.cmd,x"], false),
            r"C:\work\tool.cmd,x"
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
