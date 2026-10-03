//! The window: the menu bar, the tab strip, and the routing between them and
//! whichever view the active tab holds.

use crate::cli::Startup;
use crate::home::{HomeAction, HomeOutbox, HomeView, SharedNotice};
use crate::registry;
use ca_session::settings::{SessionSettings, SessionSettingsOverride};
use ca_session::{
    AdminPolicies, ExportSelection, ImportOptions, SavedSession, SessionId, SessionKind, Workspace,
    WorkspaceTab,
};
use ca_ui::command::{self, Command, Keystroke, Menu, MenuView};
use ca_ui::dialog::DialogMessage;
use ca_ui::options::{AppOptions, OptionsDialog, OptionsHandle, RestoreDialog};
use ca_ui::sessions::StoreHandle;
use ca_ui::settings::{Scope, SettingsDialog, SettingsOutcome};
use ca_ui::share::{ImportReportDialog, ShareMessage};
use ca_ui::theme::Variant;
use ca_ui::view::{CommandState, OpenRequest, SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::Job;
use ca_ui::workspace::{WorkspaceAction, WorkspaceManager};
use ca_view_folder::opjobs::RecoverMessage;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

/// The build identifier shown in the window title.
pub const VERSION: &str = env!("CA_VERSION");

/// What the window opens at when nothing is remembered.
pub const DEFAULT_WINDOW_SIZE: [f32; 2] = [1_280.0, 800.0];

/// Below this the two panes stop being two panes.
pub const MINIMUM_WINDOW_SIZE: [f32; 2] = [640.0, 400.0];

/// One line of a menu.
enum Entry {
    /// A command, with the reason shown when no view accepts it.
    Run(Command, &'static str),
    /// A command the vocabulary names and no view runs yet.
    Soon(Command),
    /// A rule between groups.
    Rule,
}

/// Why a line that names a command with no implementation is disabled.
const NOT_YET: &str = "Not available yet";

/// The text comparison bar, in the observed order.
///
/// A line the build does not run yet still carries its command, so the label,
/// the group and the keystroke are all declared in one place.
const TEXT_MENUS: &[(&str, &[Entry])] = &[
    (
        "Session",
        &[
            Entry::Run(Command::NewSession, ""),
            Entry::Run(Command::NewTab, ""),
            Entry::Soon(Command::NewWindow),
            Entry::Run(Command::OpenSession, ""),
            Entry::Rule,
            Entry::Run(Command::LoadWorkspace, NO_STORE),
            Entry::Run(Command::SaveWorkspaceAs, NO_STORE),
            Entry::Rule,
            Entry::Run(Command::SaveSession, NO_SESSION),
            Entry::Run(Command::SaveSessionAs, NO_SESSION),
            Entry::Run(Command::SessionSettings, NO_SETTINGS),
            Entry::Run(Command::ToggleLocked, NO_SAVED),
            Entry::Run(Command::ClearSession, NO_SESSION),
            Entry::Run(Command::CloseTab, "No tab is open"),
            Entry::Rule,
            Entry::Run(
                Command::SwapSides,
                "The active view has no two sides to swap",
            ),
            Entry::Run(Command::Reload, "The active view reads nothing to reload"),
            Entry::Run(
                Command::Recompare,
                "The active view has nothing to compare again",
            ),
            Entry::Rule,
            Entry::Run(
                Command::CompareReport,
                "The active view has no finished comparison to report",
            ),
            Entry::Soon(Command::CompareInfo),
            Entry::Rule,
            Entry::Soon(Command::CompareFilesUsing),
            Entry::Soon(Command::MergeFiles),
            Entry::Run(Command::CompareParentFolders, PARENT_FOLDERS),
            Entry::Rule,
            Entry::Run(Command::Exit, ""),
        ],
    ),
    (
        "File",
        &[
            Entry::Run(Command::OpenFile, FILE_OPENABLE),
            Entry::Soon(Command::OpenClipboard),
            Entry::Soon(Command::OpenWith),
            Entry::Rule,
            Entry::Run(Command::SaveFile, "Nothing is edited"),
            Entry::Run(Command::SaveFileAs, "Nothing is edited"),
            Entry::Soon(Command::Explorer),
            Entry::Rule,
            Entry::Run(Command::SaveBoth, "Nothing is edited"),
        ],
    ),
    (
        "Edit",
        &[
            Entry::Run(Command::Undo, "Nothing to reverse"),
            Entry::Run(Command::Redo, "Nothing to reverse"),
            Entry::Rule,
            Entry::Soon(Command::AlignWith),
            Entry::Soon(Command::Isolate),
            Entry::Soon(Command::Replacement),
            Entry::Run(Command::CopyToRight, EDITABLE),
            Entry::Run(Command::CopyLineToRight, EDITABLE),
            Entry::Run(Command::IncreaseIndent, EDITABLE),
            Entry::Run(Command::DecreaseIndent, EDITABLE),
            Entry::Rule,
            Entry::Run(Command::Cut, EDITABLE),
            Entry::Run(Command::Copy, EDITABLE),
            Entry::Run(Command::Paste, EDITABLE),
            Entry::Run(Command::Delete, ACTIONABLE),
            Entry::Rule,
            Entry::Run(Command::SelectAll, SELECTABLE),
            Entry::Run(Command::SelectSection, EDITABLE),
            Entry::Soon(Command::CompareSelectionToClipboard),
            Entry::Rule,
            Entry::Soon(Command::ConvertFile),
            Entry::Rule,
            Entry::Run(Command::CopyToLeft, EDITABLE),
            Entry::Run(Command::CopyLineToLeft, EDITABLE),
            Entry::Run(Command::ToggleOverwrite, EDITABLE),
            Entry::Run(Command::CopyToOtherSide, EDITABLE),
            Entry::Run(Command::ExpandAll, "The active view has no tree"),
            Entry::Run(Command::CollapseAll, "The active view has no tree"),
        ],
    ),
    (
        "Search",
        &[
            Entry::Run(Command::NextDifference, COMPARED),
            Entry::Run(Command::PreviousDifference, COMPARED),
            Entry::Run(Command::NextSection, COMPARED),
            Entry::Run(Command::PreviousSection, COMPARED),
            Entry::Soon(Command::NextReplacement),
            Entry::Soon(Command::PreviousReplacement),
            Entry::Rule,
            Entry::Run(Command::NextEdit, COMPARED),
            Entry::Run(Command::PreviousEdit, COMPARED),
            Entry::Rule,
            Entry::Run(Command::Find, COMPARED),
            Entry::Run(Command::Replace, COMPARED),
            Entry::Run(Command::FindNext, COMPARED),
            Entry::Run(Command::FindPrevious, COMPARED),
            Entry::Rule,
            Entry::Run(Command::GoTo, COMPARED),
            Entry::Rule,
            Entry::Soon(Command::ToggleBookmark),
            Entry::Soon(Command::GoToBookmark),
            Entry::Run(Command::ClearBookmarks, COMPARED),
        ],
    ),
    (
        "View",
        &[
            Entry::Run(Command::ShowAll, FILTERED),
            Entry::Run(Command::ShowDifferences, FILTERED),
            Entry::Run(Command::ShowSame, FILTERED),
            Entry::Run(Command::ShowNone, FILTERED),
            Entry::Rule,
            Entry::Run(Command::ShowContext, FILTERED),
            Entry::Run(Command::ToggleIgnoreUnimportant, FILTERED),
            Entry::Rule,
            Entry::Soon(Command::ToggleIgnored),
            Entry::Rule,
            Entry::Soon(Command::ToggleVisibleWhitespace),
            Entry::Run(Command::ToggleLineNumbers, "The active view has no gutter"),
            Entry::Run(
                Command::ToggleSyntaxHighlighting,
                "No recognized text grammar is selected",
            ),
            Entry::Run(
                Command::PrettifyForComparison,
                "Choose two JSON files or two XML files with valid structured text",
            ),
            Entry::Run(
                Command::CompareStructure,
                "Choose two JSON files or two XML files",
            ),
            Entry::Soon(Command::ToggleWordWrap),
            Entry::Rule,
            Entry::Soon(Command::SideBySideLayout),
            Entry::Soon(Command::OverUnderLayout),
            Entry::Soon(Command::Webpages),
            Entry::Run(Command::Thumbnail, "The active view has no thumbnail"),
            Entry::Run(
                Command::ToggleLineDetails,
                "The active view has no details area",
            ),
            Entry::Run(Command::HexDetails, "The active view has no byte details"),
            Entry::Soon(Command::AlignmentDetails),
            Entry::Soon(Command::Ruler),
            Entry::Soon(Command::FileInfo),
            Entry::Soon(Command::ToggleToolbar),
            Entry::Rule,
            Entry::Run(Command::IncreaseFontSize, "The active view has no editor"),
            Entry::Run(Command::DecreaseFontSize, "The active view has no editor"),
            Entry::Run(Command::ResetFontSize, "The active view has no editor"),
        ],
    ),
    ("Tools", TOOLS),
    ("Help", HELP),
];

/// Version and Media Compare share the common menus but expose their own
/// Compare Info and report labels.
const RECORDS_SESSION: &[Entry] = &[
    Entry::Run(Command::NewSession, ""),
    Entry::Run(Command::NewTab, ""),
    Entry::Soon(Command::NewWindow),
    Entry::Run(Command::OpenSession, ""),
    Entry::Rule,
    Entry::Run(Command::LoadWorkspace, NO_STORE),
    Entry::Run(Command::SaveWorkspaceAs, NO_STORE),
    Entry::Rule,
    Entry::Run(Command::SaveSession, NO_SESSION),
    Entry::Run(Command::SaveSessionAs, NO_SESSION),
    Entry::Run(Command::SessionSettings, NO_SETTINGS),
    Entry::Run(Command::ToggleLocked, NO_SAVED),
    Entry::Run(Command::ClearSession, NO_SESSION),
    Entry::Run(Command::CloseTab, "No tab is open"),
    Entry::Rule,
    Entry::Run(
        Command::SwapSides,
        "The active view has no two sides to swap",
    ),
    Entry::Run(Command::Reload, "The active view reads nothing to reload"),
    Entry::Run(
        Command::Recompare,
        "The active view has nothing to compare again",
    ),
    Entry::Rule,
    Entry::Run(
        Command::CompareReport,
        "The active view has no finished comparison to report",
    ),
    Entry::Run(
        Command::CompareInfo,
        "No finished comparison to show info for",
    ),
    Entry::Rule,
    Entry::Soon(Command::CompareFilesUsing),
    Entry::Soon(Command::MergeFiles),
    Entry::Run(Command::CompareParentFolders, PARENT_FOLDERS),
    Entry::Rule,
    Entry::Run(Command::Exit, ""),
];

const RECORDS_MENUS: &[(&str, &[Entry])] = &[
    ("Session", RECORDS_SESSION),
    ("File", RECORDS_FILE),
    TEXT_MENUS[2],
    TEXT_MENUS[3],
    TEXT_MENUS[4],
    TEXT_MENUS[5],
    TEXT_MENUS[6],
];

/// The record views read clipboard text only for registry exports.
const RECORDS_FILE: &[Entry] = &[
    Entry::Run(Command::OpenFile, FILE_OPENABLE),
    Entry::Run(Command::OpenClipboard, CLIPBOARD_TEXT),
    Entry::Soon(Command::OpenWith),
    Entry::Rule,
    Entry::Run(Command::SaveFile, "Nothing is edited"),
    Entry::Run(Command::SaveFileAs, "Nothing is edited"),
    Entry::Soon(Command::Explorer),
    Entry::Rule,
    Entry::Run(Command::SaveBoth, "Nothing is edited"),
];

/// The three way merge bar: the text bar's Session, File and Search menus,
/// with an Edit and a View menu of the merge's own. The Actions menu is built
/// from the merge view's declaration, as on the shared bar.
const MERGE_MENUS: &[(&str, &[Entry])] = &[
    TEXT_MENUS[0],
    TEXT_MENUS[1],
    ("Edit", MERGE_EDIT),
    TEXT_MENUS[3],
    ("View", MERGE_VIEW),
    ("Tools", TOOLS),
    ("Help", HELP),
];

/// The registry comparison bar: the text bar with an Edit menu of keys and
/// values in the observed order.
const REGISTRY_MENUS: &[(&str, &[Entry])] = &[
    ("Session", REGISTRY_SESSION),
    ("File", RECORDS_FILE),
    ("Edit", REGISTRY_EDIT),
    TEXT_MENUS[3],
    TEXT_MENUS[4],
    ("Tools", TOOLS),
    ("Help", HELP),
];

/// The Table Compare View menu, including its column controls.
const TABLE_VIEW: &[Entry] = &[
    Entry::Run(Command::ShowAll, FILTERED),
    Entry::Run(Command::ShowDifferences, FILTERED),
    Entry::Run(Command::ShowSame, FILTERED),
    Entry::Run(Command::ShowNone, FILTERED),
    Entry::Rule,
    Entry::Run(Command::ToggleIgnoreUnimportant, FILTERED),
    Entry::Run(Command::HideSameColumns, FILTERED),
    Entry::Run(Command::ResizeColumnsToFit, COMPARED),
    Entry::Rule,
    Entry::Run(
        Command::ToggleLineNumbers,
        "The active view has no row numbers",
    ),
    Entry::Run(Command::Thumbnail, "The active view has no thumbnail"),
    Entry::Run(
        Command::ToggleLineDetails,
        "The active view has no details area",
    ),
];

/// The Table Compare Edit menu, with commands that act on the current cell.
const TABLE_EDIT: &[Entry] = &[
    Entry::Run(Command::Undo, "Nothing to reverse"),
    Entry::Run(Command::Redo, "Nothing to reverse"),
    Entry::Rule,
    Entry::Run(Command::CopyCellToRight, EDITABLE),
    Entry::Run(Command::CopyCellToLeft, EDITABLE),
    Entry::Run(Command::CopyCellToOtherSide, EDITABLE),
    Entry::Rule,
    Entry::Run(Command::Cut, EDITABLE),
    Entry::Run(Command::Copy, EDITABLE),
    Entry::Run(Command::Paste, EDITABLE),
    Entry::Run(Command::Delete, ACTIONABLE),
    Entry::Rule,
    Entry::Run(Command::SelectAll, SELECTABLE),
];

/// Table Compare uses the shared session commands, with its own report names
/// and without the unsupported Merge Files command.
const TABLE_SESSION: &[Entry] = &[
    Entry::Run(Command::NewSession, ""),
    Entry::Run(Command::NewTab, ""),
    Entry::Soon(Command::NewWindow),
    Entry::Run(Command::OpenSession, ""),
    Entry::Rule,
    Entry::Run(Command::LoadWorkspace, NO_STORE),
    Entry::Run(Command::SaveWorkspaceAs, NO_STORE),
    Entry::Rule,
    Entry::Run(Command::SaveSession, NO_SESSION),
    Entry::Run(Command::SaveSessionAs, NO_SESSION),
    Entry::Run(Command::SessionSettings, NO_SETTINGS),
    Entry::Run(Command::ToggleLocked, NO_SAVED),
    Entry::Run(Command::ClearSession, NO_SESSION),
    Entry::Run(Command::CloseTab, "No tab is open"),
    Entry::Rule,
    Entry::Run(
        Command::SwapSides,
        "The active view has no two sides to swap",
    ),
    Entry::Run(Command::Reload, "The active view reads nothing to reload"),
    Entry::Run(
        Command::Recompare,
        "The active view has nothing to compare again",
    ),
    Entry::Rule,
    Entry::Run(
        Command::CompareReport,
        "The active view has no finished comparison to report",
    ),
    Entry::Run(
        Command::CompareInfo,
        "No finished comparison to show info for",
    ),
    Entry::Rule,
    Entry::Soon(Command::CompareFilesUsing),
    Entry::Run(Command::CompareParentFolders, PARENT_FOLDERS),
    Entry::Rule,
    Entry::Run(Command::Exit, ""),
];

/// Table Compare uses the shared menus with its own View menu.
const TABLE_MENUS: &[(&str, &[Entry])] = &[
    ("Session", TABLE_SESSION),
    ("File", TABLE_FILE),
    ("Edit", TABLE_EDIT),
    TEXT_MENUS[3],
    ("View", TABLE_VIEW),
    TEXT_MENUS[5],
    TEXT_MENUS[6],
];

/// The Table File menu adds clipboard text as a read-only transient side.
const TABLE_FILE: &[Entry] = &[
    Entry::Run(Command::OpenFile, FILE_OPENABLE),
    Entry::Run(Command::OpenClipboard, CLIPBOARD_TEXT),
    Entry::Soon(Command::OpenWith),
    Entry::Rule,
    Entry::Run(Command::SaveFile, "Nothing is edited"),
    Entry::Run(Command::SaveFileAs, "Nothing is edited"),
    Entry::Soon(Command::Explorer),
    Entry::Rule,
    Entry::Run(Command::SaveBoth, "Nothing is edited"),
];

/// Why an Up One Level line is disabled.
const UP_ONE_LEVEL: &str = if cfg!(windows) {
    "The side is not a live registry key below a hive root"
} else {
    NO_LIVE_REGISTRY
};

/// The Session menu of the registry bar: the text bar's Session menu with the
/// three Up One Level lines after Swap Sides.
const REGISTRY_SESSION: &[Entry] = &[
    Entry::Run(Command::NewSession, ""),
    Entry::Run(Command::NewTab, ""),
    Entry::Soon(Command::NewWindow),
    Entry::Run(Command::OpenSession, ""),
    Entry::Rule,
    Entry::Run(Command::LoadWorkspace, NO_STORE),
    Entry::Run(Command::SaveWorkspaceAs, NO_STORE),
    Entry::Rule,
    Entry::Run(Command::SaveSession, NO_SESSION),
    Entry::Run(Command::SaveSessionAs, NO_SESSION),
    Entry::Run(Command::SessionSettings, NO_SETTINGS),
    Entry::Run(Command::ToggleLocked, NO_SAVED),
    Entry::Run(Command::ClearSession, NO_SESSION),
    Entry::Run(Command::CloseTab, "No tab is open"),
    Entry::Rule,
    Entry::Run(
        Command::SwapSides,
        "The active view has no two sides to swap",
    ),
    Entry::Run(Command::UpOneLevelLeft, UP_ONE_LEVEL),
    Entry::Run(Command::UpOneLevelRight, UP_ONE_LEVEL),
    Entry::Run(Command::UpOneLevelBoth, UP_ONE_LEVEL),
    Entry::Run(Command::Reload, "The active view reads nothing to reload"),
    Entry::Run(
        Command::Recompare,
        "The active view has nothing to compare again",
    ),
    Entry::Rule,
    Entry::Run(
        Command::CompareReport,
        "The active view has no finished comparison to report",
    ),
    Entry::Run(
        Command::CompareInfo,
        "No finished comparison to show info for",
    ),
    Entry::Rule,
    Entry::Soon(Command::CompareFilesUsing),
    Entry::Soon(Command::MergeFiles),
    Entry::Run(Command::CompareParentFolders, PARENT_FOLDERS),
    Entry::Rule,
    Entry::Run(Command::Exit, ""),
];

/// The Edit menu of the registry bar.
const REGISTRY_EDIT: &[Entry] = &[
    Entry::Run(Command::SetAsBaseKeys, BASE_KEYS),
    Entry::Run(Command::SetBothAsBaseKeys, BASE_KEYS),
    Entry::Run(Command::SetAsBaseKeyOnOtherSide, BASE_KEYS),
    Entry::Rule,
    Entry::Run(Command::Undo, "Nothing to reverse"),
    Entry::Run(Command::Redo, "Nothing to reverse"),
    Entry::Rule,
    Entry::Run(Command::CopyToRight, EDITABLE),
    Entry::Run(Command::CopyToLeft, EDITABLE),
    Entry::Run(Command::CopyToOtherSide, EDITABLE),
    Entry::Rule,
    Entry::Run(Command::Copy, EDITABLE),
    Entry::Run(Command::Delete, ACTIONABLE),
    Entry::Run(Command::Rename, ACTIONABLE),
    Entry::Rule,
    Entry::Run(Command::NewKey, EDITABLE),
    Entry::Run(Command::NewValue, EDITABLE),
    Entry::Run(Command::Modify, EDITABLE),
    Entry::Rule,
    Entry::Run(Command::CopyKeyName, COMPARED),
    Entry::Run(Command::Export, COMPARED),
    Entry::Run(Command::ExportAll, COMPARED),
    Entry::Rule,
    Entry::Run(Command::SelectAll, SELECTABLE),
    Entry::Rule,
    Entry::Run(Command::ExpandAll, "The active view has no tree"),
    Entry::Run(Command::CollapseAll, "The active view has no tree"),
];

/// The Edit menu of the merge bar.
const MERGE_EDIT: &[Entry] = &[
    Entry::Run(Command::Undo, "Nothing to reverse"),
    Entry::Run(Command::Redo, "Nothing to reverse"),
    Entry::Rule,
    Entry::Soon(Command::AlignWith),
    Entry::Soon(Command::Isolate),
    Entry::Run(
        Command::ToggleConflict,
        "The current lines hold no change to review",
    ),
    Entry::Rule,
    Entry::Run(Command::Cut, EDITABLE),
    Entry::Run(Command::Copy, EDITABLE),
    Entry::Run(Command::Paste, EDITABLE),
    Entry::Rule,
    Entry::Run(Command::SelectAll, EDITABLE),
    Entry::Run(Command::SelectSection, EDITABLE),
];

/// The View menu of the merge bar, with the filters a merge selects on.
const MERGE_VIEW: &[Entry] = &[
    Entry::Run(Command::ShowAll, FILTERED),
    Entry::Run(Command::ShowDifferences, FILTERED),
    Entry::Run(Command::ShowConflicts, FILTERED),
    Entry::Run(Command::ShowLeftChanges, FILTERED),
    Entry::Run(Command::ShowRightChanges, FILTERED),
    Entry::Run(Command::ShowMergeable, FILTERED),
    Entry::Run(Command::ShowSame, FILTERED),
    Entry::Run(Command::ShowNone, FILTERED),
    Entry::Rule,
    Entry::Run(Command::ShowContext, FILTERED),
    Entry::Run(Command::ToggleIgnoreUnimportant, FILTERED),
    Entry::Run(Command::ToggleIgnoreSameChanges, FILTERED),
    Entry::Rule,
    Entry::Run(Command::ToggleSectionIgnored, FILTERED),
    Entry::Rule,
    Entry::Soon(Command::ToggleVisibleWhitespace),
    Entry::Run(Command::ToggleLineNumbers, "The active view has no gutter"),
    Entry::Soon(Command::ToggleSyntaxHighlighting),
    Entry::Soon(Command::ToggleWordWrap),
    Entry::Rule,
    Entry::Run(Command::Thumbnail, "The active view has no thumbnail"),
    Entry::Rule,
    Entry::Run(Command::IncreaseFontSize, "The active view has no editor"),
    Entry::Run(Command::DecreaseFontSize, "The active view has no editor"),
    Entry::Run(Command::ResetFontSize, "The active view has no editor"),
];

/// The folder comparison bar, in the observed order.
const FOLDER_MENUS: &[(&str, &[Entry])] = &[
    (
        "Session",
        &[
            Entry::Run(Command::NewSession, ""),
            Entry::Run(Command::NewTab, ""),
            Entry::Soon(Command::NewWindow),
            Entry::Run(Command::OpenSession, ""),
            Entry::Rule,
            Entry::Run(Command::LoadWorkspace, NO_STORE),
            Entry::Run(Command::SaveWorkspaceAs, NO_STORE),
            Entry::Rule,
            Entry::Run(Command::SaveSession, NO_SESSION),
            Entry::Run(Command::SaveSessionAs, NO_SESSION),
            Entry::Run(Command::SessionSettings, NO_SETTINGS),
            Entry::Run(Command::ToggleLocked, NO_SAVED),
            Entry::Run(Command::ClearSession, NO_SESSION),
            Entry::Run(Command::CloseTab, "No tab is open"),
            Entry::Rule,
            Entry::Run(
                Command::SwapSides,
                "The active view has no two sides to swap",
            ),
            Entry::Soon(Command::Back),
            Entry::Soon(Command::Forward),
            Entry::Soon(Command::BrowseForFolder),
            Entry::Soon(Command::UpOneLevel),
            Entry::Rule,
            Entry::Run(
                Command::CompareReport,
                "The active view has no finished comparison to report",
            ),
            Entry::Soon(Command::CompareInfo),
            Entry::Rule,
            Entry::Soon(Command::MergeBaseFolders),
            Entry::Soon(Command::SyncBaseFolders),
            Entry::Soon(Command::CompareParentFolders),
            Entry::Rule,
            Entry::Run(Command::Exit, ""),
        ],
    ),
    (
        ACTIONS_MENU,
        &[
            Entry::Soon(Command::OpenFolder),
            Entry::Soon(Command::OpenSubfolders),
            Entry::Soon(Command::CloseSubfolders),
            Entry::Soon(Command::SetAsBaseFolders),
            Entry::Soon(Command::OpenInNewView),
            Entry::Soon(Command::OpenWith),
            Entry::Rule,
            Entry::Run(
                Command::CompareContents,
                "The active view has no file pairs to read",
            ),
            Entry::Run(Command::CopyToOtherSide, ACTIONABLE),
            Entry::Run(Command::MoveToOtherSide, ACTIONABLE),
            Entry::Run(Command::CopyToFolder, ACTIONABLE),
            Entry::Run(Command::MoveToFolder, ACTIONABLE),
            Entry::Run(Command::Delete, ACTIONABLE),
            Entry::Run(Command::Rename, ACTIONABLE),
            Entry::Run(Command::Attributes, ACTIONABLE),
            Entry::Run(Command::Touch, ACTIONABLE),
            Entry::Run(Command::Exclude, ACTIONABLE),
            Entry::Run(Command::NewFolder, ACTIONABLE),
            Entry::Soon(Command::CopyFilename),
            Entry::Soon(Command::ToggleIgnored),
            Entry::Soon(Command::RefreshSelection),
            Entry::Rule,
            Entry::Run(Command::Synchronize, "Nothing is waiting to be reconciled"),
            Entry::Rule,
            Entry::Soon(Command::Explorer),
            Entry::Rule,
            Entry::Run(Command::Exchange, ACTIONABLE),
        ],
    ),
    (
        "Edit",
        &[
            Entry::Run(Command::ExpandAll, "The active view has no tree"),
            Entry::Run(Command::CollapseAll, "The active view has no tree"),
            Entry::Rule,
            Entry::Run(Command::SelectAll, SELECTABLE),
            Entry::Run(Command::SelectAllFiles, SELECTABLE),
            Entry::Run(Command::SelectNewer, SELECTABLE),
            Entry::Run(Command::SelectOrphans, SELECTABLE),
            Entry::Soon(Command::InvertSelection),
            Entry::Run(Command::Reload, "The active view reads nothing to reload"),
            Entry::Run(
                Command::Recompare,
                "The active view has nothing to compare again",
            ),
            Entry::Rule,
            Entry::Run(Command::SelectDifferences, SELECTABLE),
            Entry::Run(Command::ClearSelection, SELECTABLE),
            Entry::Run(Command::CopyToRight, EDITABLE),
            Entry::Run(Command::CopyToLeft, EDITABLE),
        ],
    ),
    (
        "Search",
        &[
            Entry::Run(Command::NextDifference, COMPARED),
            Entry::Run(Command::PreviousDifference, COMPARED),
            Entry::Run(Command::Find, COMPARED),
            Entry::Run(Command::FindNext, COMPARED),
            Entry::Run(Command::FindPrevious, COMPARED),
        ],
    ),
    (
        "View",
        &[
            Entry::Run(Command::ShowAll, FILTERED),
            Entry::Run(Command::ShowDifferences, FILTERED),
            Entry::Run(Command::ShowSame, FILTERED),
            Entry::Run(Command::ShowNone, FILTERED),
            Entry::Soon(Command::ShowNoOrphans),
            Entry::Soon(Command::ShowDifferencesNoOrphans),
            Entry::Soon(Command::ShowOrphans),
            Entry::Soon(Command::ShowLeftNewer),
            Entry::Soon(Command::ShowRightNewer),
            Entry::Soon(Command::ShowLeftNewerAndLeftOrphans),
            Entry::Soon(Command::ShowRightNewerAndRightOrphans),
            Entry::Soon(Command::ShowLeftOrphans),
            Entry::Soon(Command::ShowRightOrphans),
            Entry::Rule,
            Entry::Soon(Command::AlwaysShowFolders),
            Entry::Soon(Command::CompareFilesAndFolderStructure),
            Entry::Soon(Command::OnlyCompareFiles),
            Entry::Soon(Command::IgnoreFolderStructure),
            Entry::Rule,
            Entry::Run(Command::ToggleIgnoreUnimportant, FILTERED),
            Entry::Soon(Command::SuppressFilters),
            Entry::Rule,
            Entry::Soon(Command::Columns),
            Entry::Soon(Command::Legend),
            Entry::Soon(Command::ToggleLog),
            Entry::Soon(Command::ToggleToolbar),
            Entry::Rule,
            Entry::Run(Command::ShowContext, FILTERED),
            Entry::Run(Command::IncreaseFontSize, "The active view has no editor"),
            Entry::Run(Command::DecreaseFontSize, "The active view has no editor"),
            Entry::Run(Command::ResetFontSize, "The active view has no editor"),
        ],
    ),
    ("Tools", TOOLS),
    ("Help", HELP),
];

/// The Tools menu, which both bars carry unchanged.
const TOOLS: &[Entry] = &[
    Entry::Run(Command::Options, NO_OPTIONS),
    Entry::Soon(Command::FileFormats),
    Entry::Run(Command::Profiles, PROFILES_DISABLED),
    Entry::Rule,
    Entry::Run(Command::ExportSettings, NO_STORE),
    Entry::Run(Command::ImportSettings, NO_STORE),
    Entry::Run(Command::RestoreFactoryDefaults, NO_OPTIONS),
    Entry::Rule,
    Entry::Soon(Command::SaveSnapshot),
    Entry::Run(Command::EditTextFile, ""),
    Entry::Run(Command::ViewPatch, ""),
];

/// The Help menu, which both bars carry unchanged.
const HELP: &[Entry] = &[
    Entry::Soon(Command::HelpContents),
    Entry::Soon(Command::ContextHelp),
    Entry::Rule,
    Entry::Soon(Command::OnTheWeb),
    Entry::Run(Command::CheckForUpdates, NO_UPDATE_CHECK),
    Entry::Soon(Command::Support),
    Entry::Soon(Command::About),
];

/// The bar a view carries.
const fn menus_for(view: MenuView) -> &'static [(&'static str, &'static [Entry])] {
    match view {
        MenuView::Table => TABLE_MENUS,
        MenuView::Folder => FOLDER_MENUS,
        MenuView::Merge => MERGE_MENUS,
        MenuView::Registry => REGISTRY_MENUS,
        MenuView::Version | MenuView::Media => RECORDS_MENUS,
        MenuView::Text | MenuView::Other => TEXT_MENUS,
    }
}

/// Why an editing command is disabled.
const EDITABLE: &str = "The active view has no editable pane";
/// Why a command that needs a finished comparison is disabled.
const COMPARED: &str = "Available once a comparison finishes";
/// Why Compare Parent Folders is disabled.
const PARENT_FOLDERS: &str = "Both sides must be local files with existing parent folders";
/// Why file-kind switching is disabled.
const FILE_PAIR_REQUIRED: &str = "Both sides must be existing local files";
/// Why Open File is disabled.
const FILE_OPENABLE: &str = "The active view cannot replace a file now";
/// Why a view cannot load text from the clipboard.
const CLIPBOARD_TEXT: &str = "The active view cannot load text from the clipboard now";
/// Why a display filter command is disabled.
const FILTERED: &str = "The active view has no display filter";
/// Why a selection command is disabled.
const SELECTABLE: &str = "The active view has nothing to select";
/// Why an action command is disabled.
const ACTIONABLE: &str = "Select the items to act on first";
/// Why a base key command is disabled.
const BASE_KEYS: &str = if cfg!(windows) {
    "Put the cursor on a key of a live registry side first"
} else {
    NO_LIVE_REGISTRY
};
/// Why a command that needs a live registry side is disabled on a platform
/// without a Windows registry.
const NO_LIVE_REGISTRY: &str =
    "This platform has no Windows registry; Registry Compare reads export files only";
/// Why Check for Updates is disabled.
const NO_UPDATE_CHECK: &str = "A check is running, or a policy turns update checks off";
/// Why remote profiles are disabled.
const PROFILES_DISABLED: &str = "Remote profiles are disabled by administrator policy";
/// Why a command that needs the stored document is disabled.
const NO_STORE: &str = "The sessions document is still being read";
/// Why a command that needs an open session is disabled.
const NO_SESSION: &str = "No session is open";
/// Why a command that needs the options document is disabled.
const NO_OPTIONS: &str = "The options document is still being read";
/// Why the settings dialog is disabled.
const NO_SETTINGS: &str = "The active view has no settings of its own";
/// Why the lock toggle is disabled.
const NO_SAVED: &str = "The active session is not a saved one";
/// Why Save Session is disabled on a tab over temporary copies.
const TEMPORARIES: &str = "The tab compares temporary copies, which are deleted when the tab \
                           closes, so a saved session could not open them again";
/// Why a side changed on the Specs page does not open in a tab that holds
/// edits or runs work.
const SIDES_KEPT: &str = "The sides did not change: the tab holds unwritten edits or runs \
                          work. Save or drop the edits, or let the work end, then change the \
                          sides again.";
/// Why a side changed on the Specs page does not open in a tab over copies.
const SIDES_OF_TEMPORARIES: &str =
    "The sides did not change: the tab compares temporary copies that belong to it alone.";
/// Why the editing switch stays off in a tab that holds unwritten edits.
const EDITING_KEPT: &str = "Editing stays on: the tab holds unwritten edits. Save or drop them, \
                            then turn editing off.";
/// Why a side changed on the Specs page does not open when no view answers.
const NO_VIEW_FOR_SIDES: &str =
    "The sides did not change: this build has no view for the session type.";
/// Why a changed side was kept when the view cannot open its location form.
const SIDES_UNSUPPORTED: &str =
    "The sides did not change: this view cannot open one or more of those locations.";
/// Why the Open With line carries no entries.
const NO_OPEN_WITH: &str = "No Open With entry is stored for what this view holds";
/// Why Explorer is disabled when the view has no active file or folder.
const NO_EXPLORER_ITEM: &str = "The active view has no local file or folder to show";

/// Longest time the exit waits for a write of a settings document that is
/// already in flight.
const EXIT_WRITE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Commands the window answers for rather than the active view.
const SHELL_OWNED: &[Command] = &[
    Command::ShowNone,
    Command::NewTab,
    Command::CloseTab,
    Command::Exit,
    Command::NewSession,
    Command::OpenSession,
    Command::SaveSession,
    Command::SaveSessionAs,
    Command::SessionSettings,
    Command::ToggleLocked,
    Command::ClearSession,
    Command::LoadWorkspace,
    Command::SaveWorkspaceAs,
    Command::ExportSettings,
    Command::ImportSettings,
    Command::Options,
    Command::RestoreFactoryDefaults,
    Command::EditTextFile,
    Command::ViewPatch,
    Command::CheckForUpdates,
    Command::Profiles,
    Command::CompareFilesUsing,
    Command::CompareParentFolders,
    Command::Explorer,
];

/// The menu the shell builds from the active view's declaration.
const ACTIONS_MENU: &str = "Actions";
/// The fixed menu the Actions menu is placed after.
const ACTIONS_AFTER: &str = "Edit";

/// The window and everything in it.
///
/// The window carries one flag per thing it is in the middle of, so the count
/// of flags is the number of concurrent states rather than a sign of a type
/// doing several jobs.
#[allow(clippy::struct_excessive_bools)]
pub struct App {
    tabs: Vec<Box<dyn SessionView>>,
    active: usize,
    next_instance: u64,
    closing: bool,
    /// What the active view declared this frame, read once and used by every
    /// menu line, so a menu never rebuilds a view's whole declaration per item.
    declared: Vec<CommandState>,
    /// The journal check started at launch, dropped once it has answered.
    recovery: Option<Job<RecoverMessage>>,
    /// What that check found, read by whichever launcher is open.
    startup_notice: SharedNotice,
    /// Where saved sessions, the options and the update check state are kept.
    settings_directory: PathBuf,
    /// Where the journal check looks.
    journal_directory: PathBuf,
    /// The process exit code a view has asked for, shared with the launcher
    /// so the code outlives the window.
    exit_status: Arc<std::sync::atomic::AtomicI32>,
    /// The stored session document, read once and shared with every launcher.
    store: SharedStore,
    /// Which saved session each tab shows, where it shows one.
    tab_sessions: Vec<Option<SessionId>>,
    /// Recent identity is retained without making the tab a named session.
    tab_recent: Vec<Option<SessionId>>,
    /// A name and destination awaiting explicit confirmation.
    session_save: Option<crate::session_save::SessionSave>,
    /// The application options document.
    options: OptionsHandle,
    /// What the options resolved to this frame.
    resolved: Arc<AppOptions>,
    /// The options dialog, while it is open.
    options_dialog: Option<OptionsDialog>,
    /// The restore factory defaults wizard, while it is open.
    restore: Option<RestoreDialog>,
    /// The settings dialog, while it is open.
    settings: Option<SettingsDialog>,
    /// The tab the settings dialog is editing.
    settings_tab: usize,
    /// The sides the Specs page showed when the settings dialog opened.
    settings_shown: Option<Sides>,
    /// Why the last settings did not all apply, while it is on screen.
    settings_notice: Option<String>,
    /// The workspace manager, while it is open.
    workspaces: Option<WorkspaceManager>,
    /// The report of the last import, while it is on screen.
    import_report: Option<ImportReportDialog>,
    /// The package worker, while one is running.
    share: Option<Job<ShareMessage>>,
    /// The picker that names a package, while it is open.
    share_picker: Option<Job<DialogMessage>>,
    /// Whether the picker now open names a package to write or to read.
    share_writes: bool,
    /// Administrator policies, read once.
    policies: AdminPolicies,
    /// What every launcher of this window asked for.
    home_outbox: HomeOutbox,
    /// True once the workspace named for the start has been reopened.
    restored: bool,
    /// The startup wait has already been reported, even if dismissed.
    startup_wait_reported: bool,
    /// Normal worker startup gets a grace period before a wait notice.
    startup_wait_since: Option<std::time::Instant>,
    /// True while the question raised before several tabs are closed is open.
    exit_question: bool,
    /// The workspace a load waits to open while a tab asks about edits that
    /// are not written, or while the work of a tab runs. The load runs again
    /// once a tab closes after its answer, or once that work ends, and is
    /// dropped when the window is used for anything else or is asked to close.
    pending_workspace: Option<String>,
    /// The tab whose work the waiting load waits for.
    pending_busy_tab: Option<usize>,
    /// What the window says about a load that waits or was dropped, until
    /// the user closes the notice or a load runs.
    workspace_notice: Option<String>,
    /// What starts an Open With entry. A test states one of its own.
    spawner: Arc<dyn ca_ui::launch::Spawner>,
    /// The Open With entries that have been started and not yet reported.
    launches: Vec<Job<ca_ui::launch::LaunchMessage>>,
    /// The update check and what it found.
    update: UpdateState,
    /// The remote connection profile manager.
    profiles: Option<crate::profiles::ProfileManager>,
    /// The file manager menu and the worker that reads or changes it.
    explorer: ExplorerState,
}

/// The file manager menu of one window.
#[derive(Default)]
struct ExplorerState {
    /// The keys the menu lives under. None leaves the registry alone.
    root: Option<crate::explorer::MenuRoot>,
    /// The state last read, once a read has finished.
    installed: Option<crate::explorer::Installed>,
    /// The read or change in flight.
    job: Option<Job<crate::explorer::Message>>,
    /// True once the read at start has been started.
    started: bool,
}

/// The update check of one window.
#[derive(Default)]
struct UpdateState {
    /// The release document asked for. None turns every check off.
    url: Option<String>,
    /// True once the check at start has been decided.
    started: bool,
    /// The check in flight.
    job: Option<Job<crate::update::Outcome>>,
    /// True when the check in flight was asked for from the Help menu.
    manual: bool,
    /// What the window shows until the user closes it.
    notice: Option<UpdateNotice>,
}

/// What the update notice shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateNotice {
    /// A newer release exists.
    Newer(crate::update::Release),
    /// The answer to a check asked for from the Help menu.
    Message(String),
}

