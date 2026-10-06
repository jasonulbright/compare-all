//! Entries that hand a file to another program.
//!
//! A command is a program and a list of arguments, never one string. Nothing
//! here reaches a shell, so a path holding a space, a quotation mark or an
//! ampersand is passed through as one argument and cannot start a second
//! command.

use crate::location::StoredPath;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Which folder a launched program starts in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkingFolder {
    /// The folder this program itself runs in.
    Inherit {
        /// Fields written by another build.
        #[serde(flatten)]
        unknown: BTreeMap<String, Value>,
    },
    /// The folder holding the selected item.
    ParentFolder {
        /// Fields written by another build.
        #[serde(flatten)]
        unknown: BTreeMap<String, Value>,
    },
    /// The base folder of the selected side, or the parent folder when the
    /// command runs from a file comparison.
    BaseFolder {
        /// Fields written by another build.
        #[serde(flatten)]
        unknown: BTreeMap<String, Value>,
    },
    /// A folder named here.
    Named {
        /// The folder.
        path: StoredPath,
        /// Fields written by another build.
        #[serde(flatten)]
        unknown: BTreeMap<String, Value>,
    },
    /// A choice written by another build.
    #[serde(untagged)]
    Unknown(Value),
}

impl Default for WorkingFolder {
    fn default() -> Self {
        WorkingFolder::Inherit {
            unknown: BTreeMap::new(),
        }
    }
}

impl WorkingFolder {
    /// The stored name of the choice, for a drop down.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            WorkingFolder::Inherit { .. } => "inherit",
            WorkingFolder::ParentFolder { .. } => "parentFolder",
            WorkingFolder::BaseFolder { .. } => "baseFolder",
            WorkingFolder::Named { .. } => "named",
            WorkingFolder::Unknown(_) => "unknown",
        }
    }

    /// The choice named by `id`, keeping the path of a named folder.
    #[must_use]
    pub fn with_id(&self, id: &str) -> Self {
        let empty = BTreeMap::new;
        match id {
            "parentFolder" => WorkingFolder::ParentFolder { unknown: empty() },
            "baseFolder" => WorkingFolder::BaseFolder { unknown: empty() },
            "named" => WorkingFolder::Named {
                path: match self {
                    WorkingFolder::Named { path, .. } => path.clone(),
                    _ => StoredPath::from(PathBuf::new()),
                },
                unknown: empty(),
            },
            _ => WorkingFolder::Inherit { unknown: empty() },
        }
    }
}

/// One entry of the list of external programs.
/// An entry is a row of check boxes, so the count of flags is the shape of the
/// page rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenWithEntry {
    /// Label shown on the menu.
    pub description: String,
    /// The program to run.
    pub program: StoredPath,
    /// The arguments, one per entry, before substitution.
    pub arguments: Vec<String>,
    /// The keystroke that runs the entry, in the shared keystroke text form.
    /// Empty when the entry has none.
    pub shortcut: String,
    /// Where the launched program starts.
    pub working_folder: WorkingFolder,
    /// Text every path separator is replaced with. Empty keeps the platform's
    /// own separator.
    pub path_delimiter: String,
    /// The entry is offered when files are selected.
    pub accepts_files: bool,
    /// The entry is offered when folders are selected.
    pub accepts_folders: bool,
    /// Read the comparison again once the program finishes.
    pub refresh_when_finished: bool,
    /// Run the command once per selected item.
    pub multiple_instances: bool,
    /// Start the next run only after the previous one ends.
    pub wait_for_previous: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for OpenWithEntry {
    fn default() -> Self {
        Self {
            description: String::new(),
            program: StoredPath::from(PathBuf::new()),
            arguments: Vec::new(),
            shortcut: String::new(),
            working_folder: WorkingFolder::default(),
            path_delimiter: String::new(),
            accepts_files: true,
            accepts_folders: false,
            refresh_when_finished: false,
            multiple_instances: false,
            wait_for_previous: false,
            unknown: BTreeMap::new(),
        }
    }
}

/// The list of external programs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenWithOptions {
    /// The entries, in menu order.
    pub entries: Vec<OpenWithEntry>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// One side of what a command is run over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchSide {
    /// The selected item.
    pub path: PathBuf,
    /// The base folder the item sits under, where there is one.
    pub base: Option<PathBuf>,
    /// The line the caret is on, where the view has one.
    pub line: Option<u32>,
}

/// What a command is run over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchContext {
    /// The first, or left, side.
    pub first: LaunchSide,
    /// The second, or right, side, where the command has one.
    pub second: Option<LaunchSide>,
}

