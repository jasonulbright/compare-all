//! Encoding detection, decoding and re-encoding.
//!
//! Detection order is byte order mark, then a BOM-less UTF-16 sniff, then a
//! whole-buffer UTF-8 validity check, then statistical detection over the
//! whole buffer, then the caller supplied fallback for empty input. An explicit override short
//! circuits all of it.
//!
//! [`decode`] records everything [`encode_file`] needs to reproduce the original
//! bytes, so loading and saving a file that was not edited is byte identical.

use std::borrow::Cow;
use std::fmt;

use encoding_rs as enc;

#[cfg(test)]
std::thread_local! {
    static DETECTION_BYTES_SCANNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
use serde::de::{Deserializer, Error as _};
use serde::ser::Serializer;

/// A character encoding this crate can decode and encode.
///
/// UTF-16 and UTF-32 are handled directly because `encoding_rs` decodes UTF-16
/// but cannot encode it and does not know UTF-32 at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextEncoding {
    /// UTF-8.
    Utf8,
    /// UTF-16, little endian.
    Utf16Le,
    /// UTF-16, big endian.
    Utf16Be,
    /// UTF-32, little endian.
    Utf32Le,
    /// UTF-32, big endian.
    Utf32Be,
    /// Any single byte or legacy multi byte encoding known to `encoding_rs`.
    Legacy(&'static enc::Encoding),
}

impl TextEncoding {
    /// The stable label used in settings files and in the user interface.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "UTF-8",
            TextEncoding::Utf16Le => "UTF-16LE",
            TextEncoding::Utf16Be => "UTF-16BE",
            TextEncoding::Utf32Le => "UTF-32LE",
            TextEncoding::Utf32Be => "UTF-32BE",
            TextEncoding::Legacy(e) => e.name(),
        }
    }

    /// Looks an encoding up by label, accepting any label `encoding_rs` knows.
    ///
    /// Returns `None` when no encoding matches.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label.to_ascii_uppercase().as_str() {
            "UTF-8" | "UTF8" => return Some(TextEncoding::Utf8),
            "UTF-16LE" | "UTF-16" => return Some(TextEncoding::Utf16Le),
            "UTF-16BE" => return Some(TextEncoding::Utf16Be),
            "UTF-32LE" | "UTF-32" => return Some(TextEncoding::Utf32Le),
            "UTF-32BE" => return Some(TextEncoding::Utf32Be),
            _ => {}
        }
        enc::Encoding::for_label(label.as_bytes()).map(TextEncoding::Legacy)
    }

    /// The byte order mark for this encoding.
    #[must_use]
    pub fn bom_bytes(self) -> &'static [u8] {
        match self {
            TextEncoding::Utf8 => &[0xEF, 0xBB, 0xBF],
            TextEncoding::Utf16Le => &[0xFF, 0xFE],
            TextEncoding::Utf16Be => &[0xFE, 0xFF],
            TextEncoding::Utf32Le => &[0xFF, 0xFE, 0x00, 0x00],
            TextEncoding::Utf32Be => &[0x00, 0x00, 0xFE, 0xFF],
            TextEncoding::Legacy(_) => &[],
        }
    }

    /// True when the encoding is a byte stream in which a lone `0x1A` byte can
    /// act as an end of file marker.
    #[must_use]
    pub fn is_byte_oriented(self) -> bool {
        matches!(self, TextEncoding::Utf8 | TextEncoding::Legacy(_))
    }
}

impl fmt::Display for TextEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl serde::Serialize for TextEncoding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl<'de> serde::Deserialize<'de> for TextEncoding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let label = <String as serde::Deserialize>::deserialize(deserializer)?;
        TextEncoding::from_label(&label)
            .ok_or_else(|| D::Error::custom(format!("unknown encoding label: {label}")))
    }
}

/// An encoding together with whether the file carries a byte order mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct EncodingSpec {
    /// The encoding itself.
    pub encoding: TextEncoding,
    /// Whether a byte order mark precedes the content.
    pub bom: bool,
}

impl EncodingSpec {
    /// A spec with no byte order mark.
    #[must_use]
    pub fn bare(encoding: TextEncoding) -> Self {
        Self {
            encoding,
            bom: false,
        }
    }
}

