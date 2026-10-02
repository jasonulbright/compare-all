//! Which pages the options dialog carries and which fields sit on each one.
//!
//! The tables here are the single description of the dialog. The dialog draws
//! them, the tests walk them, and a field reaches both from one line. A field
//! no part of the program reads yet carries the reason, so nothing on a page
//! looks as though it changed something when it did not.

use crate::settings::field::{Choice, FieldShape, FieldValue};
use ca_session::options::{ColorGroup, ProgramOptions};
use ca_session::PolicyKey;

/// Why a field is drawn but takes no edit.
mod reason {
    /// The context menu verbs are registry keys, which exist on Windows only.
    #[cfg(not(windows))]
    pub const NO_SHELL: &str = "The file manager menu exists on Windows only.";
    /// No clipboard helper exists in this build.
    pub const NO_CLIPBOARD_HELPER: &str = "The clipboard helper program is not built.";
    /// The command line opens the full view.
    pub const NO_QUICK_COMPARE: &str = "The quick compare summary dialog is not built.";
    /// The command this field narrows is not built.
    pub const NO_NEXT_DIFFERENCE_FILES: &str =
        "The Next Difference Files command is not built, so nothing reads this.";
    /// The editor has no such behavior.
    pub const CARET_CLAMPED: &str = "The editor keeps the caret inside the text of its line.";
    /// Selection replacement is not optional.
    pub const TYPING_REPLACES: &str = "Typing always replaces the selection.";
    /// No view counts the lines a display filter hides.
    pub const NO_FILTERED_COUNT: &str = "No view states how many lines a display filter hides.";
    /// The area past the last line has one look.
    pub const NO_CROSSHATCH: &str = "The views draw no crosshatch past the end of a file.";
    /// The gutter position is fixed.
    pub const GUTTER_LEFT: &str = "Each pane draws its gutter on its left side.";
    /// The orphan color is not optional.
    pub const ORPHAN_ALWAYS: &str = "Lines on one side only always take the orphan color.";
    /// The text view has one scrollbar.
    pub const ONE_SCROLLBAR: &str = "The text view draws one vertical scrollbar for both panes.";
    /// The panes split the width evenly.
    pub const EQUAL_SPLIT: &str = "The panes always share the width in equal parts.";
    /// The thumbnail strip does not scroll.
    pub const THUMBNAIL_FITS: &str = "The thumbnail strip always compresses the file to fit.";
    /// No view searches as the user types.
    pub const NO_TYPE_SEARCH: &str = "No view searches as the user types, so nothing resets.";
    /// The merge panes share one row grid.
    pub const ONE_MERGE_GRID: &str =
        "Per-pane row metrics are not built, so the merge panes share one font.";
    /// Hints are drawn by the toolkit.
    pub const TOOLKIT_HINTS: &str = "The toolkit draws the hints and times them.";
    /// Comparison work runs at the thread default.
    pub const WORKER_PRIORITY: &str = "Comparison work runs at the priority the system gives it.";
    /// Buffer size is chosen by the engine.
    pub const ENGINE_BUFFER: &str = "The comparison engine chooses how much it reads at a time.";
    /// Network settings are read by the remote file systems.
    pub const REMOTE_STACK: &str = "The remote file systems choose the address family.";
    /// Scripts are run by the command line program.
    pub const SCRIPT_RUNNER: &str = "The command line program decides what a finished script does.";
    /// The folder filter control is built by the folder view.
    pub const FILTER_CONTROL: &str = "The folder view builds its own filter control.";
    /// Shared sessions are not read.
    pub const NO_SHARED_SESSIONS: &str = "This build reads no shared sessions package.";
    /// The file watcher is not built.
    pub const NO_WATCHER: &str = "A view checks its files when it saves, not while it waits.";
    /// The operation runner makes no sound.
    pub const NO_SOUND: &str = "This build plays no sound.";
    /// Optical media handling is not separated.
    pub const NO_OPTICAL_CASE: &str = "File operations treat every source the same way.";
    /// A synchronize asks each question as it reaches it.
    pub const SYNC_ASKS: &str =
        "The standing answer for a synchronize is not built, so each question is asked.";
    /// Hidden items come from the session settings.
    pub const SCAN_SETTING: &str = "The session settings decide which items a scan reads.";
}

/// Why a page or a field is unavailable because an administrator said so.
pub const POLICY_REASON: &str = "An administrator has disabled this setting.";

