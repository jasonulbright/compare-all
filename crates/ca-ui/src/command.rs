//! The command vocabulary and the keyboard routing that reaches it.
//!
//! Routing is a pure function of a keystroke and the view it is aimed at, so
//! the whole table is testable without a frame. The same key carries different
//! commands in different comparison types, which is why routing takes the view.
//! Whether a command is available at a given moment is a separate question,
//! answered by the view that would run it.

/// Which menu a command is shown in.
///
/// The grouping belongs to the command, so a view that declares a command
/// reaches the right menu without the shell being edited. [`Menu::Actions`] is
/// built from what the active view declares wherever the bar has no fixed
/// order for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Menu {
    /// Session-wide commands.
    Session,
    /// Reading and writing files.
    File,
    /// Changing content and the selection.
    Edit,
    /// Moving through a comparison.
    Search,
    /// What is displayed and how.
    View,
    /// What the active comparison type can do to its content.
    Actions,
    /// Settings and whole-program tools.
    Tools,
    /// Help and build information.
    Help,
    /// Reached by a keystroke or a toolbar only.
    Hidden,
}

/// Which menu bar a view carries.
///
/// The two compare types have their own menu names, item order and shortcuts.
/// Every other view carries the shared bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuView {
    /// The text comparison bar.
    Text,
    /// The table comparison bar.
    Table,
    /// The folder comparison bar.
    Folder,
    /// The three way merge bar: the shared bar with the merge's own Edit and
    /// View menus and take keys.
    Merge,
    /// The registry comparison bar: the shared bar with an Edit menu of
    /// registry keys and values.
    Registry,
    /// The version resource comparison bar.
    Version,
    /// The media metadata comparison bar.
    Media,
    /// The shared bar every other view carries.
    Other,
}

macro_rules! commands {
    ($($variant:ident, $label:literal, $menu:ident, $shortcut:expr, $doc:literal;)*) => {
        /// Something the user can ask for, however they ask for it.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Command {
            $(
                #[doc = $doc]
                $variant,
            )*
        }

        impl Command {
            /// Every command the vocabulary names.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The label a menu or button shows.
            #[must_use]
            pub const fn label(self) -> &'static str {
                match self { $(Self::$variant => $label,)* }
            }

            /// Which menu shows this command.
            #[must_use]
            pub const fn menu(self) -> Menu {
                match self { $(Self::$variant => Menu::$menu,)* }
            }

            /// The keystroke shown beside the label in the text comparison bar.
            #[must_use]
            pub const fn shortcut_text(self) -> Option<&'static str> {
                match self { $(Self::$variant => $shortcut,)* }
            }

            /// The stable name a stored document carries this command under.
            #[must_use]
            pub const fn id(self) -> &'static str {
                match self { $(Self::$variant => stringify!($variant),)* }
            }

            /// The command stored under `id`, where this build has one.
            #[must_use]
            pub fn from_id(id: &str) -> Option<Self> {
                match id { $(stringify!($variant) => Some(Self::$variant),)* _ => None }
            }

            /// What the command does, for the search field of the commands
            /// page.
            #[must_use]
            pub const fn description(self) -> &'static str {
                match self { $(Self::$variant => $doc,)* }
            }
        }
    };
}

