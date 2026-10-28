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
    /// The folder synchronization bar: the folder comparison bar without its
    /// copy keys.
    FolderSync,
    /// The three way merge bar: the shared bar with the merge's own Edit and
    /// View menus and take keys.
    Merge,
    /// The folder merge bar: the three way merge bar, with Ctrl+N and Ctrl+P
    /// moving between differences in place of sections.
    FolderMerge,
    /// The registry comparison bar: the shared bar with an Edit menu of
    /// registry keys and values.
    Registry,
    /// The version resource comparison bar.
    Version,
    /// The media metadata comparison bar.
    Media,
    /// The byte comparison bar: the shared bar with byte wording for the
    /// difference steps.
    Hex,
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
    Recompare, "Recompare Files", Session, Some("Ctrl+F5"), "Read every side from disk again and compare from the start.";
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
    SessionHome, "Session Home", Hidden, Some("Alt+Home"), "Show the launcher tab.";
    TabNext, "Next Tab", Hidden, Some("Ctrl+Tab"), "Activate the tab to the right, wrapping at the end.";
    TabPrevious, "Previous Tab", Hidden, Some("Ctrl+Shift+Tab"), "Activate the tab to the left, wrapping at the start.";
    NextExistingLine, "Next Existing Line", Hidden, Some("Alt+Down"), "Move to the next row that has a line on the active side.";
    PreviousExistingLine, "Previous Existing Line", Hidden, Some("Alt+Up"), "Move to the previous row that has a line on the active side.";

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
    ToggleFullscreen, "Full Screen", View, Some("F11"), "Toggle full screen in Picture Compare.";
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
        let view = view.folder_bar();
        match (self, view) {
            (Self::Reload, MenuView::Folder | MenuView::FolderMerge) => "Refresh",
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
            (
                Self::ToggleLineDetails,
                MenuView::Text | MenuView::Merge | MenuView::FolderMerge | MenuView::Other,
            ) => "Line Details",
            (Self::ToggleLineDetails, MenuView::Table) => "Text Details",
            (Self::ToggleLineNumbers, MenuView::Table) => "Row Numbers",
            (Self::ShowDifferences, MenuView::Merge | MenuView::FolderMerge) => "Show Changes",
            (Self::ShowSame, MenuView::Merge | MenuView::FolderMerge) => "Show Unchanged",
            (Self::NextSection, MenuView::Table) => "Next Difference Row",
            (Self::PreviousSection, MenuView::Table) => "Previous Difference Row",
            (Self::NextDifference, MenuView::Table) => "Next Difference Cell",
            (Self::PreviousDifference, MenuView::Table) => "Previous Difference Cell",
            (Self::NextSection, MenuView::Hex) => "Next Difference Bytes",
            (Self::PreviousSection, MenuView::Hex) => "Previous Difference Bytes",
            (Self::Reload, MenuView::Registry) => "Reload",
            (Self::Recompare, MenuView::Registry) => "Recompare",
            (Self::OpenFile, MenuView::Registry) => "Open...",
            _ => self.label(),
        }
    }

    /// The keystroke the bar of `view` shows beside the label.
    #[must_use]
    pub const fn shortcut_in(self, view: MenuView) -> Option<&'static str> {
        if matches!(view, MenuView::FolderSync)
            && matches!(self, Self::CopyToRight | Self::CopyToLeft)
        {
            return None;
        }
        let view = view.folder_bar();
        match (self, view) {
            (Self::NextExistingLine | Self::PreviousExistingLine, _)
                if !matches!(view, MenuView::Text | MenuView::Table | MenuView::Merge) =>
            {
                None
            }
            (Self::TakeLeft, MenuView::Merge | MenuView::FolderMerge) => Some("Ctrl+L"),
            (Self::TakeRight, MenuView::Merge | MenuView::FolderMerge) => Some("Ctrl+R"),
            (Self::TakeLeftThenRight, MenuView::Merge | MenuView::FolderMerge) => Some("Ctrl+B"),
            (Self::TakeRightThenLeft, MenuView::Merge | MenuView::FolderMerge) => {
                Some("Ctrl+Shift+B")
            }
            (Self::TakeLeftLine, MenuView::Merge | MenuView::FolderMerge)
            | (Self::CopyCellToLeft, MenuView::Table) => Some("Ctrl+Shift+L"),
            (Self::TakeRightLine, MenuView::Merge | MenuView::FolderMerge)
            | (Self::CopyCellToRight, MenuView::Table) => Some("Ctrl+Shift+R"),
            (Self::NextConflict, MenuView::Merge | MenuView::FolderMerge) => Some("Alt+N"),
            (Self::PreviousConflict, MenuView::Merge | MenuView::FolderMerge) => Some("Alt+P"),
            (
                Self::NextDifference,
                MenuView::Folder
                | MenuView::FolderMerge
                | MenuView::Registry
                | MenuView::Version
                | MenuView::Media,
            ) => Some("Ctrl+N"),
            (
                Self::PreviousDifference,
                MenuView::Folder
                | MenuView::FolderMerge
                | MenuView::Registry
                | MenuView::Version
                | MenuView::Media,
            ) => Some("Ctrl+P"),
            (Self::SelectAllFiles, MenuView::Merge | MenuView::FolderMerge | MenuView::Folder) => {
                Some("Ctrl+Shift+A")
            }
            (Self::RefreshSelection, MenuView::Folder) => Some("Shift+F5"),
            (Self::Rename, MenuView::Folder | MenuView::Registry) => Some("F2"),
            (Self::NewFolder, MenuView::Folder) | (Self::NewValue, MenuView::Registry) => {
                Some("Ins")
            }
            (Self::MergeInfo, MenuView::Merge) => Some("Ctrl+I"),
            (Self::Legend, MenuView::Folder) => Some("Ctrl+Alt+L"),
            (Self::Back, MenuView::Folder) => Some("Alt+Left"),
            (Self::Forward, MenuView::Folder) => Some("Alt+Right"),
            (Self::CompareInfo, MenuView::Table | MenuView::Merge)
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
                MenuView::Merge | MenuView::FolderMerge,
            )
            | (Self::CopyLineToRight | Self::CopyLineToLeft, MenuView::Table)
            | (
                Self::NextSection | Self::PreviousSection,
                MenuView::FolderMerge | MenuView::Registry | MenuView::Version | MenuView::Media,
            )
            | (Self::ToggleOverwrite, MenuView::Registry) => None,
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

    /// A press of `key` with alt and shift.
    #[must_use]
    pub const fn with_alt_shift(key: egui::Key) -> Self {
        Self {
            key,
            command: false,
            shift: true,
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

    /// The pressed keys among `events`, each with its index in the list.
    #[must_use]
    pub fn pressed_in(events: &[egui::Event]) -> Vec<(usize, Self)> {
        events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => Some((
                    index,
                    Self {
                        key: *key,
                        command: modifiers.command,
                        shift: modifiers.shift,
                        alt: modifiers.alt,
                    },
                )),
                _ => None,
            })
            .collect()
    }
}