/// One editable value of the options document.
#[derive(Clone)]
pub struct OptionField {
    /// Label shown beside the control.
    pub label: &'static str,
    /// Stable identifier, the page group and the field name joined by a dot.
    pub key: &'static str,
    /// How the field is drawn.
    pub shape: FieldShape,
    /// Choices a drop down offers; empty for every other shape.
    pub choices: &'static [Choice],
    /// Greatest value a count accepts.
    pub limit: u64,
    /// Why nothing reads the field yet, when nothing does.
    pub unavailable: Option<&'static str>,
    /// The policy that disables the field, where one does.
    pub policy: Option<PolicyKey>,
    get: fn(&ProgramOptions) -> FieldValue,
    set: fn(&mut ProgramOptions, &FieldValue),
}

impl std::fmt::Debug for OptionField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OptionField")
            .field("key", &self.key)
            .field("shape", &self.shape)
            .field("unavailable", &self.unavailable)
            .finish_non_exhaustive()
    }
}

impl OptionField {
    /// Builds a field from its accessors. Used by the declaration macro.
    #[must_use]
    pub const fn new(
        label: &'static str,
        key: &'static str,
        shape: FieldShape,
        get: fn(&ProgramOptions) -> FieldValue,
        set: fn(&mut ProgramOptions, &FieldValue),
    ) -> Self {
        Self {
            label,
            key,
            shape,
            choices: &[],
            limit: u64::MAX,
            unavailable: None,
            policy: None,
            get,
            set,
        }
    }

    /// The same field, drawn but not editable, with the reason.
    #[must_use]
    pub const fn unavailable(mut self, reason: &'static str) -> Self {
        self.unavailable = Some(reason);
        self
    }

    /// The same field with a choice list.
    #[must_use]
    pub const fn with_choices(mut self, choices: &'static [Choice]) -> Self {
        self.choices = choices;
        self
    }

    /// The same field with an upper bound on what a count accepts.
    #[must_use]
    pub const fn with_limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }

    /// The same field, disabled when the named policy is in force.
    #[must_use]
    pub const fn under_policy(mut self, key: PolicyKey) -> Self {
        self.policy = Some(key);
        self
    }

    /// True when something reads the field.
    #[must_use]
    pub const fn is_bound(&self) -> bool {
        self.unavailable.is_none()
    }

    /// Reads the field.
    #[must_use]
    pub fn read(&self, options: &ProgramOptions) -> FieldValue {
        (self.get)(options)
    }

    /// Writes the field. A value of the wrong shape is ignored.
    pub fn write(&self, options: &mut ProgramOptions, value: &FieldValue) {
        (self.set)(options, value);
    }

    /// Why the field takes no edit right now, where it takes none.
    #[must_use]
    pub fn blocked_by(&self, policies: ca_session::AdminPolicies) -> Option<&'static str> {
        if let Some(key) = self.policy {
            if policies.is_set(key) {
                return Some(POLICY_REASON);
            }
        }
        self.unavailable
    }
}

/// Point sizes are clamped to the range the page offers, which an `f32`
/// represents without loss, so the conversion is total.
#[allow(clippy::cast_possible_truncation)]
fn to_point_size(value: f64) -> f32 {
    use ca_session::options::provisional::{MAXIMUM_POINT_SIZE, MINIMUM_POINT_SIZE};
    value.clamp(MINIMUM_POINT_SIZE, MAXIMUM_POINT_SIZE) as f32
}

/// Declares a flag field.
macro_rules! flag {
    ($group:ident . $name:ident, $label:literal) => {
        OptionField::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            FieldShape::Flag,
            |options| FieldValue::Flag(options.$group.$name),
            |options, value| {
                if let FieldValue::Flag(given) = value {
                    options.$group.$name = *given;
                }
            },
        )
    };
}

/// Declares a count field over a `u32`.
macro_rules! count {
    ($group:ident . $name:ident, $label:literal) => {
        OptionField::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            FieldShape::Count,
            |options| FieldValue::Count(u64::from(options.$group.$name)),
            |options, value| {
                if let FieldValue::Count(given) = value {
                    options.$group.$name = u32::try_from(*given).unwrap_or(u32::MAX);
                }
            },
        )
    };
}