commands! {
    // Session.
    NewSession, "New Session", Session, None, "Start a session of a chosen comparison type.";
    NewTab, "New Tab", Session, Some("Ctrl+T"), "Open a tab showing the launcher.";
    NewWindow, "New Window", Session, None, "Open a second window.";
    OpenSession, "Open Session", Session, Some("Ctrl+Shift+O"), "Load a stored session.";
    LoadWorkspace, "Load Workspace", Session, None, "Load a stored set of tabs.";
    SaveWorkspaceAs, "Save Workspace As...", Session, None, "Store the open tabs under a name.";
    SaveSession, "Save Session", Session, Some("Ctrl+Shift+S"), "Store the active session.";
    SaveSessionAs, "Save Session As...", Session, None, "Store the active session under a new name.";
    SessionSettings, "Session Settings...", Session, None, "Edit the settings of the active session.";
    ToggleLocked, "Locked", Session, None, "Refuse changes to the active session, or allow them again.";
    ClearSession, "Clear Session", Session, Some("Ctrl+Shift+C"), "Return the active session to its defaults.";
    CloseTab, "Close Tab", Session, Some("Ctrl+W"), "Close the active tab.";
    SwapSides, "Swap Sides", Session, None, "Exchange the two sides.";
    Reload, "Reload Files", Session, Some("F5"), "Re-read both sides from disk.";
    Recompare, "Recompare Files", Session, Some("Ctrl+F5"), "Compare again without re-reading.";
    Back, "Back", Session, None, "Return to the previous pair of folders.";
    Forward, "Forward", Session, None, "Go to the pair of folders the last step came from.";
    BrowseForFolder, "Browse for Folder", Session, None, "Pick a base folder for one side.";
    UpOneLevel, "Up One Level", Session, None, "Move both sides to their parent folder.";
    UpOneLevelLeft, "Up One Level: Left Side", Session, None, "Move the left base key to its parent key.";
    UpOneLevelRight, "Up One Level: Right Side", Session, None, "Move the right base key to its parent key.";
    UpOneLevelBoth, "Up One Level: Both Sides", Session, None, "Move both base keys to their parent keys.";
    CompareReport, "Text Compare Report...", Session, None, "Write a report of the comparison.";
    CompareInfo, "Text Compare Info", Session, Some("Ctrl+I"), "Show what the comparison found.";
    CompareFilesUsing, "Compare Files Using", Session, None, "Open the same pair in another comparison type.";
    MergeFiles, "Merge Files", Session, None, "Open the pair in a three way merge.";
    MergeBaseFolders, "Merge Base Folders", Session, None, "Open the base folders in a folder merge.";
    SyncBaseFolders, "Sync Base Folders", Session, None, "Open the base folders in a folder sync.";
    CompareParentFolders, "Compare Parent Folders", Session, None, "Open a folder comparison of the two parents.";
    Exit, "Exit", Session, Some("Ctrl+Q"), "Leave the program.";

    // File.
    OpenFile, "Open File...", File, Some("Ctrl+O"), "Read a file into one side.";
    OpenClipboard, "Open Clipboard", File, Some("Ctrl+Shift+V"), "Read the clipboard into one side.";
    OpenWith, "Open With", File, None, "Hand the file to another program.";
    SaveFile, "Save File", File, Some("Ctrl+S"), "Write the active pane's file.";
    SaveFileAs, "Save File As...", File, Some("F12"), "Write the active pane's file under a new name.";
    SaveBoth, "Save Both Files", File, None, "Write both files.";
    Explorer, "Explorer", File, None, "Show the file in the system file browser.";

    // Edit.
    Undo, "Undo", Edit, Some("Ctrl+Z"), "Reverse the last edit in the active pane.";
    Redo, "Redo", Edit, Some("Ctrl+Y"), "Reapply the last reversed edit.";
    AlignWith, "Align With...", Edit, Some("F7"), "Tie the current line to a chosen line on the other side.";
    Isolate, "Isolate", Edit, None, "Keep the selected lines and drop the rest of the section.";
    Replacement, "Replacement...", Edit, None, "Record a text replacement the comparison treats as equal.";
    CopyToRight, "Copy to Right", Edit, Some("Ctrl+R"), "Copy the selection or the current section to the right pane.";
    CopyToLeft, "Copy to Left", Edit, Some("Ctrl+L"), "Copy the selection or the current section to the left pane.";
    CopyCellToRight, "Copy Cell to Right", Edit, None, "Copy the current cell to the right pane.";
    CopyCellToLeft, "Copy Cell to Left", Edit, None, "Copy the current cell to the left pane.";
    CopyCellToOtherSide, "Copy Cell to Other Side", Edit, None, "Copy the current cell to the opposite pane.";
    CopyLineToRight, "Copy Line to Right", Edit, Some("Ctrl+Shift+R"), "Copy the current line to the right pane.";
    CopyLineToLeft, "Copy Line to Left", Edit, Some("Ctrl+Shift+L"), "Copy the current line to the left pane.";
    IncreaseIndent, "Increase Indent", Edit, Some("Ctrl+Shift+I"), "Add one indent level to the selected lines.";
    DecreaseIndent, "Decrease Indent", Edit, Some("Ctrl+Shift+U"), "Remove one indent level from the selected lines.";
    Cut, "Cut", Edit, Some("Ctrl+X"), "Remove the selection and put it on the clipboard.";
    Copy, "Copy", Edit, Some("Ctrl+C"), "Put the selection on the clipboard.";
    Paste, "Paste", Edit, Some("Ctrl+V"), "Insert the clipboard at the caret.";
    SelectAll, "Select All", Edit, Some("Ctrl+A"), "Select every line of the active pane.";
    SelectSection, "Select Section", Edit, Some("Ctrl+D"), "Select every line of the current difference section.";
    CompareSelectionToClipboard, "Compare Selection to Clipboard", Edit, None, "Open a comparison of the selection against the clipboard.";
    ConvertFile, "Convert File", Edit, None, "Change the encoding or the line endings of the file.";
    ToggleConflict, "Conflict", Edit, None, "Set or clear conflict status on the selected lines or the current section.";
    ToggleOverwrite, "Toggle Overwrite Mode", Edit, Some("Insert"), "Swap between inserting and overwriting.";
    ExpandAll, "Expand All", Edit, None, "Open every folder of a tree.";
    CollapseAll, "Collapse All", Edit, None, "Close every folder of a tree.";
    SelectAllFiles, "Select All Files", Edit, None, "Select every file, leaving folders alone.";
    SelectNewer, "Select Newer", Edit, None, "Select every item that is newer than its counterpart.";
    SelectOrphans, "Select Orphans", Edit, None, "Select every item present on one side only.";
    InvertSelection, "Invert Selection", Edit, None, "Select what is not selected and drop what is.";
    SelectDifferences, "Select Differences", Edit, None, "Select every item whose two sides differ.";
    ClearSelection, "Clear Selection", Edit, None, "Leave nothing selected.";
    SetAsBaseKeys, "Set as Base Keys", Edit, None, "Make the selected keys the base keys of the comparison.";
    SetBothAsBaseKeys, "Set Both as Base Keys", Edit, None, "Make the selected key and its counterpart the base keys of both sides.";
    SetAsBaseKeyOnOtherSide, "Set as Base Key on Other Side", Edit, None, "Make the selected key the base key of the opposite side.";
    NewKey, "New Key", Edit, None, "Create a key under the current one.";
    NewValue, "New Value", Edit, None, "Create a value under the current key.";
    Modify, "Modify...", Edit, None, "Change the type and the data of the current value.";
    CopyKeyName, "Copy Key Name", Edit, None, "Put the full name of the current key on the clipboard.";
    Export, "Export...", Edit, None, "Write the current key to a registry file.";
    ExportAll, "Export All...", Edit, None, "Write every loaded key of the active side to a registry file.";

    // Search.
    NextDifference, "Next Difference", Search, Some("Ctrl+Shift+N"), "Move to the next differing row.";
    PreviousDifference, "Previous Difference", Search, Some("Ctrl+Shift+P"), "Move to the previous differing row.";
    NextSection, "Next Difference Section", Search, Some("Ctrl+N"), "Move to the next run of differing rows.";
    PreviousSection, "Previous Difference Section", Search, Some("Ctrl+P"), "Move to the previous run of differing rows.";
    NextReplacement, "Next Replacement", Search, Some("Ctrl+Alt+Shift+N"), "Move to the next recorded replacement.";
    PreviousReplacement, "Previous Replacement", Search, Some("Ctrl+Alt+Shift+P"), "Move to the previous recorded replacement.";
    NextEdit, "Next Edit", Search, None, "Move the caret to the next line edited in this session.";
    PreviousEdit, "Previous Edit", Search, None, "Move the caret to the previous line edited in this session.";
    Find, "Find...", Search, Some("Ctrl+F"), "Open the find panel.";
    Replace, "Replace...", Search, Some("Ctrl+H"), "Open the replace panel.";
    FindNext, "Find Next", Search, Some("F3"), "Repeat the search forward.";
    FindPrevious, "Find Previous", Search, Some("Shift+F3"), "Repeat the search backward.";
    GoTo, "Go To...", Search, Some("Ctrl+G"), "Move the caret to a given line and column.";
    ToggleBookmark, "Toggle Bookmark", Search, None, "Mark the current line, or drop its mark.";
    GoToBookmark, "Go to Bookmark", Search, None, "Move the caret to a marked line.";
    ClearBookmarks, "Clear Bookmarks", Search, None, "Remove every bookmark.";

    // View.
    ShowAll, "Show All", View, None, "Show every row.";
    ShowDifferences, "Show Differences", View, None, "Show only rows that differ.";
    ShowSame, "Show Same", View, None, "Show only rows that match.";
    ShowNone, "Show None", View, None, "Show no rows.";
    ShowNoOrphans, "Show No Orphans", View, None, "Hide rows present on one side only.";
    ShowDifferencesNoOrphans, "Show Differences but No Orphans", View, None, "Show differing rows that both sides hold.";
    ShowOrphans, "Show Orphans", View, None, "Show only rows present on one side.";
    ShowLeftNewer, "Show Left Newer", View, None, "Show only rows whose left side is newer.";
    ShowRightNewer, "Show Right Newer", View, None, "Show only rows whose right side is newer.";
    ShowLeftNewerAndLeftOrphans, "Show Left Newer and Left Orphans", View, None, "Show rows the left side is newer on or holds alone.";
    ShowRightNewerAndRightOrphans, "Show Right Newer and Right Orphans", View, None, "Show rows the right side is newer on or holds alone.";
    ShowLeftOrphans, "Show Left Orphans", View, None, "Show only rows the left side holds alone.";
    ShowRightOrphans, "Show Right Orphans", View, None, "Show only rows the right side holds alone.";
    ShowConflicts, "Show Conflicts", View, None, "Show only the lines that wait for review.";
    ShowLeftChanges, "Show Left Changes", View, None, "Show the lines the left side changed, alone or with the right side.";
    ShowRightChanges, "Show Right Changes", View, None, "Show the lines the right side changed, alone or with the left side.";
    ShowMergeable, "Show Mergeable", View, None, "Show changed lines that do not wait for review.";
    ShowContext, "Show Context", View, None, "Show differing rows with their surrounding matching rows.";
    ToggleIgnoreUnimportant, "Ignore Unimportant Differences", View, None, "Count unimportant differences as matching text, or stop doing so.";
    HideSameColumns, "Hide Same Columns", View, None, "Hide columns that contain no differences, or show them again.";
    ResizeColumnsToFit, "Resize Columns to Fit", View, None, "Make table columns wide enough for their headings and data to be fully visible.";
    ToggleIgnoreSameChanges, "Ignore Same Changes", View, None, "Count a change made the same way on both sides as no change, or stop doing so.";
    ToggleIgnored, "Ignored", View, None, "Show rows the session marks as ignored, or hide them.";
    ToggleSectionIgnored, "Ignored", View, None, "Ignore the differences of the selected lines or the current section, or stop ignoring them.";
    AlwaysShowFolders, "Always Show Folders", View, None, "Keep folders on screen whatever the filter says.";
    CompareFilesAndFolderStructure, "Compare Files and Folder Structure", View, None, "Pair items by their place in the tree.";
    OnlyCompareFiles, "Only Compare Files", View, None, "Pair files and leave the folder structure out.";
    IgnoreFolderStructure, "Ignore Folder Structure", View, None, "Pair files by name wherever they sit.";
    SuppressFilters, "Suppress Filters", View, None, "Show every item whatever the filters say, or apply them again.";
    ToggleVisibleWhitespace, "Visible Whitespace", View, None, "Draw marks for spaces and tabs, or stop drawing them.";
    ToggleLineNumbers, "Line Numbers", View, None, "Show or hide the line number gutter.";
    ToggleSyntaxHighlighting, "Syntax Highlighting", View, None, "Color the text by its grammar, or stop doing so.";
    CompareStructure, "Compare Structure", View, None, "Compare JSON or XML by path and value, or restore the source text.";
    PrettifyForComparison, "Prettify for Comparison", View, None, "Compare recognized JSON, XML, YAML, or TOML after formatting it in memory; source files are never changed.";
    ToggleWordWrap, "Word Wrap", View, None, "Break long lines at the pane edge, or let them run on.";
    SideBySideLayout, "Side-by-Side Layout", View, None, "Place the two panes beside each other.";
    OverUnderLayout, "Over-Under Layout", View, None, "Place the two panes one above the other.";
    Webpages, "Webpages", View, None, "Render the two sides as web pages.";
    Thumbnail, "Thumbnail", View, None, "Show or hide the overview strip.";
    ToggleLineDetails, "Text Details", View, None, "Show or hide the line details area under the panes.";
    HexDetails, "Hex Details", View, None, "Show or hide the byte view of the current line.";
    AlignmentDetails, "Alignment Details", View, None, "Show or hide the alignment of the current line pair.";
    Ruler, "Ruler", View, None, "Show or hide the column ruler.";
    FileInfo, "File Info", View, None, "Show or hide the file information line.";
    Columns, "Columns", View, None, "Choose which columns the panes show.";
    Legend, "Legend", View, None, "Show or hide the color legend.";
    ToggleLog, "Log", View, None, "Show or hide the operation log.";
    ToggleToolbar, "Toolbar", View, None, "Show or hide the toolbar.";
    IncreaseFontSize, "Increase Display Font Size", View, Some("Ctrl+Plus"), "Enlarge the display font.";
    DecreaseFontSize, "Decrease Display Font Size", View, Some("Ctrl+Minus"), "Shrink the display font.";
    ResetFontSize, "Reset Display Font Size", View, Some("Ctrl+0"), "Return the display font to its configured size.";

    // Actions.
    OpenFolder, "Open Folder", Actions, None, "Open the selected folder on both sides.";
    OpenSubfolders, "Open Subfolders", Actions, None, "Open every folder under the selection.";
    CloseSubfolders, "Close Subfolders", Actions, None, "Close every folder under the selection.";
    SetAsBaseFolders, "Set as Base Folders", Actions, None, "Make the selected folders the two base folders.";
    OpenInNewView, "Open in New View", Actions, None, "Open the selection in a tab of its own.";
    CompareContents, "Compare Contents", Actions, None, "Start a content comparison over the paired files.";
    CopyToOtherSide, "Copy to Other Side", Actions, None, "Copy the selection to the opposite side.";
    MoveToOtherSide, "Move to Other Side", Actions, None, "Move the selection to the opposite side.";
    CopyToFolder, "Copy to Folder...", Actions, None, "Copy the selection into a chosen folder.";
    MoveToFolder, "Move to Folder...", Actions, None, "Move the selection into a chosen folder.";
    Delete, "Delete", Actions, None, "Remove the selection.";
    Rename, "Rename", Actions, None, "Give the selection new names in place.";
    Attributes, "Attributes", Actions, None, "Write attributes to the selection.";
    Touch, "Touch", Actions, None, "Write timestamps to the selection.";
    Exclude, "Exclude", Actions, None, "Add the selection to the session's name filters.";
    NewFolder, "New Folder", Actions, None, "Create one folder.";
    CopyFilename, "Copy Filename", Actions, None, "Put the selected names on the clipboard.";
    RefreshSelection, "Refresh Selection", Actions, None, "Read the selected items again.";
    Synchronize, "Synchronize", Actions, None, "Reconcile the two sides by copies and deletions.";
    Exchange, "Exchange", Actions, None, "Move each side's selection to the other side.";

    // Actions the three way merge adds, in the order the merge view declares
    // them. The shared bar builds its Actions menu from that declaration, so
    // the order here is the order the menu and the toolbar show.
    TakeLeft, "Take Left", Actions, None, "Put the left side's content in the output for the current section.";
    TakeCenter, "Take Center", Actions, None, "Put the ancestor's content in the output for the current section.";
    TakeRight, "Take Right", Actions, None, "Put the right side's content in the output for the current section.";
    TakeLeftThenRight, "Take Left Then Right", Actions, None, "Put both sides in the output, left first.";
    TakeRightThenLeft, "Take Right Then Left", Actions, None, "Put both sides in the output, right first.";
    TakeAllNonConflicting, "Take All Non-Conflicting", Actions, None, "Resolve every section that is not a conflict.";
    FavorLeft, "Favor Left Changes", Actions, None, "Paint the changes only the left side made as unchanged in the output pane, or stop doing so.";
    FavorRight, "Favor Right Changes", Actions, None, "Paint the changes only the right side made as unchanged in the output pane, or stop doing so.";
    NextConflict, "Next Conflict Section", Actions, None, "Move to the next section that needs review.";
    PreviousConflict, "Previous Conflict Section", Actions, None, "Move to the previous section that needs review.";
    ClearConflictNext, "Clear Conflict Section, Next", Actions, None, "Accept the current section and move to the next conflict.";
    ToggleCenterPane, "Center Pane", Actions, None, "Show or hide the ancestor pane.";
    MergeInfo, "Text Merge Info", Actions, None, "Show what the merge found.";
    CompareToOutput, "Compare to Output", Actions, None, "Open a comparison of one input against the output.";
    MergeFolders, "Merge", Actions, None, "Write the merge result of the selection into the output folder.";
    CopyToOutput, "Copy to Output", Actions, None, "Copy the selection from the active input into the output folder.";
    OpenTextMerge, "Open in Text Merge", Actions, None, "Open the current file in a three way text merge.";
    TakeLeftLine, "Take Left Line", Actions, None, "Put the left side's version of the current line in the output.";
    TakeCenterLine, "Take Center Line", Actions, None, "Put the ancestor's version of the current line in the output.";
    TakeRightLine, "Take Right Line", Actions, None, "Put the right side's version of the current line in the output.";
    NextLeftTaken, "Next Left Taken", Actions, None, "Move to the next run of lines taken from the left side.";
    PreviousLeftTaken, "Previous Left Taken", Actions, None, "Move to the previous run of lines taken from the left side.";
    NextRightTaken, "Next Right Taken", Actions, None, "Move to the next run of lines taken from the right side.";
    PreviousRightTaken, "Previous Right Taken", Actions, None, "Move to the previous run of lines taken from the right side.";

    // Actions the patch view adds.
    ApplyPatch, "Apply Patch", Actions, None, "Write the patched result over the file the patch applies to.";
    NextDifferenceFiles, "Next Difference Files", Actions, None, "Show the next file pair the patch holds.";
    PreviousDifferenceFiles, "Previous Difference Files", Actions, None, "Show the previous file pair the patch holds.";

    // Tools.
    Options, "Options...", Tools, None, "Edit the program settings.";
    FileFormats, "File Formats...", Tools, None, "Edit the file format rules.";
    Profiles, "Profiles...", Tools, None, "Edit the stored settings profiles.";
    ExportSettings, "Export Settings...", Tools, None, "Write the settings to a file.";
    ImportSettings, "Import Settings...", Tools, None, "Read settings from a file.";
    RestoreFactoryDefaults, "Restore Factory Defaults...", Tools, None, "Return every setting to its built in value.";
    SaveSnapshot, "Save Snapshot...", Tools, None, "Store a copy of a folder tree for later comparison.";
    EditTextFile, "Edit Text File", Tools, None, "Open one file in the text editor.";
    ViewPatch, "View Patch...", Tools, None, "Open a patch file.";

    // Help.
    HelpContents, "Contents", Help, None, "Open the help.";
    ContextHelp, "Context Sensitive Help", Help, Some("F1"), "Open the help at the active part of the window.";
    OnTheWeb, "On the Web", Help, None, "Open the project page in a browser.";
    CheckForUpdates, "Check for Updates", Help, None, "Ask whether a newer build exists.";
    Support, "Support", Help, None, "Open the support page in a browser.";
    About, "About", Help, None, "Show the build information.";

    // Reached by a keystroke or a toolbar only.
    Cancel, "Cancel", Hidden, Some("Esc"), "Abandon the work in progress.";
    NextDisplayMode, "Next Display Mode", Hidden, None, "Move to the next way the difference pane renders.";
    ZoomIn, "Zoom In", Hidden, None, "Magnify one step in.";
    ZoomOut, "Zoom Out", Hidden, None, "Magnify one step out.";
    ZoomToFit, "Zoom to Fit", Hidden, None, "Choose the magnification that shows the whole image.";
    ActualSize, "Actual Size", Hidden, None, "Return to one screen pixel per image pixel.";
    RotateClockwise, "Rotate Clockwise", Hidden, None, "Turn the active side a quarter turn clockwise.";
    RotateCounterclockwise, "Rotate Counterclockwise", Hidden, None, "Turn the active side a quarter turn counterclockwise.";
    FlipHorizontal, "Flip Horizontal", Hidden, None, "Reflect the active side across the vertical axis.";
    FlipVertical, "Flip Vertical", Hidden, None, "Reflect the active side across the horizontal axis.";
    NudgeOffsetLeft, "Nudge Offset Left", Hidden, None, "Move the displacement one step left.";
    NudgeOffsetRight, "Nudge Offset Right", Hidden, None, "Move the displacement one step right.";
    NudgeOffsetUp, "Nudge Offset Up", Hidden, None, "Move the displacement one step up.";
    NudgeOffsetDown, "Nudge Offset Down", Hidden, None, "Move the displacement one step down.";
    ResetOffset, "Reset Offset", Hidden, None, "Return the displacement to zero.";
    ToggleAutoScale, "Auto Scale", Hidden, None, "Enlarge the smaller image to the larger one's scale, or stop doing so.";
}

