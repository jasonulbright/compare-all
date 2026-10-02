//! The typed pages of the application options document.
//!
//! One struct per page. Every struct defaults as a whole, so a document written
//! before a field existed still loads and the field takes its documented value.
//! Every struct keeps the fields this build does not know, so a document written
//! by a newer build loses nothing.

use super::colors::PaletteOptions;
use super::provisional;
use crate::location::StoredPath;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Declares a stored enum with an unknown arm and a stable identifier per
/// variant.
macro_rules! stored_choice {
    ($(#[$meta:meta])* $name:ident, $default:ident, { $($variant:ident => $id:literal, $label:literal;)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
        pub enum $name {
            $(
                #[doc = $label]
                $variant {
                    /// Fields written by another build.
                    #[serde(flatten)]
                    unknown: BTreeMap<String, Value>,
                },
            )*
            /// A choice written by another build.
            #[serde(untagged)]
            Unknown(Value),
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default { unknown: BTreeMap::new() }
            }
        }

        impl $name {
            /// Every choice this build offers, with its stored name and label.
            pub const CHOICES: &'static [(&'static str, &'static str)] =
                &[$(($id, $label),)*];

            /// The stored name of the choice.
            #[must_use]
            pub fn id(&self) -> &str {
                match self {
                    $(Self::$variant { .. } => $id,)*
                    Self::Unknown(_) => "unknown",
                }
            }

            /// The choice named by `id`, or the default when the name is not
            /// one this build offers.
            #[must_use]
            pub fn from_id(id: &str) -> Self {
                match id {
                    $($id => Self::$variant { unknown: BTreeMap::new() },)*
                    _ => Self::default(),
                }
            }
        }
    };
}

stored_choice!(
    /// Which color table the window uses.
    ThemeChoice, FollowSystem, {
        FollowSystem => "followSystem", "Follow the operating system";
        Light => "light", "Light";
        Dark => "dark", "Dark";
    }
);

stored_choice!(
    /// How a quick comparison decides whether two files match.
    QuickCompareMethod, Binary, {
        Binary => "binary", "Binary comparison";
        Rules => "rules", "Rules-based comparison";
    }
);

stored_choice!(
    /// Where a new session is placed.
    NewSessionPlacement, NewTab, {
        NewTab => "newTab", "A new tab";
        ReuseHomeTab => "reuseHomeTab", "The launcher tab it started from";
        NewWindow => "newWindow", "A new window";
    }
);

stored_choice!(
    /// What a confirmation raised during a synchronize answers.
    SynchronizeConfirmations, Prompt, {
        Prompt => "prompt", "Ask each time";
        YesToAll => "yesToAll", "Answer yes to all";
        NoToAll => "noToAll", "Answer no to all";
    }
);

stored_choice!(
    /// Where a backup copy is written.
    BackupLocation, SameFolder, {
        SameFolder => "sameFolder", "Beside the file";
        NamedFolder => "namedFolder", "A named folder";
    }
);

stored_choice!(
    /// Which line ending a save writes.
    LineEndingsOnSave, Keep, {
        Keep => "keep", "Keep what the file had";
        CrLf => "crlf", "Carriage return and line feed";
        Lf => "lf", "Line feed";
        Cr => "cr", "Carriage return";
    }
);

stored_choice!(
    /// How a thumbnail strip handles content taller than itself.
    ThumbnailMode, CompressToFit, {
        CompressToFit => "compressToFit", "Compress to fit";
        AllowScrolling => "allowScrolling", "Allow scrolling";
    }
);

stored_choice!(
    /// How much processor time background comparison work takes.
    ComparisonPriority, Normal, {
        Low => "low", "Low";
        Normal => "normal", "Normal";
        High => "high", "High";
    }
);

/// What happens at program start.
/// An options page is a list of check boxes, so the count of flags is the shape
/// of the page rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StartupOptions {
    /// Workspace loaded at start. Empty loads none.
    pub load_workspace: String,
    /// Workspace the open tabs are written to at exit. Empty writes none.
    pub save_workspace_on_exit: String,
    /// Show a summary rather than a full view for a comparison named on the
    /// command line.
    pub show_quick_compare: bool,
    /// How that summary decides whether the files match.
    pub quick_compare_method: QuickCompareMethod,
    /// Open the full view anyway when the files differ.
    pub open_view_if_different: bool,
    /// Start the clipboard helper with the operating system.
    pub run_clipboard_compare_at_startup: bool,
    /// Add the program to the file manager's context menu.
    pub shell_integration: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// Where a session opens.