/// A command ready to start: a program and its arguments, never a shell string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchCommand {
    /// The program to run.
    pub program: PathBuf,
    /// The arguments, already substituted.
    pub arguments: Vec<String>,
    /// The folder the program starts in, where the entry names one.
    pub working_directory: Option<PathBuf>,
}

impl OpenWithEntry {
    /// Builds the command this entry runs over `context`.
    ///
    /// Every variable is replaced in place. An argument that held one variable
    /// and nothing else stays one argument even when the value holds spaces.
    #[must_use]
    pub fn command(&self, context: &LaunchContext) -> LaunchCommand {
        let program =
            PathBuf::from(self.expand(&self.program.as_path().display().to_string(), context));
        let arguments = self
            .arguments
            .iter()
            .map(|argument| self.expand(argument, context))
            .collect();
        LaunchCommand {
            program,
            arguments,
            working_directory: self.working_directory(context),
        }
    }

    /// The folder the launched program starts in.
    #[must_use]
    pub fn working_directory(&self, context: &LaunchContext) -> Option<PathBuf> {
        match &self.working_folder {
            WorkingFolder::Inherit { .. } | WorkingFolder::Unknown(_) => None,
            WorkingFolder::ParentFolder { .. } => context
                .first
                .path
                .parent()
                .map(std::path::Path::to_path_buf),
            WorkingFolder::BaseFolder { .. } => context.first.base.clone().or_else(|| {
                context
                    .first
                    .path
                    .parent()
                    .map(std::path::Path::to_path_buf)
            }),
            WorkingFolder::Named { path, .. } => Some(path.as_path().to_path_buf()),
        }
    }

