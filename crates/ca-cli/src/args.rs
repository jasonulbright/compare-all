//! Reads a command line.
//!
//! A switch may be written `/name`, `-name` or `--name` on every platform. A
//! switch that takes a value is written `name=value`. An argument that begins
//! with `@` names a script file. Every other argument is a path, in the order
//! left, right, center, output.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::request::{DesktopRequest, ReadOnly};

/// How two files are compared by a quick comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuickKind {
    /// Compare the sizes only.
    Size,
    /// Compare checksums.
    Crc,
    /// Compare byte for byte.
    Binary,
    /// Compare with the format rules.
    #[default]
    RulesBased,
}

/// A quick comparison of two files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickRun {
    /// How the two files are compared.
    pub kind: QuickKind,
    /// The left file.
    pub left: PathBuf,
    /// The right file.
    pub right: PathBuf,
}

/// A script file and the arguments that follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRun {
    /// The script file.
    pub file: PathBuf,
    /// The arguments `%1` through `%9` stand for. Switches are left out.
    pub arguments: Vec<String>,
    /// Show no window and ask nothing.
    pub silent: bool,
    /// Close the script status window when the script ends.
    pub close_script: bool,
    /// Editing locks the script inherits.
    pub read_only: ReadOnly,
    /// Build every plan and run none of them.
    pub dry_run: bool,
}

/// What a command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Write the usage summary.
    Help,
    /// Write the version.
    Version,
    /// Run a script file.
    Script(Box<ScriptRun>),
    /// Compare two files and return the answer as an exit code.
    Quick(Box<QuickRun>),
    /// Open a view. Only the desktop program can act on this.
    Desktop(Box<DesktopRequest>),
}

/// Why a command line could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgError {
    /// The switch is not one this program knows.
    #[error("{0} is not a switch this program knows")]
    UnknownSwitch(String),
    /// The switch needs a value and none was given.
    #[error("{0} needs a value, written {0}=<value>")]
    MissingValue(String),
    /// The value is not one the switch accepts.
    #[error("{value} is not a value {switch} accepts")]
    BadValue {
        /// The switch.
        switch: String,
        /// The value it was given.
        value: String,
    },
    /// A quick comparison needs exactly two files.
    #[error("a quick comparison needs two files; {0} were given")]
    QuickNeedsTwoFiles(usize),
    /// More paths were given than the four panes can hold.
    #[error("at most four paths are accepted; {0} were given")]
    TooManyPaths(usize),
    /// More than one script file was named.
    #[error("only one script file is accepted")]
    TwoScripts,
}

/// Switches only the desktop program can act on.
const DESKTOP_ONLY: &[&str] = &["solo", "closescript", "edit", "sync", "reviewconflicts"];