/// Declares a point size field.
macro_rules! point_size {
    ($name:ident, $label:literal) => {
        OptionField::new(
            $label,
            concat!("fonts.", stringify!($name)),
            FieldShape::Decimal,
            |options| FieldValue::Decimal(f64::from(options.appearance.fonts.$name)),
            |options, value| {
                if let FieldValue::Decimal(given) = value {
                    options.appearance.fonts.$name = to_point_size(*given);
                }
            },
        )
    };
}

/// Declares a drop down field over a stored choice.
macro_rules! choice {
    ($group:ident . $name:ident, $ty:ty, $label:literal) => {
        OptionField::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            FieldShape::Choice,
            |options| FieldValue::Choice(options.$group.$name.id().to_owned()),
            |options, value| {
                if let FieldValue::Choice(given) = value {
                    options.$group.$name = <$ty>::from_id(given);
                }
            },
        )
    };
}

/// Declares a text field.
macro_rules! text {
    ($group:ident . $name:ident, $label:literal) => {
        OptionField::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            FieldShape::Text,
            |options| FieldValue::Text(options.$group.$name.clone()),
            |options, value| {
                if let FieldValue::Text(given) = value {
                    options.$group.$name = given.clone();
                }
            },
        )
    };
}

/// Declares a field over an optional stored path.
macro_rules! path {
    ($group:ident . $name:ident, $label:literal) => {
        OptionField::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            FieldShape::Path,
            |options| {
                FieldValue::Text(
                    options
                        .$group
                        .$name
                        .as_ref()
                        .map(|path| path.as_path().display().to_string())
                        .unwrap_or_default(),
                )
            },
            |options, value| {
                if let FieldValue::Text(given) = value {
                    options.$group.$name =
                        (!given.is_empty()).then(|| std::path::PathBuf::from(given).into());
                }
            },
        )
    };
}

/// Declares a field over a list of lines.
macro_rules! lines {
    ($group:ident . $name:ident, $label:literal) => {
        OptionField::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            FieldShape::Lines,
            |options| FieldValue::Lines(options.$group.$name.clone()),
            |options, value| {
                if let FieldValue::Lines(given) = value {
                    options.$group.$name.clone_from(given);
                }
            },
        )
    };
}

/// Declares the mask field of one archive format. An absent entry reads as
/// the format's default masks; a blank one drops the format.
macro_rules! archive_mask {
    ($format:ident, $key:literal) => {
        OptionField::new(
            ca_fs::ArchiveFormat::$format.label(),
            $key,
            FieldShape::Text,
            |options| {
                FieldValue::Text(
                    options
                        .archives
                        .masks
                        .get(ca_fs::ArchiveFormat::$format.id())
                        .cloned()
                        .unwrap_or_else(|| ca_fs::ArchiveFormat::$format.default_mask_text()),
                )
            },
            |options, value| {
                if let FieldValue::Text(given) = value {
                    options
                        .archives
                        .masks
                        .insert(ca_fs::ArchiveFormat::$format.id().to_owned(), given.clone());
                }
            },
        )
    };
}

/// One page of the options dialog.
#[derive(Debug, Clone, Copy)]
pub struct OptionPage {
    /// Name in the page tree.
    pub name: &'static str,
    /// Fields on the page, in the order they are drawn.
    pub fields: &'static [OptionField],
    /// Color groups the page edits.
    pub colors: &'static [ColorGroup],
    /// What the page draws beyond its fields.
    pub kind: PageKind,
    /// The policy that disables the whole page, where one does.
    pub policy: Option<PolicyKey>,
}

/// What a page draws beyond its fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// Fields only.
    Fields,
    /// Fields and a color table.
    Colors,
    /// The list of external programs.
    OpenWith,
    /// The per-view command list.
    Commands,
    /// The per-view toolbar order.
    Toolbars,
}

use ca_session::options::{
    BackupLocation, ComparisonPriority, LineEndingsOnSave, NewSessionPlacement, QuickCompareMethod,
    SynchronizeConfirmations, ThemeChoice, ThumbnailMode,
};

const THEME_CHOICES: &[Choice] = &[
    Choice {
        id: "followSystem",
        label: "Follow the operating system",
    },
    Choice {
        id: "light",
        label: "Light",
    },
    Choice {
        id: "dark",
        label: "Dark",
    },
];

const QUICK_COMPARE_CHOICES: &[Choice] = &[
    Choice {
        id: "binary",
        label: "Binary comparison",
    },
    Choice {
        id: "rules",
        label: "Rules-based comparison",
    },
];