impl Default for EncodingSpec {
    fn default() -> Self {
        Self::bare(TextEncoding::Utf8)
    }
}

/// Which detection step chose the encoding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetectionSource {
    /// The caller pinned the encoding.
    Override,
    /// A byte order mark named it.
    Bom,
    /// The whole buffer is valid UTF-8.
    Utf8Valid,
    /// Statistical detection chose it.
    Statistical,
    /// Nothing else applied, so the caller's fallback was used.
    Fallback,
}

/// The outcome of [`detect`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Detection {
    /// The chosen encoding and byte order mark state.
    pub spec: EncodingSpec,
    /// Which step chose it.
    pub source: DetectionSource,
}

/// How a buffer of bytes should be turned into text.
#[derive(Clone, Copy, Debug)]
pub struct DecodeOptions {
    /// Pins the encoding, skipping detection entirely.
    pub forced: Option<TextEncoding>,
    /// Used when detection reaches no confident conclusion.
    pub fallback: TextEncoding,
    /// Honors a `0x1A` byte as an end of file marker in byte oriented encodings.
    pub ctrl_z_ends_file: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            forced: None,
            fallback: TextEncoding::Utf8,
            ctrl_z_ends_file: false,
        }
    }
}

/// Decoded text plus everything needed to rebuild the original bytes.
#[derive(Clone, Debug)]
pub struct Decoded {
    /// The decoded text, with any byte order mark removed.
    pub text: String,
    /// The encoding and byte order mark state the text came from.
    pub spec: EncodingSpec,
    /// Which detection step chose the encoding.
    pub source: DetectionSource,
    /// True when the input held sequences the encoding cannot represent and
    /// replacement characters were substituted. Such input does not round trip.
    pub had_errors: bool,
    /// Bytes from an honored end of file marker onward, preserved verbatim.
    pub trailer: Vec<u8>,
}

/// How many leading bytes the BOM-less UTF-16 sniff examines.
const UTF16_SNIFF_WINDOW: usize = 4096;

/// Fraction of code unit slots that must be NUL on one side for the UTF-16
/// sniff, as a numerator over a denominator.
const UTF16_NUL_RATIO: (usize, usize) = (3, 10);

/// Returns the encoding a byte order mark names, with its length in bytes.
fn bom_of(bytes: &[u8]) -> Option<(TextEncoding, usize)> {
    // The UTF-32LE mark starts with the UTF-16LE mark, so it must be tested first.
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Some((TextEncoding::Utf32Le, 4));
    }
    if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Some((TextEncoding::Utf32Be, 4));
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some((TextEncoding::Utf8, 3));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some((TextEncoding::Utf16Le, 2));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Some((TextEncoding::Utf16Be, 2));
    }
    None
}

/// Guesses UTF-16 for bytes that carry no byte order mark.
///
/// UTF-16 text of ASCII content is a run of NUL bytes on one side of every code
/// unit, and a NUL byte is valid UTF-8, so the UTF-8 validity check accepts such
/// a file and every later heuristic sees only NUL bytes. The sniff must
/// therefore run before that check, and the binary heuristic must agree with it
/// or the same file is routed to a hex comparison instead.
///
/// Returns `None` unless one side of the leading window is heavily NUL, the
/// other side carries none, and the window decodes as UTF-16 without errors.
#[must_use]
pub fn sniff_utf16(bytes: &[u8]) -> Option<TextEncoding> {
    if bom_of(bytes).is_some() || bytes.len() < 4 {
        return None;
    }
    let limit = UTF16_SNIFF_WINDOW.min(bytes.len()) & !1;
    let window = &bytes[..limit];
    let units = limit / 2;
    if units == 0 {
        return None;
    }
    let mut even_nuls = 0usize;
    let mut odd_nuls = 0usize;
    for (offset, byte) in window.iter().enumerate() {
        if *byte == 0 {
            if offset.is_multiple_of(2) {
                even_nuls += 1;
            } else {
                odd_nuls += 1;
            }
        }
    }
    let threshold = (units * UTF16_NUL_RATIO.0)
        .div_ceil(UTF16_NUL_RATIO.1)
        .max(1);
    let big_endian = match (even_nuls, odd_nuls) {
        (0, odd) if odd >= threshold => false,
        (even, 0) if even >= threshold => true,
        _ => return None,
    };
    let (_, had_errors) = decode_utf16(window, big_endian);
    if had_errors {
        return None;
    }
    Some(if big_endian {
        TextEncoding::Utf16Be
    } else {
        TextEncoding::Utf16Le
    })
}