/// An options page is a list of check boxes, so the count of flags is the shape
/// of the page rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TabOptions {
    /// Where a session started from the launcher is placed.
    pub new_session_placement: NewSessionPlacement,
    /// Show the tab strip even when one tab is open.
    pub show_tab_strip_with_one_tab: bool,
    /// Close the window when its last tab closes.
    pub close_window_with_last_tab: bool,
    /// Ask before closing a window holding more than one tab.
    pub confirm_closing_several_tabs: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for TabOptions {
    fn default() -> Self {
        Self {
            new_session_placement: NewSessionPlacement::default(),
            show_tab_strip_with_one_tab: true,
            close_window_with_last_tab: true,
            confirm_closing_several_tabs: true,
            unknown: BTreeMap::new(),
        }
    }
}

/// Point sizes of the fonts the views draw with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FontOptions {
    /// Font of the text comparison, the merge and the table.
    pub editor_point_size: f32,
    /// Font of the byte comparison and of the byte details area.
    pub hex_point_size: f32,
    /// Font of every remaining listing.
    pub listing_point_size: f32,
    /// Folder listings use the operating system's own font.
    pub folder_uses_system_font: bool,
    /// Font of a folder listing when the system font is not used.
    pub folder_point_size: f32,
    /// Font of the merge input panes when they carry their own.
    pub merge_input_point_size: f32,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for FontOptions {
    fn default() -> Self {
        Self {
            editor_point_size: provisional::EDITOR_POINT_SIZE,
            hex_point_size: provisional::HEX_POINT_SIZE,
            listing_point_size: provisional::LISTING_POINT_SIZE,
            folder_uses_system_font: true,
            folder_point_size: provisional::FOLDER_POINT_SIZE,
            merge_input_point_size: provisional::MERGE_INPUT_POINT_SIZE,
            unknown: BTreeMap::new(),
        }
    }
}

/// Theme, colors and fonts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppearanceOptions {
    /// Which color table the window uses.
    pub theme: ThemeChoice,
    /// Apply a pending color or font change to the open views at once.
    pub enable_preview: bool,
    /// The colors each view kind states for itself.
    pub palettes: PaletteOptions,
    /// The point sizes the views draw with.
    pub fonts: FontOptions,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl AppearanceOptions {
    /// Whether the dark table applies, given what the operating system reports.
    #[must_use]
    pub fn wants_dark(&self, system_is_dark: bool) -> bool {
        match self.theme.id() {
            "light" => false,
            "dark" => true,
            _ => system_is_dark,
        }
    }
}

/// How folder listings are drawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderViewOptions {
    /// Tint the background of every other row.
    pub use_stripes: bool,
    /// Draw the selection the way the operating system does, which discards the
    /// comparison colors under it.
    pub selection_uses_system: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for FolderViewOptions {
    fn default() -> Self {
        Self {
            use_stripes: true,
            selection_uses_system: false,
            unknown: BTreeMap::new(),
        }
    }
}

/// How file listings are drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileViewOptions {
    /// Tint the background of every other row.
    pub use_stripes: bool,
    /// Draw the selection the way the operating system does.
    pub selection_uses_system: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// How picture comparisons are drawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PictureOptions {
    /// Draw a checkerboard behind transparent pixels.
    pub show_transparency_as_checkerboard: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for PictureOptions {
    fn default() -> Self {
        Self {
            show_transparency_as_checkerboard: true,
            unknown: BTreeMap::new(),
        }
    }
}

/// How the shared text editor behaves.
/// An options page is a list of check boxes, so the count of flags is the shape
/// of the page rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TextEditingOptions {
    /// Columns between tab stops.
    pub tab_stop: u32,
    /// The Tab key inserts spaces rather than a tab character.
    pub insert_spaces_for_tabs: bool,
    /// A new line starts with the leading whitespace of the line above.
    pub auto_indent: bool,
    /// Backspace inside the leading whitespace removes one indent level.
    pub backspace_unindents: bool,
    /// Which line ending a save writes.
    pub line_endings_on_save: LineEndingsOnSave,
    /// The caret may sit past the last character of a line.
    pub caret_past_line_end: bool,
    /// Typing replaces the selection rather than inserting beside it.
    pub typing_replaces_selection: bool,
    /// The find command starts from the word under the caret.
    pub find_uses_current_word: bool,
    /// State how many lines a display filter is hiding.
    pub show_filtered_line_counts: bool,
    /// Matching lines shown around a difference section under Show Context.
    pub context_lines: u32,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for TextEditingOptions {
    fn default() -> Self {
        Self {
            tab_stop: 8,
            insert_spaces_for_tabs: false,
            auto_indent: true,
            backspace_unindents: true,
            line_endings_on_save: LineEndingsOnSave::default(),
            caret_past_line_end: false,
            typing_replaces_selection: true,
            find_uses_current_word: true,
            show_filtered_line_counts: true,
            context_lines: provisional::CONTEXT_LINES,
            unknown: BTreeMap::new(),
        }
    }
}

