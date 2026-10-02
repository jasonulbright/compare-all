//! Column fitting for the plain text layouts.

use crate::escape::text_single_line;
use crate::options::Wrap;

/// Characters one side of a plain text side-by-side report occupies.
pub(crate) const COLUMN_WIDTH: usize = 56;

/// Fit one value into a column, returning the lines it occupies.
///
/// With no wrapping the value is clipped, so one long line never pushes the
/// second column off the page. Word wrapping breaks between words and falls
/// back to a hard break for a word wider than the column.
pub(crate) fn fit(value: &str, width: usize, wrap: &Wrap) -> Vec<String> {
    let value = text_single_line(value);
    let width = width.max(1);
    match wrap {
        Wrap::Word => wrap_words(&value, width),
        Wrap::Character => hard_wrap(&value, width),
        _ => vec![clip(&value, width)],
    }
}

fn clip(value: &str, width: usize) -> String {
    value.chars().take(width).collect()
}

fn hard_wrap(value: &str, width: usize) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut count = 0usize;
    for ch in value.chars() {
        line.push(ch);
        count += 1;
        if count == width {
            lines.push(std::mem::take(&mut line));
            count = 0;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn wrap_words(value: &str, width: usize) -> Vec<String> {
    if value.trim().is_empty() {
        return vec![value.chars().take(width).collect()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut count = 0usize;
    for word in value.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > width {
            if count > 0 {
                lines.push(std::mem::take(&mut line));
                count = 0;
            }
            for piece in hard_wrap(word, width) {
                lines.push(piece);
            }
            continue;
        }
        let needed = if count == 0 { word_len } else { word_len + 1 };
        if count + needed > width {
            lines.push(std::mem::take(&mut line));
            count = 0;
        }
        if count > 0 {
            line.push(' ');
            count += 1;
        }
        line.push_str(word);
        count += word_len;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Pad a value on the right to an exact character count.
pub(crate) fn pad(value: &str, width: usize) -> String {
    let count = value.chars().count();
    let mut padded = String::from(value);
    for _ in count..width {
        padded.push(' ');
    }
    padded
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{fit, pad};
    use crate::options::Wrap;

    #[test]
    fn an_unwrapped_value_is_clipped() {
        assert_eq!(fit("abcdefgh", 4, &Wrap::None), vec!["abcd"]);
    }

    #[test]
    fn character_wrapping_splits_anywhere() {
        assert_eq!(
            fit("abcdefgh", 3, &Wrap::Character),
            vec!["abc", "def", "gh"]
        );
    }

    #[test]
    fn word_wrapping_splits_between_words() {
        assert_eq!(
            fit("one two three", 8, &Wrap::Word),
            vec!["one two", "three"]
        );
    }

    #[test]
    fn a_word_wider_than_the_column_breaks_hard() {
        assert_eq!(fit("abcdefgh", 3, &Wrap::Word), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn a_line_break_inside_a_value_becomes_a_space() {
        assert_eq!(fit("a\nb", 8, &Wrap::None), vec!["a b"]);
    }

    #[test]
    fn an_empty_value_still_occupies_one_line() {
        assert_eq!(fit("", 4, &Wrap::Word), vec![""]);
        assert_eq!(fit("", 4, &Wrap::Character), vec![""]);
    }

    #[test]
    fn padding_reaches_the_requested_width() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("abcd", 2), "abcd");
    }
}