impl Command {
    /// The label the bar of `view` shows.
    ///
    /// A few commands are named for the content they act on, so the same
    /// command reads differently in the two compare types.
    #[must_use]
    pub const fn label_in(self, view: MenuView) -> &'static str {
        match (self, view) {
            (Self::Reload, MenuView::Folder) => "Refresh",
            (Self::Recompare, MenuView::Folder) => "Full Refresh",
            (Self::CompareReport, MenuView::Folder) => "Folder Compare Report...",
            (Self::CompareInfo, MenuView::Folder) => "Folder Compare Info",
            (Self::Find, MenuView::Folder) => "Find Filename...",
            (Self::FindNext, MenuView::Folder) => "Find Next Filename",
            (Self::FindPrevious, MenuView::Folder) => "Find Previous Filename",
            (Self::CompareContents, MenuView::Folder) => "Compare Contents...",
            (Self::CopyToOtherSide, MenuView::Folder) => "Copy to Side...",
            (Self::MoveToOtherSide, MenuView::Folder) => "Move to Side...",
            (Self::Delete, MenuView::Folder) => "Delete...",
            (Self::Rename, MenuView::Folder) => "Rename...",
            (Self::Attributes, MenuView::Folder) => "Attributes...",
            (Self::Touch, MenuView::Folder) => "Touch...",
            (Self::NewFolder, MenuView::Folder) => "New Folder...",
            (Self::CompareReport, MenuView::Table) => "Table Compare Report...",
            (Self::CompareInfo, MenuView::Table) => "Table Compare Info",
            (Self::CompareInfo, MenuView::Registry) => "Registry Compare Info",
            (Self::CompareReport, MenuView::Version) => "Version Compare Report...",
            (Self::CompareInfo, MenuView::Version) => "Version Compare Info",
            (Self::CompareReport, MenuView::Media) => "Media Compare Report...",
            (Self::CompareInfo, MenuView::Media) => "Media Compare Info",
            (Self::ToggleLineDetails, MenuView::Text | MenuView::Merge | MenuView::Other) => {
                "Line Details"
            }
            (Self::ToggleLineDetails, MenuView::Table) => "Text Details",
            (Self::ToggleLineNumbers, MenuView::Table) => "Row Numbers",
            (Self::ShowDifferences, MenuView::Merge) => "Show Changes",
            (Self::ShowSame, MenuView::Merge) => "Show Unchanged",
            _ => self.label(),
        }
    }

    /// The keystroke the bar of `view` shows beside the label.
    #[must_use]
    pub const fn shortcut_in(self, view: MenuView) -> Option<&'static str> {
        match (self, view) {
            (Self::TakeLeft, MenuView::Merge) => Some("Ctrl+L"),
            (Self::TakeRight, MenuView::Merge) => Some("Ctrl+R"),
            (Self::TakeLeftThenRight, MenuView::Merge) => Some("Ctrl+B"),
            (Self::TakeRightThenLeft, MenuView::Merge) => Some("Ctrl+Shift+B"),
            (Self::TakeLeftLine, MenuView::Merge) => Some("Ctrl+Shift+L"),
            (Self::TakeRightLine, MenuView::Merge) => Some("Ctrl+Shift+R"),
            (Self::NextConflict, MenuView::Merge) => Some("Alt+N"),
            (Self::PreviousConflict, MenuView::Merge) => Some("Alt+P"),
            (Self::NextDifference, MenuView::Folder) => Some("Ctrl+N"),
            (Self::PreviousDifference, MenuView::Folder) => Some("Ctrl+P"),
            (Self::SelectAllFiles, MenuView::Folder) => Some("Ctrl+Shift+A"),
            (Self::RefreshSelection, MenuView::Folder) => Some("Shift+F5"),
            (Self::Rename, MenuView::Folder) => Some("F2"),
            (Self::NewFolder, MenuView::Folder) => Some("Ins"),
            (Self::Legend, MenuView::Folder) => Some("Ctrl+Alt+L"),
            (Self::Back, MenuView::Folder) => Some("Alt+Left"),
            (Self::Forward, MenuView::Folder) => Some("Alt+Right"),
            (Self::CompareInfo, MenuView::Table)
            | (
                Self::NextSection
                | Self::PreviousSection
                | Self::AlignWith
                | Self::Undo
                | Self::Redo
                | Self::Cut
                | Self::Copy
                | Self::Paste
                | Self::SelectSection
                | Self::CopyToRight
                | Self::CopyToLeft
                | Self::CopyLineToRight
                | Self::CopyLineToLeft
                | Self::IncreaseIndent
                | Self::DecreaseIndent
                | Self::OpenFile
                | Self::OpenClipboard
                | Self::SaveFile
                | Self::SaveFileAs
                | Self::Replace
                | Self::GoTo
                | Self::ToggleOverwrite
                | Self::NextReplacement
                | Self::PreviousReplacement,
                MenuView::Folder,
            )
            | (
                Self::CopyToRight | Self::CopyToLeft | Self::CopyLineToRight | Self::CopyLineToLeft,
                MenuView::Merge,
            ) => None,
            _ => self.shortcut_text(),
        }
    }
}