/// Chooses an encoding for `bytes`.
///
/// Statistical detection reads the whole file. A windows-1252 guess is
/// retained: it represents common Western text whose bytes are invalid UTF-8,
/// while replacing those bytes would make different files compare equal.
#[must_use]
pub fn detect(bytes: &[u8], options: &DecodeOptions) -> Detection {
    if let Some(forced) = options.forced {
        let bom = bom_of(bytes).is_some_and(|(e, _)| e == forced);
        return Detection {
            spec: EncodingSpec {
                encoding: forced,
                bom,
            },
            source: DetectionSource::Override,
        };
    }
    if let Some((encoding, _)) = bom_of(bytes) {
        return Detection {
            spec: EncodingSpec {
                encoding,
                bom: true,
            },
            source: DetectionSource::Bom,
        };
    }
    if bytes.is_empty() {
        return Detection {
            spec: EncodingSpec::bare(options.fallback),
            source: DetectionSource::Fallback,
        };
    }
    if let Some(encoding) = sniff_utf16(bytes) {
        return Detection {
            spec: EncodingSpec::bare(encoding),
            source: DetectionSource::Statistical,
        };
    }
    if std::str::from_utf8(bytes).is_ok() {
        return Detection {
            spec: EncodingSpec::bare(TextEncoding::Utf8),
            source: DetectionSource::Utf8Valid,
        };
    }
    let mut detector = chardetng::EncodingDetector::new();
    #[cfg(test)]
    DETECTION_BYTES_SCANNED.with(|count| count.set(count.get().saturating_add(bytes.len())));
    detector.feed(bytes, true);
    // UTF-8 is excluded because the validity check above already ruled it out.
    let guess = detector.guess(None, false);
    Detection {
        spec: EncodingSpec::bare(TextEncoding::Legacy(guess)),
        source: DetectionSource::Statistical,
    }
}

/// Decodes `bytes` into text, detecting the encoding per [`detect`].
#[must_use]
pub fn decode(bytes: &[u8], options: &DecodeOptions) -> Decoded {
    let detection = detect(bytes, options);
    let spec = detection.spec;
    let body = if spec.bom {
        let skip = bom_of(bytes).map_or(0, |(_, len)| len);
        &bytes[skip..]
    } else {
        bytes
    };
    let (body, trailer) = if options.ctrl_z_ends_file && spec.encoding.is_byte_oriented() {
        match end_of_file_marker(body) {
            Some(at) => (&body[..at], body[at..].to_vec()),
            None => (body, Vec::new()),
        }
    } else {
        (body, Vec::new())
    };
    let (text, had_errors) = decode_body(body, spec.encoding);
    Decoded {
        text,
        spec,
        source: detection.source,
        had_errors,
        trailer,
    }
}

/// The offset of an honored `0x1A` end of file marker.
///
/// A `0x1A` only ends the file when nothing but block padding follows it, so a
/// `0x1A` occurring as a character in the middle of a file does not hide the
/// rest of the content from the decoded text.
fn end_of_file_marker(body: &[u8]) -> Option<usize> {
    let mut start = body.len();
    while start > 0 && matches!(body[start - 1], 0x1A | 0x00 | b'\r' | b'\n') {
        start -= 1;
    }
    body[start..]
        .iter()
        .position(|b| *b == 0x1A)
        .map(|offset| start + offset)
}

fn decode_body(body: &[u8], encoding: TextEncoding) -> (String, bool) {
    match encoding {
        TextEncoding::Utf8 => match String::from_utf8_lossy(body) {
            Cow::Borrowed(s) => (s.to_owned(), false),
            Cow::Owned(s) => (s, true),
        },
        TextEncoding::Legacy(e) => {
            let (text, had_errors) = e.decode_without_bom_handling(body);
            (text.into_owned(), had_errors)
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            decode_utf16(body, encoding == TextEncoding::Utf16Be)
        }
        TextEncoding::Utf32Le | TextEncoding::Utf32Be => {
            decode_utf32(body, encoding == TextEncoding::Utf32Be)
        }
    }
}