/// How the difference navigation commands behave.
/// An options page is a list of check boxes, so the count of flags is the shape
/// of the page rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NextDifferenceOptions {
    /// Move to the first difference when a comparison finishes loading.
    pub go_to_first_difference_on_load: bool,
    /// Move to the next difference after a copy.
    pub go_to_next_after_copy: bool,
    /// Keep Next Difference Files inside the current folder.
    pub limit_to_current_folder: bool,
    /// Continue from the first difference after the last one.
    pub wrap_around: bool,
    /// Show the panel that states why navigation stopped.
    pub show_message_panel: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for NextDifferenceOptions {
    fn default() -> Self {
        Self {
            go_to_first_difference_on_load: true,
            go_to_next_after_copy: true,
            limit_to_current_folder: false,
            wrap_around: true,
            show_message_panel: true,
            unknown: BTreeMap::new(),
        }
    }
}

/// Backup copies taken before a file is replaced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupOptions {
    /// Take a copy before a copy or a move replaces a file.
    pub before_overwrite: bool,
    /// Take a copy before an edited file is written.
    pub before_save: bool,
    /// Where the copy is written.
    pub location: BackupLocation,
    /// The folder copies are written to when one is named.
    pub folder: Option<StoredPath>,
    /// Text appended to the name of a copy.
    pub suffix: String,
    /// How many copies of one file are kept.
    pub copies: u32,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            before_overwrite: false,
            before_save: false,
            location: BackupLocation::default(),
            folder: None,
            suffix: provisional::BACKUP_SUFFIX.to_owned(),
            copies: provisional::BACKUP_COPIES,
            unknown: BTreeMap::new(),
        }
    }
}

/// What a file operation asks before it runs.
/// An options page is a list of check boxes, so the count of flags is the shape
/// of the page rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileOperationOptions {
    /// Ask before a copy.
    pub confirm_copy: bool,
    /// Ask before a move.
    pub confirm_move: bool,
    /// Ask before a merge.
    pub confirm_merge: bool,
    /// Ask before a delete.
    pub confirm_delete: bool,
    /// What a confirmation raised during a synchronize answers.
    pub synchronize_confirmations: SynchronizeConfirmations,
    /// Hidden items take part in file operations.
    pub include_hidden_items: bool,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for FileOperationOptions {
    fn default() -> Self {
        Self {
            confirm_copy: true,
            confirm_move: true,
            confirm_merge: true,
            confirm_delete: true,
            synchronize_confirmations: SynchronizeConfirmations::default(),
            include_hidden_items: true,
            unknown: BTreeMap::new(),
        }
    }
}

/// Filename masks naming the archive formats.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchiveOptions {
    /// Format name to the mask that selects it.
    pub masks: BTreeMap<String, String>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// Low level settings with no page of their own.
