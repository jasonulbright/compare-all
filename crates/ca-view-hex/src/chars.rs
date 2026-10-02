//! The character area's byte to character mapping.
//!
//! Each byte of a row is shown as one character beside its two hexadecimal
//! digits, so the two areas stay column for column. Only single byte encodings
//! keep that property, and the table for one is built once and then read by
//! index, so a frame never decodes anything.

use ca_text::{DecodeOptions, TextEncoding};
use std::sync::OnceLock;

/// The character shown for a byte the encoding has no printable character for.
pub const PLACEHOLDER: char = '.';

/// How the character area reads a byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharEncoding {
    /// Seven bit ASCII. Every byte above `0x7F` is unprintable.
    Ascii,
    /// The Windows single byte code page, shown under the name ANSI.
    #[default]
    Ansi,
}

/// Every encoding the character area offers, in the order a menu lists them.
///
/// Only a single byte encoding keeps one character under each pair of digits,
/// so a multi byte encoding has no place here.
pub const ENCODINGS: [CharEncoding; 2] = [CharEncoding::Ascii, CharEncoding::Ansi];

impl CharEncoding {
    /// The label a menu or a status line shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII",
            Self::Ansi => "ANSI",
        }
    }

    /// The label of the text encoding the table is built from.
    const fn source_label(self) -> Option<&'static str> {
        match self {
            Self::Ascii => None,
            Self::Ansi => Some("windows-1252"),
        }
    }

    /// The character shown for one byte.
    #[must_use]
    pub fn character(self, byte: u8) -> char {
        match table(self).get(byte as usize).copied() {
            Some(UNMAPPED) | None => PLACEHOLDER,
            Some(found) => found,
        }
    }

    /// The bytes a piece of text stands for, or `None` when the encoding cannot
    /// represent one of its characters.
    ///
    /// This is what turns a search phrase into the bytes a search looks for.
    #[must_use]
    pub fn encode(self, text: &str) -> Option<Vec<u8>> {
        let table = table(self);
        let mut out = Vec::with_capacity(text.len());
        for wanted in text.chars() {
            if wanted == UNMAPPED {
                return None;
            }
            let found = table.iter().position(|candidate| *candidate == wanted)?;
            out.push(u8::try_from(found).ok()?);
        }
        Some(out)
    }
}

/// The table entry standing for a byte the encoding has no character for.
///
/// A real character is needed, and no code page maps a byte to the null
/// character, so the slot cannot collide with a mapping.
const UNMAPPED: char = '\0';

/// The table for one encoding, built on first use.
fn table(encoding: CharEncoding) -> &'static [char; 256] {
    static ASCII: OnceLock<[char; 256]> = OnceLock::new();
    static ANSI: OnceLock<[char; 256]> = OnceLock::new();
    let slot = match encoding {
        CharEncoding::Ascii => &ASCII,
        CharEncoding::Ansi => &ANSI,
    };
    slot.get_or_init(|| build(encoding))
}

fn build(encoding: CharEncoding) -> [char; 256] {
    let mut table = [UNMAPPED; 256];
    let forced = encoding
        .source_label()
        .and_then(TextEncoding::from_label)
        .map(|found| DecodeOptions {
            forced: Some(found),
            ..DecodeOptions::default()
        });
    for (index, slot) in table.iter_mut().enumerate() {
        let byte = u8::try_from(index).unwrap_or(0);
        if !(0x20..0x7F).contains(&byte) && encoding == CharEncoding::Ascii {
            continue;
        }
        if (0x20..0x7F).contains(&byte) {
            *slot = char::from(byte);
            continue;
        }
        let Some(options) = &forced else {
            continue;
        };
        let decoded = ca_text::decode(&[byte], options);
        let mut characters = decoded.text.chars();
        // A byte the code page leaves undefined decodes to a replacement
        // character or to nothing at all; either way it stays unprintable.
        match (characters.next(), characters.next()) {
            (Some(found), None) if printable(found) => *slot = found,
            _ => {}
        }
    }
    table
}

/// True when a character can stand on its own in a fixed width column.
fn printable(value: char) -> bool {
    !value.is_control() && value != '\u{FFFD}' && value != '\u{00AD}'
}

#[cfg(test)]
mod tests {
    use super::{CharEncoding, ENCODINGS, PLACEHOLDER};

    #[test]
    fn printable_ascii_reads_the_same_in_every_encoding() {
        for encoding in ENCODINGS {
            assert_eq!(encoding.character(b'A'), 'A');
            assert_eq!(encoding.character(b' '), ' ');
            assert_eq!(encoding.character(b'~'), '~');
            assert_eq!(encoding.character(0x00), PLACEHOLDER);
            assert_eq!(encoding.character(0x0A), PLACEHOLDER);
            assert_eq!(encoding.character(0x7F), PLACEHOLDER);
        }
    }

    #[test]
    fn only_ascii_leaves_the_upper_half_unprintable() {
        assert_eq!(CharEncoding::Ascii.character(0xE9), PLACEHOLDER);
        assert_eq!(CharEncoding::Ansi.character(0xE9), 'é');
        assert_eq!(CharEncoding::Ansi.character(0x80), '€');
    }

    /// A code page leaves some bytes of its upper half undefined, and those
    /// stay unprintable rather than showing a replacement character.
    #[test]
    fn an_undefined_byte_stays_unprintable() {
        assert_eq!(CharEncoding::Ansi.character(0x81), PLACEHOLDER);
        assert_eq!(CharEncoding::Ansi.character(0x8D), PLACEHOLDER);
    }

    #[test]
    fn a_phrase_encodes_to_the_bytes_the_area_would_show() {
        assert_eq!(
            CharEncoding::Ascii.encode("Hi!"),
            Some(vec![b'H', b'i', b'!'])
        );
        assert_eq!(CharEncoding::Ansi.encode("é"), Some(vec![0xE9]));
        assert_eq!(CharEncoding::Ansi.encode("."), Some(vec![0x2E]));
        assert_eq!(CharEncoding::Ascii.encode("é"), None);
    }

    #[test]
    fn every_encoding_has_a_label() {
        for encoding in ENCODINGS {
            assert!(!encoding.label().is_empty());
        }
    }
}