/// The placements this build carries out.
///
/// A second window is stored but not offered, because the build opens one
/// window; a document that names it keeps the value and the dialog shows the
/// placement in force.
const PLACEMENT_CHOICES: &[Choice] = &[
    Choice {
        id: "newTab",
        label: "A new tab",
    },
    Choice {
        id: "reuseHomeTab",
        label: "The launcher tab it started from",
    },
];

const SYNCHRONIZE_CHOICES: &[Choice] = &[
    Choice {
        id: "prompt",
        label: "Ask each time",
    },
    Choice {
        id: "yesToAll",
        label: "Answer yes to all",
    },
    Choice {
        id: "noToAll",
        label: "Answer no to all",
    },
];

const BACKUP_LOCATION_CHOICES: &[Choice] = &[
    Choice {
        id: "sameFolder",
        label: "Beside the file",
    },
    Choice {
        id: "namedFolder",
        label: "A named folder",
    },
];

const LINE_ENDING_CHOICES: &[Choice] = &[
    Choice {
        id: "keep",
        label: "Keep what the file had",
    },
    Choice {
        id: "crlf",
        label: "Carriage return and line feed",
    },
    Choice {
        id: "lf",
        label: "Line feed",
    },
    Choice {
        id: "cr",
        label: "Carriage return",
    },
];

const THUMBNAIL_CHOICES: &[Choice] = &[
    Choice {
        id: "compressToFit",
        label: "Compress to fit",
    },
    Choice {
        id: "allowScrolling",
        label: "Allow scrolling",
    },
];

const PRIORITY_CHOICES: &[Choice] = &[
    Choice {
        id: "low",
        label: "Low",
    },
    Choice {
        id: "normal",
        label: "Normal",
    },
    Choice {
        id: "high",
        label: "High",
    },
];

const STARTUP_FIELDS: &[OptionField] = &[
    text!(startup.load_workspace, "On start, load workspace"),
    text!(startup.save_workspace_on_exit, "On exit, save workspace as"),
    flag!(startup.show_quick_compare, "Show quick compare dialog")
        .unavailable(reason::NO_QUICK_COMPARE),
    choice!(
        startup.quick_compare_method,
        QuickCompareMethod,
        "Quick compare method"
    )
    .with_choices(QUICK_COMPARE_CHOICES)
    .unavailable(reason::NO_QUICK_COMPARE),
    flag!(
        startup.open_view_if_different,
        "Open the view when the files differ"
    )
    .unavailable(reason::NO_QUICK_COMPARE),
    flag!(
        startup.run_clipboard_compare_at_startup,
        "Run the clipboard helper at system start"
    )
    .unavailable(reason::NO_CLIPBOARD_HELPER),
    SHELL_FIELD,
];

/// The field is read from and written to the registry on Windows. The shell
/// reads the installed menu before the dialog opens and changes it on apply.
#[cfg(windows)]
const SHELL_FIELD: OptionField = flag!(
    startup.shell_integration,
    "Add the program to the file manager menu"
);

#[cfg(not(windows))]
const SHELL_FIELD: OptionField = flag!(
    startup.shell_integration,
    "Add the program to the file manager menu"
)
.unavailable(reason::NO_SHELL);

const TAB_FIELDS: &[OptionField] = &[
    choice!(
        tabs.new_session_placement,
        NewSessionPlacement,
        "Open a new session in"
    )
    .with_choices(PLACEMENT_CHOICES),
    flag!(
        tabs.show_tab_strip_with_one_tab,
        "Show the tab strip with one tab open"
    ),
    flag!(
        tabs.close_window_with_last_tab,
        "Close the window with its last tab"
    ),
    flag!(
        tabs.confirm_closing_several_tabs,
        "Ask before closing several tabs"
    ),
];

const APPEARANCE_FIELDS: &[OptionField] = &[
    choice!(appearance.theme, ThemeChoice, "Theme").with_choices(THEME_CHOICES),
    flag!(
        appearance.enable_preview,
        "Apply changes to open views at once"
    ),
    point_size!(editor_point_size, "Editor font size"),
    point_size!(hex_point_size, "Byte pane font size"),
    point_size!(listing_point_size, "Listing font size"),
    point_size!(merge_input_point_size, "Merge input pane font size"),
];