fn decode_utf16(body: &[u8], big_endian: bool) -> (String, bool) {
    let mut units = Vec::with_capacity(body.len() / 2);
    for pair in body.chunks_exact(2) {
        let (a, b) = (pair[0], pair[1]);
        units.push(if big_endian {
            u16::from_be_bytes([a, b])
        } else {
            u16::from_le_bytes([a, b])
        });
    }
    let mut had_errors = !body.len().is_multiple_of(2);
    let mut text = String::with_capacity(units.len());
    for unit in char::decode_utf16(units.iter().copied()) {
        if let Ok(c) = unit {
            text.push(c);
        } else {
            had_errors = true;
            text.push(char::REPLACEMENT_CHARACTER);
        }
    }
    if !body.len().is_multiple_of(2) {
        text.push(char::REPLACEMENT_CHARACTER);
    }
    (text, had_errors)
}

fn decode_utf32(body: &[u8], big_endian: bool) -> (String, bool) {
    let mut had_errors = !body.len().is_multiple_of(4);
    let mut text = String::with_capacity(body.len() / 4);
    for quad in body.chunks_exact(4) {
        let bytes = [quad[0], quad[1], quad[2], quad[3]];
        let value = if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        };
        if let Some(c) = char::from_u32(value) {
            text.push(c);
        } else {
            had_errors = true;
            text.push(char::REPLACEMENT_CHARACTER);
        }
    }
    if !body.len().is_multiple_of(4) {
        text.push(char::REPLACEMENT_CHARACTER);
    }
    (text, had_errors)
}

/// A character the target encoding cannot represent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unmappable {
    /// The first character the encoding cannot represent.
    pub ch: char,
    /// Its index in characters from the start of the text.
    pub char_index: usize,
}

/// The bytes an encode produced, plus whether anything could not be represented.
///
/// A legacy encoder substitutes an HTML numeric character reference for a
/// character it cannot represent, which is silent corruption of the saved file.
/// Callers that reject `unmappable` never write such bytes.
#[derive(Clone, Debug)]
#[must_use]
pub struct EncodeOutcome {
    /// The encoded bytes, with substitutions in place of any unmappable character.
    pub bytes: Vec<u8>,
    /// The first character the encoding could not represent, if any.
    pub unmappable: Option<Unmappable>,
}