/// A key press with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keystroke {
    /// The key itself.
    pub key: egui::Key,
    /// True when a control-equivalent modifier was held.
    pub command: bool,
    /// True when shift was held.
    pub shift: bool,
    /// True when alt was held.
    pub alt: bool,
}

impl Keystroke {
    /// A press of `key` with no modifiers.
    #[must_use]
    pub const fn plain(key: egui::Key) -> Self {
        Self {
            key,
            command: false,
            shift: false,
            alt: false,
        }
    }

    /// A press of `key` with shift.
    #[must_use]
    pub const fn with_shift(key: egui::Key) -> Self {
        Self {
            key,
            command: false,
            shift: true,
            alt: false,
        }
    }

    /// A press of `key` with alt.
    #[must_use]
    pub const fn with_alt(key: egui::Key) -> Self {
        Self {
            key,
            command: false,
            shift: false,
            alt: true,
        }
    }

    /// A press of `key` with the control-equivalent modifier.
    #[must_use]
    pub const fn with_command(key: egui::Key) -> Self {
        Self {
            key,
            command: true,
            shift: false,
            alt: false,
        }
    }

    /// A press of `key` with the control-equivalent modifier and shift.
    #[must_use]
    pub const fn with_command_shift(key: egui::Key) -> Self {
        Self {
            key,
            command: true,
            shift: true,
            alt: false,
        }
    }