const FOLDER_VIEW_FIELDS: &[OptionField] = &[
    OptionField::new(
        "Use the system font",
        "fonts.folder_uses_system_font",
        FieldShape::Flag,
        |options| FieldValue::Flag(options.appearance.fonts.folder_uses_system_font),
        |options, value| {
            if let FieldValue::Flag(given) = value {
                options.appearance.fonts.folder_uses_system_font = *given;
            }
        },
    ),
    point_size!(folder_point_size, "Folder listing font size"),
    flag!(folder_views.use_stripes, "Use stripes"),
    flag!(
        folder_views.selection_uses_system,
        "Use the system selection color"
    ),
];

const FILE_VIEW_FIELDS: &[OptionField] = &[
    flag!(file_views.use_stripes, "Use stripes"),
    flag!(
        file_views.selection_uses_system,
        "Use the system selection color"
    ),
];

const PICTURE_FIELDS: &[OptionField] = &[flag!(
    picture.show_transparency_as_checkerboard,
    "Show transparency as checkerboarding"
)];

const TEXT_EDITING_FIELDS: &[OptionField] = &[
    count!(text_editing.tab_stop, "Columns between tab stops").with_limit(64),
    flag!(
        text_editing.insert_spaces_for_tabs,
        "The Tab key inserts spaces"
    ),
    flag!(text_editing.auto_indent, "Auto indent"),
    flag!(text_editing.backspace_unindents, "Backspace unindents"),
    choice!(
        text_editing.line_endings_on_save,
        LineEndingsOnSave,
        "Line endings on save"
    )
    .with_choices(LINE_ENDING_CHOICES),
    flag!(
        text_editing.caret_past_line_end,
        "Let the caret sit past the end of a line"
    )
    .unavailable(reason::CARET_CLAMPED),
    flag!(
        text_editing.typing_replaces_selection,
        "Typing replaces the selection"
    )
    .unavailable(reason::TYPING_REPLACES),
    flag!(
        text_editing.find_uses_current_word,
        "Start Find from the word under the caret"
    ),
    flag!(
        text_editing.show_filtered_line_counts,
        "Show filtered line counts"
    )
    .unavailable(reason::NO_FILTERED_COUNT),
    count!(text_editing.context_lines, "Number of context lines").with_limit(999),
];

const NEXT_DIFFERENCE_FIELDS: &[OptionField] = &[
    flag!(
        next_difference.go_to_first_difference_on_load,
        "Go to the first difference when files load"
    ),
    flag!(
        next_difference.go_to_next_after_copy,
        "Go to the next difference after copying"
    ),
    flag!(
        next_difference.limit_to_current_folder,
        "Limit Next Difference Files to the current folder"
    )
    .unavailable(reason::NO_NEXT_DIFFERENCE_FILES),
    flag!(
        next_difference.wrap_around,
        "Wrap around to the first difference"
    ),
    flag!(next_difference.show_message_panel, "Show the message panel"),
];

const BACKUP_FIELDS: &[OptionField] = &[
    flag!(
        backups.before_overwrite,
        "Back up a file before it is overwritten"
    ),
    flag!(backups.before_save, "Back up a file before it is saved"),
    choice!(backups.location, BackupLocation, "Where copies are written")
        .with_choices(BACKUP_LOCATION_CHOICES),
    path!(backups.folder, "Backup folder"),
    text!(backups.suffix, "Name suffix"),
    count!(backups.copies, "Copies kept").with_limit(99),
];

const FILE_OPERATION_FIELDS: &[OptionField] = &[
    flag!(file_operations.confirm_copy, "Confirm a copy"),
    flag!(file_operations.confirm_move, "Confirm a move"),
    flag!(file_operations.confirm_merge, "Confirm a merge"),
    flag!(file_operations.confirm_delete, "Confirm a delete"),
    choice!(
        file_operations.synchronize_confirmations,
        SynchronizeConfirmations,
        "Confirmations during a synchronize"
    )
    .with_choices(SYNCHRONIZE_CHOICES)
    .unavailable(reason::SYNC_ASKS),
    flag!(
        file_operations.include_hidden_items,
        "Include hidden items by default"
    )
    .unavailable(reason::SCAN_SETTING),
];