/// How the windowing backend of a host reports a typed character's modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyboardHost {
    /// Alt Gr arrives with neither Ctrl nor Alt, Ctrl+Alt still types, and an
    /// Alt menu chord delivers its letter as text.
    Windows,
    /// Option composes characters.
    Mac,
    /// The text is the symbol the key map gives for the held level, and a key
    /// map may report Alt Gr as Alt.
    Xkb,
}

impl KeyboardHost {
    const CURRENT: Self = if cfg!(target_os = "windows") {
        Self::Windows
    } else if cfg!(target_os = "macos") {
        Self::Mac
    } else {
        Self::Xkb
    };

    const fn types_text(self, modifiers: egui::Modifiers) -> bool {
        let control = modifiers.command || modifiers.ctrl;
        match self {
            Self::Windows if modifiers.alt => modifiers.ctrl,
            Self::Windows => !control,
            Self::Mac | Self::Xkb => modifiers.alt || !control,
        }
    }
}

/// True when text that arrives with a key held under `modifiers` is typing.
///
/// Ctrl or Command without Alt is a shortcut chord, and some backends deliver
/// its letter as text beside the key press. Ctrl with Alt is how Windows
/// reports Alt Gr, so its text is typed unless a command ran for the key.
/// Alt alone is a menu access chord only on Windows: it composes characters
/// on macOS, and some Linux key maps report Alt Gr as Alt.
#[must_use]
pub const fn chord_types_text(modifiers: egui::Modifiers) -> bool {
    KeyboardHost::CURRENT.types_text(modifiers)
}

/// True for a key whose press never carries text.
///
/// The backend drops control characters and gives navigation and function
/// keys no text, so text after such a press belongs to an earlier key.
const fn types_nothing(key: egui::Key) -> bool {
    use egui::Key;
    matches!(
        key,
        Key::ArrowDown
            | Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::Escape
            | Key::Tab
            | Key::Backspace
            | Key::Enter
            | Key::Insert
            | Key::Delete
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Copy
            | Key::Cut
            | Key::Paste
            | Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
            | Key::F13
            | Key::F14
            | Key::F15
            | Key::F16
            | Key::F17
            | Key::F18
            | Key::F19
            | Key::F20
            | Key::F21
            | Key::F22
            | Key::F23
            | Key::F24
            | Key::F25
            | Key::F26
            | Key::F27
            | Key::F28
            | Key::F29
            | Key::F30
            | Key::F31
            | Key::F32
            | Key::F33
            | Key::F34
            | Key::F35
    )
}

/// The key and modifiers of `event` when it is a key press.
const fn press(event: &egui::Event) -> Option<(egui::Key, egui::Modifiers)> {
    match event {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => Some((*key, *modifiers)),
        _ => None,
    }
}

/// The two key presses a text event may belong to.
///
/// The backend delivers a key press and its text as two events, the text right
/// after the key. When other events come between them, the text belongs to
/// either the last press before it or the last press of a key that can carry
/// text; key releases never own text. A text event is typed only when both
/// candidates allow it, so a press of a key that carries no text, such as an
/// arrow, does not release the text of an earlier chord. Text after a later
/// press of a key that can carry text is that key's text: the event stream
/// carries no link that could pair it with an earlier key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Owners<T> {
    last_press: T,
    last_text_key: T,
}

impl<T: Copy> Owners<T> {
    const fn both(value: T) -> Self {
        Self {
            last_press: value,
            last_text_key: value,
        }
    }

    fn note(&mut self, key: egui::Key, value: T) {
        self.last_press = value;
        if !types_nothing(key) {
            self.last_text_key = value;
        }
    }
}

/// Follows the events of one frame and decides which text events are typed.
#[derive(Clone, Copy, Debug)]
pub struct TextGate {
    host: KeyboardHost,
    typing: Owners<bool>,
}

impl TextGate {
    /// A gate whose text before any key press is typed as `held` decides.
    #[must_use]
    pub const fn new(held: egui::Modifiers) -> Self {
        Self::on(KeyboardHost::CURRENT, held)
    }

    const fn on(host: KeyboardHost, held: egui::Modifiers) -> Self {
        Self {
            host,
            typing: Owners::both(host.types_text(held)),
        }
    }

    /// Notes `event`; true when it is a text event whose text is typed.
    pub fn types(&mut self, event: &egui::Event) -> bool {
        if let Some((key, modifiers)) = press(event) {
            self.typing.note(key, self.host.types_text(modifiers));
        }
        matches!(event, egui::Event::Text(_)) && self.typing.last_press && self.typing.last_text_key
    }

    fn retain_typed(mut self, events: &mut Vec<egui::Event>) {
        events.retain(|event| self.types(event) || !matches!(event, egui::Event::Text(_)));
    }
}

/// Remove the text events that belong to the key events at `keys`.
///
/// A key that ran a command typed nothing, so without this an Alt Gr or Option
/// chord bound to a command would also insert its character into whatever
/// editor reads the frame's events.
pub fn drop_text_of(events: &mut Vec<egui::Event>, keys: &[usize]) {
    if keys.is_empty() {
        return;
    }
    let mut owners: Owners<Option<usize>> = Owners::both(None);
    let mut index = 0;
    events.retain(|event| {
        let position = index;
        index += 1;
        if let Some((key, _)) = press(event) {
            owners.note(key, Some(position));
        }
        let ran = |owner: Option<usize>| owner.is_some_and(|key| keys.contains(&key));
        !matches!(event, egui::Event::Text(_))
            || !(ran(owners.last_press) || ran(owners.last_text_key))
    });
}