/// The store every launcher in this window reads.
type SharedStore = crate::home::SharedStore;

#[cfg(not(test))]
impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// A window holding nothing.
    ///
    /// The constructor resolves the settings directory from the environment, so
    /// every window built this way shares one folder. It is withheld from the
    /// tests of this crate: tests in one process run in parallel threads and
    /// test binaries run one after another, so a shared settings directory lets
    /// one test's claim on it, or one test's stored document, decide what
    /// another test sees. A test builds a window with
    /// [`App::in_directories`] or [`App::from_startup_in`] instead.
    #[cfg(not(test))]
    #[must_use]
    pub fn new() -> Self {
        Self::in_directories(
            ca_ui::paths::settings_directory(),
            ca_ui::paths::journal_directory(),
        )
    }

    /// A window holding nothing, reading the stated directories.
    #[must_use]
    pub fn in_directories(settings_directory: PathBuf, journal_directory: PathBuf) -> Self {
        let store = std::rc::Rc::new(std::cell::RefCell::new(StoreHandle::open_in(
            settings_directory.clone(),
            Arc::new(|| {}),
        )));
        Self {
            store,
            options: OptionsHandle::open_in(settings_directory.clone(), Arc::new(|| {})),
            resolved: Arc::new(AppOptions::default()),
            options_dialog: None,
            restore: None,
            tab_sessions: Vec::new(),
            tab_recent: Vec::new(),
            session_save: None,
            settings: None,
            settings_tab: 0,
            settings_shown: None,
            settings_notice: None,
            workspaces: None,
            import_report: None,
            share: None,
            share_picker: None,
            share_writes: false,
            policies: AdminPolicies::default(),
            home_outbox: HomeOutbox::default(),
            restored: false,
            startup_wait_reported: false,
            startup_wait_since: None,
            exit_question: false,
            pending_workspace: None,
            pending_busy_tab: None,
            workspace_notice: None,
            spawner: Arc::new(ca_ui::launch::SystemSpawner),
            launches: Vec::new(),
            update: UpdateState::default(),
            profiles: None,
            explorer: ExplorerState::default(),
            tabs: Vec::new(),
            active: 0,
            next_instance: 0,
            closing: false,
            declared: Vec::new(),
            recovery: None,
            startup_notice: SharedNotice::default(),
            settings_directory,
            journal_directory,
            exit_status: Arc::new(std::sync::atomic::AtomicI32::new(0)),
        }
    }

    /// Report the exit code of whatever this window ends with to `status`.
    ///
    /// A merge started by another program answers through the exit code, so
    /// the value has to survive the window it was decided in.
    pub fn report_exit_to(&mut self, status: Arc<std::sync::atomic::AtomicI32>) {
        status.store(
            self.exit_status.load(std::sync::atomic::Ordering::SeqCst),
            std::sync::atomic::Ordering::SeqCst,
        );
        self.exit_status = status;
    }

    /// The exit code the window would return now.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.exit_status.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Record the exit code a view asks for.
    fn publish_exit(&self, view: &dyn SessionView) {
        if let Some(code) = view.exit_code() {
            self.exit_status
                .store(code, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A window showing whatever the command line asked for.
    ///
    /// Withheld from the tests of this crate for the reason [`App::new`] gives.
    #[cfg(not(test))]
    #[must_use]
    pub fn from_startup(startup: Startup, context: &ViewContext) -> Self {
        let mut app = Self::from_startup_in(
            startup,
            context,
            ca_ui::paths::settings_directory(),
            ca_ui::paths::journal_directory(),
        );
        app.check_updates_at(crate::update::RELEASES_URL.to_owned());
        if crate::explorer::AVAILABLE {
            app.manage_explorer_menu_at(crate::explorer::MenuRoot::system());
        }
        app
    }

    /// Read and change the file manager menu under `root`. A window built
    /// without this call never reads or writes the registry.
    pub fn manage_explorer_menu_at(&mut self, root: crate::explorer::MenuRoot) {
        self.explorer.root = Some(root);
    }

    /// The file manager menu state last read, once a read has finished.
    #[must_use]
    pub fn explorer_menu_state(&self) -> Option<crate::explorer::Installed> {
        self.explorer.installed
    }

    /// True while a read or a change of the file manager menu runs.
    #[must_use]
    pub fn explorer_menu_busy(&self) -> bool {
        self.explorer.job.is_some()
    }

    /// Start the read of the menu state once, on a worker.
    fn start_explorer_read(&mut self, notify: &Arc<dyn Fn() + Send + Sync>) {
        if self.explorer.started {
            return;
        }
        let Some(root) = self.explorer.root.clone() else {
            return;
        };
        self.explorer.started = true;
        self.explorer.job = Some(crate::explorer::spawn(root, None, Arc::clone(notify)));
    }

    /// Change the menu to match the option, when the two differ.
    ///
    /// Only the user keys change. Verbs that a per-machine install wrote stay
    /// until the installer removes them, and the window says so.
    fn sync_explorer_menu(&mut self, wanted: bool, notify: &Arc<dyn Fn() + Send + Sync>) {
        let (Some(root), Some(state)) = (self.explorer.root.clone(), self.explorer.installed)
        else {
            return;
        };
        if self.explorer.job.is_some() {
            return;
        }
        if !wanted && state.machine {
            self.raise(
                "The installer added the file manager menu for every user. Remove the Shell Integration feature with the installer."
                    .to_owned(),
            );
        }
        let change = if wanted { !state.any() } else { state.user };
        if change {
            self.explorer.job = Some(crate::explorer::spawn(
                root,
                Some(wanted),
                Arc::clone(notify),
            ));
        }
    }

    /// Take what the menu worker reported.
    fn poll_explorer(&mut self) {
        use crate::explorer::Message;
        let Some(job) = self.explorer.job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                Message::State(state) => self.explorer.installed = Some(state),
                Message::Failed(reason, state) => {
                    self.explorer.installed = Some(state);
                    self.raise(format!("The file manager menu was not changed: {reason}"));
                }
                Message::Cancelled => {}
            }
        }
        if finished {
            self.explorer.job = None;
        }
    }

    /// Ask `url` for the newest release. A window built without this call
    /// never checks.
    pub fn check_updates_at(&mut self, url: String) {
        self.update.url = Some(url);
    }

    /// What the update notice shows, while it is open.
    #[must_use]
    pub fn update_notice(&self) -> Option<&UpdateNotice> {
        self.update.notice.as_ref()
    }

    /// True while an update check runs.
    #[must_use]
    pub fn is_checking_for_updates(&self) -> bool {
        self.update.job.is_some()
    }

    /// A window showing whatever the command line asked for, reading the stated
    /// directories.
    #[must_use]
    pub fn from_startup_in(
        startup: Startup,
        context: &ViewContext,
        settings_directory: PathBuf,
        journal_directory: PathBuf,
    ) -> Self {
        let mut app = Self::in_directories(settings_directory, journal_directory);
        app.start_recovery_check(context);
        match startup {
            // `RememberLeft` and `CompareToLeft` are resolved by `cli::run_left_side`
            // before a window exists.
            Startup::Home | Startup::RememberLeft(_) | Startup::CompareToLeft(_) => {
                app.open_home(context);
            }
            // The window has no console behind it, so the reason a command line
            // was refused has nowhere to be printed and is carried into the
            // launcher instead.
            Startup::Rejected(reason) => {
                let instance = app.instance();
                let store = std::rc::Rc::clone(&app.store);
                let mut view = HomeView::with_notice(context, instance, store, reason);
                view.share_actions(std::rc::Rc::clone(&app.home_outbox));
                app.push(Box::new(view));
            }
            // An automatic merge that reaches the window is one whose conflicts
            // are to be reviewed, so it opens as an ordinary merge.
            Startup::Open(request) | Startup::AutoMerge(request, _) => app.open(&request, context),
        }
        app
    }

    /// How many tabs are open.
    #[must_use]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// The active tab's title.
    #[must_use]
    pub fn active_title(&self) -> Option<String> {
        self.tabs.get(self.active).map(|tab| tab.title())
    }

    /// True when the active tab has finished whatever it loads.
    #[must_use]
    pub fn active_is_ready(&self) -> bool {
        self.tabs.get(self.active).is_some_and(|tab| tab.is_ready())
    }

    /// True when the tab at `index` has finished whatever it loads.
    #[must_use]
    pub fn tab_is_ready(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|tab| tab.is_ready())
    }

    /// The settings the tab at `index` reports, which name the sides it has
    /// open.
    #[must_use]
    pub fn tab_settings(&self, index: usize) -> Option<SessionSettings> {
        self.tabs.get(index).and_then(|tab| tab.settings())
    }

    /// The description the session of the tab at `index` carries, which the
    /// tab strip shows when the pointer rests on the tab, or `None` when it
    /// carries none.
    #[must_use]
    pub fn tab_description(&self, index: usize) -> Option<String> {
        self.tab_settings(index)
            .and_then(|settings| settings.specs().map(|specs| specs.description.clone()))
            .filter(|description| !description.trim().is_empty())
    }

    /// Whatever the active tab is reporting outside its own content.
    #[must_use]
    pub fn active_notice(&self) -> Option<String> {
        self.tabs.get(self.active).and_then(|tab| tab.notice())
    }

    /// True once the window has been asked to close.
    #[must_use]
    pub fn is_closing(&self) -> bool {
        self.closing
    }

    fn instance(&mut self) -> u64 {
        self.next_instance += 1;
        self.next_instance
    }

    /// Open the launcher in a new tab.
    pub fn open_home(&mut self, context: &ViewContext) {
        let instance = self.instance();
        let mut view = HomeView::new(context, instance, std::rc::Rc::clone(&self.store));
        view.watch(Arc::clone(&self.startup_notice));
        view.share_actions(std::rc::Rc::clone(&self.home_outbox));
        self.push(Box::new(view));
    }

    /// The stored session document this window reads.
    #[must_use]
    pub fn store(&self) -> &SharedStore {
        &self.store
    }

    /// The administrator policies in force.
    #[must_use]
    pub fn policies(&self) -> AdminPolicies {
        self.policies
    }

    /// The application options document.
    #[must_use]
    pub fn options(&self) -> &OptionsHandle {
        &self.options
    }

    /// What the options resolved to on the last frame.
    #[must_use]
    pub fn resolved_options(&self) -> &Arc<AppOptions> {
        &self.resolved
    }

    /// The options dialog, while one is open.
    #[must_use]
    pub fn options_dialog(&self) -> Option<&OptionsDialog> {
        self.options_dialog.as_ref()
    }

    /// The options dialog for editing, while one is open.
    pub fn options_dialog_mut(&mut self) -> Option<&mut OptionsDialog> {
        self.options_dialog.as_mut()
    }

    /// The restore wizard, while one is open.
    #[must_use]
    pub fn restore_dialog(&self) -> Option<&RestoreDialog> {
        self.restore.as_ref()
    }

    /// Resolves the stored options into the tables and the routing this frame
    /// runs under.
    ///
    /// The dialog's own copy wins while a preview is asked for, so a color
    /// change is judged in the open views before it is committed.
    fn resolve_options(&self, ctx: &egui::Context) -> AppOptions {
        let stored = match self.options_dialog.as_ref() {
            Some(dialog) if dialog.previews() => dialog.options().clone(),
            _ => self.options.options_or_default(),
        };
        let variant =
            Variant::from_dark_mode(stored.appearance.wants_dark(ctx.style().visuals.dark_mode));
        AppOptions::resolve(stored, variant, self.policies)
    }

    /// Take what the options dialog produced.
    pub fn apply_options(&mut self, options: ca_session::ProgramOptions) {
        self.options.replace(options);
    }

    /// Carry out a restore of the factory defaults.
    pub fn apply_restore(&mut self, selection: &ca_session::RestoreSelection) {
        let mut options = self.options.options_or_default();
        options.restore(selection);
        self.apply_options(options);
        if selection.holds(ca_session::RestoreCategory::Sessions) {
            let mut save = false;
            if let Ok(mut handle) = self.store.try_borrow_mut() {
                if let Some(store) = handle.store_mut() {
                    if selection.delete_all_sessions {
                        store.root.clear();
                        store.auto_saved.clear();
                    }
                    store.layers = ca_session::SettingsLayers::new();
                    save = true;
                }
            }
            if save {
                self.save_store();
            }
        }
    }

    /// Take the policies read from the machine-wide store.
    pub fn set_policies(&mut self, policies: AdminPolicies) {
        self.policies = policies;
    }

    /// State what starts an Open With entry.
    pub fn set_spawner(&mut self, spawner: Arc<dyn ca_ui::launch::Spawner>) {
        self.spawner = spawner;
    }

    /// The Open With entries the active view would offer, in menu order.
    ///
    /// A view that names no files offers none, so the menu never lists an entry
    /// that would start a program over nothing.
    #[must_use]
    pub fn open_with_entries(&self) -> Vec<(usize, String)> {
        let Some(target) = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.launch_target())
        else {
            return Vec::new();
        };
        let options = self.resolved.stored.open_with.clone();
        ca_ui::launch::offered(&options, target.selection)
            .into_iter()
            .map(|(index, entry)| (index, ca_ui::launch::label_of(entry)))
            .collect()
    }

    /// Start the Open With entry at `index` over whatever the active view names.
    pub fn start_open_with(&mut self, index: usize, context: &ViewContext) {
        let Some(target) = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.launch_target())
        else {
            return;
        };
        let options = self.resolved.stored.open_with.clone();
        let Some(entry) = options.entries.get(index) else {
            return;
        };
        self.launches.push(ca_ui::launch::spawn(
            entry,
            &target.context,
            Arc::clone(&self.spawner),
            Arc::clone(&context.notify),
        ));
    }

    /// Show the active tab's current item in the platform file manager.
    fn reveal_active_item(&mut self, context: &ViewContext) {
        let Some((path, selection)) = self.active_local_explorer_target() else {
            return;
        };
        self.launches.push(ca_ui::launch::reveal(
            &path,
            selection,
            Arc::clone(&self.spawner),
            Arc::clone(&context.notify),
        ));
    }

    /// Close every tab after each view has agreed to close.
    ///
    /// Both settings documents are written on this thread before the window
    /// goes, because the process ends soon after and a worker still writing
    /// then dies with it. Every wait for a write already in flight, including
    /// the waits of the two handles as they close, is bounded by
    /// [`EXIT_WRITE_WAIT`] in all, and each document is then written once.
    fn leave(&mut self) {
        self.leave_within(EXIT_WRITE_WAIT);
    }

    fn leave_within(&mut self, limit: std::time::Duration) {
        let deadline = std::time::Instant::now() + limit;
        let remaining = || deadline.saturating_duration_since(std::time::Instant::now());
        // A save in flight holds the store, so the records below would find
        // nothing to write into.
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            handle.wait(remaining());
            handle.close_by(deadline);
        }
        for index in 0..self.tabs.len() {
            if let Some(tab) = self.tabs.get(index) {
                self.publish_exit(tab.as_ref());
            }
            self.record_auto_saved(index);
        }
        self.save_workspace_on_exit();
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            handle.save_now();
        }
        self.options.finish(remaining());
        self.options.close_by(deadline);
        for mut tab in self.tabs.drain(..) {
            tab.on_close();
        }
        self.tab_sessions.clear();
        self.tab_recent.clear();
        self.session_save = None;
        self.active = 0;
        self.exit_question = false;
        self.closing = true;
    }

    fn try_leave(&mut self) {
        if self
            .profiles
            .as_mut()
            .is_some_and(|profiles| !profiles.request_exit())
        {
            return;
        }
        if self.select_refusing_tab() {
            return;
        }
        self.leave();
    }

    /// Ask every tab, from the active one on, whether it may close.
    ///
    /// The first tab that refuses raises its own question and becomes the
    /// active tab, and the answer is true. The tabs after it are not asked.
    fn select_refusing_tab(&mut self) -> bool {
        let count = self.tabs.len();
        for offset in 0..count {
            let index = (self.active + offset) % count;
            if !self.tabs[index].may_close() {
                self.active = index;
                return true;
            }
        }
        false
    }

    fn request_exit(&mut self) {
        self.drop_pending_workspace();
        if self.closing || self.exit_question {
            return;
        }
        if self.tabs.len() > 1 && self.resolved.stored.tabs.confirm_closing_several_tabs {
            self.exit_question = true;
        } else {
            self.try_leave();
        }
    }

    /// True while the window is asking whether to close several tabs.
    #[must_use]
    pub const fn asks_before_closing(&self) -> bool {
        self.exit_question
    }

    /// Answer the question raised before several tabs are closed.
    pub fn answer_exit(&mut self, leave: bool) {
        if !self.exit_question {
            return;
        }
        self.exit_question = false;
        if leave {
            self.try_leave();
        }
    }

    /// Write the open tabs to the workspace the Startup page names, if it names
    /// one.
    fn save_workspace_on_exit(&mut self) {
        let name = self.resolved.stored.startup.save_workspace_on_exit.clone();
        if name.is_empty() {
            return;
        }
        let workspace = self.capture_workspace(&name);
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            if let Some(store) = handle.store_mut() {
                store.save_workspace(workspace);
            }
        }
    }

    /// Take whatever the started programs reported.
    /// Start the check at start once the options have been read.
    fn start_update_check(&mut self, notify: &Arc<dyn Fn() + Send + Sync>) {
        if self.update.started || !self.options.is_ready() {
            return;
        }
        self.update.started = true;
        let tweaks = &self.resolved.stored.tweaks;
        if !tweaks.check_for_updates || self.policies.check_for_updates_disabled() {
            return;
        }
        let days = tweaks.check_for_updates_days;
        self.spawn_update_check(days, false, notify);
    }

    /// Start a check on a worker, unless one is running.
    fn spawn_update_check(
        &mut self,
        days: u32,
        forced: bool,
        notify: &Arc<dyn Fn() + Send + Sync>,
    ) {
        if self.update.job.is_some() || self.policies.check_for_updates_disabled() {
            return;
        }
        let Some(url) = self.update.url.clone() else {
            return;
        };
        let Some(current) = crate::update::Version::parse(VERSION) else {
            return;
        };
        let mut check =
            crate::update::Check::published(current, self.settings_directory.clone(), days, forced);
        check.url = url;
        self.update.manual = forced;
        self.update.job = Some(crate::update::spawn(check, Arc::clone(notify)));
    }

    /// Take what the update check found.
    ///
    /// A check at start reports a newer release only. A check asked for from
    /// the Help menu also reports that none exists and why it failed.
    fn poll_update(&mut self) {
        use crate::update::Outcome;
        let Some(job) = self.update.job.as_mut() else {
            return;
        };
        let outcomes = job.drain();
        let finished = job.is_finished();
        let manual = self.update.manual;
        for outcome in outcomes {
            let notice = match outcome {
                Outcome::Newer(release) => Some(UpdateNotice::Newer(release)),
                Outcome::Current if manual => Some(UpdateNotice::Message(format!(
                    "No newer release exists. This build is {VERSION}."
                ))),
                Outcome::Failed(reason) if manual => Some(UpdateNotice::Message(format!(
                    "The update check failed: {reason}"
                ))),
                _ => None,
            };
            if notice.is_some() {
                self.update.notice = notice;
            }
        }
        if finished {
            self.update.job = None;
        }
    }

    /// Open the page of the release the notice names in the browser.
    pub fn open_release_page(&mut self, notify: &Arc<dyn Fn() + Send + Sync>) {
        let Some(UpdateNotice::Newer(release)) = self.update.notice.take() else {
            return;
        };
        self.launches.push(ca_ui::launch::open_url_with_system(
            &release.page,
            Arc::clone(&self.spawner),
            Arc::clone(notify),
        ));
    }

    /// Draw the update notice in a corner of the window. It blocks nothing.
    fn draw_update_notice(&mut self, ctx: &egui::Context, context: &ViewContext) {
        let Some(notice) = self.update.notice.clone() else {
            return;
        };
        let mut open = true;
        let mut visit = false;
        egui::Window::new("Check for Updates")
            .id(egui::Id::new("update-notice"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -12.0])
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| match &notice {
                UpdateNotice::Newer(release) => {
                    ui.label(format!(
                        "Compare All {} is available. This build is {VERSION}.",
                        release.version
                    ));
                    ui.horizontal(|ui| {
                        ui.label("Release notes:");
                        ui.add(egui::Label::new(&release.page).selectable(true));
                    });
                    visit = ui.button("Open Release Page").clicked();
                }
                UpdateNotice::Message(text) => {
                    widgets::notice_current(ui, ca_ui::icons::Icon::Info, 24.0, text);
                }
            });
        if visit {
            self.open_release_page(&context.notify);
        } else if !open {
            self.update.notice = None;
        }
    }

    fn draw_workspace_notice(&mut self, ctx: &egui::Context) {
        let Some(text) = self.workspace_notice.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Load Workspace")
            .id(egui::Id::new("workspace-notice"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(text);
            });
        if !open {
            self.workspace_notice = None;
        }
    }

    fn poll_launches(&mut self) {
        let mut reports = Vec::new();
        self.launches.retain_mut(|job| {
            reports.extend(job.drain());
            !job.is_finished()
        });
        for report in reports {
            if let ca_ui::launch::LaunchMessage::Failed { reason } = report {
                self.raise(reason);
            }
        }
    }

    /// The saved session the active tab shows, where it shows one.
    #[must_use]
    pub fn active_session(&self) -> Option<SessionId> {
        self.tab_sessions.get(self.active).cloned().flatten()
    }

    /// The settings the active tab reports, which name the sides it has
    /// open. Save Session stores these.
    #[must_use]
    pub fn active_settings(&self) -> Option<SessionSettings> {
        self.tabs.get(self.active).and_then(|tab| tab.settings())
    }

    /// The settings dialog, while one is open.
    #[must_use]
    pub fn settings_dialog(&self) -> Option<&SettingsDialog> {
        self.settings.as_ref()
    }

    /// The settings dialog for editing, while one is open.
    pub fn settings_dialog_mut(&mut self) -> Option<&mut SettingsDialog> {
        self.settings.as_mut()
    }

    /// Accept the settings dialog as its OK button does.
    pub fn accept_settings(&mut self, context: &ViewContext) {
        let Some(outcome) = self.settings.as_mut().map(SettingsDialog::accept) else {
            return;
        };
        self.apply_settings(&outcome, context);
        self.settings = None;
    }

    /// The workspace manager, while one is open.
    #[must_use]
    pub fn workspace_manager(&self) -> Option<&WorkspaceManager> {
        self.workspaces.as_ref()
    }

    /// The import report, while one is on screen.
    #[must_use]
    pub fn import_report(&self) -> Option<&ImportReportDialog> {
        self.import_report.as_ref()
    }

    /// Take whatever the store worker has posted and write what is due.
    fn poll_store(&mut self) {
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            handle.poll();
        }
        self.poll_share();
    }

    /// Write the stored document.
    fn save_store(&mut self) {
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            handle.save();
        }
    }

    /// Open the workspace named for the start, once the document has been read.
    fn restore_workspace(&mut self, context: &ViewContext) {
        if self.restored {
            return;
        }
        if !self.options.is_ready() {
            return;
        }
        let named = self.resolved.stored.startup.load_workspace.clone();
        let shared_store = std::rc::Rc::clone(&self.store);
        let Ok(handle) = shared_store.try_borrow() else {
            self.report_startup_wait(&named);
            return;
        };
        let Some(store) = handle.store() else {
            drop(handle);
            self.report_startup_wait(&named);
            return;
        };
        // The Startup page names a workspace outright; the stored "last
        // workspace" is the fallback for a build that names none.
        if !named.is_empty() {
            drop(handle);
            self.restored = true;
            self.load_workspace(&named, context);
            return;
        }
        if !store.restore_last_workspace {
            self.restored = true;
            return;
        }
        let Some(name) = store.last_workspace.clone() else {
            self.restored = true;
            return;
        };
        drop(handle);
        self.restored = true;
        self.load_workspace(&name, context);
    }

    /// Explain a delayed named workspace once; restoration still retries.
    fn report_startup_wait(&mut self, name: &str) {
        if name.is_empty() || self.startup_wait_reported {
            return;
        }
        let since = self
            .startup_wait_since
            .get_or_insert_with(std::time::Instant::now);
        if since.elapsed() >= std::time::Duration::from_secs(1) && self.workspace_notice.is_none() {
            self.startup_wait_reported = true;
            self.workspace_notice = Some(format!(
                "Waiting for saved sessions before opening workspace {name}."
            ));
        }
    }

    /// Close every tab and reopen the tabs a workspace holds.
    ///
    /// Every tab is asked first, as an exit asks. A tab that refuses raises
    /// its own question, and the load waits until that tab closes. A tab
    /// that refuses while its work runs asks nothing, and the load waits until
    /// that work ends.
    pub fn load_workspace(&mut self, name: &str, context: &ViewContext) {
        self.pending_workspace = None;
        self.pending_busy_tab = None;
        let tabs: Vec<WorkspaceTab> = {
            let Ok(handle) = self.store.try_borrow() else {
                return;
            };
            let Some(workspace) = handle.store().and_then(|store| store.workspace(name)) else {
                self.workspace_notice = Some(format!(
                    "Workspace {name} could not be found in saved sessions."
                ));
                return;
            };
            workspace
                .windows
                .iter()
                .flat_map(|window| window.tabs.iter().cloned())
                .collect()
        };
        if self.select_refusing_tab() {
            let title = self.active_title().unwrap_or_default();
            let busy = self.tabs.get(self.active).is_some_and(|tab| tab.is_busy());
            self.workspace_notice = Some(if busy {
                format!("The workspace {name} opens when the work in the tab {title} ends.")
            } else {
                format!(
                    "The workspace {name} opens when the tab {title} closes. Answer its question."
                )
            });
            self.pending_workspace = Some(name.to_owned());
            self.pending_busy_tab = busy.then_some(self.active);
            return;
        }
        self.workspace_notice = None;
        self.session_save = None;
        while !self.tabs.is_empty() {
            let mut view = self.tabs.remove(0);
            view.on_close();
            self.tab_sessions.remove(0);
            self.tab_recent.remove(0);
        }
        self.closing = false;
        self.active = 0;
        for tab in tabs {
            match tab {
                WorkspaceTab::Saved { session, .. } => self.open_saved(&session, context),
                WorkspaceTab::Unsaved { session, .. } => {
                    if self.open_session(&session, context) {
                        let recent = self.store.try_borrow().ok().is_some_and(|handle| {
                            handle.store().is_some_and(|store| {
                                store.find(&session.id).is_none()
                                    && store.auto_saved.iter().any(|held| held.id == session.id)
                            })
                        });
                        if recent {
                            let last = self.tabs.len() - 1;
                            self.tab_recent[last] = Some(session.id.clone());
                        }
                    }
                }
                _ => self.open_home(context),
            }
        }
        if self.tabs.is_empty() {
            self.open_home(context);
        }
    }

    /// Forget a load that waits for a tab to close, and say so.
    fn drop_pending_workspace(&mut self) {
        self.pending_busy_tab = None;
        if let Some(name) = self.pending_workspace.take() {
            self.workspace_notice = Some(format!("The workspace {name} was not loaded."));
        }
    }

    /// What the window says about a workspace load that waits or was
    /// dropped.
    #[must_use]
    pub fn workspace_notice(&self) -> Option<&str> {
        self.workspace_notice.as_deref()
    }

    /// The open tabs as a workspace under `name`.
    ///
    /// A tab that shows a saved session names it. A tab that was never named
    /// carries its session, which a load opens as the launcher opens an
    /// automatic session. A tab over temporary copies, and one with no
    /// settings, is the launcher.
    #[must_use]
    pub fn capture_workspace(&self, name: &str) -> Workspace {
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                if tab.holds_temporaries() {
                    WorkspaceTab::home()
                } else if let Some(id) = self.tab_sessions.get(index).cloned().flatten() {
                    WorkspaceTab::saved(id)
                } else {
                    self.unnamed_tab(index, tab.as_ref())
                }
            })
            .collect();
        ca_ui::workspace::one_window(name, tabs, self.active)
    }

    /// The workspace entry of the tab at `index`, which shows no saved
    /// session.
    fn unnamed_tab(&self, index: usize, tab: &dyn SessionView) -> WorkspaceTab {
        if tab.holds_temporaries() {
            return WorkspaceTab::home();
        }
        let Some(settings) = tab.settings() else {
            return WorkspaceTab::home();
        };
        let held = self
            .tab_recent
            .get(index)
            .and_then(Option::as_ref)
            .and_then(|id| {
                let handle = self.store.try_borrow().ok()?;
                handle
                    .store()?
                    .auto_saved
                    .iter()
                    .find(|held| &held.id == id)
                    .cloned()
            });
        let mut session = held.unwrap_or_else(|| {
            let id = SessionId::from_raw(format!("workspace-tab-{index}"));
            SavedSession::new(id, tab.title(), settings.kind())
        });
        session.settings = self.stored_form(&settings);
        WorkspaceTab::unsaved(session)
    }

    /// Open a named or recent session with this identifier.
    pub fn open_saved(&mut self, id: &SessionId, context: &ViewContext) {
        let (session, named) = {
            let Ok(handle) = self.store.try_borrow() else {
                return;
            };
            let named = handle
                .store()
                .and_then(|store| store.find_session(id))
                .cloned();
            let is_named = named.is_some();
            let session = named.or_else(|| {
                handle
                    .store()
                    .and_then(|store| store.auto_saved.iter().find(|session| &session.id == id))
                    .cloned()
            });
            (session, is_named)
        };
        let Some(session) = session else {
            self.open_home(context);
            return;
        };
        if self.open_session(&session, context) {
            let last = self.tabs.len() - 1;
            if named {
                self.tab_sessions[last] = Some(id.clone());
            } else {
                self.tab_recent[last] = Some(id.clone());
            }
        }
    }

    /// Open a view over one stored session, reporting whether one opened.
    fn open_session(&mut self, session: &SavedSession, context: &ViewContext) -> bool {
        let resolved = {
            let Ok(handle) = self.store.try_borrow() else {
                return false;
            };
            let layers = handle
                .store()
                .map(|store| store.layers.clone())
                .unwrap_or_default();
            layers
                .resolve(&session.kind, &session.settings)
                .unwrap_or_else(|_| SessionSettings::defaults_for(&session.kind))
        };
        let Some(specs) = specs_of(&resolved) else {
            self.settings_notice = Some(SIDES_UNSUPPORTED.to_owned());
            return false;
        };
        let request = OpenRequest::new(
            session.kind.clone(),
            specs.0.unwrap_or_default(),
            specs.1.unwrap_or_default(),
        )
        .with_center(specs.2)
        .with_output(specs.3);
        let before = self.tabs.len();
        self.open(&request, context);
        if self.tabs.len() == before {
            return false;
        }
        if let Some(tab) = self.tabs.last_mut() {
            tab.apply_settings(&resolved);
        }
        true
    }

    /// Record the active session under a name, and write the document.
    fn save_session_as(
        &mut self,
        tab: usize,
        name: &str,
        parent: Option<&SessionId>,
    ) -> Result<(), String> {
        if self
            .tabs
            .get(tab)
            .is_none_or(|view| view.holds_temporaries())
        {
            return Err("This tab cannot be stored as a session.".to_owned());
        }
        let Some(settings) = self.tabs.get(tab).and_then(|view| view.settings()) else {
            return Err("This tab has no session settings.".to_owned());
        };
        if name.trim().is_empty() {
            return Err("Enter a session name.".to_owned());
        }
        let kind = settings.kind();
        let saved = {
            let mut handle = self
                .store
                .try_borrow_mut()
                .map_err(|_| "Saved sessions are busy.".to_owned())?;
            let store = handle
                .store_mut()
                .ok_or_else(|| "Saved sessions are still loading or saving.".to_owned())?;
            let recent = self.tab_recent.get(tab).cloned().flatten();
            if let Some(id) = recent.filter(|id| store.auto_saved.iter().any(|held| &held.id == id))
            {
                let held = store
                    .auto_saved
                    .iter()
                    .find(|held| held.id == id)
                    .ok_or_else(|| "The recent session is no longer available.".to_owned())?;
                if held.locked || held.kind != kind {
                    return Err(
                        "The recent session cannot be promoted with these settings.".to_owned()
                    );
                }
                store
                    .promote_auto_saved_with_settings(
                        &id,
                        name,
                        parent,
                        self.stored_form(&settings),
                    )
                    .map_err(|error| error.to_string())?
            } else {
                let id = store.next_id();
                let mut session = SavedSession::new(id.clone(), name, kind);
                session.settings = self.stored_form(&settings);
                store
                    .add_session(parent, session)
                    .map_err(|error| error.to_string())?;
                id
            }
        };
        self.tab_sessions[tab] = Some(saved);
        self.tab_recent[tab] = None;
        self.save_store();
        Ok(())
    }

    fn request_session_save(&mut self) {
        if self.active_holds_temporaries() {
            return;
        }
        let pair = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.settings())
            .and_then(|settings| specs_of(&settings))
            .and_then(|(left, right, _, _)| Some((left?, right?)));
        let name = pair
            .and_then(|(left, right)| {
                Some(format!(
                    "{} <--> {}",
                    left.file_name()?.to_string_lossy(),
                    right.file_name()?.to_string_lossy()
                ))
            })
            .unwrap_or_else(|| self.active_title().unwrap_or_else(|| "Untitled".to_owned()));
        self.session_save = Some(crate::session_save::SessionSave::new(self.active, name));
    }

    /// Write the active session back over the one it was opened from.
    fn save_session(&mut self) {
        if self.active_holds_temporaries() {
            return;
        }
        let Some(id) = self.active_session() else {
            self.request_session_save();
            return;
        };
        let Some(settings) = self.tabs.get(self.active).and_then(|tab| tab.settings()) else {
            return;
        };
        let mut failed = None;
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            if let Some(store) = handle.store_mut() {
                let overrides = self.stored_form(&settings);
                if let Err(error) = store.update_session_settings(&id, overrides) {
                    failed = Some(error.to_string());
                }
            }
        }
        match failed {
            Some(reason) => self.raise(reason),
            None => self.save_store(),
        }
    }

    /// Keep the session a closing tab held, so it can be reopened unnamed.
    fn record_auto_saved(&mut self, index: usize) {
        if self
            .tabs
            .get(index)
            .is_some_and(|tab| tab.holds_temporaries())
        {
            return;
        }
        let Some(settings) = self.tabs.get(index).and_then(|tab| tab.settings()) else {
            return;
        };
        if self.tab_sessions.get(index).cloned().flatten().is_some() {
            return;
        }
        let title = self.tabs[index].title();
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            if let Some(store) = handle.store_mut() {
                let recent = self.tab_recent.get(index).cloned().flatten();
                let mut session = recent
                    .as_ref()
                    .and_then(|id| store.auto_saved.iter().find(|held| &held.id == id))
                    .cloned()
                    .unwrap_or_else(|| {
                        // A recent identifier promoted from another tab now
                        // names a tree node; reusing it would shadow that node.
                        let id = recent
                            .filter(|id| store.find(id).is_none())
                            .unwrap_or_else(|| store.next_id());
                        SavedSession::new(id, &title, settings.kind())
                    });
                session.name = title;
                session.settings = self.stored_form(&settings);
                store.record_auto_saved(session);
            }
        }
    }

    /// Open the settings dialog over the active view.
    ///
    /// The Specs page of a tab that shows a saved session names the sides the
    /// stored session names; every other page shows what the view runs under.
    fn open_settings(&mut self, instance: u64) {
        let Some(mut settings) = self.tabs.get(self.active).and_then(|tab| tab.settings()) else {
            return;
        };
        let Ok(handle) = self.store.try_borrow() else {
            return;
        };
        let layers = handle
            .store()
            .map(|store| store.layers.clone())
            .unwrap_or_default();
        let stored = self
            .active_session()
            .and_then(|id| handle.store().and_then(|store| store.find_session(&id)))
            .cloned();
        drop(handle);
        if let Some(session) = stored {
            if let Ok(resolved) = layers.resolve(&session.kind, &session.settings) {
                set_sides(&mut settings, &sides_of(&resolved));
            }
        }
        self.settings_shown = Some(sides_of(&settings));
        let overrides = SessionSettingsOverride::from_full(&settings);
        self.settings = Some(SettingsDialog::new(
            settings.kind(),
            &overrides,
            &layers,
            instance,
        ));
        self.settings_tab = self.active;
    }

    /// Apply what the settings dialog produced.
    ///
    /// A side changed on the Specs page opens in the tab: the tab opens again
    /// over the sides the page names, as a session opens from the launcher.
    /// A tab that holds unwritten edits, runs work or reads temporary copies
    /// keeps its files, the stored session keeps the sides the page showed,
    /// and the window says why. A side left as the page showed it changes
    /// nothing, so the tab keeps the files it has open. The editing switch
    /// turns on only in a tab that holds no unwritten edit.
    fn apply_settings(&mut self, outcome: &SettingsOutcome, context: &ViewContext) {
        let index = self.settings_tab;
        let shown = self
            .settings_shown
            .take()
            .unwrap_or_else(|| sides_of(&outcome.settings));
        let mut settings = outcome.settings.clone();
        let mut kept: Option<&'static str> = None;
        if self.disables_editing_over_edits(index, &settings) {
            if let Some(specs) = settings.specs_mut() {
                specs.disable_editing = false;
            }
            kept = Some(EDITING_KEPT);
        }
        let mut reopened = false;
        if sides_of(&settings) != shown {
            match self.reopen_over(index, &settings, context, false) {
                Ok(()) => reopened = true,
                Err(reason) => {
                    set_sides(&mut settings, &shown);
                    kept = Some(reason);
                }
            }
        }
        let overrides = if kept.is_some() {
            self.override_of(&settings)
        } else {
            outcome.overrides.clone()
        };
        self.settings_notice = kept.map(str::to_owned);
        if !reopened {
            if let Some(tab) = self.tabs.get_mut(index) {
                let mut applied = settings.clone();
                if let Some(own) = tab.settings() {
                    set_sides(&mut applied, &sides_of(&own));
                }
                tab.apply_settings(&applied);
            }
        }
        let mut save = false;
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            if let Some(store) = handle.store_mut() {
                if outcome.scope == Scope::UpdateSessionDefaults {
                    let _ = store
                        .layers
                        .update_session_defaults_from(&settings.kind(), &settings);
                    save = true;
                }
                if let Some(id) = self.tab_sessions.get(index).cloned().flatten() {
                    let _ = store.update_session_settings(&id, overrides);
                    save = true;
                }
            }
        }
        if save {
            self.save_store();
        }
    }

    /// True when `settings` turns the editing switch on for the tab at
    /// `index` while the tab holds an unwritten edit, which the switch would
    /// then keep from being written.
    fn disables_editing_over_edits(&self, index: usize, settings: &SessionSettings) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        let turns_on = settings.specs().is_some_and(|specs| specs.disable_editing);
        let was_on = tab
            .settings()
            .is_some_and(|own| own.specs().is_some_and(|specs| specs.disable_editing));
        turns_on && !was_on && tab.holds_unwritten_edits()
    }

    /// What the window says about the last settings that did not all apply.
    #[must_use]
    pub fn settings_notice(&self) -> Option<&str> {
        self.settings_notice.as_deref()
    }

    fn draw_settings_notice(&mut self, ctx: &egui::Context) {
        let Some(text) = self.settings_notice.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Session Settings")
            .id(egui::Id::new("settings-notice"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -72.0])
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(text);
            });
        if !open {
            self.settings_notice = None;
        }
    }

    /// Open the tab at `index` again, in its place, over the sides
    /// `settings` names and under `settings`.
    ///
    /// # Errors
    /// Returns why the tab keeps its files: it reads temporary copies, holds
    /// unwritten edits or runs work, or this build has no view for the kind.
    fn reopen_over(
        &mut self,
        index: usize,
        settings: &SessionSettings,
        context: &ViewContext,
        allow_temporaries: bool,
    ) -> Result<(), &'static str> {
        let Some(tab) = self.tabs.get(index) else {
            return Err(NO_VIEW_FOR_SIDES);
        };
        if !allow_temporaries && tab.holds_temporaries() {
            return Err(SIDES_OF_TEMPORARIES);
        }
        if tab.is_busy() || tab.holds_unwritten_edits() {
            return Err(SIDES_KEPT);
        }
        let (left, right, center, output) = specs_of(settings).ok_or(SIDES_UNSUPPORTED)?;
        let request = OpenRequest::new(
            settings.kind(),
            left.unwrap_or_default(),
            right.unwrap_or_default(),
        )
        .with_center(center)
        .with_output(output);
        let instance = self.instance();
        let Some(mut view) = registry::open(&request, context, instance) else {
            return Err(NO_VIEW_FOR_SIDES);
        };
        view.apply_settings(settings);
        let Some(slot) = self.tabs.get_mut(index) else {
            return Err(NO_VIEW_FOR_SIDES);
        };
        let mut replaced = std::mem::replace(slot, view);
        replaced.on_close();
        if self
            .session_save
            .as_ref()
            .is_some_and(|dialog| dialog.tab == index)
        {
            self.session_save = None;
        }
        Ok(())
    }

    /// `settings` as the override a session stores: what differs from the
    /// session defaults of its kind.
    fn override_of(&self, settings: &SessionSettings) -> SessionSettingsOverride {
        let layers = self
            .store
            .try_borrow()
            .ok()
            .and_then(|handle| handle.store().map(|store| store.layers.clone()))
            .unwrap_or_default();
        ca_ui::settings::override_against(settings, &layers.resolve_defaults(&settings.kind()))
    }

    /// The form of a session's settings that is written to disk.
    ///
    /// A policy can forbid storing a password, so the stored form drops it
    /// while the open session keeps it.
    #[must_use]
    pub fn stored_form(&self, settings: &SessionSettings) -> SessionSettingsOverride {
        let overrides = SessionSettingsOverride::from_full(settings);
        if self.policies.saved_passwords_disabled() {
            return without_passwords(&overrides);
        }
        overrides
    }

    /// Say something that has nowhere else to appear.
    fn raise(&self, text: String) {
        if let Ok(mut slot) = self.startup_notice.lock() {
            *slot = Some(text);
        }
    }

    /// Raise a picker that names a package to write or to read.
    fn pick_package(&mut self, writes: bool, context: &ViewContext) {
        if self.share_picker.is_some() {
            return;
        }
        self.share_writes = writes;
        let pick = if writes {
            ca_ui::dialog::Pick::SaveFile
        } else {
            ca_ui::dialog::Pick::File
        };
        self.share_picker = Some(ca_ui::dialog::spawn(pick, Arc::clone(&context.notify)));
    }

    /// Take whatever the picker and the package worker posted.
    fn poll_share(&mut self) {
        let mut chosen = None;
        if let Some(job) = self.share_picker.as_mut() {
            let messages = job.drain();
            let finished = job.is_finished();
            for message in messages {
                match message {
                    DialogMessage::Chosen(path) => chosen = Some(path),
                    DialogMessage::Dismissed => {}
                    DialogMessage::Failed(reason) => self.raise(reason),
                }
            }
            if finished {
                self.share_picker = None;
            }
        }
        if let Some(path) = chosen {
            self.start_share(path);
        }
        let mut applied = None;
        if let Some(job) = self.share.as_mut() {
            let messages = job.drain();
            let finished = job.is_finished();
            for message in messages {
                applied = Some(message);
            }
            if finished {
                self.share = None;
            }
        }
        match applied {
            Some(ShareMessage::Exported { path, sessions }) => {
                self.raise(format!(
                    "{sessions} sessions were written to {}.",
                    path.display()
                ));
            }
            Some(ShareMessage::Imported {
                store,
                options,
                report,
            }) => {
                if let Ok(mut handle) = self.store.try_borrow_mut() {
                    if let Some(held) = handle.store_mut() {
                        *held = *store;
                    }
                }
                if let Some(options) = options {
                    self.apply_options(*options);
                }
                self.import_report = Some(ImportReportDialog::new(&report, self.next_instance));
                self.save_store();
            }
            Some(ShareMessage::Failed { store, reason }) => {
                if let (Some(store), Ok(mut handle)) = (store, self.store.try_borrow_mut()) {
                    if let Some(held) = handle.store_mut() {
                        *held = *store;
                    }
                }
                self.raise(reason);
            }
            None => {}
        }
    }

    /// Start writing or reading the package the picker named.
    fn start_share(&mut self, path: PathBuf) {
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        if self.share_writes {
            let Ok(handle) = self.store.try_borrow() else {
                return;
            };
            let Some(store) = handle.store() else {
                return;
            };
            let options = self.options.options_or_default();
            self.share = Some(ca_ui::share::spawn_export(
                path,
                store,
                &options,
                &ExportSelection::everything(),
                notify,
            ));
            return;
        }
        let taken = {
            let Ok(handle) = self.store.try_borrow_mut() else {
                return;
            };
            handle.store().cloned()
        };
        let Some(store) = taken else {
            return;
        };
        self.share = Some(ca_ui::share::spawn_import(
            path,
            Box::new(store),
            ImportOptions::default(),
            notify,
        ));
    }

    /// Read the journals once, on a worker, so an unfinished batch is reported
    /// whether or not a folder comparison is ever opened.
    ///
    /// The check reports; it removes nothing. What to do about a batch is
    /// decided in the folder view, which confirms first.
    fn start_recovery_check(&mut self, context: &ViewContext) {
        if self.recovery.is_some() {
            return;
        }
        self.recovery = Some(ca_view_folder::recovery_scan_in(
            self.journal_directory.clone(),
            Arc::clone(&context.notify),
        ));
    }

    /// Take whatever the journal check reported.
    fn poll_recovery(&mut self) {
        let Some(job) = self.recovery.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            if let RecoverMessage::Done { notices, .. } = message {
                if notices.is_empty() {
                    continue;
                }
                let unfinished: usize = notices.iter().map(|notice| notice.unfinished).sum();
                let mut text = format!(
                    "{} unfinished file operation batches were found, holding {unfinished} steps that never reported an end. Open a folder comparison to review them.",
                    notices.len()
                );
                // A batch whose files are gone is still listed, so the notice
                // says so rather than leaving the choice uninformed.
                let missing = notices.iter().filter(|notice| notice.files_missing).count();
                if missing > 0 {
                    let _ = write!(
                        text,
                        " For {missing} of them, {}.",
                        ca_view_folder::MISSING_FILES
                    );
                }
                if let Some(path) = notices.iter().find_map(|notice| notice.first_path.as_ref()) {
                    let _ = write!(text, " The first names {}.", path.display());
                }
                if let Ok(mut slot) = self.startup_notice.lock() {
                    *slot = Some(text);
                }
            }
        }
        if finished {
            self.recovery = None;
        }
    }

    /// Open the comparison a request names, if this build has a view for it.
    ///
    /// A kind with no view leaves the tabs as they are: the launcher only
    /// offers kinds the registry answers for, so this path is reached from a
    /// command line alone.
    pub fn open(&mut self, request: &OpenRequest, context: &ViewContext) {
        let instance = self.instance();
        if let Some(view) = registry::open(request, context, instance) {
            self.push(view);
        }
    }

    /// Open a comparison of `kind` over two paths.
    pub fn open_kind(
        &mut self,
        kind: &SessionKind,
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
    ) {
        self.open(&OpenRequest::new(kind.clone(), left, right), context);
    }

    /// Replace the active view with an empty view of the same kind under its
    /// session defaults. A running view or unwritten edit keeps its tab.
    fn clear_session(&mut self, context: &ViewContext) {
        let index = self.active;
        let Some(current) = self.tabs.get(index).and_then(|tab| tab.settings()) else {
            return;
        };
        let layers = self
            .store
            .try_borrow()
            .ok()
            .and_then(|handle| handle.store().map(|store| store.layers.clone()))
            .unwrap_or_default();
        let mut defaults = layers.resolve_defaults(&current.kind());
        set_sides(&mut defaults, &Sides::default());
        match self.reopen_over(index, &defaults, context, true) {
            Ok(()) => {
                if let Some(slot) = self.tab_sessions.get_mut(index) {
                    *slot = None;
                }
                self.tab_recent[index] = None;
                self.session_save = None;
                self.settings_notice = None;
            }
            Err(reason) => self.settings_notice = Some(reason.to_owned()),
        }
    }

    fn push(&mut self, view: Box<dyn SessionView>) {
        self.tabs.push(view);
        self.tab_sessions.push(None);
        self.tab_recent.push(None);
        self.active = self.tabs.len() - 1;
    }

    /// Close the tab at `index`, stopping whatever work it had running.
    pub fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        // Keep the view's latest result code even when the close is deferred.
        if let Some(tab) = self.tabs.get(index) {
            self.publish_exit(tab.as_ref());
        }
        if self.tabs.get_mut(index).is_some_and(|tab| !tab.may_close()) {
            self.active = index;
            return;
        }
        // The session a tab held is kept so it can be reopened from the
        // launcher without ever having been named.
        self.record_auto_saved(index);
        if self
            .session_save
            .as_ref()
            .is_some_and(|dialog| index <= dialog.tab)
        {
            self.session_save = None;
        }
        let mut view = self.tabs.remove(index);
        if index < self.tab_sessions.len() {
            self.tab_sessions.remove(index);
            self.tab_recent.remove(index);
        }
        view.on_close();
        if self.tabs.is_empty() {
            self.closing = self.resolved.stored.tabs.close_window_with_last_tab;
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    /// Why the shell refuses `command` this frame, where the reason is more
    /// specific than the one its menu line carries.
    #[must_use]
    pub fn refusal(&self, command: Command) -> Option<&'static str> {
        match command {
            Command::Explorer if self.active_holds_temporaries() => Some(TEMPORARIES),
            Command::SaveSession
            | Command::SaveSessionAs
            | Command::CompareFilesUsing
            | Command::CompareParentFolders
                if self.active_holds_temporaries() =>
            {
                Some(TEMPORARIES)
            }
            _ => None,
        }
    }

    /// True while the active tab reads copies made for it alone.
    fn active_holds_temporaries(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|tab| tab.holds_temporaries())
    }

    /// The active local item to show, after rejecting temporary and remote
    /// paths that a platform file manager cannot reveal.
    fn active_local_explorer_target(&self) -> Option<(PathBuf, ca_ui::launch::Selection)> {
        let tab = self.tabs.get(self.active)?;
        if !tab.is_ready() || tab.holds_temporaries() {
            return None;
        }
        let (path, selection) = tab.explorer_target()?;
        let settings = tab.settings()?;
        let specs = settings.specs()?;
        let matches_local_side =
            [&specs.left, &specs.right]
                .into_iter()
                .flatten()
                .any(|location| match location {
                    ca_session::SideLocation::Local { path: root, .. } => {
                        let root: &std::path::Path = root.as_ref();
                        path == root || path.starts_with(root)
                    }
                    _ => false,
                });
        matches_local_side.then_some((path, selection))
    }

    /// Parent paths of two local files on the active tab.
    fn parent_folder_paths(&self) -> Option<(PathBuf, PathBuf)> {
        let (left, right) = self.file_pair_paths()?;
        let parent = |path: PathBuf| path.parent().map(std::path::Path::to_path_buf);
        Some((parent(left)?, parent(right)?))
    }

    /// Local file paths on a ready two-file tab.
    fn file_pair_paths(&self) -> Option<(PathBuf, PathBuf)> {
        let tab = self.tabs.get(self.active)?;
        if !tab.is_ready() || tab.holds_temporaries() {
            return None;
        }
        let settings = tab.settings()?;
        if settings.kind().is_folder_kind() {
            return None;
        }
        let (left, right, _, _) = specs_of(&settings)?;
        Some((left?, right?))
    }

    /// File comparison kinds other than the active kind, in launcher order.
    fn other_file_comparison_kinds(&self) -> Vec<SessionKind> {
        let Some(tab) = self.tabs.get(self.active) else {
            return Vec::new();
        };
        let Some(settings) = tab.settings() else {
            return Vec::new();
        };
        let active_kind = settings.kind();
        SessionKind::ALL
            .iter()
            .filter(|kind| {
                !kind.is_folder_kind() && kind.side_count() == 2 && **kind != active_kind
            })
            .cloned()
            .collect()
    }

    /// Open the selected file comparison kind over the same two local files.
    fn open_files_using(&mut self, kind: &SessionKind, context: &ViewContext) {
        if !self.other_file_comparison_kinds().contains(kind) {
            return;
        }
        if let Some((left, right)) = self.file_pair_paths() {
            self.open_kind(kind, left, right, context);
        }
    }

    /// True when the active view can run `command`, or when the shell can.
    #[must_use]
    pub fn accepts(&self, command: Command) -> bool {
        let ready = self
            .store
            .try_borrow()
            .is_ok_and(|handle| handle.is_ready());
        let has_settings = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.settings().is_some());
        match command {
            Command::NewTab
            | Command::Exit
            | Command::NewSession
            | Command::OpenSession
            | Command::EditTextFile
            | Command::ViewPatch => true,
            Command::CloseTab | Command::ClearSession => !self.tabs.is_empty(),
            Command::LoadWorkspace
            | Command::SaveWorkspaceAs
            | Command::ExportSettings
            | Command::ImportSettings => ready,
            Command::Options | Command::RestoreFactoryDefaults => self.options.is_ready(),
            Command::Profiles => !self.policies.remote_profiles_disabled(),
            Command::CheckForUpdates => {
                self.update.url.is_some()
                    && self.update.job.is_none()
                    && !self.policies.check_for_updates_disabled()
            }
            Command::SaveSession | Command::SaveSessionAs => {
                ready && has_settings && !self.active_holds_temporaries()
            }
            Command::SessionSettings => has_settings,
            Command::ToggleLocked => ready && self.active_session().is_some(),
            Command::CompareFilesUsing => self.file_pair_paths().is_some(),
            Command::CompareParentFolders => ready && self.parent_folder_paths().is_some(),
            Command::Explorer => self.active_local_explorer_target().is_some(),
            other => self
                .tabs
                .get(self.active)
                .is_some_and(|tab| tab.accepts(other)),
        }
    }

    /// Run a command, at the shell when it owns it and in the active view
    /// otherwise.
    pub fn run(&mut self, command: Command, context: &ViewContext) {
        self.drop_pending_workspace();
        if command != Command::Exit {
            if let Some(profiles) = self.profiles.as_mut() {
                profiles.cancel_exit_request();
            }
        }
        match command {
            Command::NewTab | Command::NewSession | Command::OpenSession => self.open_home(context),
            Command::CloseTab => self.close_tab(self.active),
            Command::Exit => self.request_exit(),
            Command::SaveSession => self.save_session(),
            Command::SaveSessionAs => {
                self.request_session_save();
            }
            Command::SessionSettings => {
                let instance = self.instance();
                self.open_settings(instance);
            }
            Command::ToggleLocked => {
                if let Some(id) = self.active_session() {
                    let mut changed = false;
                    if let Ok(mut handle) = self.store.try_borrow_mut() {
                        if let Some(store) = handle.store_mut() {
                            let locked = store.is_locked(&id);
                            changed = store.set_locked(&id, !locked).is_ok();
                        }
                    }
                    if changed {
                        self.save_store();
                    }
                }
            }
            Command::ClearSession => self.clear_session(context),
            Command::SaveWorkspaceAs => {
                let instance = self.instance();
                self.workspaces = Some(WorkspaceManager::with_name(instance, "Workspace"));
            }
            Command::LoadWorkspace => {
                let instance = self.instance();
                self.workspaces = Some(WorkspaceManager::new(instance));
            }
            Command::Options => {
                let instance = self.instance();
                let mut shown = self.options.options_or_default();
                if let Some(state) = self.explorer.installed {
                    shown.startup.shell_integration = state.any();
                }
                self.options_dialog = Some(OptionsDialog::new(&shown, self.policies, instance));
            }
            Command::RestoreFactoryDefaults => {
                let instance = self.instance();
                self.restore = Some(RestoreDialog::new(instance));
            }
            Command::Profiles => {
                if !self.policies.remote_profiles_disabled() {
                    if let Some(profiles) = self.profiles.as_mut() {
                        profiles.bring_to_front();
                    } else {
                        self.profiles = Some(crate::profiles::ProfileManager::open(
                            &self.settings_directory,
                            Arc::clone(&context.notify),
                        ));
                    }
                }
            }
            Command::EditTextFile => {
                self.open_kind(
                    &SessionKind::TextEdit,
                    PathBuf::new(),
                    PathBuf::new(),
                    context,
                );
            }
            Command::ViewPatch => {
                self.open_kind(
                    &SessionKind::TextPatch,
                    PathBuf::new(),
                    PathBuf::new(),
                    context,
                );
            }
            Command::CheckForUpdates => self.spawn_update_check(0, true, &context.notify),
            Command::CompareFilesUsing => {}
            Command::Explorer => self.reveal_active_item(context),
            Command::CompareParentFolders => {
                if let Some((left, right)) = self.parent_folder_paths() {
                    self.open_kind(&SessionKind::FolderCompare, left, right, context);
                }
            }
            Command::ExportSettings => self.pick_package(true, context),
            Command::ImportSettings => self.pick_package(false, context),
            other => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if tab.accepts(other) {
                        tab.run(other);
                    }
                }
            }
        }
    }

    fn apply(&mut self, actions: Vec<ViewAction>, context: &ViewContext) {
        for action in actions {
            match action {
                ViewAction::Open(request) => self.open(&request, context),
                ViewAction::OpenHome => self.open_home(context),
                ViewAction::Close => {
                    self.drop_pending_workspace();
                    self.close_tab(self.active);
                }
            }
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context, context: &ViewContext) {
        let strokes: Vec<Keystroke> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some(Keystroke {
                        key: *key,
                        command: modifiers.command,
                        shift: modifiers.shift,
                        alt: modifiers.alt,
                    }),
                    _ => None,
                })
                .collect()
        });
        let view = self.menu_view();
        let table = self.resolved.shortcuts.clone();
        let escape_closes = self.resolved.stored.tweaks.escape_closes_file_views;
        for stroke in strokes {
            if escape_closes
                && stroke == Keystroke::plain(egui::Key::Escape)
                && view != MenuView::Folder
                && self.accepts(Command::CloseTab)
            {
                self.run(Command::CloseTab, context);
                continue;
            }
            if let Some(command) = command::route_with(view, stroke, &table) {
                if self.accepts(command) {
                    self.run(command, context);
                }
            }
        }
    }

    /// True when a menu line may be used this frame.
    ///
    /// The shell answers for the commands it owns; every other command is
    /// answered by whatever the active view declared.
    fn line_enabled(&self, command: Command) -> bool {
        if SHELL_OWNED.contains(&command) {
            return self.accepts(command);
        }
        self.declared
            .iter()
            .any(|state| state.command == command && state.enabled)
    }

    /// The action commands the active view declared, in declaration order.
    ///
    /// The menu is built from the declaration rather than from a fixed list, so
    /// a view type carries its own actions into the bar without the shell
    /// naming them.
    fn declared_actions(&self) -> Vec<CommandState> {
        self.declared
            .iter()
            .filter(|state| state.command.menu() == Menu::Actions)
            .copied()
            .collect()
    }

    /// Which bar the active tab carries.
    fn menu_view(&self) -> MenuView {
        self.tabs
            .get(self.active)
            .map_or(MenuView::Other, |tab| tab.menu_view())
    }

    /// The Actions menu, shown only while a view declares an action command.
    ///
    /// A bar that names its own Actions order carries it in the fixed tables
    /// instead, so this builds a menu only for the shared bar.
    fn actions_menu(&mut self, ui: &mut egui::Ui, context: &ViewContext) {
        let actions = self.declared_actions();
        if actions.is_empty() {
            return;
        }
        let view = self.menu_view();
        let mut chosen = None;
        ui.menu_button(ACTIONS_MENU, |ui| {
            ui.set_min_width(220.0);
            for state in &actions {
                if widgets::command_item_in(ui, view, state.command, state.enabled, ACTIONABLE) {
                    chosen = Some(state.command);
                }
            }
        });
        if let Some(command) = chosen {
            self.run(command, context);
        }
    }

    /// The Open With line, which carries the stored entries as a submenu.
    ///
    /// The line is disabled rather than absent when the active view names no
    /// files or the list is empty, so its place in the menu never moves.
    fn open_with_item(&self, ui: &mut egui::Ui, view: MenuView) -> Option<usize> {
        let entries = self.open_with_entries();
        let label = Command::OpenWith.label_in(view);
        if entries.is_empty() {
            widgets::command_item_in(ui, view, Command::OpenWith, false, NO_OPEN_WITH);
            return None;
        }
        let mut chosen = None;
        ui.menu_button(label, |ui| {
            ui.set_min_width(220.0);
            for (index, name) in entries {
                if ui.button(name).clicked() {
                    chosen = Some(index);
                    ui.close_menu();
                }
            }
        });
        chosen
    }

    /// The Compare Files Using line, which selects another two-file view.
    fn compare_files_using_item(&self, ui: &mut egui::Ui, view: MenuView) -> Option<SessionKind> {
        if self.file_pair_paths().is_none() {
            widgets::command_item_in(
                ui,
                view,
                Command::CompareFilesUsing,
                false,
                self.refusal(Command::CompareFilesUsing)
                    .unwrap_or(FILE_PAIR_REQUIRED),
            );
            return None;
        }
        let kinds = self.other_file_comparison_kinds();
        let mut chosen = None;
        ui.menu_button(Command::CompareFilesUsing.label_in(view), |ui| {
            ui.set_min_width(220.0);
            for kind in kinds {
                if ui
                    .add(
                        widgets::IconButton::new(
                            kind.title(),
                            Some(ca_ui::icons::session_icon(&kind)),
                        )
                        .menu(),
                    )
                    .clicked()
                {
                    chosen = Some(kind);
                    ui.close_menu();
                }
            }
        });
        chosen
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, context: &ViewContext) {
        let view = self.menu_view();
        let mut chosen_entry: Option<usize> = None;
        let mut chosen_file_kind = None;
        egui::menu::bar(ui, |ui| {
            for (name, entries) in menus_for(view) {
                ui.menu_button(*name, |ui| {
                    ui.set_min_width(260.0);
                    for entry in *entries {
                        match entry {
                            Entry::Rule => ui.separator(),
                            Entry::Soon(Command::OpenWith) => {
                                chosen_entry = self.open_with_item(ui, view);
                                continue;
                            }
                            Entry::Soon(Command::CompareFilesUsing) => {
                                chosen_file_kind = self.compare_files_using_item(ui, view);
                                continue;
                            }
                            Entry::Soon(Command::Explorer) => {
                                if widgets::command_item_in(
                                    ui,
                                    view,
                                    Command::Explorer,
                                    self.accepts(Command::Explorer),
                                    self.refusal(Command::Explorer).unwrap_or(NO_EXPLORER_ITEM),
                                ) {
                                    self.run(Command::Explorer, context);
                                }
                                continue;
                            }
                            Entry::Soon(command) => {
                                widgets::command_item_in(ui, view, *command, false, NOT_YET);
                                continue;
                            }
                            Entry::Run(command, reason) => {
                                let enabled = self.line_enabled(*command);
                                let reason: &str = self.refusal(*command).unwrap_or(reason);
                                if widgets::command_item_in(ui, view, *command, enabled, reason) {
                                    self.run(*command, context);
                                }
                                continue;
                            }
                        };
                    }
                    if *name == "Help" {
                        ui.separator();
                        ui.label(crate::product_title());
                    }
                });
                if matches!(view, MenuView::Other | MenuView::Merge) && *name == ACTIONS_AFTER {
                    self.actions_menu(ui, context);
                }
            }
        });
        if let Some(index) = chosen_entry {
            self.start_open_with(index, context);
        }
        if let Some(kind) = chosen_file_kind {
            self.open_files_using(&kind, context);
        }
    }

    /// True when the tab strip is drawn this frame.
    #[must_use]
    pub fn shows_tab_strip(&self) -> bool {
        self.tabs.len() >= 2 || self.resolved.stored.tabs.show_tab_strip_with_one_tab
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        if !self.shows_tab_strip() {
            return;
        }
        let mut close: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            for index in 0..self.tabs.len() {
                let title = self.tabs[index].title();
                let selected = index == self.active;
                let icon = if self.tabs[index].is_launcher() {
                    Some(ca_ui::icons::Icon::Home)
                } else {
                    self.tabs[index]
                        .kind()
                        .as_ref()
                        .map(ca_ui::icons::session_icon)
                };
                let mut label = ui.add(
                    widgets::IconButton::new(&title, icon)
                        .inline()
                        .selected(selected),
                );
                if let Some(description) = self.tab_description(index) {
                    label = label.on_hover_text(description);
                }
                if label.clicked() {
                    self.active = index;
                    self.drop_pending_workspace();
                }
                if widgets::icon_only(ui, "Close Tab", ca_ui::icons::Icon::Close, None).clicked() {
                    close = Some(index);
                }
                ui.separator();
            }
        });
        if let Some(index) = close {
            self.drop_pending_workspace();
            self.close_tab(index);
        }
    }

    /// Take what the launcher asked for and act on it.
    fn apply_home_actions(&mut self, context: &ViewContext) {
        let asked: Vec<HomeAction> = self.home_outbox.borrow_mut().drain(..).collect();
        for action in asked {
            match action {
                HomeAction::OpenSaved(id) => {
                    self.from_launcher(context, |app, context| app.open_saved(&id, context));
                }
                HomeAction::NewSession(kind) => {
                    self.from_launcher(context, |app, context| {
                        app.open_kind(&kind, PathBuf::new(), PathBuf::new(), context);
                    });
                }
                HomeAction::Save => self.save_store(),
            }
        }
    }

    /// Open something the launcher asked for and place it as the Tabs page says.
    pub fn from_launcher(
        &mut self,
        context: &ViewContext,
        open: impl FnOnce(&mut Self, &ViewContext),
    ) {
        let launcher = self.active;
        let before = self.tabs.len();
        open(self, context);
        if self.tabs.len() > before {
            self.place_new_session(launcher);
        }
    }

    /// Drop the launcher the session was started from, where the Tabs page
    /// asks for the session to take its place.
    fn place_new_session(&mut self, launcher: usize) {
        if self.resolved.stored.tabs.new_session_placement.id() != "reuseHomeTab" {
            return;
        }
        if !self.tabs.get(launcher).is_some_and(|tab| tab.is_launcher()) {
            return;
        }
        let mut view = self.tabs.remove(launcher);
        self.tab_sessions.remove(launcher);
        self.tab_recent.remove(launcher);
        self.session_save = None;
        view.on_close();
        self.active = self.tabs.len().saturating_sub(1);
    }

    /// The naming dialog owns input until it closes or completes its save.
    fn draw_session_save(&mut self, ctx: &egui::Context) {
        if let Some(mut dialog) = self.session_save.take() {
            let save = if let Ok(handle) = self.store.try_borrow() {
                dialog.show(ctx, handle.store())
            } else {
                dialog.show(ctx, None)
            };
            if save {
                match self.save_session_as(dialog.tab, &dialog.name, dialog.parent.as_ref()) {
                    Ok(()) => dialog.open = false,
                    Err(error) => dialog.error = Some(error),
                }
            }
            if dialog.open {
                self.session_save = Some(dialog);
            }
        }
    }

    /// Draw whichever dialogs are open and act on what they produced.
    fn draw_dialogs(&mut self, ctx: &egui::Context, context: &ViewContext) {
        let palette = context.palette;
        let mut options_outcome = None;
        if let Some(dialog) = self.options_dialog.as_mut() {
            options_outcome = dialog.show(ctx, &palette);
        }
        if let Some(produced) = options_outcome {
            let wanted = produced.options.startup.shell_integration;
            self.apply_options(*produced.options);
            self.sync_explorer_menu(wanted, &context.notify);
            if produced.closed {
                self.options_dialog = None;
            }
        }
        let mut restore_outcome = None;
        if let Some(dialog) = self.restore.as_mut() {
            restore_outcome = dialog.show(ctx);
            if !dialog.is_open() && restore_outcome.is_none() {
                self.restore = None;
            }
        }
        if let Some(produced) = restore_outcome {
            self.apply_restore(&produced.selection);
            self.restore = None;
        }
        let mut outcome = None;
        if let Some(dialog) = self.settings.as_mut() {
            outcome = dialog.show(ctx, &palette);
            if !dialog.is_open() && outcome.is_none() {
                self.settings = None;
            }
        }
        if let Some(outcome) = outcome {
            self.apply_settings(&outcome, context);
            self.settings = None;
        }
        let mut asked = None;
        if let Some(manager) = self.workspaces.as_mut() {
            if let Ok(handle) = self.store.try_borrow() {
                if let Some(store) = handle.store() {
                    asked = manager.show(ctx, store);
                }
            }
            if !manager.is_open() {
                self.workspaces = None;
            }
        }
        if let Some(action) = asked {
            self.apply_workspace(&action, context);
        }
        if let Some(report) = self.import_report.as_mut() {
            report.show(ctx);
            if !report.is_open() {
                self.import_report = None;
            }
        }
        let mut profiles_closed = false;
        let mut profiles_approved_exit = false;
        if let Some(manager) = self.profiles.as_mut() {
            profiles_closed = manager.show(ctx);
            profiles_approved_exit = manager.take_exit_allowed();
        }
        if profiles_closed {
            self.profiles = None;
        }
        if profiles_approved_exit {
            self.try_leave();
        }
        self.exit_question_panel(ctx);
    }

    /// The question raised before a window holding several tabs closes.
    fn exit_question_panel(&mut self, ctx: &egui::Context) {
        if !self.exit_question {
            return;
        }
        let mut answer = None;
        egui::Window::new("Close every tab?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                widgets::notice_current(
                    ui,
                    ca_ui::icons::Icon::Question,
                    24.0,
                    &format!("{} tabs are open.", self.tabs.len()),
                );
                ui.horizontal(|ui| {
                    if ui.button("Close them").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Keep them open").clicked() {
                        answer = Some(false);
                    }
                });
            });
        if let Some(leave) = answer {
            self.answer_exit(leave);
        }
    }

    /// Act on what the workspace manager asked for.
    pub fn apply_workspace(&mut self, action: &WorkspaceAction, context: &ViewContext) {
        match action {
            WorkspaceAction::Load(name) => {
                self.load_workspace(name, context);
                self.workspaces = None;
            }
            WorkspaceAction::Save(name) => {
                let workspace = self.capture_workspace(name);
                if let Ok(mut handle) = self.store.try_borrow_mut() {
                    if let Some(store) = handle.store_mut() {
                        store.save_workspace(workspace);
                        store.last_workspace = Some(name.clone());
                    }
                }
                self.save_store();
            }
            WorkspaceAction::Rename { from, to } => {
                let mut failed = None;
                if let Ok(mut handle) = self.store.try_borrow_mut() {
                    if let Some(store) = handle.store_mut() {
                        if let Err(error) = store.rename_workspace(from, to.clone()) {
                            failed = Some(error.to_string());
                        }
                    }
                }
                match failed {
                    Some(reason) => self.raise(reason),
                    None => self.save_store(),
                }
            }
            WorkspaceAction::Delete(name) => {
                if let Ok(mut handle) = self.store.try_borrow_mut() {
                    if let Some(store) = handle.store_mut() {
                        store.delete_workspace(name);
                    }
                }
                self.save_store();
            }
            WorkspaceAction::RestoreAtStart(name) => {
                if let Ok(mut handle) = self.store.try_borrow_mut() {
                    if let Some(store) = handle.store_mut() {
                        store.restore_last_workspace = name.is_some();
                        if let Some(name) = name {
                            store.last_workspace = Some(name.clone());
                        }
                    }
                }
                self.save_store();
            }
        }
    }

    /// Take what the views recorded about their last report and store it.
    ///
    /// A view cannot reach the options document, so it leaves a record and the
    /// shell, which owns the document, writes it.
    fn store_report_settings(&mut self) {
        let records = ca_ui::report::take_records();
        if records.is_empty() {
            return;
        }
        let Some(options) = self.options.options_mut() else {
            return;
        };
        for (view, preference) in records {
            options.reports.set_view(&view, preference);
        }
        self.options.save();
    }

    /// Paint one frame into `ctx`.
    ///
    /// Split out from the eframe entry point so a frame can be run headless.
    pub fn frame(&mut self, ctx: &egui::Context) {
        ca_ui::theme::chrome::install(ctx);
        let repaint = ctx.clone();
        self.options.poll();
        let resolved = Arc::new(self.resolve_options(ctx));
        ca_ui::options::install(ctx, Arc::clone(&resolved));
        let context = ViewContext {
            options: Arc::clone(&resolved),
            palette: resolved.tables.main,
            notify: Arc::new(move || repaint.request_repaint()),
        };
        self.resolved = resolved;
        self.poll_recovery();
        self.poll_store();
        self.poll_launches();
        self.start_update_check(&context.notify);
        self.poll_update();
        self.start_explorer_read(&context.notify);
        self.poll_explorer();
        ca_ui::format::probe_offset(&context.notify);
        self.store_report_settings();
        self.restore_workspace(&context);
        if !self.restored && self.startup_wait_since.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        // Every tab, not only the one on screen: a background tab that never
        // drained its queues would grow one and never see its work finish.
        let active = self.active;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            tab.set_active(index == active);
            tab.tick();
        }
        self.declared = self
            .tabs
            .get(self.active)
            .map(|tab| tab.commands())
            .unwrap_or_default();
        let naming = self.session_save.is_some();
        if naming {
            self.draw_session_save(ctx);
        } else {
            self.keyboard(ctx, &context);
        }
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            if naming {
                ui.disable();
            }
            self.menu_bar(ui, &context);
            self.tab_strip(ui);
        });
        let mut actions = Vec::new();
        egui::CentralPanel::default().show(ctx, |ui| {
            if naming {
                ui.disable();
            }
            if let Some(tab) = self.tabs.get_mut(self.active) {
                actions = tab.ui(ui, &context);
            } else {
                ui.label("No session is open");
            }
        });
        if !naming {
            self.apply(actions, &context);
            self.apply_home_actions(&context);
            self.draw_dialogs(ctx, &context);
        }
        self.draw_update_notice(ctx, &context);
        self.draw_workspace_notice(ctx);
        self.draw_settings_notice(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.closing {
            self.request_exit();
            if !self.closing {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
        let wants_close: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.wants_close())
            .map(|(index, _)| index)
            .collect();
        let closed = !wants_close.is_empty();
        for index in wants_close.into_iter().rev() {
            self.close_tab(index);
        }
        let work_ended = self
            .pending_busy_tab
            .is_some_and(|index| self.tabs.get(index).is_none_or(|tab| !tab.is_busy()));
        if closed || work_ended {
            if let Some(name) = self.pending_workspace.take() {
                self.load_workspace(&name, &context);
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let _ = frame;
        self.frame(ctx);
        if self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// The key a stored side carries its password under.
const PASSWORD_KEY: &str = "password";

/// Drops every password from settings about to be written.
///
/// A policy can forbid writing one. The value stays in memory for the open
/// session; only the stored copy loses it.
#[must_use]
pub fn without_passwords(overrides: &SessionSettingsOverride) -> SessionSettingsOverride {
    let Ok(mut value) = serde_json::to_value(overrides) else {
        return overrides.clone();
    };
    strip_passwords(&mut value);
    serde_json::from_value(value).unwrap_or_else(|_| overrides.clone())
}

fn strip_passwords(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.remove(PASSWORD_KEY);
            for child in object.values_mut() {
                strip_passwords(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                strip_passwords(item);
            }
        }
        _ => {}
    }
}

/// The sides a session's settings name: left, right, center and output.
type Sides = [Option<ca_session::SideLocation>; 4];
type ResolvedSpecs = (
    Option<PathBuf>,
    Option<PathBuf>,
    Option<PathBuf>,
    Option<PathBuf>,
);

/// The sides `settings` names, or none for a kind this build does not
/// understand.
fn sides_of(settings: &SessionSettings) -> Sides {
    settings.specs().map_or_else(Sides::default, |specs| {
        [
            specs.left.clone(),
            specs.right.clone(),
            specs.ancestor.clone(),
            specs.output.clone(),
        ]
    })
}

/// Give `settings` the sides `sides` names.
fn set_sides(settings: &mut SessionSettings, sides: &Sides) {
    if let Some(specs) = settings.specs_mut() {
        let [left, right, ancestor, output] = sides.clone();
        specs.left = left;
        specs.right = right;
        specs.ancestor = ancestor;
        specs.output = output;
    }
}

/// The four sides a session's settings name, as paths a view can open.
///
/// Locations this view cannot represent are rejected. In particular, do not
/// turn a non-local side into an empty path and replace a working tab with a
/// view of different sides.
fn specs_of(settings: &SessionSettings) -> Option<ResolvedSpecs> {
    let specs = settings.specs()?;
    let folder_view = matches!(
        settings.kind(),
        SessionKind::FolderCompare | SessionKind::FolderMerge | SessionKind::FolderSync
    );
    let path = |side: Option<&ca_session::SideLocation>| -> Option<Option<PathBuf>> {
        match side {
            None => Some(None),
            Some(ca_session::SideLocation::Local { path, .. }) => Some(Some(path.to_path_buf())),
            Some(ca_session::SideLocation::Snapshot { path, .. }) if folder_view => {
                Some(Some(path.to_path_buf()))
            }
            Some(ca_session::SideLocation::Archive { archive, inner, .. })
                if folder_view && inner.is_empty() =>
            {
                Some(Some(archive.to_path_buf()))
            }
            _ => None,
        }
    };
    Some((
        path(specs.left.as_ref())?,
        path(specs.right.as_ref())?,
        path(specs.ancestor.as_ref())?,
        path(specs.output.as_ref())?,
    ))
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{menus_for, specs_of, App, Entry, ACTIONS_MENU, NOT_YET, SIDES_UNSUPPORTED};
    use crate::cli::Startup;
    use crate::registry;
    use ca_session::options::LaunchCommand;
    use ca_session::{SavedSession, SessionId, SessionKind};
    use ca_ui::command::{Command, Menu, MenuView};
    use ca_ui::launch::Spawner;
    use ca_ui::testing::{context, raw_input};
    use ca_ui::view::{OpenRequest, SessionView, ViewAction, ViewContext};
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct LaunchRecorder(Mutex<Vec<LaunchCommand>>);

    impl Spawner for LaunchRecorder {
        fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
            self.0.lock().unwrap().push(command.clone());
            Ok(())
        }
    }

    struct UnsavedView {
        close_checks: Rc<Cell<u32>>,
    }

    struct TemporaryView;

    impl SessionView for TemporaryView {
        fn title(&self) -> String {
            "Clipboard".to_owned()
        }

        fn ui(&mut self, _ui: &mut egui::Ui, _context: &ViewContext) -> Vec<ViewAction> {
            Vec::new()
        }

        fn holds_temporaries(&self) -> bool {
            true
        }
    }

    impl SessionView for UnsavedView {
        fn title(&self) -> String {
            "Unsaved edit".to_owned()
        }

        fn ui(&mut self, _ui: &mut egui::Ui, _context: &ViewContext) -> Vec<ViewAction> {
            Vec::new()
        }

        fn may_close(&mut self) -> bool {
            self.close_checks.set(self.close_checks.get() + 1);
            false
        }
    }

    /// A tab that refuses to close and asks its question, and then closes by
    /// itself once `answered` is set, as a view does after Save or Discard.
    struct AskingView {
        asked: Rc<Cell<u32>>,
        answered: Rc<Cell<bool>>,
    }

    impl SessionView for AskingView {
        fn title(&self) -> String {
            "Asking".to_owned()
        }

        fn ui(&mut self, _ui: &mut egui::Ui, _context: &ViewContext) -> Vec<ViewAction> {
            Vec::new()
        }

        fn may_close(&mut self) -> bool {
            self.asked.set(self.asked.get() + 1);
            self.answered.get()
        }

        fn wants_close(&self) -> bool {
            self.answered.get()
        }
    }

    /// A window with one asking tab and a stored workspace of two launchers.
    fn asking_app() -> (tempfile::TempDir, App, Rc<Cell<u32>>, Rc<Cell<bool>>) {
        let (settings, mut app) = app_with_two_launchers();
        let asked = Rc::new(Cell::new(0));
        let answered = Rc::new(Cell::new(false));
        app.push(Box::new(AskingView {
            asked: Rc::clone(&asked),
            answered: Rc::clone(&answered),
        }));
        (settings, app, asked, answered)
    }

    /// A window with no tab and a stored workspace named `two` of two
    /// launchers.
    fn app_with_two_launchers() -> (tempfile::TempDir, App) {
        let (settings, mut app) = empty_app();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store().borrow().is_ready()
            }
        ));
        {
            let store = app.store();
            let mut handle = store.borrow_mut();
            handle
                .store_mut()
                .unwrap()
                .save_workspace(ca_ui::workspace::one_window(
                    "two",
                    vec![
                        ca_session::WorkspaceTab::home(),
                        ca_session::WorkspaceTab::home(),
                    ],
                    0,
                ));
        }
        (settings, app)
    }

    /// A named startup workspace is retried when the document is temporarily
    /// with a worker or borrowed elsewhere, rather than consumed unread.
    #[test]
    fn named_startup_waits_until_the_workspace_document_is_available() {
        let (_settings, mut app) = app_with_two_launchers();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.options.poll();
                app.options.is_ready()
            }
        ));
        Arc::make_mut(&mut app.resolved)
            .stored
            .startup
            .load_workspace = "two".to_owned();
        let store = Rc::clone(app.store());
        app.startup_wait_since =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(2));
        {
            let _borrow = store.borrow_mut();
            app.restore_workspace(&context());
            assert!(
                !app.restored,
                "an unavailable document consumed restoration"
            );
            assert_eq!(app.tab_count(), 0);
            assert!(app.workspace_notice().unwrap().contains("Waiting"));
            assert!(app.workspace_notice().unwrap().contains("two"));
        }
        store.borrow_mut().save();
        app.restore_workspace(&context());
        assert!(
            !app.restored,
            "a document with the worker consumed restoration"
        );
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                store.borrow().is_ready()
            }
        ));
        app.restore_workspace(&context());
        assert!(app.restored);
        assert_eq!(app.tab_count(), 2);
        assert!(app.workspace_notice().is_none());
        app.restore_workspace(&context());
        assert_eq!(app.tab_count(), 2);
    }

    /// A tab that refuses to close while its work runs and asks nothing, as a
    /// folder view does while a batch writes files.
    struct BusyView {
        busy: Rc<Cell<bool>>,
    }

    impl SessionView for BusyView {
        fn title(&self) -> String {
            "Busy".to_owned()
        }

        fn ui(&mut self, _ui: &mut egui::Ui, _context: &ViewContext) -> Vec<ViewAction> {
            Vec::new()
        }

        fn may_close(&mut self) -> bool {
            !self.busy.get()
        }

        fn is_busy(&self) -> bool {
            self.busy.get()
        }
    }

    /// A load that waits for a tab whose work runs says so, and runs once the
    /// work ends, with no question asked and no other command given.
    #[test]
    fn a_workspace_load_waits_for_the_work_of_a_tab_and_runs_once_it_ends() {
        let (_settings, mut app) = app_with_two_launchers();
        let busy = Rc::new(Cell::new(true));
        app.push(Box::new(BusyView {
            busy: Rc::clone(&busy),
        }));
        let context = context();
        app.load_workspace("two", &context);
        assert_eq!(app.tab_count(), 1);
        let waiting = app.workspace_notice().unwrap().to_owned();
        assert!(
            waiting.contains("two")
                && waiting.contains("Busy")
                && waiting.contains("work in the tab")
                && !waiting.contains("question"),
            "{waiting}"
        );
        let _ = egui::Context::default().run(raw_input(), |ctx| app.frame(ctx));
        assert_eq!(app.tab_count(), 1, "the load ran while the work ran");

        busy.set(false);
        let _ = egui::Context::default().run(raw_input(), |ctx| app.frame(ctx));
        assert_eq!(app.tab_count(), 2, "the workspace did not load");
        assert_eq!(app.active_title().as_deref(), Some("Home"));
        assert_eq!(app.workspace_notice(), None);
    }

    #[test]
    fn a_workspace_load_waits_for_a_tab_that_asked_and_runs_once_it_closes() {
        let (_settings, mut app, asked, answered) = asking_app();
        let context = context();
        app.load_workspace("two", &context);
        assert_eq!(asked.get(), 1);
        assert_eq!(app.tab_count(), 1);
        assert_eq!(app.active_title().as_deref(), Some("Asking"));

        answered.set(true);
        let _ = egui::Context::default().run(raw_input(), |ctx| app.frame(ctx));
        assert_eq!(app.tab_count(), 2, "the workspace did not load");
        assert_eq!(app.active_title().as_deref(), Some("Home"));
        assert!(!app.is_closing());
    }

    #[test]
    fn a_workspace_load_is_dropped_when_the_window_is_used_for_something_else() {
        let (_settings, mut app, asked, answered) = asking_app();
        let context = context();
        app.load_workspace("two", &context);
        assert_eq!(asked.get(), 1);
        app.run(Command::NewTab, &context);
        assert_eq!(app.tab_count(), 2);

        answered.set(true);
        let _ = egui::Context::default().run(raw_input(), |ctx| app.frame(ctx));
        assert_eq!(app.tab_count(), 1, "the dropped workspace load ran");
        assert_eq!(app.active_title().as_deref(), Some("Home"));
    }

    #[test]
    fn a_frame_makes_the_zone_offset_known() {
        let (_settings, mut app) = empty_app();
        let _ = egui::Context::default().run(raw_input(), |ctx| app.frame(ctx));
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(5),
            || ca_ui::format::probed_offset().is_some()
        ));
    }

    /// The window close button pressed while a load waits ends the wait: the
    /// load never runs while the window closes.
    #[test]
    fn a_workspace_load_never_runs_once_the_window_is_closing() {
        let (_settings, mut app, asked, answered) = asking_app();
        let context = context();
        app.load_workspace("two", &context);
        assert_eq!(asked.get(), 1);

        let mut input = raw_input();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let _ = egui::Context::default().run(input, |ctx| app.frame(ctx));
        assert_eq!(asked.get(), 2, "the exit asked the tab");
        assert!(!app.is_closing());

        answered.set(true);
        let _ = egui::Context::default().run(raw_input(), |ctx| app.frame(ctx));
        assert_eq!(app.tab_count(), 0, "the workspace loaded during the exit");
        assert!(app.is_closing());
    }

    /// A load that waits names the tab it waits for, a load the window drops
    /// says it did not load, and a load that runs clears what was said.
    #[test]
    fn a_workspace_load_says_when_it_waits_and_when_it_is_dropped() {
        let (_settings, mut app, _asked, answered) = asking_app();
        let context = context();
        assert_eq!(app.workspace_notice(), None);
        app.load_workspace("two", &context);
        let waiting = app.workspace_notice().unwrap().to_owned();
        assert!(
            waiting.contains("two") && waiting.contains("Asking"),
            "{waiting}"
        );
        app.run(Command::NewTab, &context);
        let dropped = app.workspace_notice().unwrap();
        assert!(dropped.contains("not loaded"), "{dropped}");

        app.run(Command::CloseTab, &context);
        answered.set(true);
        app.load_workspace("two", &context);
        assert_eq!(app.tab_count(), 2);
        assert_eq!(app.workspace_notice(), None);
    }

    /// The window close button drops a load that waits, and says so.
    #[test]
    fn an_exit_request_says_that_the_waiting_load_was_dropped() {
        let (_settings, mut app, _asked, _answered) = asking_app();
        let context = context();
        app.load_workspace("two", &context);
        let mut input = raw_input();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let _ = egui::Context::default().run(input, |ctx| app.frame(ctx));
        let dropped = app.workspace_notice().unwrap();
        assert!(dropped.contains("not loaded"), "{dropped}");
    }

    /// The exit waits for the saves in flight within one bound in all: the
    /// waits of the exit and the waits of the two documents as they close
    /// share it.
    #[test]
    fn the_exit_waits_for_saves_in_flight_within_one_bound() {
        let (_settings, mut app) = empty_app();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.options.poll();
                app.store().borrow().is_ready() && app.options.is_ready()
            }
        ));
        {
            let store = Rc::clone(app.store());
            let mut handle = store.borrow_mut();
            ca_ui::testing::stall_saves(&mut handle, &mut app.options);
        }
        let limit = std::time::Duration::from_millis(200);
        let started = std::time::Instant::now();
        app.leave_within(limit);
        drop(app);
        let spent = started.elapsed();
        assert!(
            spent < limit + std::time::Duration::from_secs(1),
            "the exit waited {spent:?}"
        );
    }

    /// A settings directory this test alone reads and writes.
    ///
    /// Tests of one binary run in parallel threads and the test binaries run
    /// one after another over the same folder. A shared settings directory
    /// therefore lets one test's claim on it, or one test's stored document,
    /// decide what another test sees. The returned directory is removed when
    /// the value is dropped, so it is bound beside the window that reads it.
    fn settings() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// The option reads the installed menu and changes the user keys on a
    /// worker. Only a throwaway key under `HKEY_CURRENT_USER` is touched.
    #[cfg(windows)]
    #[test]
    fn the_explorer_option_adds_and_removes_the_menu_under_its_root() {
        use winreg::enums::HKEY_CURRENT_USER;
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = winreg::RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
            }
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let key = format!(
            r"Software\compare-all-tests\shell-option-{}-{nanos}",
            std::process::id()
        );
        let _cleanup = Cleanup(key.clone());
        let notify: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {});
        let (_settings, mut app) = empty_app();
        app.manage_explorer_menu_at(crate::explorer::MenuRoot {
            user: key,
            machine: None,
        });
        let settle = |app: &mut App| {
            assert!(ca_ui::testing::wait_until(
                std::time::Duration::from_secs(20),
                || {
                    app.poll_explorer();
                    !app.explorer_menu_busy()
                }
            ));
        };
        app.start_explorer_read(&notify);
        settle(&mut app);
        assert_eq!(
            app.explorer_menu_state()
                .map(crate::explorer::Installed::any),
            Some(false)
        );
        app.sync_explorer_menu(true, &notify);
        settle(&mut app);
        let state = app.explorer_menu_state().unwrap();
        assert!(state.user && !state.machine);
        app.sync_explorer_menu(false, &notify);
        settle(&mut app);
        assert_eq!(
            app.explorer_menu_state()
                .map(crate::explorer::Installed::any),
            Some(false)
        );
    }

    #[test]
    fn menu_and_update_jobs_stop_counting_as_busy_in_the_poll_that_delivers_their_answer() {
        let (_settings, mut app) = empty_app();
        let (explorer, _explorer_held) =
            ca_ui::testing::job_held_after(vec![crate::explorer::Message::Cancelled]);
        app.explorer.job = Some(explorer);
        let (update, _update_held) =
            ca_ui::testing::job_held_after(vec![crate::update::Outcome::Current]);
        app.update.job = Some(update);
        assert!(app.explorer_menu_busy());
        assert!(app.is_checking_for_updates());

        app.poll_explorer();
        app.poll_update();

        assert!(
            !app.explorer_menu_busy(),
            "the menu reads as busy after its worker answered"
        );
        assert!(
            !app.is_checking_for_updates(),
            "the update check reads as running after it answered"
        );
    }

    /// A window holding nothing, over a settings directory of its own.
    fn empty_app() -> (tempfile::TempDir, App) {
        let dir = settings();
        let app = App::in_directories(dir.path().to_path_buf(), dir.path().join("journals"));
        (dir, app)
    }

    /// A window built from `startup`, over a settings directory of its own.
    fn started_app(startup: Startup, context: &ViewContext) -> (tempfile::TempDir, App) {
        let dir = settings();
        let app = App::from_startup_in(
            startup,
            context,
            dir.path().to_path_buf(),
            dir.path().join("journals"),
        );
        (dir, app)
    }

    #[test]
    fn a_new_window_holds_nothing() {
        let (_settings, app) = empty_app();
        assert_eq!(app.tab_count(), 0);
        assert!(app.active_title().is_none());
    }

    fn app_with_recent_session() -> (tempfile::TempDir, App, SessionId, PathBuf, PathBuf) {
        let (directory, app) = app_with_two_launchers();
        let left = directory.path().join("recent-left.txt");
        let right = directory.path().join("recent-right.txt");
        std::fs::write(&left, "old\n").unwrap();
        std::fs::write(&right, "new\n").unwrap();
        let id = {
            let store = app.store();
            let mut handle = store.borrow_mut();
            let store = handle.store_mut().unwrap();
            let id = store.next_id();
            let mut recent =
                ca_session::SavedSession::new(id.clone(), "recent", SessionKind::TextCompare);
            recent.settings = serde_json::from_value(serde_json::json!({
                "kind": "text-compare",
                "specs": {
                    "left": { "type": "local", "path": left },
                    "right": { "type": "local", "path": right }
                }
            }))
            .unwrap();
            store.record_auto_saved(recent);
            assert!(store.find_session(&id).is_none());
            id
        };
        (directory, app, id, left, right)
    }

    #[test]
    fn a_recent_session_opens_its_view_without_a_named_session_binding() {
        let (_directory, mut app, id, left, right) = app_with_recent_session();
        app.open_saved(&id, &context());
        assert_eq!(app.tab_count(), 1);
        let settings = app.tabs[0].settings().unwrap();
        assert_eq!(settings.kind(), SessionKind::TextCompare);
        let paths = specs_of(&settings).unwrap();
        assert_eq!(paths.0, Some(left));
        assert_eq!(paths.1, Some(right));
        assert_eq!(app.tab_sessions, vec![None]);
        assert!(matches!(
            app.capture_workspace("recent").windows[0].tabs[0],
            ca_session::WorkspaceTab::Unsaved { .. }
        ));
    }

    #[test]
    fn reopening_and_closing_a_recent_session_does_not_duplicate_or_evict_history() {
        let (_directory, mut app, id, _, _) = app_with_recent_session();
        let older = {
            let mut handle = app.store.borrow_mut();
            let store = handle.store_mut().unwrap();
            store.set_max_auto_saved(2);
            let older = store.next_id();
            store.record_auto_saved(SavedSession::new(
                older.clone(),
                "older",
                SessionKind::TextCompare,
            ));
            older
        };
        for _ in 0..3 {
            app.open_saved(&id, &context());
            app.close_tab(0);
            assert_eq!(app.tab_count(), 0);
            let handle = app.store.borrow();
            let store = handle.store().unwrap();
            assert_eq!(store.auto_saved.len(), 2);
            assert_eq!(store.auto_saved[0].id, id);
            assert_eq!(store.auto_saved[1].id, older);
        }
    }

    #[test]
    fn session_naming_owns_shortcuts_until_cancellation_finishes() {
        let (_directory, mut app, id, _, _) = app_with_recent_session();
        app.open_saved(&id, &context());
        let ctx = egui::Context::default();
        let frame = |app: &mut App, events| {
            let _ = ctx.run(ca_ui::testing::event_input(1280.0, 800.0, events), |ctx| {
                app.frame(ctx);
            });
        };
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                frame(&mut app, Vec::new());
                app.active_is_ready()
            }
        ));
        app.run(Command::SaveSessionAs, &context());
        frame(&mut app, Vec::new());
        let shortcut = egui::Event::Key {
            key: egui::Key::W,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                command: true,
                ..egui::Modifiers::NONE
            },
        };
        frame(&mut app, vec![shortcut.clone()]);
        assert_eq!(app.tab_count(), 1);
        assert!(app.session_save.is_some());
        frame(
            &mut app,
            vec![
                egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                shortcut,
            ],
        );
        assert!(app.session_save.is_none());
        assert_eq!(app.tab_count(), 1);
        assert_eq!(app.store.borrow().store().unwrap().auto_saved[0].id, id);
    }

    #[test]
    fn workspace_restores_recent_identity_without_duplicating_history() {
        let (_directory, mut app, id, _, _) = app_with_recent_session();
        app.open_saved(&id, &context());
        let workspace = app.capture_workspace("recent tabs");
        app.store
            .borrow_mut()
            .store_mut()
            .unwrap()
            .save_workspace(workspace);
        app.load_workspace("recent tabs", &context());
        assert_eq!(app.tab_recent[0], Some(id.clone()));
        app.close_tab(0);
        let handle = app.store.borrow();
        let store = handle.store().unwrap();
        assert_eq!(store.auto_saved.len(), 1);
        assert_eq!(store.auto_saved[0].id, id);
    }

    #[test]
    fn save_session_as_asks_then_promotes_the_recent_session_into_the_chosen_folder() {
        let (_directory, mut app, id, _, _) = app_with_recent_session();
        let parent = app
            .store
            .borrow_mut()
            .store_mut()
            .unwrap()
            .create_folder(None, "Kept")
            .unwrap();
        app.open_saved(&id, &context());
        app.run(Command::SaveSessionAs, &context());
        assert_eq!(app.session_save.as_ref().unwrap().parent, None);
        assert_eq!(
            app.session_save.as_ref().unwrap().name,
            "recent-left.txt <--> recent-right.txt"
        );
        assert_eq!(app.store.borrow().store().unwrap().auto_saved.len(), 1);
        assert!(app
            .store
            .borrow()
            .store()
            .unwrap()
            .find_session(&id)
            .is_none());
        app.session_save = None;
        assert_eq!(app.store.borrow().store().unwrap().auto_saved.len(), 1);
        app.save_session_as(0, "My comparison", Some(&parent))
            .unwrap();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store.borrow().is_ready()
            }
        ));
        let handle = app.store.borrow();
        let store = handle.store().unwrap();
        assert!(store.auto_saved.is_empty());
        let folder = store.find(&parent).unwrap();
        assert_eq!(folder.children()[0].id(), &id);
        assert_eq!(folder.children()[0].name(), "My comparison");
        assert_eq!(app.tab_sessions[0], Some(id));
        assert_eq!(app.tab_recent[0], None);
    }

    /// Two tabs can show one recent session. Promoting it from one tab moves
    /// its identifier into the named tree, so the other tab's close must not
    /// record a recent entry under that identifier again.
    #[test]
    fn closing_another_tab_of_a_promoted_recent_session_keeps_identifiers_unique() {
        let (_directory, mut app, id, _, _) = app_with_recent_session();
        app.open_saved(&id, &context());
        app.open_saved(&id, &context());
        assert_eq!(app.tab_count(), 2);
        app.save_session_as(0, "Named", None).unwrap();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store.borrow().is_ready()
            }
        ));
        app.close_tab(1);
        let handle = app.store.borrow();
        let store = handle.store().unwrap();
        assert!(store.find_session(&id).is_some());
        assert_eq!(store.auto_saved.len(), 1);
        assert!(
            store.auto_saved.iter().all(|held| held.id != id),
            "a recent entry shares the identifier of a named session"
        );
    }

    #[test]
    fn a_refused_session_save_keeps_recent_history_and_the_original_name() {
        let (_directory, mut app, id, _, _) = app_with_recent_session();
        {
            let mut handle = app.store.borrow_mut();
            let store = handle.store_mut().unwrap();
            let named = store.next_id();
            store
                .add_session(
                    None,
                    SavedSession::new(named, "taken", SessionKind::TextCompare),
                )
                .unwrap();
        }
        app.open_saved(&id, &context());
        assert!(app.save_session_as(0, "taken", None).is_err());
        let handle = app.store.borrow();
        let store = handle.store().unwrap();
        assert_eq!(store.auto_saved.len(), 1);
        assert_eq!(store.auto_saved[0].id, id);
        assert_eq!(store.auto_saved[0].name, "recent");
        assert_eq!(app.tab_sessions[0], None);
        assert_eq!(app.tab_recent[0], Some(id));
    }

    #[test]
    fn startup_wait_is_delayed_and_does_not_replace_an_existing_notice() {
        let (_directory, mut app) = empty_app();
        app.report_startup_wait("two");
        assert!(app.workspace_notice().is_none());
        assert!(!app.startup_wait_reported);
        app.startup_wait_since =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(2));
        app.workspace_notice = Some("The earlier workspace load was dropped.".to_owned());
        app.report_startup_wait("two");
        assert_eq!(
            app.workspace_notice(),
            Some("The earlier workspace load was dropped.")
        );
        assert!(!app.startup_wait_reported);
        app.workspace_notice = None;
        app.report_startup_wait("two");
        assert!(app.workspace_notice().unwrap().contains("Waiting"));
        app.workspace_notice = None;
        app.report_startup_wait("two");
        assert!(app.workspace_notice().is_none());
    }

    #[test]
    fn a_workspace_treats_a_temporary_saved_tab_as_the_launcher() {
        let (_settings, mut app) = empty_app();
        app.push(Box::new(TemporaryView));
        app.tab_sessions[0] = Some(ca_session::SessionId::from_raw("saved-session"));

        let workspace = app.capture_workspace("temporary");

        assert_eq!(
            workspace.windows[0].tabs,
            [ca_session::WorkspaceTab::home()]
        );
    }

    #[test]
    fn opening_profiles_again_keeps_the_existing_editor() {
        let (_settings, mut app) = empty_app();
        let context = context();
        let egui_context = egui::Context::default();
        app.run(Command::Profiles, &context);
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(5),
            || {
                let Some(profiles) = app.profiles.as_mut() else {
                    return false;
                };
                let _ = egui_context.run(ca_ui::testing::sized_input(1_280.0, 800.0), |ctx| {
                    profiles.show(ctx);
                });
                !profiles.is_busy_for_test()
            }
        ));
        app.profiles
            .as_mut()
            .unwrap()
            .set_description_for_test("unsaved description");
        assert!(app.profiles.as_ref().unwrap().is_dirty_for_test());

        app.run(Command::Profiles, &context);

        let current = app.profiles.as_ref().unwrap();
        assert!(
            current.is_dirty_for_test(),
            "opening Profiles again replaced the unsaved editor draft"
        );
    }

    #[test]
    fn exit_waits_for_an_unsaved_profile_draft() {
        let (_settings, mut app) = empty_app();
        let context = context();
        let egui_context = egui::Context::default();
        app.run(Command::Profiles, &context);
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(5),
            || {
                let Some(profiles) = app.profiles.as_mut() else {
                    return false;
                };
                let _ = egui_context.run(ca_ui::testing::sized_input(1_280.0, 800.0), |ctx| {
                    profiles.show(ctx);
                });
                !profiles.is_busy_for_test()
            }
        ));
        app.profiles
            .as_mut()
            .unwrap()
            .set_description_for_test("unsaved description");

        app.try_leave();

        assert!(!app.closing, "the shell closed over an unsaved profile");
        assert!(app.profiles.as_ref().unwrap().is_dirty_for_test());
    }

    #[test]
    fn clear_session_reopens_the_active_kind_with_empty_sides() {
        let context = context();
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, "left\n").unwrap();
        std::fs::write(&right, "right\n").unwrap();
        let (_settings, mut app) = empty_app();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store().borrow().is_ready()
            }
        ));
        app.open_kind(&SessionKind::TextCompare, left, right, &context);
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(20),
            || {
                app.tabs[0].tick();
                app.tabs[0].is_ready()
            }
        ));

        app.run(Command::ClearSession, &context);

        let settings = app.tabs[0].settings().unwrap();
        let specs = settings.specs().unwrap();
        assert!(specs.left.is_none());
        assert!(specs.right.is_none());
        assert_eq!(app.tab_sessions[0], None);
        assert_eq!(app.settings_notice(), None);
    }

    #[test]
    fn exit_keeps_an_unsaved_tab_open_and_selects_it() {
        let (_settings, mut app) = empty_app();
        let close_checks = Rc::new(Cell::new(0));
        app.push(Box::new(UnsavedView {
            close_checks: Rc::clone(&close_checks),
        }));
        app.run(Command::Exit, &context());
        assert_eq!(close_checks.get(), 1);
        assert!(!app.is_closing());
        assert_eq!(app.active_title().as_deref(), Some("Unsaved edit"));
    }

    #[test]
    fn confirmed_exit_selects_a_background_tab_that_refuses() {
        let (_settings, mut app) = empty_app();
        let close_checks = Rc::new(Cell::new(0));
        app.push(Box::new(UnsavedView {
            close_checks: Rc::clone(&close_checks),
        }));
        app.open_home(&context());
        app.run(Command::Exit, &context());
        if app.asks_before_closing() {
            app.answer_exit(true);
        }
        assert_eq!(close_checks.get(), 1);
        assert!(!app.is_closing());
        assert_eq!(app.active_title().as_deref(), Some("Unsaved edit"));
    }

    #[test]
    fn window_close_request_is_cancelled_for_an_unsaved_tab() {
        let (_settings, mut app) = empty_app();
        let close_checks = Rc::new(Cell::new(0));
        app.push(Box::new(UnsavedView {
            close_checks: Rc::clone(&close_checks),
        }));
        let mut input = raw_input();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let output = egui::Context::default().run(input, |ctx| app.frame(ctx));
        assert!(output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::CancelClose));
        assert_eq!(close_checks.get(), 1);
        assert!(!app.is_closing());
    }

    #[test]
    fn window_close_request_exits_when_no_tab_refuses() {
        let (_settings, mut app) = empty_app();
        let mut input = raw_input();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let output = egui::Context::default().run(input, |ctx| app.frame(ctx));
        assert!(app.is_closing());
        assert!(!output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::CancelClose));
    }

    #[test]
    fn the_launcher_opens_from_an_empty_command_line() {
        let (_settings, app) = started_app(Startup::Home, &context());
        assert_eq!(app.tab_count(), 1);
        assert_eq!(app.active_title().as_deref(), Some("Home"));
    }

    #[test]
    fn a_rejected_command_line_still_opens_the_launcher() {
        let (_settings, app) = started_app(Startup::Rejected("bad".into()), &context());
        assert_eq!(app.active_title().as_deref(), Some("Home"));
    }

    #[test]
    fn new_tab_and_close_tab_move_the_selection() {
        let (_settings, mut app) = empty_app();
        let context = context();
        app.run(Command::NewTab, &context);
        app.run(Command::NewTab, &context);
        assert_eq!(app.tab_count(), 2);
        app.run(Command::CloseTab, &context);
        assert_eq!(app.tab_count(), 1);
        app.run(Command::CloseTab, &context);
        assert_eq!(app.tab_count(), 0);
        assert!(app.is_closing());
    }

    #[test]
    fn closing_an_index_past_the_end_does_nothing() {
        let (_settings, mut app) = empty_app();
        app.open_home(&context());
        app.close_tab(9);
        assert_eq!(app.tab_count(), 1);
    }

    #[test]
    fn the_shell_owns_its_own_commands_and_delegates_the_rest() {
        let (_settings, mut app) = empty_app();
        assert!(app.accepts(Command::NewTab));
        assert!(!app.accepts(Command::CloseTab));
        assert!(!app.accepts(Command::NextDifference));
        app.open_home(&context());
        assert!(app.accepts(Command::CloseTab));
        assert!(!app.accepts(Command::NextDifference));
    }

    #[test]
    fn a_command_line_pair_opens_the_matching_view() {
        let (_settings, app) = started_app(
            Startup::open(
                SessionKind::FolderCompare,
                Path::new("left"),
                Path::new("right"),
            ),
            &context(),
        );
        assert_eq!(app.active_title().as_deref(), Some("left - right"));
    }

    #[test]
    fn a_command_line_kind_opens_the_view_registered_for_it() {
        let (_settings, app) = started_app(
            Startup::open(
                SessionKind::HexCompare,
                Path::new("one.bin"),
                Path::new("two.bin"),
            ),
            &context(),
        );
        assert_eq!(app.tab_count(), 1);
        assert_eq!(app.active_title().as_deref(), Some("one.bin - two.bin"));
    }

    /// Every kind the vocabulary names reaches a view, so a request for any of
    /// them opens exactly one tab rather than being dropped.
    #[test]
    fn every_kind_opens_one_tab() {
        let (_settings, mut app) = empty_app();
        for (count, kind) in SessionKind::ALL.iter().enumerate() {
            app.open_kind(kind, "left".into(), "right".into(), &context());
            assert_eq!(app.tab_count(), count + 1, "{kind} opened no tab");
        }
    }

    #[test]
    fn the_tools_menu_opens_an_empty_editor_and_an_empty_patch_view() {
        let (_settings, mut app) = empty_app();
        assert!(app.accepts(Command::EditTextFile));
        app.run(Command::EditTextFile, &context());
        assert_eq!(app.tab_count(), 1);
        assert_eq!(app.active_title().as_deref(), Some("Untitled"));
        app.run(Command::ViewPatch, &context());
        assert_eq!(app.tab_count(), 2);
        assert_eq!(app.active_title().as_deref(), Some("Text Patch"));
    }

    /// A merge answers its caller through the exit code, so the window carries
    /// the code of the tab that closed rather than losing it with the tab.
    #[test]
    fn exit_deletes_temporary_copies_and_keeps_no_session_over_them() {
        let context = context();
        let dir = tempfile::tempdir().unwrap();
        let copies = dir.path().join("copies");
        std::fs::create_dir_all(&copies).unwrap();
        let write = |folder: &Path, name: &str| {
            let path = folder.join(name);
            std::fs::write(&path, b"a\n").unwrap();
            path
        };
        let (_settings, mut app) = empty_app();
        app.open(
            &OpenRequest::new(
                SessionKind::TextCompare,
                write(&copies, "left.txt"),
                write(&copies, "right.txt"),
            )
            .over_temporaries(vec![copies.clone()]),
            &context,
        );
        app.open(
            &OpenRequest::new(
                SessionKind::TextCompare,
                write(dir.path(), "left.txt"),
                write(dir.path(), "right.txt"),
            ),
            &context,
        );
        let settle = |app: &App| {
            assert!(ca_ui::testing::wait_until(
                std::time::Duration::from_secs(10),
                || {
                    let mut handle = app.store().borrow_mut();
                    handle.poll();
                    handle.is_ready()
                }
            ));
        };
        settle(&app);
        assert_eq!(app.tab_count(), 2);
        app.run(Command::Exit, &context);
        if app.asks_before_closing() {
            app.answer_exit(true);
        }
        assert!(app.is_closing());
        assert_eq!(app.tab_count(), 0);
        settle(&app);
        let kept = app
            .store()
            .try_borrow()
            .unwrap()
            .store()
            .map(|store| store.auto_saved.len());
        assert_eq!(kept, Some(1));
        assert!(ca_ui::view::wait_for_temporary_deletes(
            std::time::Duration::from_secs(20)
        ));
        assert!(!copies.exists());
    }

    #[test]
    fn a_closing_tab_hands_its_exit_code_to_the_window() {
        let context = context();
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text.as_bytes()).unwrap();
            path
        };
        let (_settings, mut app) = started_app(
            Startup::Open(
                OpenRequest::new(
                    SessionKind::TextMerge,
                    write("left.txt", "a\nL\nc\n"),
                    write("right.txt", "a\nR\nc\n"),
                )
                .with_center(Some(write("center.txt", "a\nb\nc\n")))
                .with_output(Some(dir.path().join("merged.txt"))),
            ),
            &context,
        );
        assert_eq!(app.tab_count(), 1);
        assert_eq!(app.exit_code(), 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            app.tabs[0].tick();
            if app.tabs[0].is_ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.close_tab(0);
        // The conflict was never reviewed and no output was written, which is
        // the code the caller reads.
        assert_eq!(app.exit_code(), 101);
    }

    /// One expected bar, as menu name, then the lines in order.
    ///
    /// The table is written from the observed menus rather than read from the
    /// shell, so a line that moves has to be moved here too.
    type Bar = &'static [(&'static str, &'static [&'static str])];

    const EXPECTED_TEXT: Bar = &[
        (
            "Session",
            &[
                "New Session",
                "New Tab\tCtrl+T",
                "New Window",
                "Open Session\tCtrl+Shift+O",
                "---",
                "Load Workspace",
                "Save Workspace As...",
                "---",
                "Save Session\tCtrl+Shift+S",
                "Save Session As...",
                "Session Settings...",
                "Locked",
                "Clear Session\tCtrl+Shift+C",
                "Close Tab\tCtrl+W",
                "---",
                "Swap Sides",
                "Reload Files\tF5",
                "Recompare Files\tCtrl+F5",
                "---",
                "Text Compare Report...",
                "Text Compare Info\tCtrl+I",
                "---",
                "Compare Files Using",
                "Merge Files",
                "Compare Parent Folders",
                "---",
                "Exit\tCtrl+Q",
            ],
        ),
        (
            "File",
            &[
                "Open File...\tCtrl+O",
                "Open Clipboard\tCtrl+Shift+V",
                "Open With",
                "---",
                "Save File\tCtrl+S",
                "Save File As...\tF12",
                "Explorer",
                "---",
                "Save Both Files",
            ],
        ),
        (
            "Edit",
            &[
                "Undo\tCtrl+Z",
                "Redo\tCtrl+Y",
                "---",
                "Align With...\tF7",
                "Isolate",
                "Replacement...",
                "Copy to Right\tCtrl+R",
                "Copy Line to Right\tCtrl+Shift+R",
                "Increase Indent\tCtrl+Shift+I",
                "Decrease Indent\tCtrl+Shift+U",
                "---",
                "Cut\tCtrl+X",
                "Copy\tCtrl+C",
                "Paste\tCtrl+V",
                "Delete",
                "---",
                "Select All\tCtrl+A",
                "Select Section\tCtrl+D",
                "Compare Selection to Clipboard",
                "---",
                "Convert File",
                "---",
                "Copy to Left\tCtrl+L",
                "Copy Line to Left\tCtrl+Shift+L",
                "Toggle Overwrite Mode\tInsert",
                "Copy to Other Side",
                "Expand All",
                "Collapse All",
            ],
        ),
        (
            "Search",
            &[
                "Next Difference\tCtrl+Shift+N",
                "Previous Difference\tCtrl+Shift+P",
                "Next Difference Section\tCtrl+N",
                "Previous Difference Section\tCtrl+P",
                "Next Replacement\tCtrl+Alt+Shift+N",
                "Previous Replacement\tCtrl+Alt+Shift+P",
                "---",
                "Next Edit",
                "Previous Edit",
                "---",
                "Find...\tCtrl+F",
                "Replace...\tCtrl+H",
                "Find Next\tF3",
                "Find Previous\tShift+F3",
                "---",
                "Go To...\tCtrl+G",
                "---",
                "Toggle Bookmark",
                "Go to Bookmark",
                "Clear Bookmarks",
            ],
        ),
        (
            "View",
            &[
                "Show All",
                "Show Differences",
                "Show Same",
                "Show None",
                "---",
                "Show Context",
                "Ignore Unimportant Differences",
                "---",
                "Ignored",
                "---",
                "Visible Whitespace",
                "Line Numbers",
                "Syntax Highlighting",
                "Prettify for Comparison",
                "Compare Structure",
                "Word Wrap",
                "---",
                "Side-by-Side Layout",
                "Over-Under Layout",
                "Webpages",
                "Thumbnail",
                "Line Details",
                "Hex Details",
                "Alignment Details",
                "Ruler",
                "File Info",
                "Toolbar",
                "---",
                "Increase Display Font Size\tCtrl+Plus",
                "Decrease Display Font Size\tCtrl+Minus",
                "Reset Display Font Size\tCtrl+0",
            ],
        ),
        ("Tools", EXPECTED_TOOLS),
        ("Help", EXPECTED_HELP),
    ];

    const EXPECTED_FOLDER: Bar = &[
        (
            "Session",
            &[
                "New Session",
                "New Tab\tCtrl+T",
                "New Window",
                "Open Session\tCtrl+Shift+O",
                "---",
                "Load Workspace",
                "Save Workspace As...",
                "---",
                "Save Session\tCtrl+Shift+S",
                "Save Session As...",
                "Session Settings...",
                "Locked",
                "Clear Session\tCtrl+Shift+C",
                "Close Tab\tCtrl+W",
                "---",
                "Swap Sides",
                "Back\tAlt+Left",
                "Forward\tAlt+Right",
                "Browse for Folder",
                "Up One Level",
                "---",
                "Folder Compare Report...",
                "Folder Compare Info\tCtrl+I",
                "---",
                "Merge Base Folders",
                "Sync Base Folders",
                "Compare Parent Folders",
                "---",
                "Exit\tCtrl+Q",
            ],
        ),
        (
            "Actions",
            &[
                "Open Folder",
                "Open Subfolders",
                "Close Subfolders",
                "Set as Base Folders",
                "Open in New View",
                "Open With",
                "---",
                "Compare Contents...",
                "Copy to Side...",
                "Move to Side...",
                "Copy to Folder...",
                "Move to Folder...",
                "Delete...",
                "Rename...\tF2",
                "Attributes...",
                "Touch...",
                "Exclude",
                "New Folder...\tIns",
                "Copy Filename",
                "Ignored",
                "Refresh Selection\tShift+F5",
                "---",
                "Synchronize",
                "---",
                "Explorer",
                "---",
                "Exchange",
            ],
        ),
        (
            "Edit",
            &[
                "Expand All",
                "Collapse All",
                "---",
                "Select All\tCtrl+A",
                "Select All Files\tCtrl+Shift+A",
                "Select Newer",
                "Select Orphans",
                "Invert Selection",
                "Refresh\tF5",
                "Full Refresh\tCtrl+F5",
                "---",
                "Select Differences",
                "Clear Selection",
                "Copy to Right",
                "Copy to Left",
            ],
        ),
        (
            "Search",
            &[
                "Next Difference\tCtrl+N",
                "Previous Difference\tCtrl+P",
                "Find Filename...\tCtrl+F",
                "Find Next Filename\tF3",
                "Find Previous Filename\tShift+F3",
            ],
        ),
        (
            "View",
            &[
                "Show All",
                "Show Differences",
                "Show Same",
                "Show None",
                "Show No Orphans",
                "Show Differences but No Orphans",
                "Show Orphans",
                "Show Left Newer",
                "Show Right Newer",
                "Show Left Newer and Left Orphans",
                "Show Right Newer and Right Orphans",
                "Show Left Orphans",
                "Show Right Orphans",
                "---",
                "Always Show Folders",
                "Compare Files and Folder Structure",
                "Only Compare Files",
                "Ignore Folder Structure",
                "---",
                "Ignore Unimportant Differences",
                "Suppress Filters",
                "---",
                "Columns",
                "Legend\tCtrl+Alt+L",
                "Log",
                "Toolbar",
                "---",
                "Show Context",
                "Increase Display Font Size\tCtrl+Plus",
                "Decrease Display Font Size\tCtrl+Minus",
                "Reset Display Font Size\tCtrl+0",
            ],
        ),
        ("Tools", EXPECTED_TOOLS),
        ("Help", EXPECTED_HELP),
    ];

    const EXPECTED_TOOLS: &[&str] = &[
        "Options...",
        "File Formats...",
        "Profiles...",
        "---",
        "Export Settings...",
        "Import Settings...",
        "Restore Factory Defaults...",
        "---",
        "Save Snapshot...",
        "Edit Text File",
        "View Patch...",
    ];

    const EXPECTED_HELP: &[&str] = &[
        "Contents",
        "Context Sensitive Help\tF1",
        "---",
        "On the Web",
        "Check for Updates",
        "Support",
        "About",
    ];

    /// The bar as the window draws it: menu name, then one string per line.
    fn drawn(view: MenuView) -> Vec<(&'static str, Vec<String>)> {
        menus_for(view)
            .iter()
            .map(|(name, entries)| {
                let lines = entries
                    .iter()
                    .map(|entry| match entry {
                        Entry::Rule => "---".to_owned(),
                        Entry::Run(command, _) | Entry::Soon(command) => {
                            match command.shortcut_in(view) {
                                Some(shortcut) => {
                                    format!("{}\t{shortcut}", command.label_in(view))
                                }
                                None => command.label_in(view).to_owned(),
                            }
                        }
                    })
                    .collect();
                (*name, lines)
            })
            .collect()
    }

    /// Menu names, item order, separators, labels and shortcuts all follow the
    /// observation for both compare types.
    #[test]
    fn each_compare_bar_matches_the_observed_order() {
        for (view, expected) in [
            (MenuView::Text, EXPECTED_TEXT),
            (MenuView::Folder, EXPECTED_FOLDER),
        ] {
            let drawn = drawn(view);
            let names: Vec<&str> = drawn.iter().map(|(name, _)| *name).collect();
            let wanted: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
            assert_eq!(names, wanted, "{view:?} menu names");
            for ((name, lines), (_, wanted)) in drawn.iter().zip(expected.iter()) {
                assert_eq!(lines.len(), wanted.len(), "{view:?} {name} line count");
                for (index, (line, want)) in lines.iter().zip(wanted.iter()).enumerate() {
                    assert_eq!(line, want, "{view:?} {name} line {index}");
                }
            }
        }
    }

    #[test]
    fn table_compare_lists_column_fit_only_in_its_view_menu() {
        let table = drawn(MenuView::Table);
        let text = drawn(MenuView::Text);
        let table_session = table
            .iter()
            .find(|(name, _)| *name == "Session")
            .map(|(_, lines)| lines)
            .unwrap();
        let table_view = table
            .iter()
            .find(|(name, _)| *name == "View")
            .map(|(_, lines)| lines)
            .unwrap();
        let table_edit = table
            .iter()
            .find(|(name, _)| *name == "Edit")
            .map(|(_, lines)| lines)
            .unwrap();
        let text_view = text
            .iter()
            .find(|(name, _)| *name == "View")
            .map(|(_, lines)| lines)
            .unwrap();
        let text_edit = text
            .iter()
            .find(|(name, _)| *name == "Edit")
            .map(|(_, lines)| lines)
            .unwrap();
        assert!(table_view
            .iter()
            .any(|line| line == "Resize Columns to Fit"));
        assert!(table_view.iter().any(|line| line == "Hide Same Columns"));
        assert!(!text_view.iter().any(|line| line == "Resize Columns to Fit"));
        assert!(table_session
            .iter()
            .any(|line| line == "Table Compare Report..."));
        assert!(table_session
            .iter()
            .any(|line| line == "Table Compare Info"));
        assert!(!table_session.iter().any(|line| line == "Merge Files"));
        for label in [
            "Copy Cell to Right",
            "Copy Cell to Left",
            "Copy Cell to Other Side",
        ] {
            assert!(table_edit.iter().any(|line| line == label));
            assert!(!text_edit.iter().any(|line| line == label));
        }
    }

    #[test]
    fn live_key_commands_give_the_platform_reason_where_no_registry_exists() {
        let live_only = [
            Command::UpOneLevelLeft,
            Command::UpOneLevelRight,
            Command::UpOneLevelBoth,
            Command::SetAsBaseKeys,
            Command::SetBothAsBaseKeys,
            Command::SetAsBaseKeyOnOtherSide,
        ];
        let mut seen = Vec::new();
        for (_, entries) in super::REGISTRY_MENUS {
            for entry in *entries {
                if let Entry::Run(command, reason) = entry {
                    if live_only.contains(command) {
                        seen.push(*command);
                        assert_eq!(
                            *reason == super::NO_LIVE_REGISTRY,
                            !cfg!(windows),
                            "{command:?}: {reason}"
                        );
                    }
                }
            }
        }
        assert_eq!(seen, live_only);
    }

    #[test]
    fn registry_session_labels_compare_info_for_its_view() {
        let registry = drawn(MenuView::Registry);
        let registry_session = registry
            .iter()
            .find(|(name, _)| *name == "Session")
            .map(|(_, lines)| lines)
            .unwrap();
        assert!(registry_session
            .iter()
            .any(|line| line == "Registry Compare Info\tCtrl+I"));
    }

    #[test]
    fn version_and_media_sessions_label_compare_info_for_their_views() {
        for (view, expected) in [
            (MenuView::Version, "Version Compare Info\tCtrl+I"),
            (MenuView::Media, "Media Compare Info\tCtrl+I"),
        ] {
            let session = drawn(view)
                .into_iter()
                .find(|(name, _)| *name == "Session")
                .map(|(_, lines)| lines)
                .unwrap();
            assert!(session.iter().any(|line| line == expected));
        }
    }

    /// A request over two placeholder paths, for building one view of a kind.
    fn sample_request(kind: &SessionKind) -> OpenRequest {
        OpenRequest::new(kind.clone(), "left".into(), "right".into())
    }

    /// Every command a fixed bar lists, with the bar and the menu it sits in.
    fn listed() -> Vec<(MenuView, &'static str, Command)> {
        MenuView::ALL
            .iter()
            .copied()
            .flat_map(|view| {
                menus_for(view).iter().flat_map(move |(name, entries)| {
                    entries.iter().filter_map(move |entry| match entry {
                        Entry::Run(command, _)
                        | Entry::Soon(command @ Command::CompareFilesUsing) => {
                            Some((view, *name, *command))
                        }
                        _ => None,
                    })
                })
            })
            .collect()
    }

    /// Every command a view declares has to reach that view's own bar, or the
    /// view offers something the window never shows.
    #[test]
    fn every_declared_command_reaches_the_bar_of_its_view() {
        let listed = listed();
        let context = context();
        for (kind, build) in registry::VIEWS {
            let view = build(&sample_request(kind), &context, 1);
            let bar = view.menu_view();
            for state in view.commands() {
                let menu = state.command.menu();
                let reached = state.command.menu() == Menu::Hidden
                    || listed
                        .iter()
                        .any(|(on, _, known)| *on == bar && *known == state.command)
                    || (matches!(bar, MenuView::Other | MenuView::Merge) && menu == Menu::Actions);
                assert!(
                    reached,
                    "{kind} declares {:?}, which its bar never shows",
                    state.command
                );
            }
        }
    }

    /// On the shared bar a line sits in the menu its command names, so the
    /// Actions menu never repeats a line a fixed menu carries.
    ///
    /// The two compare bars follow the observed order instead, which
    /// `each_compare_bar_matches_the_observed_order` holds.
    #[test]
    fn a_menu_line_of_the_shared_bar_sits_in_the_menu_its_command_names() {
        for (view, name, command) in listed() {
            // An action command reaches whichever menu the bar puts it in.
            if !matches!(view, MenuView::Other | MenuView::Merge | MenuView::Registry)
                || command.menu() == Menu::Actions
            {
                continue;
            }
            let expected = match command.menu() {
                Menu::Session => "Session",
                Menu::File => "File",
                Menu::Edit => "Edit",
                Menu::Search => "Search",
                Menu::View => "View",
                Menu::Actions => ACTIONS_MENU,
                Menu::Tools => "Tools",
                Menu::Help => "Help",
                Menu::Hidden => {
                    unreachable!("{command:?} is listed in {name} but names no fixed menu")
                }
            };
            assert_eq!(
                expected, name,
                "{view:?} lists {command:?} in the wrong menu"
            );
        }
    }

    /// The reverse guard: a line that claims to run something nothing answers
    /// for is a dead line. A line the build does not run yet says so instead.
    #[test]
    fn every_runnable_menu_line_is_declared_by_a_view_or_owned_by_the_shell() {
        let context = context();
        let declared: Vec<Command> = registry::VIEWS
            .iter()
            .flat_map(|(kind, build)| {
                build(&sample_request(kind), &context, 1)
                    .commands()
                    .into_iter()
                    .map(|state| state.command)
            })
            .collect();
        for (view, name, command) in listed() {
            let owned = super::SHELL_OWNED.contains(&command);
            assert!(
                owned || declared.contains(&command),
                "{view:?} {name} lists {command:?}, which no registered view declares"
            );
        }
    }

    /// A line with no implementation still carries a command, so its label and
    /// its keystroke come from the one vocabulary.
    #[test]
    fn a_line_with_no_implementation_is_shown_disabled() {
        let mut pending = 0usize;
        for view in [MenuView::Text, MenuView::Folder] {
            for (_, entries) in menus_for(view) {
                for entry in *entries {
                    if let Entry::Soon(command) = entry {
                        if *command == Command::CompareFilesUsing {
                            continue;
                        }
                        pending += 1;
                        assert!(!command.label_in(view).is_empty());
                    }
                }
            }
        }
        assert!(pending > 0, "no line is waiting on an implementation");
        assert_eq!(NOT_YET, "Not available yet");
    }

    /// The journal check runs whether or not a folder comparison is opened, and
    /// it runs once.
    #[test]
    fn the_journal_check_starts_at_launch_and_is_not_started_again() {
        let context = context();
        let (_settings, mut app) = started_app(Startup::Home, &context);
        assert!(app.recovery.is_some(), "no journal check was started");
        app.start_recovery_check(&context);
        // A second call while the first is in flight leaves the first alone.
        assert!(app.recovery.is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while app.recovery.is_some() && std::time::Instant::now() < deadline {
            app.poll_recovery();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.recovery.is_none(), "the journal check never answered");
    }

    /// Whatever the check reports reaches the launcher, which opens before the
    /// check has answered.
    #[test]
    fn a_notice_raised_after_the_launcher_opened_still_reaches_it() {
        let context = context();
        let (_settings, mut app) = empty_app();
        app.open_home(&context);
        app.tabs[0].tick();
        assert!(app.active_notice().is_none());
        if let Ok(mut slot) = app.startup_notice.lock() {
            *slot = Some("two unfinished batches".to_owned());
        }
        app.tabs[0].tick();
        assert_eq!(
            app.active_notice().as_deref(),
            Some("two unfinished batches")
        );
    }

    /// A view that declares action commands reaches the Actions menu; one that
    /// declares none leaves the menu off the bar.
    #[test]
    fn the_actions_menu_follows_what_the_active_view_declares() {
        let context = context();
        let (_settings, mut app) = empty_app();
        app.open_home(&context);
        app.declared = app.tabs[0].commands();
        assert!(app.declared_actions().is_empty());
        app.open_kind(
            &SessionKind::FolderCompare,
            "left".into(),
            "right".into(),
            &context,
        );
        app.declared = app.tabs[1].commands();
        let actions = app.declared_actions();
        assert!(actions.iter().any(|state| state.command == Command::Delete));
        assert!(actions
            .iter()
            .all(|state| state.command.menu() == Menu::Actions));
    }

    #[test]
    fn compare_parent_folders_opens_the_local_file_directories() {
        let context = context();
        let files = tempfile::tempdir().unwrap();
        let left_folder = files.path().join("left");
        let right_folder = files.path().join("right");
        std::fs::create_dir(&left_folder).unwrap();
        std::fs::create_dir(&right_folder).unwrap();
        let left = left_folder.join("left.csv");
        let right = right_folder.join("right.csv");
        std::fs::write(&left, "Name,Value\nA,1\n").unwrap();
        std::fs::write(&right, "Name,Value\nA,2\n").unwrap();

        let (_settings, mut app) = empty_app();
        assert!(!app.accepts(Command::CompareParentFolders));
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store().borrow().is_ready()
            }
        ));
        app.open_kind(&SessionKind::TableCompare, left, right, &context);
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(20),
            || {
                app.tabs[0].tick();
                app.tabs[0].is_ready()
            }
        ));
        assert!(app.accepts(Command::CompareParentFolders));
        assert!(app.accepts(Command::OpenFile));

        app.run(Command::CompareParentFolders, &context);

        assert_eq!(app.tab_count(), 2);
        let opened = app.tab_settings(1).unwrap();
        assert_eq!(opened.kind(), SessionKind::FolderCompare);
        let specs = specs_of(&opened).unwrap();
        assert_eq!(specs.0, Some(left_folder));
        assert_eq!(specs.1, Some(right_folder));
        assert!(!app.accepts(Command::CompareParentFolders));
    }

    #[test]
    fn explorer_shows_a_table_side_through_the_configured_spawner() {
        let context = context();
        let files = tempfile::tempdir().unwrap();
        let left = files.path().join("left file.csv");
        let right = files.path().join("right.csv");
        std::fs::write(&left, "Name,Value\nA,1\n").unwrap();
        std::fs::write(&right, "Name,Value\nA,2\n").unwrap();
        let recorder = Arc::new(LaunchRecorder::default());

        let (_settings, mut app) = empty_app();
        app.set_spawner(recorder.clone());
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store().borrow().is_ready()
            }
        ));
        app.open_kind(&SessionKind::TableCompare, left.clone(), right, &context);
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(20),
            || {
                app.tabs[0].tick();
                app.tabs[0].is_ready()
            }
        ));

        assert!(app.accepts(Command::Explorer));
        app.run(Command::Explorer, &context);
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || !recorder.0.lock().unwrap().is_empty()
        ));

        let started = recorder.0.lock().unwrap();
        assert_eq!(started.len(), 1);
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        assert!(started[0]
            .arguments
            .iter()
            .any(|argument| argument.contains("left file.csv")));
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert_eq!(
            started[0].arguments.last().map(String::as_str),
            left.parent().map(|path| path.to_str().unwrap())
        );
    }

    #[test]
    fn compare_files_using_opens_another_two_file_view_with_the_same_paths() {
        let context = context();
        let files = tempfile::tempdir().unwrap();
        let left = files.path().join("left.csv");
        let right = files.path().join("right.csv");
        std::fs::write(&left, "Name,Value\nA,1\n").unwrap();
        std::fs::write(&right, "Name,Value\nA,2\n").unwrap();

        let (_settings, mut app) = empty_app();
        assert!(!app.accepts(Command::CompareFilesUsing));
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store().borrow().is_ready()
            }
        ));
        app.open_kind(
            &SessionKind::TextCompare,
            left.clone(),
            right.clone(),
            &context,
        );
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(20),
            || {
                app.tabs[0].tick();
                app.tabs[0].is_ready()
            }
        ));

        assert!(app.accepts(Command::CompareFilesUsing));
        let kinds = app.other_file_comparison_kinds();
        assert!(!kinds.contains(&SessionKind::TextCompare));
        assert!(!kinds.contains(&SessionKind::TextMerge));
        assert!(!kinds.contains(&SessionKind::TextEdit));
        assert!(!kinds.contains(&SessionKind::FolderCompare));
        assert!(kinds.contains(&SessionKind::TableCompare));
        app.open_files_using(&SessionKind::TextCompare, &context);
        assert_eq!(app.tab_count(), 1, "the active file view is not offered");

        app.open_files_using(&SessionKind::TableCompare, &context);

        assert_eq!(app.tab_count(), 2);
        let opened = app.tab_settings(1).unwrap();
        assert_eq!(opened.kind(), SessionKind::TableCompare);
        let specs = specs_of(&opened).unwrap();
        assert_eq!(specs.0, Some(left));
        assert_eq!(specs.1, Some(right));
    }

    #[test]
    fn file_comparison_menu_state_does_not_recheck_the_disk_each_frame() {
        let context = context();
        let files = tempfile::tempdir().unwrap();
        let left_folder = files.path().join("left");
        let right_folder = files.path().join("right");
        std::fs::create_dir(&left_folder).unwrap();
        std::fs::create_dir(&right_folder).unwrap();
        let left = left_folder.join("left.csv");
        let right = right_folder.join("right.csv");
        std::fs::write(&left, "Name,Value\nA,1\n").unwrap();
        std::fs::write(&right, "Name,Value\nA,2\n").unwrap();

        let (_settings, mut app) = empty_app();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store().borrow().is_ready()
            }
        ));
        app.open_kind(
            &SessionKind::TableCompare,
            left.clone(),
            right.clone(),
            &context,
        );
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(20),
            || {
                app.tabs[0].tick();
                app.tabs[0].is_ready()
            }
        ));
        assert!(app.accepts(Command::CompareFilesUsing));
        assert!(app.accepts(Command::CompareParentFolders));

        std::fs::remove_dir_all(&left_folder).unwrap();
        std::fs::remove_dir_all(&right_folder).unwrap();

        assert!(app.accepts(Command::CompareFilesUsing));
        assert!(app.accepts(Command::CompareParentFolders));
    }

    #[test]
    fn temporary_file_tabs_refuse_derived_comparisons() {
        let context = context();
        let files = tempfile::tempdir().unwrap();
        let copies = files.path().join("copies");
        std::fs::create_dir(&copies).unwrap();
        let left = copies.join("left.csv");
        let right = copies.join("right.csv");
        std::fs::write(&left, "Name,Value\nA,1\n").unwrap();
        std::fs::write(&right, "Name,Value\nA,2\n").unwrap();

        let (_settings, mut app) = empty_app();
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(10),
            || {
                app.poll_store();
                app.store().borrow().is_ready()
            }
        ));
        app.open(
            &OpenRequest::new(SessionKind::TextCompare, left, right).over_temporaries(vec![copies]),
            &context,
        );
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(20),
            || {
                app.tabs[0].tick();
                app.tabs[0].is_ready()
            }
        ));

        assert!(!app.accepts(Command::CompareParentFolders));
        assert!(!app.accepts(Command::CompareFilesUsing));
        assert_eq!(
            app.refusal(Command::CompareParentFolders),
            Some(super::TEMPORARIES)
        );
        assert_eq!(
            app.refusal(Command::CompareFilesUsing),
            Some(super::TEMPORARIES)
        );

        app.run(Command::CompareParentFolders, &context);
        app.open_files_using(&SessionKind::TableCompare, &context);

        assert_eq!(app.tab_count(), 1);
    }

    /// Folder roots stored as archives and snapshots reopen through their
    /// backing paths, while ordinary sides keep their local paths.
    #[test]
    fn specs_resolve_local_sides_and_supported_folder_roots() {
        let mut settings = ca_session::SessionSettings::defaults_for(&SessionKind::FolderCompare);
        let specs = settings.specs_mut().unwrap();
        specs.left = Some(ca_session::SideLocation::local("left"));
        specs.right = Some(ca_session::SideLocation::archive("bundle.zip", ""));
        specs.ancestor = Some(ca_session::SideLocation::snapshot("base.casnap"));

        assert_eq!(
            specs_of(&settings),
            Some((
                Some("left".into()),
                Some("bundle.zip".into()),
                Some("base.casnap".into()),
                None,
            ))
        );
    }

    /// A location the view cannot open is rejected instead of becoming an
    /// empty or semantically different path.
    #[test]
    fn specs_reject_locations_the_view_cannot_open() {
        let mut folder = ca_session::SessionSettings::defaults_for(&SessionKind::FolderCompare);
        folder.specs_mut().unwrap().left = Some(ca_session::SideLocation::archive(
            "bundle.zip",
            "inside/file.txt",
        ));
        assert_eq!(specs_of(&folder), None);

        let mut text = ca_session::SessionSettings::defaults_for(&SessionKind::TextCompare);
        text.specs_mut().unwrap().left = Some(ca_session::SideLocation::snapshot("base.casnap"));
        assert_eq!(specs_of(&text), None);
    }

    /// A rejected location leaves the current view in place instead of
    /// replacing it with a comparison over an empty path.
    #[test]
    fn reopening_an_unsupported_location_keeps_the_current_tab() {
        let context = context();
        let (_directory, mut app) = empty_app();
        app.open_kind(
            &SessionKind::FolderCompare,
            "left".into(),
            "right".into(),
            &context,
        );
        let original_title = app.tabs[0].title();
        let mut settings = ca_session::SessionSettings::defaults_for(&SessionKind::FolderCompare);
        settings.specs_mut().unwrap().left = Some(ca_session::SideLocation::archive(
            "bundle.zip",
            "inside/file.txt",
        ));

        assert_eq!(
            app.reopen_over(0, &settings, &context, false),
            Err(SIDES_UNSUPPORTED)
        );
        assert_eq!(app.tabs[0].title(), original_title);
    }
}