const TWEAK_FIELDS: &[OptionField] = &[
    flag!(tweaks.check_for_updates, "Check for updates")
        .under_policy(PolicyKey::DisableCheckForUpdates),
    count!(tweaks.check_for_updates_days, "Days between update checks")
        .with_limit(365)
        .under_policy(PolicyKey::DisableCheckForUpdates),
    flag!(
        tweaks.syntax_highlighting_on_difference_lines,
        "Show syntax highlighting on difference lines"
    ),
    flag!(
        tweaks.crosshatch_past_end_of_file,
        "Use crosshatching past the end of the file"
    )
    .unavailable(reason::NO_CROSSHATCH),
    flag!(
        tweaks.right_side_gutter_for_left_editor,
        "Right side gutter for the left editor"
    )
    .unavailable(reason::GUTTER_LEFT),
    flag!(tweaks.use_orphan_color, "Use the orphan color").unavailable(reason::ORPHAN_ALWAYS),
    count!(tweaks.extra_line_spacing, "Extra line spacing").with_limit(32),
    count!(tweaks.column_line_at, "Show a column line at").with_limit(999),
    count!(tweaks.dim_inactive_pane_percent, "Dim the inactive pane by").with_limit(100),
    flag!(
        tweaks.different_font_for_merge_input_panes,
        "Use a different font for the merge input panes"
    )
    .unavailable(reason::ONE_MERGE_GRID),
    flag!(
        tweaks.beep_after_long_file_operations,
        "Sound a tone after a long file operation"
    )
    .unavailable(reason::NO_SOUND),
    flag!(
        tweaks.remove_read_only_flag_from_optical_media,
        "Remove the read-only flag when copying from optical media"
    )
    .unavailable(reason::NO_OPTICAL_CASE),
    flag!(
        tweaks.check_for_files_changed_on_disk,
        "Check for files changed on disk"
    )
    .unavailable(reason::NO_WATCHER),
    flag!(
        tweaks.automatically_reload_unless_changes_are_discarded,
        "Reload automatically unless changes would be discarded"
    )
    .unavailable(reason::NO_WATCHER),
    flag!(tweaks.use_ipv6, "Use IPv6 when it is available")
        .under_policy(PolicyKey::DisableRemoteProfiles)
        .unavailable(reason::REMOTE_STACK),
    choice!(
        tweaks.comparison_priority,
        ComparisonPriority,
        "Comparison priority"
    )
    .with_choices(PRIORITY_CHOICES)
    .unavailable(reason::WORKER_PRIORITY),
    OptionField::new(
        "Buffer size for a binary compare",
        "tweaks.binary_buffer_bytes",
        FieldShape::Count,
        |options| FieldValue::Count(options.tweaks.binary_buffer_bytes),
        |options, value| {
            if let FieldValue::Count(given) = value {
                options.tweaks.binary_buffer_bytes = *given;
            }
        },
    )
    .with_limit(64 * 1024 * 1024)
    .unavailable(reason::ENGINE_BUFFER),
    flag!(tweaks.show_hints, "Show hints for toolbar buttons").unavailable(reason::TOOLKIT_HINTS),
    flag!(
        tweaks.show_keyboard_shortcut_in_hints,
        "Show the keyboard shortcut in hints"
    )
    .unavailable(reason::TOOLKIT_HINTS),
    flag!(
        tweaks.escape_closes_file_views,
        "The Escape key closes file views"
    ),
    flag!(
        tweaks.single_vertical_scrollbar,
        "Single vertical scrollbar"
    )
    .unavailable(reason::ONE_SCROLLBAR),
    flag!(tweaks.sticky_splitter_position, "Sticky splitter position")
        .unavailable(reason::EQUAL_SPLIT),
    choice!(tweaks.thumbnail_mode, ThumbnailMode, "Thumbnail display")
        .with_choices(THUMBNAIL_CHOICES)
        .unavailable(reason::THUMBNAIL_FITS),
    count!(
        tweaks.hint_delay_milliseconds,
        "Milliseconds before a hint appears"
    )
    .with_limit(60_000)
    .unavailable(reason::TOOLKIT_HINTS),
    count!(
        tweaks.hint_duration_milliseconds,
        "Milliseconds before a hint disappears"
    )
    .with_limit(60_000)
    .unavailable(reason::TOOLKIT_HINTS),
    count!(
        tweaks.incremental_search_reset_milliseconds,
        "Milliseconds before an incremental search resets"
    )
    .with_limit(60_000)
    .unavailable(reason::NO_TYPE_SEARCH),
    lines!(tweaks.name_filter_presets, "Name filter presets").unavailable(reason::FILTER_CONTROL),
    flag!(
        tweaks.script_beep_when_finished,
        "Scripts: sound a tone when finished"
    )
    .unavailable(reason::SCRIPT_RUNNER),
    flag!(
        tweaks.script_close_when_finished,
        "Scripts: close when finished"
    )
    .unavailable(reason::SCRIPT_RUNNER),
    path!(tweaks.shared_sessions_file, "Shared sessions file")
        .unavailable(reason::NO_SHARED_SESSIONS),
];