    /// Replaces every variable of one string.
    fn expand(&self, text: &str, context: &LaunchContext) -> String {
        let mut out = String::with_capacity(text.len());
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            if character != '%' {
                out.push(character);
                continue;
            }
            let Some(name) = characters.next() else {
                out.push('%');
                break;
            };
            if name == '%' {
                out.push('%');
                continue;
            }
            let suffix = match characters.peek() {
                Some(digit @ ('1' | '2')) => {
                    let digit = *digit;
                    characters.next();
                    Some(digit)
                }
                _ => None,
            };
            let side = if suffix == Some('2') { 2 } else { 1 };
            if let Some(value) = self.value_of(name, side, context) {
                // A value that opens the argument with a hyphen would reach
                // the program as an option; the current folder prefix keeps
                // it a path.
                if out.is_empty() && value.starts_with('-') {
                    out.push_str(&self.delimit(&format!(".{}", std::path::MAIN_SEPARATOR)));
                }
                out.push_str(&value);
            } else {
                out.push('%');
                out.push(name);
                if let Some(digit) = suffix {
                    out.push(digit);
                }
            }
        }
        out
    }

    /// The value of one variable, or nothing when the name is not one.
    fn value_of(&self, name: char, side: u8, context: &LaunchContext) -> Option<String> {
        let chosen = match side {
            2 => context.second.as_ref()?,
            _ => &context.first,
        };
        let path = chosen.path.clone();
        let text = |value: PathBuf| self.delimit(&value.display().to_string());
        match name {
            'f' => Some(text(path)),
            'l' => Some(chosen.line.unwrap_or(1).to_string()),
            'n' => Some(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            'p' => Some(text(
                path.parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default(),
            )),
            'x' => Some(
                path.extension()
                    .map(|value| format!(".{}", value.to_string_lossy()))
                    .unwrap_or_default(),
            ),
            'b' => Some(
                path.file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            'F' => Some(text(relative_to(&path, chosen.base.as_deref()))),
            'P' => Some(text(relative_to(
                path.parent().unwrap_or(std::path::Path::new("")),
                chosen.base.as_deref(),
            ))),
            _ => None,
        }
    }

    /// Applies the entry's separator to a rendered path.
    fn delimit(&self, text: &str) -> String {
        if self.path_delimiter.is_empty() {
            return text.to_owned();
        }
        text.replace(['/', '\\'], &self.path_delimiter)
    }
}

/// The part of `path` below `base`, or the whole path when it is not under it.
fn relative_to(path: &std::path::Path, base: Option<&std::path::Path>) -> PathBuf {
    match base {
        Some(base) => path
            .strip_prefix(base)
            .map_or_else(|_| path.to_path_buf(), std::path::Path::to_path_buf),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{LaunchContext, LaunchSide, OpenWithEntry, WorkingFolder};
    use std::path::PathBuf;

    fn context() -> LaunchContext {
        LaunchContext {
            first: LaunchSide {
                path: PathBuf::from("/base/dir/report file.txt"),
                base: Some(PathBuf::from("/base")),
                line: Some(42),
            },
            second: Some(LaunchSide {
                path: PathBuf::from("/other/second.md"),
                base: Some(PathBuf::from("/other")),
                line: Some(7),
            }),
        }
    }

    fn entry(arguments: &[&str]) -> OpenWithEntry {
        OpenWithEntry {
            program: PathBuf::from("editor").into(),
            arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
            ..OpenWithEntry::default()
        }
    }

    #[test]
    fn every_documented_variable_is_substituted() {
        let entry = entry(&["%f", "%l", "%n", "%p", "%x", "%b", "%F", "%P"]);
        let command = entry.command(&context());
        assert_eq!(command.program, PathBuf::from("editor"));
        assert_eq!(
            command.arguments,
            vec![
                PathBuf::from("/base/dir/report file.txt")
                    .display()
                    .to_string(),
                "42".to_owned(),
                "report file.txt".to_owned(),
                PathBuf::from("/base/dir").display().to_string(),
                ".txt".to_owned(),
                "report file".to_owned(),
                PathBuf::from("dir/report file.txt").display().to_string(),
                PathBuf::from("dir").display().to_string(),
            ]
        );
    }

    /// A program reads an argument that starts with a hyphen as an option.
    #[test]
    fn a_name_that_starts_with_a_hyphen_reaches_the_program_as_a_path() {
        let side = |base: &str| LaunchSide {
            path: PathBuf::from(base).join("-dir").join("--output=evil.txt"),
            base: Some(PathBuf::from(base)),
            line: Some(3),
        };
        let context = LaunchContext {
            first: side("/work/left"),
            second: Some(side("/work/right")),
        };
        let entry = entry(&["%F1", "%F2", "%n", "%b", "%P", "--line=%l", "x%n"]);
        let command = entry.command(&context);
        let here = |rest: &str| format!(".{}{rest}", std::path::MAIN_SEPARATOR);
        let relative = PathBuf::from("-dir")
            .join("--output=evil.txt")
            .display()
            .to_string();
        assert_eq!(
            command.arguments,
            vec![
                here(&relative),
                here(&relative),
                here("--output=evil.txt"),
                here("--output=evil"),
                here("-dir"),
                "--line=3".to_owned(),
                "x--output=evil.txt".to_owned(),
            ]
        );
    }

    /// A path holding a space stays one argument, because no shell reads it.
    #[test]
    fn a_path_with_a_space_is_one_argument() {
        let command = entry(&["--file=%f"]).command(&context());
        assert_eq!(command.arguments.len(), 1);
        assert!(command.arguments[0].contains("report file.txt"));
    }

    #[test]
    fn a_side_suffix_picks_the_second_file() {
        let command = entry(&["%n1", "%n2", "%l2"]).command(&context());
        assert_eq!(
            command.arguments,
            vec![
                "report file.txt".to_owned(),
                "second.md".to_owned(),
                "7".to_owned()
            ]
        );
    }

    #[test]
    fn a_second_side_that_is_absent_leaves_the_variable_alone() {
        let mut context = context();
        context.second = None;
        let command = entry(&["%n2"]).command(&context);
        assert_eq!(command.arguments, vec!["%n2".to_owned()]);
    }

    #[test]
    fn a_doubled_percent_is_one_percent_and_an_unknown_name_is_kept() {
        let command = entry(&["100%%", "%z"]).command(&context());
        assert_eq!(command.arguments, vec!["100%".to_owned(), "%z".to_owned()]);
    }

    #[test]
    fn the_delimiter_replaces_every_separator_of_a_path() {
        let mut entry = entry(&["%f"]);
        entry.path_delimiter = "|".to_owned();
        let command = entry.command(&context());
        assert!(!command.arguments[0].contains('/'));
        assert!(!command.arguments[0].contains('\\'));
        assert!(command.arguments[0].contains('|'));
    }

    #[test]
    fn each_working_folder_choice_resolves() {
        let mut entry = entry(&[]);
        assert_eq!(entry.working_directory(&context()), None);
        entry.working_folder = WorkingFolder::default().with_id("parentFolder");
        assert_eq!(
            entry.working_directory(&context()),
            Some(PathBuf::from("/base/dir"))
        );
        entry.working_folder = WorkingFolder::default().with_id("baseFolder");
        assert_eq!(
            entry.working_directory(&context()),
            Some(PathBuf::from("/base"))
        );
        entry.working_folder = WorkingFolder::Named {
            path: PathBuf::from("/start/here").into(),
            unknown: std::collections::BTreeMap::new(),
        };
        assert_eq!(
            entry.working_directory(&context()),
            Some(PathBuf::from("/start/here"))
        );
    }
}
