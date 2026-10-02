//! Named default values for settings fields.
//!
//! Changing one of these values changes observable comparison results, so each
//! is named rather than written inline at its use site.

/// Defaults of documented comparison behavior.
pub mod documented {
    /// Line ending style differences are ignored unless the session asks for
    /// them to be compared.
    pub const COMPARE_LINE_ENDINGS: bool = false;
    /// Content larger than this is treated as binary rather than as text.
    pub const BINARY_SIZE_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024;
}

/// Defaults that decide which differences are unimportant.
pub mod measured {
    /// Capitalization differences in unclaimed text are unimportant: a pair of
    /// lines differing only in letter case shows as an unimportant difference.
    pub const CHARACTER_CASE_IMPORTANT: bool = false;
}

/// Defaults that no other setting constrains.
pub mod provisional {
    /// Seconds two timestamps may differ by before the pair counts as changed.
    pub const TIMESTAMP_TOLERANCE_SECONDS: u32 = 0;
    /// Lines scanned ahead and behind when looking for a matching line.
    pub const SKEW_TOLERANCE_LINES: u32 = 500;
    /// Rows scanned ahead and behind when looking for a matching row.
    pub const SKEW_TOLERANCE_ROWS: u32 = 500;
    /// Minutes between automatic refreshes when automatic refresh is on.
    pub const AUTO_REFRESH_MINUTES: u32 = 5;
    /// Bytes shown per row in a hexadecimal view.
    pub const HEX_BYTES_PER_ROW: u32 = 16;
    /// Content at or below this size is read without holding a file lock.
    pub const HEX_UNLOCKED_LOAD_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
    /// Greatest per-pixel difference still counted as unimportant.
    pub const PICTURE_TOLERANCE: u8 = 0;
    /// Percentage of the second image mixed into a blended view.
    pub const PICTURE_BLEND_PERCENT: u8 = 50;
    /// Seconds two media durations may differ by before counting as changed.
    pub const MEDIA_DURATION_TOLERANCE_SECONDS: u32 = 0;
    /// Difference two numeric cells may show before counting as important.
    pub const NUMERIC_TOLERANCE: f64 = 0.0;
    /// Seconds two date cells may differ by before counting as important.
    pub const DATE_TOLERANCE_SECONDS: u32 = 0;
    /// Sessions retained by automatic saving; zero disables automatic saving.
    pub const MAX_AUTO_SAVED_SESSIONS: usize = 20;
}
