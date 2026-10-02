//! Line ending detection, per-line reporting and whole-text conversion.
//!
//! Text is stored with its original line endings, so leaving a file alone
//! preserves a mixed file exactly. Conversion is explicit through [`convert`].

/// A line ending style a file can be converted to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum EolStyle {
    /// A single line feed, `\n`.
    Lf,
    /// A carriage return followed by a line feed, `\r\n`.
    CrLf,
    /// A single carriage return, `\r`.
    Cr,
}

impl EolStyle {
    /// The characters this style writes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EolStyle::Lf => "\n",
            EolStyle::CrLf => "\r\n",
            EolStyle::Cr => "\r",
        }
    }

    /// The style native to the host platform.
    #[must_use]
    pub fn platform() -> Self {
        if cfg!(windows) {
            EolStyle::CrLf
        } else {
            EolStyle::Lf
        }
    }
}

/// The ending that actually terminates one line of a text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineEnding {
    /// A single line feed.
    Lf,
    /// A carriage return followed by a line feed.
    CrLf,
    /// A single carriage return.
    Cr,
    /// No terminator, which only the final line of a text can have.
    None,
}

impl LineEnding {
    /// The characters this ending occupies, empty for [`LineEnding::None`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
            LineEnding::None => "",
        }
    }

    /// The convertible style, or `None` for an unterminated final line.
    #[must_use]
    pub fn style(self) -> Option<EolStyle> {
        match self {
            LineEnding::Lf => Some(EolStyle::Lf),
            LineEnding::CrLf => Some(EolStyle::CrLf),
            LineEnding::Cr => Some(EolStyle::Cr),
            LineEnding::None => None,
        }
    }
}

/// Counts of each line ending style in a text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EolSummary {
    /// Number of lines ending in a lone line feed.
    pub lf: u64,
    /// Number of lines ending in a carriage return and line feed.
    pub crlf: u64,
    /// Number of lines ending in a lone carriage return.
    pub cr: u64,
    /// The style to present as the file's own; ties go to the first style seen.
    pub dominant: EolStyle,
    /// True when more than one style occurs.
    pub mixed: bool,
    /// True when the last line carries a terminator.
    pub final_newline: bool,
}

impl EolSummary {
    /// Total number of terminated lines.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.lf + self.crlf + self.cr
    }
}

/// Walks a text yielding each line's content and its terminator.
pub struct Lines<'a> {
    rest: &'a str,
    done: bool,
}

impl<'a> Iterator for Lines<'a> {
    type Item = (&'a str, LineEnding);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.rest.find(['\r', '\n']) {
            None => {
                self.done = true;
                if self.rest.is_empty() {
                    None
                } else {
                    let line = self.rest;
                    self.rest = "";
                    Some((line, LineEnding::None))
                }
            }
            Some(at) => {
                let line = &self.rest[..at];
                let tail = &self.rest[at..];
                let (ending, width) = if tail.starts_with("\r\n") {
                    (LineEnding::CrLf, 2)
                } else if tail.starts_with('\r') {
                    (LineEnding::Cr, 1)
                } else {
                    (LineEnding::Lf, 1)
                };
                self.rest = &tail[width..];
                Some((line, ending))
            }
        }
    }
}

/// Iterates the lines of `text` with their terminators.
///
/// A text ending in a terminator does not yield a trailing empty line.
#[must_use]
pub fn lines(text: &str) -> Lines<'_> {
    Lines {
        rest: text,
        done: false,
    }
}

/// The terminator of every line of `text`, in order.
#[must_use]
pub fn line_endings(text: &str) -> Vec<LineEnding> {
    lines(text).map(|(_, e)| e).collect()
}

/// Counts the line ending styles used in `text`.
#[must_use]
pub fn scan(text: &str) -> EolSummary {
    let mut summary = EolSummary {
        lf: 0,
        crlf: 0,
        cr: 0,
        dominant: EolStyle::platform(),
        mixed: false,
        final_newline: false,
    };
    let mut first_seen: Option<EolStyle> = None;
    let mut last = LineEnding::None;
    for (_, ending) in lines(text) {
        last = ending;
        let style = match ending {
            LineEnding::Lf => {
                summary.lf += 1;
                EolStyle::Lf
            }
            LineEnding::CrLf => {
                summary.crlf += 1;
                EolStyle::CrLf
            }
            LineEnding::Cr => {
                summary.cr += 1;
                EolStyle::Cr
            }
            LineEnding::None => continue,
        };
        if first_seen.is_none() {
            first_seen = Some(style);
        }
    }
    summary.final_newline = last != LineEnding::None;
    summary.mixed = [summary.lf, summary.crlf, summary.cr]
        .iter()
        .filter(|n| **n > 0)
        .count()
        > 1;
    if let Some(first) = first_seen {
        summary.dominant = dominant_of(&summary, first);
    }
    summary
}

/// Picks the highest count, breaking ties in favor of the first style seen.
fn dominant_of(summary: &EolSummary, first: EolStyle) -> EolStyle {
    let count = |style: EolStyle| match style {
        EolStyle::Lf => summary.lf,
        EolStyle::CrLf => summary.crlf,
        EolStyle::Cr => summary.cr,
    };
    let mut best = first;
    for style in [EolStyle::Lf, EolStyle::CrLf, EolStyle::Cr] {
        if count(style) > count(best) {
            best = style;
        }
    }
    best
}

/// Rewrites every line ending in `text` to `style`.
///
/// An unterminated final line stays unterminated, so the operation is idempotent.
#[must_use]
pub fn convert(text: &str, style: EolStyle) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 32);
    for (line, ending) in lines(text) {
        out.push_str(line);
        if ending != LineEnding::None {
            out.push_str(style.as_str());
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_ending() {
        assert_eq!(
            line_endings("a\nb\r\nc\rd"),
            [
                LineEnding::Lf,
                LineEnding::CrLf,
                LineEnding::Cr,
                LineEnding::None
            ]
        );
    }

    #[test]
    fn mixed_flag_and_dominant() {
        let s = scan("a\r\nb\r\nc\n");
        assert!(s.mixed);
        assert_eq!(s.dominant, EolStyle::CrLf);
        assert_eq!(s.total(), 3);
        assert!(s.final_newline);
    }

    #[test]
    fn tie_goes_to_first_seen() {
        let s = scan("a\nb\r\n");
        assert_eq!(s.dominant, EolStyle::Lf);
    }

    #[test]
    fn no_endings_at_all() {
        let s = scan("solitary");
        assert!(!s.mixed);
        assert!(!s.final_newline);
        assert_eq!(s.total(), 0);
    }

    #[test]
    fn empty_text_has_no_lines() {
        assert_eq!(lines("").count(), 0);
    }

    #[test]
    fn conversion_is_idempotent() {
        let once = convert("a\r\nb\rc\n", EolStyle::Lf);
        assert_eq!(once, "a\nb\nc\n");
        assert_eq!(convert(&once, EolStyle::Lf), once);
    }

    #[test]
    fn conversion_keeps_unterminated_last_line() {
        assert_eq!(convert("a\nb", EolStyle::CrLf), "a\r\nb");
    }
}