impl EncodeOutcome {
    /// The bytes, accepting any substitution the encoder made.
    #[must_use]
    pub fn into_lossy_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// The first character of `text` that `encoding` cannot represent.
fn first_unmappable(text: &str, encoding: &'static enc::Encoding) -> Option<Unmappable> {
    let mut scratch = [0u8; 4];
    text.chars().enumerate().find_map(|(char_index, ch)| {
        let one = ch.encode_utf8(&mut scratch);
        encoding
            .encode(one)
            .2
            .then_some(Unmappable { ch, char_index })
    })
}

/// Encodes `text` in `spec`, emitting the byte order mark when the spec carries one.
pub fn encode(text: &str, spec: EncodingSpec) -> EncodeOutcome {
    let mut unmappable = None;
    let mut out = Vec::with_capacity(text.len() + 4);
    if spec.bom {
        out.extend_from_slice(spec.encoding.bom_bytes());
    }
    match spec.encoding {
        TextEncoding::Utf8 => out.extend_from_slice(text.as_bytes()),
        TextEncoding::Legacy(e) => {
            let (bytes, _, had_unmappable) = e.encode(text);
            out.extend_from_slice(&bytes);
            if had_unmappable {
                unmappable = first_unmappable(text, e);
            }
        }
        TextEncoding::Utf16Le => {
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
        TextEncoding::Utf16Be => {
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }
        TextEncoding::Utf32Le => {
            for c in text.chars() {
                out.extend_from_slice(&(c as u32).to_le_bytes());
            }
        }
        TextEncoding::Utf32Be => {
            for c in text.chars() {
                out.extend_from_slice(&(c as u32).to_be_bytes());
            }
        }
    }
    EncodeOutcome {
        bytes: out,
        unmappable,
    }
}

/// Encodes `text` and re-appends the preserved end of file trailer.
///
/// Passing the text and trailer from a [`Decoded`] back unchanged reproduces the
/// original bytes exactly, provided `had_errors` was false and the outcome
/// reports no unmappable character.
pub fn encode_file(text: &str, spec: EncodingSpec, trailer: &[u8]) -> EncodeOutcome {
    let mut outcome = encode(text, spec);
    outcome.bytes.extend_from_slice(trailer);
    outcome
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bom_wins_over_content() {
        let bytes = b"\xEF\xBB\xBFhello";
        let d = decode(bytes, &DecodeOptions::default());
        assert_eq!(d.text, "hello");
        assert_eq!(d.source, DetectionSource::Bom);
        assert!(d.spec.bom);
        assert_eq!(encode_file(&d.text, d.spec, &d.trailer).bytes, bytes);
    }

    #[test]
    fn utf32_bom_is_not_read_as_utf16() {
        let bytes = b"\xFF\xFE\x00\x00A\x00\x00\x00";
        let d = decode(bytes, &DecodeOptions::default());
        assert_eq!(d.spec.encoding, TextEncoding::Utf32Le);
        assert_eq!(d.text, "A");
    }

    #[test]
    fn plain_ascii_is_utf8_without_bom() {
        let d = decode(b"plain", &DecodeOptions::default());
        assert_eq!(d.source, DetectionSource::Utf8Valid);
        assert!(!d.spec.bom);
    }

    #[test]
    fn empty_input_uses_fallback() {
        let options = DecodeOptions {
            fallback: TextEncoding::Legacy(enc::WINDOWS_1251),
            ..DecodeOptions::default()
        };
        let d = decode(b"", &options);
        assert_eq!(d.source, DetectionSource::Fallback);
        assert_eq!(d.spec.encoding, TextEncoding::Legacy(enc::WINDOWS_1251));
    }

    #[test]
    fn override_skips_detection() {
        let options = DecodeOptions {
            forced: Some(TextEncoding::Legacy(enc::WINDOWS_1252)),
            ..DecodeOptions::default()
        };
        let d = decode(b"caf\xE9", &options);
        assert_eq!(d.text, "café");
        assert_eq!(d.source, DetectionSource::Override);
        assert_eq!(encode_file(&d.text, d.spec, &d.trailer).bytes, b"caf\xE9");
    }

    #[test]
    fn statistical_windows_1252_keeps_distinct_western_bytes_distinct() {
        let left = decode(b"caf\xE9\n", &DecodeOptions::default());
        let right = decode(b"caf\xE8\n", &DecodeOptions::default());

        assert_eq!(left.spec.encoding, TextEncoding::Legacy(enc::WINDOWS_1252));
        assert_eq!(left.source, DetectionSource::Statistical);
        assert_eq!(left.text, "café\n");
        assert_eq!(right.text, "cafè\n");
        assert!(!left.had_errors);
        assert!(!right.had_errors);
    }

    #[test]
    fn statistical_detection_includes_bytes_between_the_old_windows() {
        let window = 1 << 20;
        let mut bytes = vec![0; 3 * window];
        let utf8_text = "café résumé — déjà vu\n".as_bytes();
        for chunk in bytes[..window].chunks_mut(utf8_text.len()) {
            let count = chunk.len();
            chunk.copy_from_slice(&utf8_text[..count]);
        }
        for chunk in bytes[2 * window..].chunks_mut(utf8_text.len()) {
            let count = chunk.len();
            chunk.copy_from_slice(&utf8_text[..count]);
        }
        let western_text = b"cafe\xE9 d\xE9j\xE0 vu ma\xF1ana ni\xF1o\n";
        for chunk in bytes[window..2 * window].chunks_mut(western_text.len()) {
            let count = chunk.len();
            chunk.copy_from_slice(&western_text[..count]);
        }

        DETECTION_BYTES_SCANNED.with(|count| count.set(0));

        let detection = detect(&bytes, &DecodeOptions::default());

        assert_eq!(
            detection.spec.encoding,
            TextEncoding::Legacy(enc::WINDOWS_1252)
        );
        assert_eq!(detection.source, DetectionSource::Statistical);
        DETECTION_BYTES_SCANNED.with(|count| assert_eq!(count.get(), bytes.len()));
    }

    #[test]
    fn an_invalid_legacy_sequence_does_not_alias_another_invalid_sequence() {
        let left = decode(b"caf\x81\n", &DecodeOptions::default());
        let right = decode(b"caf\x8D\n", &DecodeOptions::default());

        assert_ne!(left.text, right.text);
    }

    #[test]
    fn utf16_le_round_trips() {
        let spec = EncodingSpec {
            encoding: TextEncoding::Utf16Le,
            bom: true,
        };
        let bytes = encode("héllo 🎉", spec).bytes;
        let options = DecodeOptions::default();
        let d = decode(&bytes, &options);
        assert_eq!(d.text, "héllo 🎉");
        assert!(!d.had_errors);
        assert_eq!(encode_file(&d.text, d.spec, &d.trailer).bytes, bytes);
    }

    #[test]
    fn unpaired_surrogate_reports_errors() {
        let bytes = [0x00, 0xD8, 0x41, 0x00];
        let options = DecodeOptions {
            forced: Some(TextEncoding::Utf16Le),
            ..DecodeOptions::default()
        };
        let d = decode(&bytes, &options);
        assert!(d.had_errors);
    }

    #[test]
    fn ctrl_z_truncates_and_is_preserved() {
        let options = DecodeOptions {
            ctrl_z_ends_file: true,
            ..DecodeOptions::default()
        };
        let d = decode(b"visible\r\n\x1A\x00\x00", &options);
        assert_eq!(d.text, "visible\r\n");
        assert_eq!(d.trailer, b"\x1A\x00\x00");
        assert_eq!(
            encode_file(&d.text, d.spec, &d.trailer).bytes,
            b"visible\r\n\x1A\x00\x00"
        );
    }

    #[test]
    fn ctrl_z_ignored_when_option_is_off() {
        let d = decode(b"visible\x1Ahidden", &DecodeOptions::default());
        assert_eq!(d.text, "visible\u{1A}hidden");
    }

    #[test]
    fn mid_file_ctrl_z_stays_in_the_text() {
        let options = DecodeOptions {
            ctrl_z_ends_file: true,
            ..DecodeOptions::default()
        };
        let d = decode(b"visible\x1Ahidden", &options);
        assert_eq!(d.text, "visible\u{1A}hidden");
        assert!(d.trailer.is_empty());
    }

    #[test]
    fn legacy_encode_reports_an_unmappable_character() {
        let spec = EncodingSpec::bare(TextEncoding::Legacy(enc::WINDOWS_1252));
        let outcome = encode("ab中c", spec);
        let reported = outcome.unmappable.unwrap();
        assert_eq!(reported.ch, '中');
        assert_eq!(reported.char_index, 2);
        assert!(encode("abc", spec).unmappable.is_none());
    }

    #[test]
    fn utf16_without_a_bom_is_detected() {
        let bytes = b"h\x00i\x00!\x00\n\x00";
        let d = decode(bytes, &DecodeOptions::default());
        assert_eq!(d.spec.encoding, TextEncoding::Utf16Le);
        assert_eq!(d.text, "hi!\n");
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn statistical_detection_scans_a_large_file_within_budget() {
        let mut bytes = vec![b'a'; 16 * 1024 * 1024];
        bytes[0] = 0xE9;
        let started = std::time::Instant::now();
        let d = detect(&bytes, &DecodeOptions::default());
        assert_eq!(d.source, DetectionSource::Statistical);
        assert_eq!(d.spec.encoding, TextEncoding::Legacy(enc::WINDOWS_1252));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// Correctness half of the bounded detection case, on a buffer a debug
    /// build reads quickly: the Windows-1252 guess decodes Western text.
    #[test]
    fn statistical_detection_keeps_windows_1252_on_a_smaller_file() {
        let mut bytes = vec![b'a'; 256 * 1024];
        bytes[0] = 0xE9;
        let d = detect(&bytes, &DecodeOptions::default());
        assert_eq!(d.source, DetectionSource::Statistical);
        assert_eq!(d.spec.encoding, TextEncoding::Legacy(enc::WINDOWS_1252));
    }

    #[test]
    fn labels_round_trip() {
        for e in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
            TextEncoding::Utf32Le,
            TextEncoding::Utf32Be,
            TextEncoding::Legacy(enc::WINDOWS_1252),
        ] {
            assert_eq!(TextEncoding::from_label(e.label()), Some(e));
        }
    }
}