    /// A press of `key` with the control-equivalent modifier and alt.
    #[must_use]
    pub const fn with_command_alt(key: egui::Key) -> Self {
        Self {
            key,
            command: true,
            shift: false,
            alt: true,
        }
    }

    /// A press of `key` with the control-equivalent modifier, alt and shift.
    #[must_use]
    pub const fn with_command_alt_shift(key: egui::Key) -> Self {
        Self {
            key,
            command: true,
            shift: true,
            alt: true,
        }
    }
}

impl MenuView {
    /// Every bar, in the order the commands page lists them.
    pub const ALL: &'static [MenuView] = &[
        MenuView::Text,
        MenuView::Table,
        MenuView::Folder,
        MenuView::Merge,
        MenuView::Registry,
        MenuView::Version,
        MenuView::Media,
        MenuView::Other,
    ];

    /// The stable name a stored document carries this bar under.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            MenuView::Text => "text",
            MenuView::Table => "table",
            MenuView::Folder => "folder",
            MenuView::Merge => "merge",
            MenuView::Registry => "registry",
            MenuView::Version => "version",
            MenuView::Media => "media",
            MenuView::Other => "other",
        }
    }

    /// The name the commands page shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            MenuView::Text => "File comparisons",
            MenuView::Table => "Table comparisons",
            MenuView::Folder => "Folder comparisons",
            MenuView::Merge => "Text merges",
            MenuView::Registry => "Registry comparisons",
            MenuView::Version => "Version comparisons",
            MenuView::Media => "Media comparisons",
            MenuView::Other => "Every other view",
        }
    }

    /// The bar stored under `id`, where this build has one.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|view| view.id() == id)
    }
}

/// One modifier's text and the flag it sets.
type Modifier = (&'static str, fn(&mut Keystroke));

/// Text form of the modifier keys, in the order a keystroke prints them.
const MODIFIER_NAMES: [Modifier; 3] = [
    ("Ctrl", |stroke| stroke.command = true),
    ("Alt", |stroke| stroke.alt = true),
    ("Shift", |stroke| stroke.shift = true),
];

impl Keystroke {
    /// The stored text of the keystroke, such as `Ctrl+Shift+N`.
    #[must_use]
    pub fn to_text(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.command {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        let key = self.key.name();
        parts.push(key);
        parts.join("+")
    }

    /// Reads a keystroke from its stored text, or nothing when the text names
    /// no key this build has.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let mut stroke = Keystroke::plain(egui::Key::Space);
        let mut key = None;
        for part in text
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            match MODIFIER_NAMES
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(part))
            {
                Some((_, apply)) => apply(&mut stroke),
                None => key = Some(egui::Key::from_name(part)?),
            }
        }
        stroke.key = key?;
        Some(stroke)
    }
}

/// The routing table in force, built from the built-in bindings and whatever
/// the options document states.
///
/// A command with a stated binding loses every built-in stroke it had, so a
/// rebound key stops answering for its old command in that bar.
#[derive(Debug, Clone, Default)]
pub struct ShortcutTable {
    /// One entry per bar: the strokes that reach each command.
    per_view: std::collections::BTreeMap<&'static str, Vec<(Keystroke, Command)>>,
}