/// An options page is a list of check boxes, so the count of flags is the shape
/// of the page rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TweakOptions {
    /// Ask whether a newer build exists.
    pub check_for_updates: bool,
    /// Days between those checks.
    pub check_for_updates_days: u32,
    /// Color lines that are also marked as differences by their grammar.
    pub syntax_highlighting_on_difference_lines: bool,
    /// Draw a hatch past the last line.
    pub crosshatch_past_end_of_file: bool,
    /// Put the left pane's copy gutter on its right edge.
    pub right_side_gutter_for_left_editor: bool,
    /// Give items present on one side a color of their own.
    pub use_orphan_color: bool,
    /// Padding added between text rows.
    pub extra_line_spacing: u32,
    /// Column the vertical ruler is drawn at. Zero hides it.
    pub column_line_at: u32,
    /// Percentage the pane without focus is darkened by.
    pub dim_inactive_pane_percent: u32,
    /// Draw the merge input panes in their own font.
    pub different_font_for_merge_input_panes: bool,
    /// Sound a tone once a long file operation ends.
    pub beep_after_long_file_operations: bool,
    /// Clear the read-only attribute on files copied off optical media.
    pub remove_read_only_flag_from_optical_media: bool,
    /// Notice a file that changed on disk while a view holds it.
    pub check_for_files_changed_on_disk: bool,
    /// Read such a file again without asking, unless edits would be lost.
    pub automatically_reload_unless_changes_are_discarded: bool,
    /// Use IPv6 where it is available.
    pub use_ipv6: bool,
    /// How much processor time background comparison work takes.
    pub comparison_priority: ComparisonPriority,
    /// Bytes read per step of a byte for byte comparison.
    pub binary_buffer_bytes: u64,
    /// Show a hint over a control the pointer rests on.
    pub show_hints: bool,
    /// State the keystroke in that hint.
    pub show_keyboard_shortcut_in_hints: bool,
    /// Escape closes a file comparison view.
    pub escape_closes_file_views: bool,
    /// One vertical scrollbar for both panes rather than one each.
    pub single_vertical_scrollbar: bool,
    /// Keep a splitter where it was dragged.
    pub sticky_splitter_position: bool,
    /// How a thumbnail strip handles content taller than itself.
    pub thumbnail_mode: ThumbnailMode,
    /// Milliseconds before a hint appears.
    pub hint_delay_milliseconds: u32,
    /// Milliseconds a hint stays on screen.
    pub hint_duration_milliseconds: u32,
    /// Milliseconds of idle time after which type ahead search starts again.
    pub incremental_search_reset_milliseconds: u32,
    /// Wildcard sets the filter control offers, one per entry.
    pub name_filter_presets: Vec<String>,
    /// Sound a tone once a script ends.
    pub script_beep_when_finished: bool,
    /// Leave the program once a script ends.
    pub script_close_when_finished: bool,
    /// Package other users' shared sessions are read from.
    pub shared_sessions_file: Option<StoredPath>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for TweakOptions {
    fn default() -> Self {
        Self {
            check_for_updates: true,
            check_for_updates_days: provisional::UPDATE_CHECK_DAYS,
            syntax_highlighting_on_difference_lines: false,
            different_font_for_merge_input_panes: false,
            remove_read_only_flag_from_optical_media: false,
            single_vertical_scrollbar: false,
            sticky_splitter_position: false,
            crosshatch_past_end_of_file: true,
            right_side_gutter_for_left_editor: false,
            use_orphan_color: true,
            extra_line_spacing: provisional::EXTRA_LINE_SPACING,
            column_line_at: provisional::COLUMN_LINE_AT,
            dim_inactive_pane_percent: provisional::DIM_INACTIVE_PANE_PERCENT,
            beep_after_long_file_operations: false,
            check_for_files_changed_on_disk: true,
            automatically_reload_unless_changes_are_discarded: true,
            use_ipv6: true,
            comparison_priority: ComparisonPriority::default(),
            binary_buffer_bytes: provisional::BINARY_BUFFER_BYTES,
            show_hints: true,
            show_keyboard_shortcut_in_hints: true,
            escape_closes_file_views: false,
            thumbnail_mode: ThumbnailMode::default(),
            hint_delay_milliseconds: provisional::HINT_DELAY_MILLISECONDS,
            hint_duration_milliseconds: provisional::HINT_DURATION_MILLISECONDS,
            incremental_search_reset_milliseconds:
                provisional::INCREMENTAL_SEARCH_RESET_MILLISECONDS,
            name_filter_presets: Vec::new(),
            script_beep_when_finished: false,
            script_close_when_finished: false,
            shared_sessions_file: None,
            unknown: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{AppearanceOptions, ThemeChoice};

    #[test]
    fn every_choice_reads_back_from_its_stored_name() {
        for (id, _) in ThemeChoice::CHOICES {
            assert_eq!(&ThemeChoice::from_id(id).id(), id);
        }
    }

    #[test]
    fn a_name_this_build_does_not_offer_falls_back_to_the_default() {
        assert_eq!(ThemeChoice::from_id("sepia"), ThemeChoice::default());
    }

    #[test]
    fn the_theme_choice_decides_which_table_applies() {
        let mut appearance = AppearanceOptions::default();
        assert!(appearance.wants_dark(true));
        assert!(!appearance.wants_dark(false));
        appearance.theme = ThemeChoice::from_id("dark");
        assert!(appearance.wants_dark(false));
        appearance.theme = ThemeChoice::from_id("light");
        assert!(!appearance.wants_dark(true));
    }
}