/// Read a command line.
///
/// # Errors
/// Returns the first [`ArgError`] the arguments raise.
#[allow(clippy::too_many_lines)]
pub fn parse<I, S>(arguments: I) -> Result<Invocation, ArgError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut request = DesktopRequest::default();
    let mut script: Option<PathBuf> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut center: Option<String> = None;
    let mut quick: Option<QuickKind> = None;
    let mut dry_run = false;
    let mut switches_ended = false;

    for raw in arguments {
        let raw: String = raw.into();
        if switches_ended {
            positional.push(raw);
            continue;
        }
        if raw == "--" {
            switches_ended = true;
            continue;
        }
        if let Some(file) = raw.strip_prefix('@') {
            if script.is_some() {
                return Err(ArgError::TwoScripts);
            }
            script = Some(PathBuf::from(file));
            continue;
        }
        let Some(body) = switch_body(&raw) else {
            positional.push(raw);
            continue;
        };
        let (name, value) = match body.split_once('=') {
            Some((name, value)) => (name.to_ascii_lowercase(), Some(value.to_string())),
            None => (body.to_ascii_lowercase(), None),
        };
        let need = |value: Option<String>| -> Result<String, ArgError> {
            value.ok_or_else(|| ArgError::MissingValue(format!("/{name}")))
        };
        match name.as_str() {
            "?" | "h" | "help" => return Ok(Invocation::Help),
            "version" => return Ok(Invocation::Version),
            "dry-run" | "dryrun" => dry_run = true,
            "automerge" => request.automerge.enabled = true,
            "favorleft" => request.automerge.favor_left = true,
            "favorright" => request.automerge.favor_right = true,
            "iu" | "ignoreunimportant" => request.automerge.ignore_unimportant = true,
            "force" => request.automerge.force = true,
            "reviewconflicts" => {
                request.automerge.review_conflicts = true;
                request.desktop_only.push("reviewconflicts".to_string());
            }
            "center" => center = Some(need(value)?),
            "closescript" => {
                request.close_script = true;
                request.desktop_only.push("closescript".to_string());
            }
            "edit" => {
                request.edit = true;
                request.desktop_only.push("edit".to_string());
            }
            "expandall" => request.expand_all = true,
            "filters" => request.filters = Some(need(value)?),
            "fv" | "fileviewer" => request.file_viewer = Some(need(value)?),
            "mergeoutput" => request.merge_output = Some(PathBuf::from(need(value)?)),
            "nobackups" => request.no_backups = true,
            "qc" | "quickcompare" => {
                quick = Some(
                    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
                        None | Some("" | "rules-based" | "rulesbased") => QuickKind::RulesBased,
                        Some("size") => QuickKind::Size,
                        Some("crc") => QuickKind::Crc,
                        Some("binary") => QuickKind::Binary,
                        Some(other) => {
                            return Err(ArgError::BadValue {
                                switch: format!("/{name}"),
                                value: other.to_string(),
                            })
                        }
                    },
                );
            }
            "ro" | "readonly" => {
                request.read_only = ReadOnly {
                    left: true,
                    right: true,
                };
            }
            "ro1" | "lro" | "leftreadonly" => request.read_only.left = true,
            "ro2" | "rro" | "rightreadonly" => request.read_only.right = true,
            "savetarget" => request.save_target = Some(PathBuf::from(need(value)?)),
            "silent" => request.silent = true,
            "solo" => {
                request.solo = true;
                request.desktop_only.push("solo".to_string());
            }
            "sync" => {
                request.folder_sync = true;
                request.desktop_only.push("sync".to_string());
            }
            "title1" | "lefttitle" => request.titles[0] = Some(need(value)?),
            "title2" | "righttitle" => request.titles[1] = Some(need(value)?),
            "title3" | "centertitle" => request.titles[2] = Some(need(value)?),
            "title4" | "outputtitle" => request.titles[3] = Some(need(value)?),
            "vcs1" | "vcsleft" => request.vcs_paths[0] = Some(need(value)?),
            "vcs2" | "vcsright" => request.vcs_paths[1] = Some(need(value)?),
            "vcs3" | "vcscenter" => request.vcs_paths[2] = Some(need(value)?),
            "vcs4" | "vcsoutput" => request.vcs_paths[3] = Some(need(value)?),
            _ => return Err(ArgError::UnknownSwitch(format!("/{name}"))),
        }
    }

    if let Some(file) = script {
        return Ok(Invocation::Script(Box::new(ScriptRun {
            file,
            arguments: positional,
            silent: request.silent,
            close_script: request.close_script,
            read_only: request.read_only,
            dry_run,
        })));
    }

    if let Some(kind) = quick {
        if positional.len() != 2 {
            return Err(ArgError::QuickNeedsTwoFiles(positional.len()));
        }
        let mut paths = positional.into_iter();
        let (Some(left), Some(right)) = (paths.next(), paths.next()) else {
            return Err(ArgError::QuickNeedsTwoFiles(0));
        };
        return Ok(Invocation::Quick(Box::new(QuickRun {
            kind,
            left: PathBuf::from(left),
            right: PathBuf::from(right),
        })));
    }

    if positional.len() > 4 {
        return Err(ArgError::TooManyPaths(positional.len()));
    }
    request.paths = positional.into_iter().map(PathBuf::from).collect();
    if let Some(center) = center {
        set_center(&mut request.paths, center);
    }
    request
        .desktop_only
        .retain(|name| DESKTOP_ONLY.contains(&name.as_str()));
    Ok(Invocation::Desktop(Box::new(request)))
}

/// Read native command-line arguments without panicking on non-Unicode paths.
///
/// Switches remain text; positional paths retain their original operating
/// system representation through parsing.
///
/// # Errors
/// Returns the same [`ArgError`] values as [`parse`].
pub fn parse_os<I, S>(arguments: I) -> Result<Invocation, ArgError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut native_paths = HashMap::new();
    let mut text = Vec::new();
    for (index, argument) in arguments.into_iter().enumerate() {
        let argument = argument.into();
        if let Some(argument) = argument.to_str() {
            text.push(argument.to_owned());
            continue;
        }
        let (argument, native_path) = match strip_script_marker(&argument) {
            Some(path) => (format!("@{index}"), path),
            None => (format!("{index}"), argument),
        };
        let placeholder = format!("\u{fdd0}non-unicode-{index}\u{fdd1}");
        native_paths.insert(placeholder.clone(), native_path);
        text.push(if argument.starts_with('@') {
            format!("@{placeholder}")
        } else {
            placeholder
        });
    }

    let mut invocation = parse(text)?;
    let restore = |path: &mut PathBuf| {
        if let Some(placeholder) = path.to_str() {
            if let Some(native) = native_paths.get(placeholder) {
                *path = PathBuf::from(native);
            }
        }
    };
    match &mut invocation {
        Invocation::Script(request) => {
            restore(&mut request.file);
            for argument in &mut request.arguments {
                if let Some(native) = native_paths.get(argument) {
                    *argument = native.to_string_lossy().into_owned();
                }
            }
        }
        Invocation::Quick(request) => {
            restore(&mut request.left);
            restore(&mut request.right);
        }
        Invocation::Desktop(request) => {
            for path in &mut request.paths {
                restore(path);
            }
            if let Some(path) = &mut request.merge_output {
                restore(path);
            }
            if let Some(path) = &mut request.save_target {
                restore(path);
            }
        }
        Invocation::Help | Invocation::Version => {}
    }
    Ok(invocation)
}

