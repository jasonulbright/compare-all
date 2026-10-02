//! Named defaults of option pages.
//!
//! Each value is named so a change edits one line rather than a literal buried
//! in a struct. Other defaults are written directly in the `Default`
//! implementation of the page that owns them.

/// Point size of the editor font.
///
/// The text comparison draws at this size when no font is stored, so the row
/// height does not change when the options document names no font.
pub const EDITOR_POINT_SIZE: f32 = 10.5;
/// Point size of the byte pane font.
pub const HEX_POINT_SIZE: f32 = 13.0;
/// Point size of every remaining listing.
pub const LISTING_POINT_SIZE: f32 = 12.0;
/// Point size of a folder listing when the system font is not used.
pub const FOLDER_POINT_SIZE: f32 = 12.0;
/// Point size of the merge input panes when they carry their own font.
pub const MERGE_INPUT_POINT_SIZE: f32 = 10.5;

/// Matching lines shown around a difference section under Show Context.
pub const CONTEXT_LINES: u32 = 2;
/// Days between automatic update checks.
pub const UPDATE_CHECK_DAYS: u32 = 7;
/// Padding added between text rows.
pub const EXTRA_LINE_SPACING: u32 = 0;
/// Column the vertical ruler is drawn at. Zero hides it.
pub const COLUMN_LINE_AT: u32 = 0;
/// Percentage the pane without focus is darkened by.
pub const DIM_INACTIVE_PANE_PERCENT: u32 = 0;
/// Bytes read per step of a byte for byte comparison.
pub const BINARY_BUFFER_BYTES: u64 = 65_536;
/// Milliseconds before a hint appears.
pub const HINT_DELAY_MILLISECONDS: u32 = 500;
/// Milliseconds a hint stays on screen.
pub const HINT_DURATION_MILLISECONDS: u32 = 5_000;
/// Milliseconds of idle time after which type ahead search starts a new term.
pub const INCREMENTAL_SEARCH_RESET_MILLISECONDS: u32 = 1_000;

/// Number of backup copies kept per file.
pub const BACKUP_COPIES: u32 = 1;
/// Suffix added to the name of a backup copy.
pub const BACKUP_SUFFIX: &str = ".bak";

/// Greatest value a point size field accepts.
pub const MAXIMUM_POINT_SIZE: f64 = 72.0;
/// Smallest value a point size field accepts.
pub const MINIMUM_POINT_SIZE: f64 = 4.0;
