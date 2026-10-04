//! Text loading and editing for compare-all.
//!
//! The crate covers everything between a file's bytes and an editable buffer a
//! diff layer can consume:
//!
//! - [`encoding`]: detect, decode and re-encode, preserving byte order marks so
//!   an unedited load and save is byte identical.
//! - [`eol`]: per-line ending detection, the dominant style, and conversion.
//! - [`buffer`]: a rope backed [`TextBuffer`] with line indexed access, grouped
//!   undo and redo, a modified flag and change notifications.
//! - [`transform`]: the editing conversions, each one a single undo group.
//! - [`search`]: find and replace, including the documented regular expression
//!   dialect.
//! - [`binary`]: the heuristic that routes a file to a hex comparison instead.
//!
//! Nothing here performs I/O or touches a user interface.

#![allow(clippy::module_name_repetitions)]

use std::sync::Arc;

pub mod binary;
pub mod buffer;
pub mod encoding;
pub mod eol;
pub mod search;
pub mod transform;

pub use binary::{inspect, looks_binary, BinaryHeuristic, BinaryVerdict};
pub use buffer::{
    AppliedEdit, Change, EditError, EditKind, EditSnapshot, LineRange, TextBuffer, UndoGroup,
};
pub use encoding::{
    decode, detect, encode, encode_file, sniff_utf16, DecodeOptions, Decoded, Detection,
    DetectionSource, EncodeOutcome, EncodingSpec, TextEncoding, Unmappable,
};
pub use eol::{
    convert as convert_eol, line_endings, scan as scan_eol, EolStyle, EolSummary, LineEnding,
};
pub use search::{
    expand_replacement, translate_pattern, Match, SearchError, SearchOptions, SearchResults,
    Searcher,
};
pub use transform::{
    convert_line_endings, decrease_indent, display_width, increase_indent, insert_line_after,
    insert_line_before, leading_spaces_to_tabs, spaces_to_tabs, tabs_to_spaces, to_lower_case,
    to_upper_case, trim_trailing_whitespace, whole_buffer, TabSettings,
};

/// Why a loaded file cannot be written back without losing content.
#[derive(Clone, Debug, thiserror::Error)]
pub enum SaveError {
    /// The text holds a character the file's encoding cannot represent. Writing
    /// anyway substitutes a numeric character reference for it.
    #[error("{encoding} cannot represent {ch:?} at character {char_index}")]
    Unmappable {
        /// The character the encoding cannot represent.
        ch: char,
        /// Its index in characters from the start of the text.
        char_index: usize,
        /// The encoding that cannot represent it.
        encoding: TextEncoding,
    },
    /// The load needed replacement characters, so the buffer no longer holds the
    /// bytes the file did and an edited save would destroy them.
    #[error("the file did not decode cleanly, so an edited save cannot reproduce its bytes")]
    LossyLoad,
}

/// A loaded text file: its content plus the facts needed to write it back.
#[derive(Clone, Debug)]
pub struct LoadedText {
    /// The editable buffer.
    pub buffer: TextBuffer,
    /// The encoding and byte order mark the bytes carried.
    pub spec: EncodingSpec,
    /// Which detection step chose the encoding.
    pub source: DetectionSource,
    /// Line ending statistics as loaded.
    pub eol: EolSummary,
    /// True when decoding needed replacement characters, so saving is lossy.
    pub had_errors: bool,
    /// Bytes past an honored end of file marker, preserved verbatim.
    trailer: Arc<[u8]>,
    /// The bytes as loaded, kept only when decoding was lossy. Re-encoding the
    /// decoded text would write replacement characters over content the buffer
    /// never held, so an unedited save replays these instead.
    original: Option<Arc<[u8]>>,
}

impl LoadedText {
    /// Decodes `bytes` into a buffer, recording how to write them back.
    #[must_use]
    pub fn load(bytes: &[u8], options: &DecodeOptions) -> Self {
        let decoded = decode(bytes, options);
        let eol = scan_eol(&decoded.text);
        Self {
            buffer: TextBuffer::from_text(&decoded.text),
            spec: decoded.spec,
            source: decoded.source,
            eol,
            had_errors: decoded.had_errors,
            trailer: Arc::from(decoded.trailer),
            original: decoded.had_errors.then(|| Arc::from(bytes)),
        }
    }

    /// Encodes the current buffer content back to bytes.
    ///
    /// Line endings are left exactly as the buffer holds them, so a file that
    /// was not edited and not converted is reproduced byte for byte, including
    /// one that did not decode cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`SaveError`] rather than bytes that differ from what the buffer
    /// shows: when the encoding cannot represent a character the buffer holds,
    /// and when an edited buffer came from a file that did not decode cleanly.
    /// [`LoadedText::to_bytes_lossy`] accepts both.
    pub fn to_bytes(&self) -> Result<Vec<u8>, SaveError> {
        if self.had_errors {
            if self.buffer.is_modified() {
                return Err(SaveError::LossyLoad);
            }
            if let Some(original) = &self.original {
                return Ok(original.to_vec());
            }
        }
        let outcome = encode_file(&self.buffer.text(), self.spec, &self.trailer);
        if let Some(bad) = outcome.unmappable {
            return Err(SaveError::Unmappable {
                ch: bad.ch,
                char_index: bad.char_index,
                encoding: self.spec.encoding,
            });
        }
        Ok(outcome.bytes)
    }