fn strip_script_marker(argument: &OsStr) -> Option<OsString> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = argument.encode_wide().collect();
        (units.first() == Some(&(u16::from(b'@')))).then(|| OsString::from_wide(&units[1..]))
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = argument.as_bytes();
        bytes
            .first()
            .is_some_and(|first| *first == b'@')
            .then(|| OsString::from_vec(bytes[1..].to_vec()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = argument;
        None
    }
}

/// Every switch name, including the short spellings.
pub const SWITCH_NAMES: &[&str] = &[
    "?",
    "automerge",
    "center",
    "centertitle",
    "closescript",
    "dry-run",
    "dryrun",
    "edit",
    "expandall",
    "favorleft",
    "favorright",
    "fileviewer",
    "filters",
    "force",
    "fv",
    "h",
    "help",
    "ignoreunimportant",
    "iu",
    "lefttitle",
    "leftreadonly",
    "lro",
    "mergeoutput",
    "nobackups",
    "outputtitle",
    "qc",
    "quickcompare",
    "readonly",
    "reviewconflicts",
    "righttitle",
    "rightreadonly",
    "ro",
    "ro1",
    "ro2",
    "rro",
    "savetarget",
    "silent",
    "solo",
    "sync",
    "title1",
    "title2",
    "title3",
    "title4",
    "vcs1",
    "vcs2",
    "vcs3",
    "vcs4",
    "vcscenter",
    "vcsleft",
    "vcsoutput",
    "vcsright",
    "version",
];

fn is_switch_name(text: &str) -> bool {
    let head = text.split_once('=').map_or(text, |(head, _)| head);
    SWITCH_NAMES
        .iter()
        .any(|name| head.eq_ignore_ascii_case(name))
}

/// The text of a switch, when the argument is one.
///
/// A `/` argument counts as a switch only when the name after it is one this
/// program knows, so an absolute path on a Unix file system stays a path.
fn switch_body(raw: &str) -> Option<&str> {
    if let Some(rest) = raw.strip_prefix("--") {
        return (!rest.is_empty()).then_some(rest);
    }
    if let Some(rest) = raw.strip_prefix('/') {
        return is_switch_name(rest).then_some(rest);
    }
    if let Some(rest) = raw.strip_prefix('-') {
        return is_switch_name(rest).then_some(rest);
    }
    None
}

/// Put the merge ancestor in the third pane, filling the first two if needed.
fn set_center(paths: &mut Vec<PathBuf>, value: String) {
    while paths.len() < 2 {
        paths.push(PathBuf::new());
    }
    if paths.len() == 2 {
        paths.push(PathBuf::from(value));
    } else {
        paths[2] = PathBuf::from(value);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod native_argument_tests {
    use super::{parse_os, Invocation, QuickKind};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[cfg(windows)]
    fn invalid_path() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[
            u16::from(b'f'),
            0xD800,
            u16::from(b'.'),
            u16::from(b't'),
            u16::from(b'x'),
            u16::from(b't'),
        ])
    }

    #[cfg(unix)]
    fn invalid_path() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(b"file-\xFF.txt".to_vec())
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn a_quick_comparison_keeps_a_non_unicode_path() {
        let left = invalid_path();
        let invocation = parse_os([
            OsString::from("/qc=binary"),
            left.clone(),
            OsString::from("right.txt"),
        ])
        .unwrap();
        let Invocation::Quick(request) = invocation else {
            panic!("expected a quick comparison");
        };

        assert_eq!(request.kind, QuickKind::Binary);
        assert_eq!(request.left, PathBuf::from(left));
    }

    #[test]
    fn quick_comparison_values_ignore_letter_case() {
        let invocation = parse_os([
            OsString::from("/qc=Binary"),
            OsString::from("x.txt"),
            OsString::from("y.txt"),
        ])
        .unwrap();
        let Invocation::Quick(request) = invocation else {
            panic!("expected a quick comparison");
        };

        assert_eq!(request.kind, QuickKind::Binary);
        assert_eq!(request.left, PathBuf::from("x.txt"));
        assert_eq!(request.right, PathBuf::from("y.txt"));
    }
}
