//! What a command line asks the desktop program to open.
//!
//! The console program parses every documented switch. The switches it cannot
//! act on by itself land in [`DesktopRequest`], which the desktop program reads
//! when it adopts this parser.

use std::path::PathBuf;

/// The four panes a command line can name, in the documented order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The left pane.
    Left,
    /// The right pane.
    Right,
    /// The common ancestor pane of a merge.
    Center,
    /// The pane a merge result is written from.
    Output,
}

impl Pane {
    /// The four panes in order.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [Self::Left, Self::Right, Self::Center, Self::Output]
    }

    /// Position in the four element arrays of a request.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Center => 2,
            Self::Output => 3,
        }
    }
}

/// What a merge does without a person watching.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutoMerge {
    /// Merge without asking, stopping only at a conflict.
    pub enabled: bool,
    /// Take a non-conflicting change from the left side.
    pub favor_left: bool,
    /// Take a non-conflicting change from the right side.
    pub favor_right: bool,
    /// Treat a difference the rules call unimportant as no conflict.
    pub ignore_unimportant: bool,
    /// Write a conflict into the output with markers instead of failing.
    pub force: bool,
    /// Open the merge view only when conflicts are left.
    pub review_conflicts: bool,
}

/// Editing locks, one per side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadOnly {
    /// The left side is locked.
    pub left: bool,
    /// The right side is locked.
    pub right: bool,
}

/// A command line the desktop program can act on.
///
/// Fields the console program handles itself, such as a script or a quick
/// comparison, are not here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopRequest {
    /// Left, right, center and output paths, in that order.
    pub paths: Vec<PathBuf>,
    /// Replacement path text for each pane.
    pub titles: [Option<String>; 4],
    /// Version control paths for each pane.
    pub vcs_paths: [Option<String>; 4],
    /// Editing locks.
    pub read_only: ReadOnly,
    /// The view type a `/fv=` switch named.
    pub file_viewer: Option<String>,
    /// Name masks for the first folder comparison.
    pub filters: Option<String>,
    /// Open every subfolder during the first folder comparison.
    pub expand_all: bool,
    /// Open the paths in the folder sync view.
    pub folder_sync: bool,
    /// Open the path in the text edit view.
    pub edit: bool,
    /// What a merge does without a person watching.
    pub automerge: AutoMerge,
    /// The file a merge result is written to.
    pub merge_output: Option<PathBuf>,
    /// The file the save command writes instead of the original.
    pub save_target: Option<PathBuf>,
    /// Write no backup files for this run.
    pub no_backups: bool,
    /// Show no window and ask nothing.
    pub silent: bool,
    /// Start a process of its own rather than reusing a running one.
    pub solo: bool,
    /// Close the script status window when the script ends.
    pub close_script: bool,
    /// Switches that only the desktop program can act on.
    pub desktop_only: Vec<String>,
}

impl DesktopRequest {
    /// The path of one pane, when the command line named it.
    #[must_use]
    pub fn path(&self, pane: Pane) -> Option<&std::path::Path> {
        self.paths.get(pane.index()).map(PathBuf::as_path)
    }

    /// The replacement path text of one pane.
    #[must_use]
    pub fn title(&self, pane: Pane) -> Option<&str> {
        self.titles[pane.index()].as_deref()
    }

    /// The version control path of one pane.
    #[must_use]
    pub fn vcs_path(&self, pane: Pane) -> Option<&str> {
        self.vcs_paths[pane.index()].as_deref()
    }
}
