//! Heuristic that flags a file as binary so callers can route it to hex compare.

/// Tuning for [`inspect`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BinaryHeuristic {
    /// How many leading bytes to examine. Zero examines everything.
    pub sample_bytes: usize,
    /// Fraction of control characters at or above which the file counts as binary.
    pub control_ratio: f32,
    /// Whether a single NUL byte alone is enough to call the file binary.
    pub nul_is_binary: bool,
}

impl Default for BinaryHeuristic {
    fn default() -> Self {
        Self {
            sample_bytes: 8192,
            control_ratio: 0.3,
            nul_is_binary: true,
        }
    }
}

/// What [`inspect`] found.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BinaryVerdict {
    /// The heuristic's conclusion.
    pub is_binary: bool,
    /// Whether a NUL byte appeared in the sample.
    pub has_nul: bool,
    /// Fraction of sampled bytes that were control characters.
    pub control_ratio: f32,
    /// Number of bytes examined.
    pub sampled: usize,
}

/// True when a byte is a control character that text files do not normally carry.
///
/// Tab, line feed, carriage return, form feed and escape are excluded because
/// plain text and terminal captures use them.
fn is_odd_control(byte: u8) -> bool {
    match byte {
        b'\t' | b'\n' | b'\r' | 0x0C | 0x1B => false,
        0x00..=0x1F | 0x7F => true,
        _ => false,
    }
}

/// True when the bytes begin with a Unicode byte order mark.
fn has_unicode_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xEF, 0xBB, 0xBF])
        || bytes.starts_with(&[0xFF, 0xFE])
        || bytes.starts_with(&[0xFE, 0xFF])
        || bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
}

/// Examines `bytes` and reports whether they look like a binary file.
///
/// A Unicode byte order mark settles the question: UTF-16 and UTF-32 text is
/// full of NUL bytes and would otherwise be misread as binary.
#[must_use]
pub fn inspect(bytes: &[u8], heuristic: &BinaryHeuristic) -> BinaryVerdict {
    let limit = if heuristic.sample_bytes == 0 {
        bytes.len()
    } else {
        heuristic.sample_bytes.min(bytes.len())
    };
    let sample = &bytes[..limit];
    let has_nul = sample.contains(&0);
    let controls = sample.iter().filter(|b| is_odd_control(**b)).count();
    #[allow(clippy::cast_precision_loss)]
    let ratio = if sample.is_empty() {
        0.0
    } else {
        controls as f32 / sample.len() as f32
    };
    // UTF-16 detected without a byte order mark must reach the same verdict as
    // UTF-16 carrying one, or the same file loads as text on one path and as
    // binary on the other.
    let is_binary = if sample.is_empty()
        || has_unicode_bom(bytes)
        || crate::encoding::sniff_utf16(bytes).is_some()
    {
        false
    } else {
        (heuristic.nul_is_binary && has_nul) || ratio >= heuristic.control_ratio
    };
    BinaryVerdict {
        is_binary,
        has_nul,
        control_ratio: ratio,
        sampled: sample.len(),
    }
}

/// Convenience wrapper around [`inspect`] with the default heuristic.
#[must_use]
pub fn looks_binary(bytes: &[u8]) -> bool {
    inspect(bytes, &BinaryHeuristic::default()).is_binary
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_not_binary() {
        assert!(!looks_binary(b"hello\r\nworld\n\ttabbed\n"));
    }

    #[test]
    fn empty_input_is_not_binary() {
        assert!(!looks_binary(b""));
    }

    #[test]
    fn a_nul_byte_flags_binary() {
        assert!(looks_binary(b"MZ\x00\x00\x90"));
    }

    #[test]
    fn utf16_with_a_bom_is_text() {
        assert!(!looks_binary(b"\xFF\xFEh\x00i\x00"));
    }

    #[test]
    fn utf16_without_a_bom_is_text() {
        assert!(!looks_binary(b"h\x00i\x00!\x00\n\x00"));
    }

    #[test]
    fn control_heavy_content_flags_binary() {
        let bytes: Vec<u8> = (0..100u8)
            .map(|i| if i % 2 == 0 { 0x01 } else { b'a' })
            .collect();
        assert!(looks_binary(&bytes));
    }

    #[test]
    fn escape_sequences_stay_text() {
        assert!(!looks_binary(b"\x1B[31mred\x1B[0m\n"));
    }

    #[test]
    fn sample_limit_is_honored() {
        let mut bytes = vec![b'a'; 100];
        bytes.push(0);
        let heuristic = BinaryHeuristic {
            sample_bytes: 50,
            ..BinaryHeuristic::default()
        };
        let verdict = inspect(&bytes, &heuristic);
        assert_eq!(verdict.sampled, 50);
        assert!(!verdict.is_binary);
    }
}