impl ShortcutTable {
    /// The built-in bindings with nothing customized.
    #[must_use]
    pub fn built_in() -> Self {
        Self::from_options(&ca_session::options::CommandOptions::default())
    }

    /// The bindings a stored document states, over the built-in ones.
    #[must_use]
    pub fn from_options(options: &ca_session::options::CommandOptions) -> Self {
        let mut per_view = std::collections::BTreeMap::new();
        for view in MenuView::ALL {
            let mut table = bindings(*view);
            let inherited = matches!(view, MenuView::Version | MenuView::Media)
                .then(|| options.view(MenuView::Other.id()))
                .flatten();
            for stated in inherited.into_iter().chain(options.view(view.id())) {
                for (command, keys) in &stated.shortcuts {
                    let Some(command) = Command::from_id(command) else {
                        continue;
                    };
                    table.retain(|(_, held)| *held != command);
                    for text in keys {
                        if let Some(stroke) = Keystroke::from_text(text) {
                            table.retain(|(held, _)| *held != stroke);
                            table.push((stroke, command));
                        }
                    }
                }
            }
            per_view.insert(view.id(), table);
        }
        Self { per_view }
    }

    /// Every stroke that reaches `command` in `view`.
    #[must_use]
    pub fn shortcuts_of(&self, view: MenuView, command: Command) -> Vec<Keystroke> {
        self.per_view
            .get(view.id())
            .map(|table| {
                table
                    .iter()
                    .filter(|(_, held)| *held == command)
                    .map(|(stroke, _)| *stroke)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The command `stroke` already reaches in `view`, where one does.
    #[must_use]
    pub fn claimed_by(&self, view: MenuView, stroke: Keystroke) -> Option<Command> {
        self.per_view
            .get(view.id())?
            .iter()
            .find(|(held, _)| *held == stroke)
            .map(|(_, command)| *command)
    }

    /// Every binding of one bar, for the tests and for the commands page.
    #[must_use]
    pub fn bindings_of(&self, view: MenuView) -> &[(Keystroke, Command)] {
        self.per_view.get(view.id()).map_or(&[][..], Vec::as_slice)
    }
}

/// The command a keystroke asks for in `view` under the built-in bindings.
///
/// Alt is reserved for the menu bar, so a binding fires under alt only where
/// the observed shortcut carries it.
#[must_use]
pub fn route(view: MenuView, stroke: Keystroke) -> Option<Command> {
    shared(stroke).or_else(|| match view {
        MenuView::Text
        | MenuView::Table
        | MenuView::Registry
        | MenuView::Version
        | MenuView::Media
        | MenuView::Other => text(stroke),
        MenuView::Folder => folder(stroke),
        MenuView::Merge => merge(stroke).or_else(|| text(stroke)),
    })
}

/// The command a keystroke asks for in `view` under a customized table.
///
/// The window routes through this so a rebound key takes effect on the next
/// frame rather than at the next start.
#[must_use]
pub fn route_with(view: MenuView, stroke: Keystroke, table: &ShortcutTable) -> Option<Command> {
    table.claimed_by(view, stroke)
}

/// The bindings every bar carries.
fn shared(stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if stroke.alt {
        return None;
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, false, Key::T) => Some(Command::NewTab),
        (true, false, Key::W) => Some(Command::CloseTab),
        (true, false, Key::Q) => Some(Command::Exit),
        (true, true, Key::O) => Some(Command::OpenSession),
        (true, true, Key::S) => Some(Command::SaveSession),
        (true, true, Key::C) => Some(Command::ClearSession),
        (true, false, Key::I) => Some(Command::CompareInfo),
        (true, false, Key::F5) => Some(Command::Recompare),
        (false, false, Key::F5) => Some(Command::Reload),
        (true, false, Key::A) => Some(Command::SelectAll),
        (true, false, Key::F) => Some(Command::Find),
        (false, false, Key::F3) => Some(Command::FindNext),
        (false, true, Key::F3) => Some(Command::FindPrevious),
        (false, false, Key::Escape) => Some(Command::Cancel),
        (false, false, Key::F1) => Some(Command::ContextHelp),
        (true, false, Key::Plus | Key::Equals) => Some(Command::IncreaseFontSize),
        (true, false, Key::Minus) => Some(Command::DecreaseFontSize),
        (true, false, Key::Num0) => Some(Command::ResetFontSize),
        _ => None,
    }
}

/// The bindings the text comparison bar adds.
fn text(stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if stroke.alt {
        return match (stroke.command, stroke.shift, stroke.key) {
            (true, true, Key::N) => Some(Command::NextReplacement),
            (true, true, Key::P) => Some(Command::PreviousReplacement),
            _ => None,
        };
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, true, Key::N) => Some(Command::NextDifference),
        (true, true, Key::P) => Some(Command::PreviousDifference),
        (true, false, Key::N) => Some(Command::NextSection),
        (true, false, Key::P) => Some(Command::PreviousSection),
        (true, false, Key::O) => Some(Command::OpenFile),
        (true, true, Key::V) => Some(Command::OpenClipboard),
        (true, false, Key::S) => Some(Command::SaveFile),
        (false, false, Key::F12) => Some(Command::SaveFileAs),
        (true, false, Key::Z) => Some(Command::Undo),
        (true, false, Key::Y) => Some(Command::Redo),
        (true, false, Key::X) => Some(Command::Cut),
        (true, false, Key::C) => Some(Command::Copy),
        (true, false, Key::V) => Some(Command::Paste),
        (true, false, Key::D) => Some(Command::SelectSection),
        (false, false, Key::F7) => Some(Command::AlignWith),
        (true, false, Key::R) => Some(Command::CopyToRight),
        (true, false, Key::L) => Some(Command::CopyToLeft),
        (true, true, Key::R) => Some(Command::CopyLineToRight),
        (true, true, Key::L) => Some(Command::CopyLineToLeft),
        (true, true, Key::I) => Some(Command::IncreaseIndent),
        (true, true, Key::U) => Some(Command::DecreaseIndent),
        (true, false, Key::H) => Some(Command::Replace),
        (true, false, Key::G) => Some(Command::GoTo),
        (false, false, Key::Insert) => Some(Command::ToggleOverwrite),
        _ => None,
    }
}

/// The bindings the merge bar puts ahead of the text comparison bar.
///
/// The take keys reuse the strokes the text bar gives the copy commands, so a
/// merge answers them before the text table is asked.
fn merge(stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if stroke.alt {
        return match (stroke.command, stroke.shift, stroke.key) {
            (false, false, Key::N) => Some(Command::NextConflict),
            (false, false, Key::P) => Some(Command::PreviousConflict),
            _ => None,
        };
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, false, Key::L) => Some(Command::TakeLeft),
        (true, false, Key::R) => Some(Command::TakeRight),
        (true, false, Key::B) => Some(Command::TakeLeftThenRight),
        (true, true, Key::B) => Some(Command::TakeRightThenLeft),
        (true, true, Key::L) => Some(Command::TakeLeftLine),
        (true, true, Key::R) => Some(Command::TakeRightLine),
        _ => None,
    }
}

