//! Text helpers for the value column and the details area.

use std::fmt::Write as _;
use std::ops::Range;

/// Bytes the hex details area shows for one side at most.
pub const HEX_DETAIL_LIMIT: usize = 256;

/// Bytes one hex details line carries.
const HEX_LINE: usize = 16;

/// The part of each value that differs from the other.
///
/// The two values share a leading run and a trailing run of characters; what
/// lies between them is the difference. Both ranges are byte ranges on
/// character boundaries.
#[must_use]
pub fn inline_difference(left: &str, right: &str) -> (Range<usize>, Range<usize>) {
    let prefix: usize = left
        .chars()
        .zip(right.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let left_rest = left.get(prefix..).unwrap_or_default();
    let right_rest = right.get(prefix..).unwrap_or_default();
    let suffix: usize = left_rest
        .chars()
        .rev()
        .zip(right_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    (
        prefix..left.len().saturating_sub(suffix).max(prefix),
        prefix..right.len().saturating_sub(suffix).max(prefix),
    )
}

/// Lines of an offset, hex and character dump of `bytes`, up to
/// [`HEX_DETAIL_LIMIT`] bytes.
#[must_use]
pub fn hex_lines(bytes: &[u8]) -> Vec<String> {
    let shown = &bytes[..bytes.len().min(HEX_DETAIL_LIMIT)];
    let mut lines = Vec::with_capacity(shown.len().div_ceil(HEX_LINE) + 1);
    for (index, chunk) in shown.chunks(HEX_LINE).enumerate() {
        let mut line = format!("{:08X}  ", index * HEX_LINE);
        for column in 0..HEX_LINE {
            match chunk.get(column) {
                Some(byte) => {
                    let _ = write!(line, "{byte:02X} ");
                }
                None => line.push_str("   "),
            }
        }
        line.push(' ');
        line.extend(chunk.iter().map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            }
        }));
        lines.push(line);
    }
    if bytes.len() > shown.len() {
        lines.push(format!(
            "First {} of {} bytes shown",
            shown.len(),
            bytes.len()
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{hex_lines, inline_difference, HEX_DETAIL_LIMIT};

    #[test]
    fn the_shared_start_and_end_are_left_out() {
        let (left, right) = inline_difference("version 1.2.3", "version 1.4.3");
        assert_eq!(&"version 1.2.3"[left], "2");
        assert_eq!(&"version 1.4.3"[right], "4");
    }

    #[test]
    fn an_insertion_is_empty_on_one_side() {
        let (left, right) = inline_difference("abc", "abXc");
        assert!(left.is_empty());
        assert_eq!(&"abXc"[right], "X");
    }

    #[test]
    fn a_repeated_character_does_not_overlap_the_two_runs() {
        let (left, right) = inline_difference("aa", "aaa");
        assert!(left.start <= left.end);
        assert_eq!(right.len(), 1);
    }

    #[test]
    fn multi_byte_characters_stay_whole() {
        let (left, right) = inline_difference("café", "cafè");
        assert_eq!(&"café"[left], "é");
        assert_eq!(&"cafè"[right], "è");
    }

    #[test]
    fn a_long_payload_is_cut_and_says_so() {
        let bytes = vec![0x41u8; HEX_DETAIL_LIMIT + 10];
        let lines = hex_lines(&bytes);
        assert_eq!(lines.len(), HEX_DETAIL_LIMIT / 16 + 1);
        assert!(lines[0].starts_with("00000000  41 41"));
        assert!(lines[0].ends_with("AAAAAAAAAAAAAAAA"));
        assert!(lines
            .last()
            .is_some_and(|line| line.contains("of 266 bytes")));
    }
}