    /// True when two unedited loads reproduce the same original bytes.
    ///
    /// A lossy decode can map different byte sequences to the same replacement
    /// text. This comparison uses the retained source bytes for those loads
    /// and re-encodes clean loads only when needed.
    #[must_use]
    pub fn original_bytes_equal(&self, other: &Self) -> bool {
        if self.buffer.is_modified() || other.buffer.is_modified() {
            return false;
        }
        match (self.had_errors, other.had_errors) {
            (true, true) => self.original == other.original,
            (true, false) => other
                .to_bytes()
                .ok()
                .zip(self.original.as_deref())
                .is_some_and(|(encoded, original)| encoded == original),
            (false, true) => self
                .to_bytes()
                .ok()
                .zip(other.original.as_deref())
                .is_some_and(|(encoded, original)| encoded == original),
            (false, false) => self
                .to_bytes()
                .ok()
                .zip(other.to_bytes().ok())
                .is_some_and(|(left, right)| left == right),
        }
    }

    /// Encodes the current buffer content, accepting the losses [`LoadedText::to_bytes`] refuses.
    ///
    /// A character the encoding cannot represent becomes a numeric character
    /// reference, and a buffer loaded with replacement characters writes those
    /// replacement characters out.
    #[must_use]
    pub fn to_bytes_lossy(&self) -> Vec<u8> {
        encode_file(&self.buffer.text(), self.spec, &self.trailer).bytes
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn load_and_save_round_trips() {
        let bytes = b"\xEF\xBB\xBFone\r\ntwo\n";
        let loaded = LoadedText::load(bytes, &DecodeOptions::default());
        assert_eq!(loaded.buffer.len_lines(), 3);
        assert!(loaded.eol.mixed);
        assert_eq!(loaded.to_bytes().unwrap(), bytes);
    }

    #[test]
    fn an_unmodified_lossy_load_saves_byte_identically() {
        let bytes = b"\xFF\xFEh\x00i\x00\x21";
        let loaded = LoadedText::load(bytes, &DecodeOptions::default());
        assert!(loaded.had_errors);
        assert_eq!(loaded.to_bytes().unwrap(), bytes);
    }

    #[test]
    fn lossy_loads_compare_the_retained_bytes_instead_of_replacement_text() {
        let options = DecodeOptions {
            forced: Some(TextEncoding::Utf8),
            ..DecodeOptions::default()
        };
        let left = LoadedText::load(b"\xFF", &options);
        let different = LoadedText::load(b"\xFE", &options);
        let same = LoadedText::load(b"\xFF", &options);

        assert_eq!(left.buffer.text(), different.buffer.text());
        assert!(!left.original_bytes_equal(&different));
        assert!(left.original_bytes_equal(&same));
    }

    #[test]
    fn a_modified_lossy_load_refuses_to_save() {
        let bytes = b"\xFF\xFEh\x00i\x00\x21";
        let mut loaded = LoadedText::load(bytes, &DecodeOptions::default());
        loaded.buffer.insert(0, "x");
        assert!(matches!(loaded.to_bytes(), Err(SaveError::LossyLoad)));
        assert!(!loaded.to_bytes_lossy().is_empty());
    }

    #[test]
    fn an_unmappable_character_refuses_to_save() {
        let bytes = b"caf\xE9";
        let options = DecodeOptions {
            forced: Some(TextEncoding::Legacy(encoding_rs::WINDOWS_1252)),
            ..DecodeOptions::default()
        };
        let mut loaded = LoadedText::load(bytes, &options);
        assert_eq!(loaded.to_bytes().unwrap(), bytes);
        loaded.buffer.insert(0, "中");
        assert!(matches!(
            loaded.to_bytes(),
            Err(SaveError::Unmappable { ch: '中', .. })
        ));
        assert_eq!(loaded.to_bytes_lossy(), b"&#20013;caf\xE9");
    }

    #[test]
    fn editing_then_saving_keeps_the_encoding() {
        let spec = EncodingSpec {
            encoding: TextEncoding::Utf16Be,
            bom: true,
        };
        let bytes = encode("a\nb\n", spec).bytes;
        let mut loaded = LoadedText::load(&bytes, &DecodeOptions::default());
        loaded.buffer.insert(0, "x");
        let written = loaded.to_bytes().unwrap();
        let again = LoadedText::load(&written, &DecodeOptions::default());
        assert_eq!(again.buffer.text(), "xa\nb\n");
        assert_eq!(again.spec, spec);
    }
}