/// Remove the text events whose key press is a chord that does not type.
///
/// Windows delivers the letter of an Alt menu chord as text, and text fields
/// insert every text event they read, so this runs before any widget does.
/// Text with no key press before it in the frame is kept.
pub fn drop_chord_text(events: &mut Vec<egui::Event>) {
    TextGate::on(KeyboardHost::CURRENT, egui::Modifiers::NONE).retain_typed(events);
}

impl MenuView {
    /// Every bar, in the order the commands page lists them.
    pub const ALL: &'static [MenuView] = &[
        MenuView::Text,
        MenuView::Table,
        MenuView::Folder,
        MenuView::FolderSync,
        MenuView::Merge,
        MenuView::FolderMerge,
        MenuView::Registry,
        MenuView::Version,
        MenuView::Media,
        MenuView::Hex,
        MenuView::Other,
    ];

    /// The stable name a stored document carries this bar under.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            MenuView::Text => "text",
            MenuView::Table => "table",
            MenuView::Folder => "folder",
            MenuView::FolderSync => "folder-sync",
            MenuView::Merge => "merge",
            MenuView::FolderMerge => "folder-merge",
            MenuView::Registry => "registry",
            MenuView::Version => "version",
            MenuView::Media => "media",
            MenuView::Hex => "hex",
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
            MenuView::FolderSync => "Folder synchronizations",
            MenuView::Merge => "Text merges",
            MenuView::FolderMerge => "Folder merges",
            MenuView::Registry => "Registry comparisons",
            MenuView::Version => "Version comparisons",
            MenuView::Media => "Media comparisons",
            MenuView::Hex => "Byte comparisons",
            MenuView::Other => "Every other view",
        }
    }

    /// The folder comparison bar for a folder synchronization, whose labels
    /// and other keys it shares; every other bar as it is.
    #[must_use]
    pub const fn folder_bar(self) -> Self {
        match self {
            MenuView::FolderSync => MenuView::Folder,
            view => view,
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
            let own = options.view(view.id());
            let inherited = match view {
                MenuView::Version | MenuView::Media | MenuView::Hex => {
                    options.view(MenuView::Other.id())
                }
                MenuView::FolderMerge => options.view(MenuView::Merge.id()),
                MenuView::FolderSync => options.view(MenuView::Folder.id()),
                _ => None,
            };
            // A folder merge takes a text merge binding only for a command it
            // shares and states no binding of its own for: applying both
            // would let the inherited keys take strokes from other commands.
            // A window command has no view to differ in, so it is always shared.
            let inherited = inherited
                .into_iter()
                .flat_map(|stated| &stated.shortcuts)
                .filter(|(command, _)| {
                    *view != MenuView::FolderMerge
                        || (own.is_none_or(|own| !own.shortcuts.contains_key(*command))
                            && Command::from_id(command).is_some_and(|command| {
                                shared_with_folder_merges(command) || is_window_command(command)
                            }))
                });
            let stated = inherited.chain(own.into_iter().flat_map(|stated| &stated.shortcuts));
            for (command, keys) in stated {
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
/// A binding fires under alt only where the observed shortcut carries it, so
/// an alt press that names no command stays free for the menu bar.
#[must_use]
pub fn route(view: MenuView, stroke: Keystroke) -> Option<Command> {
    if view == MenuView::Merge {
        if let Some(command) = merge_info(stroke) {
            return Some(command);
        }
    }
    shared(stroke).or_else(|| match view {
        MenuView::Text => existing_line(stroke).or_else(|| text(stroke)),
        MenuView::Hex | MenuView::Other => text(stroke),
        MenuView::Table => table(stroke)
            .or_else(|| existing_line(stroke))
            .or_else(|| text(stroke)),
        MenuView::Registry | MenuView::Version | MenuView::Media => {
            records(view, stroke).or_else(|| text(stroke))
        }
        MenuView::Folder | MenuView::FolderSync => folder(view, stroke),
        MenuView::Merge => merge(stroke)
            .or_else(|| existing_line(stroke))
            .or_else(|| text(stroke)),
        MenuView::FolderMerge => folder_merge(stroke)
            .or_else(|| merge(stroke))
            .or_else(|| text(stroke)),
    })
}

/// Every command a folder merge view answers for.
pub const FOLDER_MERGE_COMMANDS: &[Command] = &[
    Command::SwapSides,
    Command::Reload,
    Command::Recompare,
    Command::CompareReport,
    Command::SelectAll,
    Command::SelectAllFiles,
    Command::Find,
    Command::FindNext,
    Command::FindPrevious,
    Command::NextDifference,
    Command::PreviousDifference,
    Command::ShowAll,
    Command::ShowDifferences,
    Command::ShowConflicts,
    Command::ShowLeftChanges,
    Command::ShowRightChanges,
    Command::ShowMergeable,
    Command::ShowSame,
    Command::ShowNone,
    Command::ToggleIgnoreSameChanges,
    Command::TakeLeft,
    Command::TakeCenter,
    Command::TakeRight,
    Command::NextConflict,
    Command::PreviousConflict,
    Command::ToggleCenterPane,
    Command::MergeFolders,
    Command::CopyToOutput,
    Command::OpenTextMerge,
    Command::CompareToOutput,
];

/// True for a command a folder merge has in common with a text merge: one
/// the folder merge view answers for, or one every bar routes alike.
fn shared_with_folder_merges(command: Command) -> bool {
    FOLDER_MERGE_COMMANDS.contains(&command)
        || bindings(MenuView::FolderMerge)
            .into_iter()
            .any(|(stroke, held)| held == command && shared(stroke) == Some(command))
}

/// True for a command that acts on the window, not on the content of a view.
fn is_window_command(command: Command) -> bool {
    matches!(command.menu(), Menu::Tools | Menu::Help)
        || matches!(
            command,
            Command::NewSession
                | Command::NewTab
                | Command::NewWindow
                | Command::OpenSession
                | Command::LoadWorkspace
                | Command::SaveWorkspaceAs
                | Command::SaveSession
                | Command::SaveSessionAs
                | Command::SessionSettings
                | Command::ToggleLocked
                | Command::ClearSession
                | Command::CloseTab
                | Command::Exit
                | Command::SessionHome
                | Command::TabNext
                | Command::TabPrevious
        )
}

/// The bindings the folder merge bar puts ahead of the merge bar: a folder
/// merge has no sections, so Ctrl+N and Ctrl+P move between differences as
/// they do in a folder comparison.
fn folder_merge(stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if stroke.alt {
        return None;
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, false, Key::N) => Some(Command::NextDifference),
        (true, false, Key::P) => Some(Command::PreviousDifference),
        _ => None,
    }
}

/// The bindings the table bar puts ahead of the text bar: Ctrl with Shift
/// copies cells where the text bar copies lines.
fn table(stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if stroke.alt {
        return None;
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, true, Key::R) => Some(Command::CopyCellToRight),
        (true, true, Key::L) => Some(Command::CopyCellToLeft),
        _ => None,
    }
}

/// The bindings the record bars put ahead of the text bar: the records have
/// one kind of difference, so Ctrl+N and Ctrl+P step through it.
fn records(view: MenuView, stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if stroke.alt {
        return None;
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, false, Key::N) => Some(Command::NextDifference),
        (true, false, Key::P) => Some(Command::PreviousDifference),
        (false, false, Key::F2) if view == MenuView::Registry => Some(Command::Rename),
        (false, false, Key::Insert) if view == MenuView::Registry => Some(Command::NewValue),
        _ => None,
    }
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
        return match (stroke.command, stroke.shift, stroke.key) {
            (false, false, Key::Home) => Some(Command::SessionHome),
            _ => None,
        };
    }
    match (stroke.command, stroke.shift, stroke.key) {
        (true, false, Key::T) => Some(Command::NewTab),
        (true, false, Key::W | Key::F4) => Some(Command::CloseTab),
        (true, false, Key::Tab) => Some(Command::TabNext),
        (true, true, Key::Tab) => Some(Command::TabPrevious),
        (true, false, Key::Q) => Some(Command::Exit),
        (true, true, Key::O) => Some(Command::OpenSession),
        (true, true, Key::S) => Some(Command::SaveSession),
        (true, true, Key::C) => Some(Command::ClearSession),
        (true, false, Key::I) => Some(Command::CompareInfo),
        (true, false, Key::F5) => Some(Command::Recompare),
        (false, false, Key::F5) => Some(Command::Reload),
        (false, false, Key::F11) => Some(Command::ToggleFullscreen),
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

/// The existing-line moves, bound only in the bars whose views make them.
fn existing_line(stroke: Keystroke) -> Option<Command> {
    use egui::Key;
    if !stroke.alt || stroke.command || stroke.shift {
        return None;
    }
    match stroke.key {
        Key::ArrowDown => Some(Command::NextExistingLine),
        Key::ArrowUp => Some(Command::PreviousExistingLine),
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
            (false, false, Key::Backspace) => Some(Command::Undo),
            (false, true, Key::Backspace) => Some(Command::Redo),
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
        (true, false, Key::Y) | (true, true, Key::Z) => Some(Command::Redo),
        (true, false, Key::X) | (false, true, Key::Delete) => Some(Command::Cut),
        (true, false, Key::C | Key::Insert) => Some(Command::Copy),
        (true, false, Key::V) | (false, true, Key::Insert) => Some(Command::Paste),
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

/// The binding the merge bar puts ahead of the shared bindings: the merge has
/// its own info page.
fn merge_info(stroke: Keystroke) -> Option<Command> {
    (stroke == Keystroke::with_command(egui::Key::I)).then_some(Command::MergeInfo)
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
        (true, true, Key::A) => Some(Command::SelectAllFiles),
        _ => None,
    }
}

/// The bindings the folder comparison bar adds; a folder synchronization has
/// no copy keys.
fn folder(view: MenuView, stroke: Keystroke) -> Option<Command> {
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
        (true, false, Key::R) if view == MenuView::Folder => Some(Command::CopyToRight),
        (true, false, Key::L) if view == MenuView::Folder => Some(Command::CopyToLeft),
        (true, true, Key::A) => Some(Command::SelectAllFiles),
        (false, false, Key::F2) => Some(Command::Rename),
        (false, false, Key::Insert) => Some(Command::NewFolder),
        (false, true, Key::F5) => Some(Command::RefreshSelection),
        _ => None,
    }
}

/// The second keys of the edit commands, and the record bars' F2.
const EDIT_ALTERNATES: [Keystroke; 7] = [
    Keystroke::plain(egui::Key::F2),
    Keystroke::with_command_shift(egui::Key::Z),
    Keystroke::with_shift(egui::Key::Delete),
    Keystroke::with_command(egui::Key::Insert),
    Keystroke::with_shift(egui::Key::Insert),
    Keystroke::with_alt(egui::Key::Backspace),
    Keystroke::with_alt_shift(egui::Key::Backspace),
];

/// Every keystroke the bar of `view` answers, for the tests and for a future
/// customization surface.
#[must_use]
pub fn bindings(view: MenuView) -> Vec<(Keystroke, Command)> {
    use egui::Key;
    let mut strokes = vec![
        Keystroke::with_command(Key::T),
        Keystroke::with_command(Key::W),
        Keystroke::with_command(Key::F4),
        Keystroke::with_command(Key::Tab),
        Keystroke::with_command_shift(Key::Tab),
        Keystroke::with_alt(Key::Home),
        Keystroke::with_alt(Key::ArrowDown),
        Keystroke::with_alt(Key::ArrowUp),
        Keystroke::with_command(Key::Q),
        Keystroke::with_command_shift(Key::O),
        Keystroke::with_command_shift(Key::S),
        Keystroke::with_command_shift(Key::C),
        Keystroke::with_command(Key::I),
        Keystroke::with_command(Key::F5),
        Keystroke::plain(Key::F5),
        Keystroke::plain(Key::F11),
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
    if matches!(view, MenuView::Merge | MenuView::FolderMerge) {
        strokes.extend([
            Keystroke::with_command(Key::B),
            Keystroke::with_command_shift(Key::B),
            Keystroke::with_command_shift(Key::A),
            Keystroke::with_alt(Key::N),
            Keystroke::with_alt(Key::P),
        ]);
    }
    match view {
        MenuView::Text
        | MenuView::Table
        | MenuView::Merge
        | MenuView::FolderMerge
        | MenuView::Registry
        | MenuView::Version
        | MenuView::Media
        | MenuView::Hex
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
            strokes.extend(EDIT_ALTERNATES);
        }
        MenuView::Folder | MenuView::FolderSync => strokes.extend([
            Keystroke::with_command(Key::R),
            Keystroke::with_command(Key::L),
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
    use super::{
        bindings, chord_types_text, drop_chord_text, drop_text_of, route, Command, KeyboardHost,
        Keystroke, MenuView, TextGate,
    };
    use egui::Key;

    fn key_event(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn only_the_text_of_a_key_that_ran_a_command_is_dropped() {
        let altgr = egui::Modifiers {
            alt: true,
            ctrl: true,
            command: true,
            ..egui::Modifiers::NONE
        };
        let mut events = vec![
            key_event(Key::Q, altgr),
            egui::Event::Text("@".to_owned()),
            key_event(Key::E, altgr),
            egui::Event::Text("\u{20ac}".to_owned()),
            egui::Event::Text("x".to_owned()),
        ];
        let pressed = Keystroke::pressed_in(&events);
        assert_eq!(pressed.len(), 2);
        assert_eq!(pressed[1].0, 2);
        drop_text_of(&mut events, &[2]);
        assert_eq!(
            events,
            vec![
                key_event(Key::Q, altgr),
                egui::Event::Text("@".to_owned()),
                key_event(Key::E, altgr),
            ]
        );
    }

    #[test]
    fn altgr_and_option_chords_type_and_control_chords_do_not() {
        let with = |ctrl: bool, alt: bool| egui::Modifiers {
            alt,
            ctrl,
            command: ctrl,
            ..egui::Modifiers::NONE
        };
        assert!(chord_types_text(with(false, false)));
        assert!(chord_types_text(with(true, true)), "AltGr");
        assert_eq!(
            chord_types_text(with(false, true)),
            !cfg!(target_os = "windows"),
            "Alt alone is a menu chord only on Windows"
        );
        assert!(!chord_types_text(with(true, false)));
    }

    #[test]
    fn text_of_a_chord_that_does_not_type_is_dropped() {
        let alt = egui::Modifiers {
            alt: true,
            ..egui::Modifiers::NONE
        };
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..egui::Modifiers::NONE
        };
        let altgr = egui::Modifiers { alt: true, ..ctrl };
        let mut events = vec![
            egui::Event::Text("i".to_owned()),
            key_event(Key::F, alt),
            egui::Event::Text("f".to_owned()),
            key_event(Key::C, ctrl),
            egui::Event::Text("c".to_owned()),
            key_event(Key::Q, altgr),
            egui::Event::Text("@".to_owned()),
            key_event(Key::G, egui::Modifiers::NONE),
            egui::Event::Text("g".to_owned()),
        ];
        drop_chord_text(&mut events);
        let typed: String = events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let expected = if cfg!(target_os = "windows") {
            "i@g"
        } else {
            "if@g"
        };
        assert_eq!(typed, expected);
    }

    fn typed(events: &[egui::Event]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn released(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn delayed_text_of_an_alt_chord_stays_with_the_alt_key() {
        let alt = egui::Modifiers {
            alt: true,
            ..egui::Modifiers::NONE
        };
        let mut events = vec![
            key_event(Key::F, alt),
            key_event(Key::ArrowDown, egui::Modifiers::NONE),
            released(Key::G),
            egui::Event::Text("f".to_owned()),
            key_event(Key::G, egui::Modifiers::NONE),
            egui::Event::Text("g".to_owned()),
        ];
        drop_chord_text(&mut events);
        let expected = if cfg!(target_os = "windows") {
            "g"
        } else {
            "fg"
        };
        assert_eq!(typed(&events), expected);
    }

    #[test]
    fn delayed_text_of_a_key_that_ran_a_command_is_dropped() {
        let mut events = vec![
            key_event(Key::Q, egui::Modifiers::NONE),
            released(Key::Q),
            key_event(Key::Home, egui::Modifiers::NONE),
            egui::Event::Text("q".to_owned()),
            key_event(Key::G, egui::Modifiers::NONE),
            egui::Event::Text("g".to_owned()),
        ];
        drop_text_of(&mut events, &[0]);
        assert_eq!(typed(&events), "g");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_alt_gr_reported_as_alt_types_its_character() {
        let alt = egui::Modifiers {
            alt: true,
            ..egui::Modifiers::NONE
        };
        let mut events = vec![key_event(Key::Q, alt), egui::Event::Text("@".to_owned())];
        drop_chord_text(&mut events);
        assert_eq!(typed(&events), "@");
        assert!(chord_types_text(alt));
    }

    fn typed_on(host: KeyboardHost, events: &[egui::Event]) -> String {
        let mut gate = TextGate::on(host, egui::Modifiers::NONE);
        events
            .iter()
            .filter(|event| gate.types(event))
            .filter_map(|event| match event {
                egui::Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_host_types_what_its_backend_delivers_for_alt_and_alt_gr() {
        let alt = egui::Modifiers {
            alt: true,
            ..egui::Modifiers::NONE
        };
        let ctrl_alt = egui::Modifiers {
            ctrl: true,
            command: true,
            ..alt
        };
        let alt_letter = [key_event(Key::F, alt), egui::Event::Text("f".to_owned())];
        let alt_gr_unmodified = [
            key_event(Key::Q, egui::Modifiers::NONE),
            egui::Event::Text("@".to_owned()),
        ];
        let alt_gr_as_alt = [key_event(Key::Q, alt), egui::Event::Text("@".to_owned())];
        let alt_gr_as_ctrl_alt = [
            key_event(Key::Q, ctrl_alt),
            egui::Event::Text("@".to_owned()),
        ];
        let delayed = [
            key_event(Key::F, alt),
            key_event(Key::ArrowDown, egui::Modifiers::NONE),
            released(Key::G),
            egui::Event::Text("f".to_owned()),
        ];
        for (host, letter) in [
            (KeyboardHost::Windows, ""),
            (KeyboardHost::Mac, "f"),
            (KeyboardHost::Xkb, "f"),
        ] {
            assert_eq!(typed_on(host, &alt_letter), letter, "{host:?} Alt+F");
            assert_eq!(typed_on(host, &delayed), letter, "{host:?} delayed Alt+F");
            assert_eq!(typed_on(host, &alt_gr_unmodified), "@", "{host:?}");
            assert_eq!(typed_on(host, &alt_gr_as_ctrl_alt), "@", "{host:?}");
        }
        assert_eq!(typed_on(KeyboardHost::Xkb, &alt_gr_as_alt), "@");
        assert_eq!(typed_on(KeyboardHost::Mac, &alt_gr_as_alt), "@");
    }

    #[test]
    fn a_folder_merge_moves_between_differences_where_a_text_merge_moves_between_sections() {
        let stroke = |key| Keystroke::with_command(key);
        assert_eq!(
            route(MenuView::FolderMerge, stroke(Key::N)),
            Some(Command::NextDifference)
        );
        assert_eq!(
            route(MenuView::FolderMerge, stroke(Key::P)),
            Some(Command::PreviousDifference)
        );
        assert_eq!(
            route(MenuView::Merge, stroke(Key::N)),
            Some(Command::NextSection)
        );
        assert_eq!(
            route(MenuView::Merge, stroke(Key::P)),
            Some(Command::PreviousSection)
        );
        assert_eq!(
            route(MenuView::FolderMerge, stroke(Key::L)),
            Some(Command::TakeLeft)
        );
        assert_eq!(
            Command::NextDifference.shortcut_in(MenuView::FolderMerge),
            Some("Ctrl+N")
        );
        assert_eq!(
            Command::NextSection.shortcut_in(MenuView::FolderMerge),
            None
        );
    }

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
        for view in [
            MenuView::Text,
            MenuView::Folder,
            MenuView::FolderSync,
            MenuView::Merge,
            MenuView::FolderMerge,
        ] {
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
    fn folder_merges_keep_text_merge_bindings_of_the_commands_they_share() {
        use super::ShortcutTable;
        let mut options = ca_session::options::CommandOptions::default();
        for (command, key) in [
            (Command::TakeLeft, "Ctrl+Alt+J"),
            (Command::CloseTab, "Ctrl+Alt+K"),
            (Command::NextSection, "Ctrl+Alt+M"),
            (Command::TakeRight, "Ctrl+Alt+U"),
        ] {
            options.set_shortcuts(MenuView::Merge.id(), command.id(), vec![key.to_owned()]);
        }
        options.set_shortcuts(
            MenuView::FolderMerge.id(),
            Command::TakeRight.id(),
            vec!["Ctrl+Alt+Y".to_owned()],
        );
        let table = ShortcutTable::from_options(&options);
        let folder = MenuView::FolderMerge;

        assert_eq!(
            table.shortcuts_of(folder, Command::TakeLeft),
            [Keystroke::with_command_alt(Key::J)]
        );
        assert_eq!(
            super::route_with(folder, Keystroke::with_command(Key::L), &table),
            None,
            "the built-in key still answers"
        );
        assert_eq!(
            table.shortcuts_of(folder, Command::CloseTab),
            [Keystroke::with_command_alt(Key::K)]
        );
        assert_eq!(
            table.shortcuts_of(folder, Command::TakeRight),
            [Keystroke::with_command_alt(Key::Y)],
            "the text merge binding replaced the folder merge one"
        );
        assert_eq!(
            super::route_with(folder, Keystroke::with_command_alt(Key::U), &table),
            None
        );
        assert_eq!(
            super::route_with(folder, Keystroke::with_command_alt(Key::M), &table),
            None,
            "a command folder merges lack took a text merge binding"
        );
        assert_eq!(
            super::route_with(folder, Keystroke::with_command(Key::N), &table),
            Some(Command::NextDifference)
        );

        let merge = MenuView::Merge;
        assert_eq!(
            table.shortcuts_of(merge, Command::TakeRight),
            [Keystroke::with_command_alt(Key::U)],
            "a folder merge binding reached text merges"
        );
        assert_eq!(
            table.shortcuts_of(merge, Command::NextSection),
            [Keystroke::with_command_alt(Key::M)]
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

    fn every_bar_but(skipped: &[MenuView]) -> Vec<MenuView> {
        MenuView::ALL
            .iter()
            .copied()
            .filter(|view| !skipped.contains(view))
            .collect()
    }

    #[test]
    fn a_folder_comparison_copies_with_ctrl_r_and_ctrl_l() {
        assert_eq!(
            route(MenuView::Folder, Keystroke::with_command(Key::R)),
            Some(Command::CopyToRight)
        );
        assert_eq!(
            route(MenuView::Folder, Keystroke::with_command(Key::L)),
            Some(Command::CopyToLeft)
        );
        assert_eq!(
            Command::CopyToRight.shortcut_in(MenuView::Folder),
            Some("Ctrl+R")
        );
    }

    #[test]
    fn a_table_copies_cells_with_ctrl_shift_and_rows_with_ctrl() {
        for (stroke, command) in [
            (
                Keystroke::with_command_shift(Key::R),
                Command::CopyCellToRight,
            ),
            (
                Keystroke::with_command_shift(Key::L),
                Command::CopyCellToLeft,
            ),
            (Keystroke::with_command(Key::R), Command::CopyToRight),
            (Keystroke::with_command(Key::L), Command::CopyToLeft),
        ] {
            assert_eq!(route(MenuView::Table, stroke), Some(command), "{stroke:?}");
        }
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command_shift(Key::R)),
            Some(Command::CopyLineToRight)
        );
        assert_eq!(
            Command::CopyCellToLeft.shortcut_in(MenuView::Table),
            Some("Ctrl+Shift+L")
        );
        assert_eq!(Command::CopyLineToLeft.shortcut_in(MenuView::Table), None);
    }

    #[test]
    fn a_text_merge_shows_its_info_page_on_ctrl_i() {
        assert_eq!(
            route(MenuView::Merge, Keystroke::with_command(Key::I)),
            Some(Command::MergeInfo)
        );
        assert_eq!(
            route(MenuView::FolderMerge, Keystroke::with_command(Key::I)),
            Some(Command::CompareInfo)
        );
        assert_eq!(
            route(MenuView::Text, Keystroke::with_command(Key::I)),
            Some(Command::CompareInfo)
        );
        assert_eq!(
            Command::MergeInfo.shortcut_in(MenuView::Merge),
            Some("Ctrl+I")
        );
        assert_eq!(Command::CompareInfo.shortcut_in(MenuView::Merge), None);
    }

    #[test]
    fn record_views_step_through_differences_with_ctrl_n_and_ctrl_p() {
        for view in [MenuView::Registry, MenuView::Version, MenuView::Media] {
            assert_eq!(
                route(view, Keystroke::with_command(Key::N)),
                Some(Command::NextDifference),
                "{view:?}"
            );
            assert_eq!(
                route(view, Keystroke::with_command(Key::P)),
                Some(Command::PreviousDifference),
                "{view:?}"
            );
            assert_eq!(
                Command::NextDifference.shortcut_in(view),
                Some("Ctrl+N"),
                "{view:?}"
            );
            assert_eq!(Command::NextSection.shortcut_in(view), None, "{view:?}");
        }
    }

    #[test]
    fn the_registry_renames_on_f2_and_adds_a_value_on_insert() {
        assert_eq!(
            route(MenuView::Registry, Keystroke::plain(Key::F2)),
            Some(Command::Rename)
        );
        assert_eq!(
            route(MenuView::Registry, Keystroke::plain(Key::Insert)),
            Some(Command::NewValue)
        );
        assert_eq!(
            route(MenuView::Text, Keystroke::plain(Key::Insert)),
            Some(Command::ToggleOverwrite)
        );
        assert_eq!(route(MenuView::Version, Keystroke::plain(Key::F2)), None);
    }

    #[test]
    fn ctrl_f4_closes_the_tab_in_every_view() {
        for view in MenuView::ALL.iter().copied() {
            assert_eq!(
                route(view, Keystroke::with_command(Key::F4)),
                Some(Command::CloseTab),
                "{view:?}"
            );
            assert!(
                bindings(view).contains(&(Keystroke::with_command(Key::F4), Command::CloseTab)),
                "{view:?}"
            );
        }
    }

    #[test]
    fn the_edit_commands_carry_their_secondary_keys() {
        for view in every_bar_but(&[MenuView::Folder, MenuView::FolderSync]) {
            for (stroke, command) in [
                (Keystroke::with_alt(Key::Backspace), Command::Undo),
                (Keystroke::with_alt_shift(Key::Backspace), Command::Redo),
                (Keystroke::with_command_shift(Key::Z), Command::Redo),
                (Keystroke::with_shift(Key::Delete), Command::Cut),
                (Keystroke::with_command(Key::Insert), Command::Copy),
                (Keystroke::with_shift(Key::Insert), Command::Paste),
            ] {
                assert_eq!(route(view, stroke), Some(command), "{stroke:?} in {view:?}");
                assert!(
                    bindings(view).contains(&(stroke, command)),
                    "{stroke:?} in {view:?}"
                );
            }
        }
    }

    #[test]
    fn a_secondary_key_leaves_the_primary_key_in_place() {
        for view in every_bar_but(&[MenuView::Folder, MenuView::FolderSync]) {
            for (stroke, command) in [
                (Keystroke::with_command(Key::Z), Command::Undo),
                (Keystroke::with_command(Key::Y), Command::Redo),
                (Keystroke::with_command(Key::X), Command::Cut),
                (Keystroke::with_command(Key::C), Command::Copy),
                (Keystroke::with_command(Key::V), Command::Paste),
            ] {
                assert_eq!(route(view, stroke), Some(command), "{stroke:?} in {view:?}");
            }
        }
        assert_eq!(Command::Cut.shortcut_in(MenuView::Text), Some("Ctrl+X"));
    }

    #[test]
    fn captions_follow_the_content_each_view_steps_through() {
        for (command, view, caption) in [
            (Command::Reload, MenuView::FolderMerge, "Refresh"),
            (Command::NextSection, MenuView::Table, "Next Difference Row"),
            (
                Command::PreviousSection,
                MenuView::Table,
                "Previous Difference Row",
            ),
            (
                Command::NextDifference,
                MenuView::Table,
                "Next Difference Cell",
            ),
            (
                Command::PreviousDifference,
                MenuView::Table,
                "Previous Difference Cell",
            ),
            (Command::NextSection, MenuView::Hex, "Next Difference Bytes"),
            (
                Command::PreviousSection,
                MenuView::Hex,
                "Previous Difference Bytes",
            ),
            (Command::Reload, MenuView::Registry, "Reload"),
            (Command::Recompare, MenuView::Registry, "Recompare"),
            (Command::OpenFile, MenuView::Registry, "Open..."),
        ] {
            assert_eq!(command.label_in(view), caption, "{command:?} in {view:?}");
        }
        assert_eq!(
            Command::NextSection.label_in(MenuView::Text),
            "Next Difference Section"
        );
    }

    #[test]
    fn alt_with_a_navigation_key_routes_and_alt_with_a_letter_stays_with_the_menu_bar() {
        for view in MenuView::ALL.iter().copied() {
            let stroke = Keystroke::with_alt(Key::Home);
            assert_eq!(route(view, stroke), Some(Command::SessionHome), "{view:?}");
            assert!(
                bindings(view).contains(&(stroke, Command::SessionHome)),
                "{view:?}"
            );
            for letter in [Key::F, Key::E, Key::V, Key::T, Key::H, Key::S, Key::A] {
                assert_eq!(
                    route(view, Keystroke::with_alt(letter)),
                    None,
                    "Alt+{letter:?} in {view:?}"
                );
            }
        }
    }

    #[test]
    fn ctrl_tab_and_ctrl_shift_tab_switch_tabs_in_every_view() {
        for view in MenuView::ALL.iter().copied() {
            assert_eq!(
                route(view, Keystroke::with_command(Key::Tab)),
                Some(Command::TabNext),
                "{view:?}"
            );
            assert_eq!(
                route(view, Keystroke::with_command_shift(Key::Tab)),
                Some(Command::TabPrevious),
                "{view:?}"
            );
        }
        assert_eq!(Command::TabNext.shortcut_text(), Some("Ctrl+Tab"));
    }

    #[test]
    fn a_user_binding_beats_a_changed_default() {
        use super::ShortcutTable;
        let mut options = ca_session::options::CommandOptions::default();
        options.set_shortcuts(
            MenuView::Text.id(),
            Command::TabNext.id(),
            vec!["Ctrl+Alt+K".to_owned()],
        );
        options.set_shortcuts(
            MenuView::Text.id(),
            Command::FindNext.id(),
            vec!["Ctrl+Tab".to_owned()],
        );
        options.set_shortcuts(
            MenuView::Text.id(),
            Command::Cut.id(),
            vec!["Ctrl+Alt+X".to_owned()],
        );
        let table = ShortcutTable::from_options(&options);
        let text = MenuView::Text;
        assert_eq!(
            super::route_with(text, Keystroke::with_command(Key::Tab), &table),
            Some(Command::FindNext)
        );
        assert_eq!(
            table.shortcuts_of(text, Command::TabNext),
            [Keystroke::with_command_alt(Key::K)]
        );
        assert_eq!(
            table.shortcuts_of(text, Command::Cut),
            [Keystroke::with_command_alt(Key::X)],
            "the secondary key went with the primary one"
        );
        assert_eq!(
            super::route_with(text, Keystroke::with_shift(Key::Delete), &table),
            None
        );
        assert_eq!(
            super::route_with(MenuView::Folder, Keystroke::with_command(Key::Tab), &table),
            Some(Command::TabNext),
            "another bar keeps its default"
        );
    }

    #[test]
    fn a_text_merge_binding_of_a_window_command_reaches_folder_merges() {
        use super::ShortcutTable;
        let mut options = ca_session::options::CommandOptions::default();
        options.set_shortcuts(
            MenuView::Merge.id(),
            Command::Options.id(),
            vec!["Ctrl+Alt+O".to_owned()],
        );
        options.set_shortcuts(
            MenuView::Merge.id(),
            Command::About.id(),
            vec!["Ctrl+Alt+B".to_owned()],
        );
        options.set_shortcuts(
            MenuView::FolderMerge.id(),
            Command::About.id(),
            vec!["Ctrl+Alt+A".to_owned()],
        );
        let table = ShortcutTable::from_options(&options);
        assert_eq!(
            table.shortcuts_of(MenuView::FolderMerge, Command::Options),
            [Keystroke::with_command_alt(Key::O)]
        );
        assert_eq!(
            table.shortcuts_of(MenuView::FolderMerge, Command::About),
            [Keystroke::with_command_alt(Key::A)],
            "a folder merge binding of its own wins"
        );
        assert!(table
            .shortcuts_of(MenuView::Text, Command::Options)
            .is_empty());
    }

    #[test]
    fn a_text_merge_binding_of_a_command_folder_merges_refuse_stays_in_text_merges() {
        use super::ShortcutTable;
        let mut options = ca_session::options::CommandOptions::default();
        options.set_shortcuts(
            MenuView::Merge.id(),
            Command::TakeLeft.id(),
            vec!["Ctrl+Alt+T".to_owned()],
        );
        options.set_shortcuts(
            MenuView::Merge.id(),
            Command::NextExistingLine.id(),
            vec!["Ctrl+L".to_owned()],
        );
        let table = ShortcutTable::from_options(&options);
        assert_eq!(
            table.claimed_by(MenuView::Merge, Keystroke::with_command(Key::L)),
            Some(Command::NextExistingLine)
        );
        assert_eq!(
            table.shortcuts_of(MenuView::FolderMerge, Command::TakeLeft),
            [Keystroke::with_command_alt(Key::T)],
            "a shared command follows the text merge binding"
        );
        assert_eq!(
            table.claimed_by(MenuView::FolderMerge, Keystroke::with_command(Key::L)),
            None
        );
        assert!(table
            .shortcuts_of(MenuView::FolderMerge, Command::NextExistingLine)
            .is_empty());
    }

    #[test]
    fn the_existing_line_keys_bind_only_in_the_views_that_move_by_existing_lines() {
        for view in MenuView::ALL.iter().copied() {
            let handles = matches!(view, MenuView::Text | MenuView::Table | MenuView::Merge);
            for (stroke, command) in [
                (
                    Keystroke::with_alt(Key::ArrowDown),
                    Command::NextExistingLine,
                ),
                (
                    Keystroke::with_alt(Key::ArrowUp),
                    Command::PreviousExistingLine,
                ),
            ] {
                let expected = handles.then_some(command);
                assert_eq!(route(view, stroke), expected, "{stroke:?} in {view:?}");
                assert_eq!(
                    bindings(view).contains(&(stroke, command)),
                    handles,
                    "{stroke:?} in {view:?}"
                );
            }
        }
    }

    #[test]
    fn copy_keys_of_a_folder_comparison_stay_out_of_folder_syncs() {
        for stroke in [
            Keystroke::with_command(Key::R),
            Keystroke::with_command(Key::L),
        ] {
            assert_eq!(route(MenuView::FolderSync, stroke), None, "{stroke:?}");
        }
        assert_eq!(
            route(MenuView::Folder, Keystroke::with_command(Key::R)),
            Some(Command::CopyToRight)
        );
    }
}