const ARCHIVE_FIELDS: &[OptionField] = &[
    archive_mask!(SevenZip, "archives.7z"),
    archive_mask!(Snapshot, "archives.snapshot"),
    archive_mask!(Bz2, "archives.bz2"),
    archive_mask!(TarBz2, "archives.tbz"),
    archive_mask!(DiskImage, "archives.img"),
    archive_mask!(Deb, "archives.deb"),
    archive_mask!(Gz, "archives.gz"),
    archive_mask!(TarGz, "archives.tgz"),
    archive_mask!(Cab, "archives.cab"),
    archive_mask!(Rar, "archives.rar"),
    archive_mask!(Rpm, "archives.rpm"),
    archive_mask!(Tar, "archives.tar"),
    archive_mask!(Xz, "archives.xz"),
    archive_mask!(TarXz, "archives.txz"),
    archive_mask!(Zip, "archives.zip"),
];

const COLOR_TEXT: &[ColorGroup] = &[
    ColorGroup::Text,
    ColorGroup::Hex,
    ColorGroup::Table,
    ColorGroup::Merge,
];
const COLOR_FOLDER: &[ColorGroup] = &[ColorGroup::Folder];
const COLOR_PICTURE: &[ColorGroup] = &[ColorGroup::Picture];
const NO_COLORS: &[ColorGroup] = &[];

/// Every page of the dialog, in the order the page tree lists them.
pub const PAGES: &[OptionPage] = &[
    OptionPage {
        name: "Startup",
        fields: STARTUP_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "Tabs",
        fields: TAB_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "Appearance",
        fields: APPEARANCE_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "Folder Views",
        fields: FOLDER_VIEW_FIELDS,
        colors: COLOR_FOLDER,
        kind: PageKind::Colors,
        policy: None,
    },
    OptionPage {
        name: "File Views",
        fields: FILE_VIEW_FIELDS,
        colors: COLOR_TEXT,
        kind: PageKind::Colors,
        policy: None,
    },
    OptionPage {
        name: "Picture Compare",
        fields: PICTURE_FIELDS,
        colors: COLOR_PICTURE,
        kind: PageKind::Colors,
        policy: None,
    },
    OptionPage {
        name: "Text Editing",
        fields: TEXT_EDITING_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "Next Difference",
        fields: NEXT_DIFFERENCE_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "Backups",
        fields: BACKUP_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "File Operations",
        fields: FILE_OPERATION_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "Archive Types",
        fields: ARCHIVE_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
    OptionPage {
        name: "Commands",
        fields: &[],
        colors: NO_COLORS,
        kind: PageKind::Commands,
        policy: None,
    },
    OptionPage {
        name: "Toolbars",
        fields: &[],
        colors: NO_COLORS,
        kind: PageKind::Toolbars,
        policy: None,
    },
    OptionPage {
        name: "Open With",
        fields: &[],
        colors: NO_COLORS,
        kind: PageKind::OpenWith,
        policy: None,
    },
    OptionPage {
        name: "Tweaks",
        fields: TWEAK_FIELDS,
        colors: NO_COLORS,
        kind: PageKind::Fields,
        policy: None,
    },
];

/// The page named `name`, where the dialog has one.
#[must_use]
pub fn page(name: &str) -> Option<&'static OptionPage> {
    PAGES.iter().find(|page| page.name == name)
}

/// The field named `key`, on any page.
#[must_use]
pub fn field(key: &str) -> Option<&'static OptionField> {
    PAGES
        .iter()
        .flat_map(|page| page.fields.iter())
        .find(|field| field.key == key)
}