/// The bindings the folder comparison bar adds.
fn folder(stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if stroke.alt {
        return match (stroke.command, stroke.shift, stroke.key) {
            (false, false, Key::ArrowLeft) => Some(Command::Back),
            (false, false, Key::ArrowRight) => Some(Command::Forward),
            (true, false, Key::L) => Some(Command::Legend),
            _ => None,
        };
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, false, Key::N) => Some(Command::NextDifference),
        (true, false, Key::P) => Some(Command::PreviousDifference),
        (true, true, Key::A) => Some(Command::SelectAllFiles),
        (false, false, Key::F2) => Some(Command::Rename),
        (false, false, Key::Insert) => Some(Command::NewFolder),
        (false, true, Key::F5) => Some(Command::RefreshSelection),
        _ => None,
    }
}

/// Every keystroke the bar of `view` answers, for the tests and for a future
/// customization surface.
#[must_use]
pub fn bindings(view: MenuView) -> Vec<(Keystroke, Command)> {
    use egui::Key;
    let mut strokes = vec![
        Keystroke::with_command(Key::T),
        Keystroke::with_command(Key::W),
        Keystroke::with_command(Key::Q),
        Keystroke::with_command_shift(Key::O),
        Keystroke::with_command_shift(Key::S),
        Keystroke::with_command_shift(Key::C),
        Keystroke::with_command(Key::I),
        Keystroke::with_command(Key::F5),
        Keystroke::plain(Key::F5),
        Keystroke::with_command(Key::A),
        Keystroke::with_command(Key::F),
        Keystroke::plain(Key::F3),
        Keystroke::with_shift(Key::F3),
        Keystroke::plain(Key::F1),
        Keystroke::plain(Key::Escape),
        Keystroke::with_command(Key::Plus),
        Keystroke::with_command(Key::Equals),
        Keystroke::with_command(Key::Minus),
        Keystroke::with_command(Key::Num0),
    ];
    if view == MenuView::Merge {
        strokes.extend([
            Keystroke::with_command(Key::B),
            Keystroke::with_command_shift(Key::B),
            Keystroke::with_alt(Key::N),
            Keystroke::with_alt(Key::P),
        ]);
    }
    match view {
        MenuView::Text
        | MenuView::Table
        | MenuView::Merge
        | MenuView::Registry
        | MenuView::Version
        | MenuView::Media
        | MenuView::Other => {
            strokes.extend([
                Keystroke::with_command_shift(Key::N),
                Keystroke::with_command_shift(Key::P),
                Keystroke::with_command(Key::N),
                Keystroke::with_command(Key::P),
                Keystroke::with_command(Key::O),
                Keystroke::with_command_shift(Key::V),
                Keystroke::with_command(Key::S),
                Keystroke::plain(Key::F12),
                Keystroke::with_command(Key::Z),
                Keystroke::with_command(Key::Y),
                Keystroke::with_command(Key::X),
                Keystroke::with_command(Key::C),
                Keystroke::with_command(Key::V),
                Keystroke::with_command(Key::D),
                Keystroke::plain(Key::F7),
                Keystroke::with_command(Key::R),
                Keystroke::with_command(Key::L),
                Keystroke::with_command_shift(Key::R),
                Keystroke::with_command_shift(Key::L),
                Keystroke::with_command_shift(Key::I),
                Keystroke::with_command_shift(Key::U),
                Keystroke::with_command(Key::H),
                Keystroke::with_command(Key::G),
                Keystroke::plain(Key::Insert),
                Keystroke::with_command_alt_shift(Key::N),
                Keystroke::with_command_alt_shift(Key::P),
            ]);
        }
        MenuView::Folder => strokes.extend([
            Keystroke::with_command(Key::N),
            Keystroke::with_command(Key::P),
            Keystroke::with_command_shift(Key::A),
            Keystroke::plain(Key::F2),
            Keystroke::plain(Key::Insert),
            Keystroke::with_shift(Key::F5),
            Keystroke::with_alt(Key::ArrowLeft),
            Keystroke::with_alt(Key::ArrowRight),
            Keystroke::with_command_alt(Key::L),
        ]),
    }
    strokes
        .into_iter()
        .filter_map(|stroke| route(view, stroke).map(|command| (stroke, command)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{bindings, route, Command, Keystroke, MenuView};
    use egui::Key;

    #[test]
    fn the_same_stroke_carries_a_different_command_in_each_view() {
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command(Key::N)),
            Some(Command::NextSection)
        );
        assert_eq!(
            route(MenuView::Folder, Keystroke::with_command(Key::N)),
            Some(Command::NextDifference)
        );
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command_shift(Key::N)),
            Some(Command::NextDifference)
        );
        assert_eq!(
            route(MenuView::Folder, Keystroke::with_command_shift(Key::N)),
            None
        );
    }

    #[test]
    fn f7_aligns_in_the_text_view_and_makes_no_folder() {
        assert_eq!(
            route(MenuView::Text, Keystroke::plain(Key::F7)),
            Some(Command::AlignWith)
        );
        assert_eq!(route(MenuView::Folder, Keystroke::plain(Key::F7)), None);
        assert_eq!(
            route(MenuView::Folder, Keystroke::plain(Key::Insert)),
            Some(Command::NewFolder)
        );
        assert_eq!(
            Command::NewFolder.shortcut_in(MenuView::Folder),
            Some("Ins")
        );
    }

    #[test]
    fn reload_and_recompare_differ_only_by_the_modifier_in_both_views() {
        for view in [MenuView::Text, MenuView::Folder] {
            assert_eq!(
                route(view, Keystroke::plain(Key::F5)),
                Some(Command::Reload)
            );
            assert_eq!(
                route(view, Keystroke::with_command(Key::F5)),
                Some(Command::Recompare)
            );
        }
        assert_eq!(Command::Reload.label_in(MenuView::Text), "Reload Files");
        assert_eq!(Command::Reload.label_in(MenuView::Folder), "Refresh");
        assert_eq!(
            Command::Recompare.label_in(MenuView::Folder),
            "Full Refresh"
        );
    }

    #[test]
    fn a_stray_alt_press_is_left_to_the_menu_bar() {
        let mut stroke = Keystroke::with_command(Key::N);
        stroke.alt = true;
        assert_eq!(route(MenuView::Text, stroke), None);
        assert_eq!(route(MenuView::Folder, stroke), None);
        assert_eq!(
            route(MenuView::Folder, Keystroke::with_alt(Key::ArrowLeft)),
            Some(Command::Back)
        );
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command_alt_shift(Key::N)),
            Some(Command::NextReplacement)
        );
    }

    #[test]
    fn the_merge_bar_takes_where_the_text_bar_copies() {
        for (stroke, command) in [
            (Keystroke::with_command(Key::L), Command::TakeLeft),
            (Keystroke::with_command(Key::R), Command::TakeRight),
            (Keystroke::with_command_shift(Key::L), Command::TakeLeftLine),
            (
                Keystroke::with_command_shift(Key::R),
                Command::TakeRightLine,
            ),
            (Keystroke::with_command(Key::B), Command::TakeLeftThenRight),
            (
                Keystroke::with_command_shift(Key::B),
                Command::TakeRightThenLeft,
            ),
            (Keystroke::with_alt(Key::N), Command::NextConflict),
            (Keystroke::with_alt(Key::P), Command::PreviousConflict),
        ] {
            assert_eq!(route(MenuView::Merge, stroke), Some(command), "{stroke:?}");
        }
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command_shift(Key::L)),
            Some(Command::CopyLineToLeft)
        );
        assert_eq!(
            route(MenuView::Merge, Keystroke::with_command(Key::S)),
            Some(Command::SaveFile)
        );
        assert_eq!(
            Command::ShowSame.label_in(MenuView::Merge),
            "Show Unchanged"
        );
        assert_eq!(Command::CopyLineToLeft.shortcut_in(MenuView::Merge), None);
        assert_eq!(
            Command::TakeLeftLine.shortcut_in(MenuView::Merge),
            Some("Ctrl+Shift+L")
        );
    }

    #[test]
    fn an_unbound_stroke_routes_nowhere() {
        assert_eq!(route(MenuView::Text, Keystroke::plain(Key::A)), None);
        assert_eq!(route(MenuView::Text, Keystroke::with_command(Key::J)), None);
    }

    #[test]
    fn both_plus_keys_enlarge_the_font() {
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command(Key::Plus)),
            Some(Command::IncreaseFontSize)
        );
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command(Key::Equals)),
            Some(Command::IncreaseFontSize)
        );
    }

    #[test]
    fn no_two_bindings_of_one_view_claim_the_same_stroke() {
        for view in MenuView::ALL.iter().copied() {
            let table = bindings(view);
            for (index, (stroke, _)) in table.iter().enumerate() {
                for (other, _) in table.iter().skip(index + 1) {
                    assert_ne!(stroke, other, "{stroke:?} is bound twice in {view:?}");
                }
            }
        }
    }

    /// Every shortcut a bar prints has to be a stroke that bar routes.
    #[test]
    fn a_printed_shortcut_is_a_stroke_that_view_routes() {
        for view in [MenuView::Text, MenuView::Folder, MenuView::Merge] {
            let table = bindings(view);
            for command in Command::ALL {
                let Some(printed) = command.shortcut_in(view) else {
                    continue;
                };
                assert!(
                    table.iter().any(|(_, bound)| bound == command),
                    "{view:?} prints {printed} for {command:?}, which nothing routes"
                );
            }
        }
    }

    #[test]
    fn a_keystroke_round_trips_through_its_stored_text() {
        for stroke in [
            Keystroke::plain(Key::F5),
            Keystroke::with_command(Key::N),
            Keystroke::with_command_shift(Key::P),
            Keystroke::with_command_alt_shift(Key::N),
            Keystroke::with_alt(Key::ArrowLeft),
        ] {
            let text = stroke.to_text();
            assert_eq!(Keystroke::from_text(&text), Some(stroke), "{text}");
        }
        assert_eq!(Keystroke::with_command(Key::N).to_text(), "Ctrl+N");
        assert_eq!(Keystroke::from_text("Nonsense"), None);
        assert_eq!(Keystroke::from_text("Ctrl"), None);
    }

    #[test]
    fn every_command_has_a_stable_name_that_reads_back() {
        for command in Command::ALL {
            assert_eq!(Command::from_id(command.id()), Some(*command));
            assert!(!command.description().is_empty(), "{command:?}");
        }
        assert_eq!(Command::from_id("NoSuchCommand"), None);
    }

    #[test]
    fn the_built_in_table_routes_what_the_pure_function_routes() {
        use super::ShortcutTable;
        let table = ShortcutTable::built_in();
        for view in MenuView::ALL.iter().copied() {
            for (stroke, command) in bindings(view) {
                assert_eq!(
                    super::route_with(view, stroke, &table),
                    Some(command),
                    "{stroke:?} in {view:?}"
                );
                assert_eq!(route(view, stroke), Some(command));
            }
        }
    }

    #[test]
    fn a_rebound_key_routes_to_its_command_and_the_old_key_routes_nowhere() {
        use super::ShortcutTable;
        let mut options = ca_session::options::CommandOptions::default();
        options.set_shortcuts(
            MenuView::Text.id(),
            Command::FindNext.id(),
            vec!["Ctrl+Alt+J".to_owned()],
        );
        let table = ShortcutTable::from_options(&options);
        assert_eq!(
            super::route_with(MenuView::Text, Keystroke::with_command_alt(Key::J), &table),
            Some(Command::FindNext)
        );
        assert_eq!(
            super::route_with(MenuView::Text, Keystroke::plain(Key::F3), &table),
            None,
            "the built-in key still answers"
        );
        assert_eq!(
            super::route_with(MenuView::Folder, Keystroke::plain(Key::F3), &table),
            Some(Command::FindNext),
            "the other bar was changed too"
        );
    }

    #[test]
    fn version_and_media_shortcuts_inherit_every_other_view_bindings() {
        use super::ShortcutTable;
        let mut options = ca_session::options::CommandOptions::default();
        options.set_shortcuts(
            MenuView::Other.id(),
            Command::Recompare.id(),
            vec!["Ctrl+Shift+Y".to_owned()],
        );
        let table = ShortcutTable::from_options(&options);
        let rebound = Keystroke::with_command_shift(egui::Key::Y);

        for view in [MenuView::Version, MenuView::Media] {
            assert_eq!(table.shortcuts_of(view, Command::Recompare), [rebound]);
            assert_eq!(
                super::route_with(view, rebound, &table),
                Some(Command::Recompare)
            );
        }
        assert_eq!(
            table.shortcuts_of(MenuView::Version, Command::Recompare),
            [rebound]
        );
    }

    #[test]
    fn an_unbound_command_keeps_no_stroke() {
        use super::ShortcutTable;
        let mut options = ca_session::options::CommandOptions::default();
        options.set_shortcuts(MenuView::Text.id(), Command::FindNext.id(), Vec::new());
        let table = ShortcutTable::from_options(&options);
        assert!(table
            .shortcuts_of(MenuView::Text, Command::FindNext)
            .is_empty());
        assert_eq!(
            super::route_with(MenuView::Text, Keystroke::plain(Key::F3), &table),
            None
        );
    }

    #[test]
    fn every_bar_reads_back_from_its_stored_name() {
        for view in MenuView::ALL {
            assert_eq!(MenuView::from_id(view.id()), Some(*view));
            assert!(!view.label().is_empty());
        }
        assert_eq!(MenuView::from_id("picture"), None);
        assert_eq!(
            route(MenuView::Registry, Keystroke::with_command(Key::Z)),
            Some(Command::Undo),
            "the registry bar routes the edit keys of the text bar"
        );
    }

    #[test]
    fn mit_distribution_has_no_activation_command() {
        assert!(Command::ALL
            .iter()
            .all(|command| command.label() != "Enter Key..."));
    }

    #[test]
    fn every_command_carries_a_label() {
        for command in Command::ALL {
            assert!(!command.label().is_empty(), "{command:?}");
            assert!(
                !command.label_in(MenuView::Folder).is_empty(),
                "{command:?}"
            );
        }
    }
}
