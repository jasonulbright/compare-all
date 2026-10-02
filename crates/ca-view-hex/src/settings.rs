//! The one conversion from stored settings to the options the byte engine
//! takes.

use crate::chars::CharEncoding;
use crate::jobs::CompareSettings;
use ca_diff::ByteAlignment;
use ca_session::settings::binary::{
    HexAlignment, HexCharEncoding, HexCompareSettings, HexComparisonSettings,
};

/// Smallest row width a byte pane can draw.
const MINIMUM_BYTES_PER_ROW: u32 = 1;

/// The pairing rule a stored alignment names.
///
/// A rule this build does not understand falls back to the complete pass
/// rather than refusing the session.
#[must_use]
pub fn byte_alignment(alignment: &HexAlignment) -> ByteAlignment {
    match alignment {
        HexAlignment::Fast => ByteAlignment::Fast,
        HexAlignment::None => ByteAlignment::None,
        _ => ByteAlignment::Complete,
    }
}

/// The options a stored comparison group states.
#[must_use]
pub fn options_from(comparison: &HexComparisonSettings) -> CompareSettings {
    CompareSettings {
        alignment: byte_alignment(&comparison.alignment),
        // A row of no bytes would divide by zero when the layout is built.
        bytes_per_row: comparison.bytes_per_row.max(MINIMUM_BYTES_PER_ROW),
    }
}

/// The character mapping a stored encoding names.
///
/// An encoding this build has no table for falls back to the single byte code
/// page rather than refusing the session.
#[must_use]
pub fn char_encoding(encoding: &HexCharEncoding) -> CharEncoding {
    match encoding {
        HexCharEncoding::Ascii => CharEncoding::Ascii,
        _ => CharEncoding::Ansi,
    }
}

/// The options of a whole byte comparison session.
#[must_use]
pub fn options_of(settings: &HexCompareSettings) -> CompareSettings {
    options_from(&settings.comparison)
}

/// Writes the row width back into the settings it came from, so the toolbar
/// control and the settings page edit one value.
pub fn write_bytes_per_row(comparison: &mut HexComparisonSettings, bytes: u32) {
    comparison.bytes_per_row = bytes.max(MINIMUM_BYTES_PER_ROW);
}

/// Writes the pairing rule back into the settings it came from.
pub fn write_alignment(comparison: &mut HexComparisonSettings, alignment: ByteAlignment) {
    comparison.alignment = match alignment {
        ByteAlignment::Fast => HexAlignment::Fast,
        ByteAlignment::None => HexAlignment::None,
        ByteAlignment::Complete => HexAlignment::Complete,
    };
}

/// Writes the character mapping back into the settings it came from, so the
/// toolbar list and a settings page edit one value.
pub fn write_char_encoding(comparison: &mut HexComparisonSettings, encoding: CharEncoding) {
    comparison.char_encoding = match encoding {
        CharEncoding::Ascii => HexCharEncoding::Ascii,
        CharEncoding::Ansi => HexCharEncoding::Ansi,
    };
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{byte_alignment, options_of, write_alignment, write_bytes_per_row};
    use ca_diff::ByteAlignment;
    use ca_session::settings::binary::{HexAlignment, HexCompareSettings};

    #[test]
    fn every_stored_alignment_reaches_its_own_pass() {
        assert_eq!(
            byte_alignment(&HexAlignment::Complete),
            ByteAlignment::Complete
        );
        assert_eq!(byte_alignment(&HexAlignment::Fast), ByteAlignment::Fast);
        assert_eq!(byte_alignment(&HexAlignment::None), ByteAlignment::None);
        assert_eq!(
            byte_alignment(&HexAlignment::Unknown(serde_json::json!("future"))),
            ByteAlignment::Complete,
            "a rule this build has no pass for still opens"
        );
    }

    #[test]
    fn the_row_width_reaches_the_layout_unchanged() {
        let mut settings = HexCompareSettings::default();
        settings.comparison.bytes_per_row = 32;
        assert_eq!(options_of(&settings).bytes_per_row, 32);
    }

    #[test]
    fn a_row_of_no_bytes_is_raised_to_one() {
        let mut settings = HexCompareSettings::default();
        settings.comparison.bytes_per_row = 0;
        assert_eq!(options_of(&settings).bytes_per_row, 1);
    }

    #[test]
    fn the_stored_defaults_produce_the_documented_options() {
        let options = options_of(&HexCompareSettings::default());
        assert_eq!(options.alignment, ByteAlignment::Complete);
        assert_eq!(options.bytes_per_row, 16);
    }

    #[test]
    fn the_toolbar_writes_reach_the_settings_they_came_from() {
        let mut settings = HexCompareSettings::default();
        write_bytes_per_row(&mut settings.comparison, 8);
        write_alignment(&mut settings.comparison, ByteAlignment::Fast);
        let options = options_of(&settings);
        assert_eq!(options.bytes_per_row, 8);
        assert_eq!(options.alignment, ByteAlignment::Fast);
        write_bytes_per_row(&mut settings.comparison, 0);
        assert_eq!(settings.comparison.bytes_per_row, 1);
    }
}