/// What the Archive Types page asks of a mask, and what reads it.
pub const ARCHIVE_PAGE_NOTE: &str = "Separate masks with semicolons. A blank mask drops the      format. Scripts read these masks when they open an archive. RAR archives are read      through the system tar (bsdtar) and are read-only.";

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{field, PageKind, PAGES};
    use crate::settings::field::FieldValue;
    use ca_session::options::ProgramOptions;
    use ca_session::{AdminPolicies, InMemoryPolicySource, PolicyKey, PolicyLoader};

    #[test]
    fn the_page_tree_matches_the_documented_order() {
        let names: Vec<&str> = PAGES.iter().map(|page| page.name).collect();
        assert_eq!(
            names,
            vec![
                "Startup",
                "Tabs",
                "Appearance",
                "Folder Views",
                "File Views",
                "Picture Compare",
                "Text Editing",
                "Next Difference",
                "Backups",
                "File Operations",
                "Archive Types",
                "Commands",
                "Toolbars",
                "Open With",
                "Tweaks",
            ]
        );
    }

    #[test]
    fn the_archive_page_names_every_readable_format_by_its_stored_key() {
        let page = super::page("Archive Types").unwrap();
        for format in ca_fs::ArchiveFormat::all() {
            let key = format!("archives.{}", format.id());
            let listed = page.fields.iter().any(|field| field.key == key);
            assert_eq!(listed, format.is_supported(), "{key}");
        }
        let options = ProgramOptions::default();
        let zip = field("archives.zip").unwrap();
        assert_eq!(
            zip.read(&options),
            FieldValue::Text(ca_fs::ArchiveFormat::Zip.default_mask_text())
        );
    }

    #[test]
    fn no_two_fields_share_a_key() {
        let mut keys: Vec<&str> = PAGES
            .iter()
            .flat_map(|page| page.fields.iter())
            .map(|field| field.key)
            .collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "a key is declared twice");
    }

    /// A field that takes an edit has to hold it. A field that takes none has
    /// to say why.
    #[test]
    fn every_field_either_round_trips_an_edit_or_states_why_it_does_not() {
        for page in PAGES {
            for field in page.fields {
                let mut options = ProgramOptions::default();
                let before = field.read(&options);
                let edited = match &before {
                    FieldValue::Flag(value) => FieldValue::Flag(!value),
                    FieldValue::Count(value) => FieldValue::Count(value.saturating_add(1).min(7)),
                    FieldValue::Decimal(_) => FieldValue::Decimal(19.0),
                    FieldValue::Text(_) => FieldValue::Text("probe".to_owned()),
                    FieldValue::Choice(id) => FieldValue::Choice(
                        field
                            .choices
                            .iter()
                            .map(|choice| choice.id)
                            .find(|other| *other != id)
                            .unwrap_or("probe")
                            .to_owned(),
                    ),
                    FieldValue::Lines(_) => FieldValue::Lines(vec!["probe".to_owned()]),
                    other => panic!("{} carries {other:?}", field.key),
                };
                field.write(&mut options, &edited);
                assert_eq!(
                    field.read(&options),
                    edited,
                    "{} did not hold the edit",
                    field.key
                );
                if !field.is_bound() {
                    assert!(
                        field.unavailable.is_some_and(|text| text.ends_with('.')),
                        "{} states no reason",
                        field.key
                    );
                }
            }
        }
    }

    #[test]
    fn a_choice_field_offers_the_value_it_reads() {
        for page in PAGES {
            for field in page.fields {
                let options = ProgramOptions::default();
                if let FieldValue::Choice(id) = field.read(&options) {
                    assert!(
                        field.choices.iter().any(|choice| choice.id == id),
                        "{} reads {id}, which its list does not offer",
                        field.key
                    );
                }
            }
        }
    }

    #[test]
    fn a_policy_disables_the_fields_it_names_and_states_why() {
        let policies: AdminPolicies = InMemoryPolicySource::new()
            .with(PolicyKey::DisableCheckForUpdates, true)
            .load()
            .unwrap();
        let field = field("tweaks.check_for_updates").unwrap();
        assert_eq!(field.blocked_by(policies), Some(super::POLICY_REASON));
        assert_eq!(field.blocked_by(AdminPolicies::default()), None);
    }

    #[test]
    fn each_page_that_edits_colors_names_at_least_one_group() {
        for page in PAGES {
            if page.kind == PageKind::Colors {
                assert!(!page.colors.is_empty(), "{}", page.name);
            } else {
                assert!(page.colors.is_empty(), "{}", page.name);
            }
        }
    }
}
