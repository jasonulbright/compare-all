//! Turns script text into logical lines of tokens.
//!
//! One logical line is one command. A trailing `&` joins a physical line to the
//! next one; an unquoted `#` ends a physical line; a run between quotation
//! marks keeps its spaces.

use crate::error::ParseError;

/// Characters one logical line may hold.
///
/// The limit keeps a single hostile line from growing the token buffer without
/// bound; a real command is far shorter.
pub const MAX_LOGICAL_LINE_CHARS: usize = 64 * 1024;

/// Bytes a script file may hold before the command line runner refuses it.
pub const MAX_SCRIPT_BYTES: u64 = 64 * 1024 * 1024;

/// Physical lines one `&` chain may join.
pub const MAX_CONTINUATION_LINES: usize = 256;

/// Commands one script may hold.
pub const MAX_COMMANDS: usize = 50_000;

/// One argument, or the command word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The text with the quotation marks removed.
    pub text: String,
    /// True when the token began inside quotation marks. Such a token is never
    /// read as a keyword or as a keyed argument, so a folder named `all` still
    /// reaches the command that takes a path.
    pub quoted: bool,
    /// One based line the token started on.
    pub line: u32,
    /// One based column the token started at.
    pub column: u32,
}

/// One command's worth of tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalLine {
    /// The tokens, command word first.
    pub tokens: Vec<Token>,
    /// One based line the command started on.
    pub line: u32,
    /// One based column the command started at.
    pub column: u32,
}

/// One source character with the place it came from.
#[derive(Debug, Clone, Copy)]
struct Located {
    value: char,
    line: u32,
    column: u32,
}

/// Split script text into logical lines of tokens.
///
/// # Errors
/// Returns a [`ParseError`] for a NUL byte, an unclosed quotation mark, a
/// logical line past [`MAX_LOGICAL_LINE_CHARS`], a continuation chain past
/// [`MAX_CONTINUATION_LINES`], or more than [`MAX_COMMANDS`] commands.
pub fn logical_lines(source: &str) -> Result<Vec<LogicalLine>, ParseError> {
    let mut out: Vec<LogicalLine> = Vec::new();
    let mut pending: Vec<Located> = Vec::new();
    let mut joined = 0usize;
    let mut continued_from: Option<(u32, u32)> = None;

    for (index, raw) in source.lines().enumerate() {
        let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let code = strip_comment(raw, number)?;
        let (mut body, continues) = split_continuation(code);
        trim_end(&mut body);

        if pending.is_empty() {
            trim_start(&mut body);
        } else if !body.is_empty() {
            pending.push(Located {
                value: ' ',
                line: number,
                column: 1,
            });
        }

        if continues {
            if continued_from.is_none() {
                continued_from = Some((number, 1));
            }
            joined += 1;
            if joined > MAX_CONTINUATION_LINES {
                return Err(ParseError::new(
                    number,
                    1,
                    "too many continued lines in one command",
                ));
            }
        }

        pending.extend(body);
        if pending.len() > MAX_LOGICAL_LINE_CHARS {
            return Err(ParseError::new(number, 1, "the command line is too long"));
        }
        if continues {
            continue;
        }

        joined = 0;
        continued_from = None;
        if pending.is_empty() {
            continue;
        }
        let tokens = tokenize(&pending)?;
        pending.clear();
        let Some(first) = tokens.first() else {
            continue;
        };
        let line = LogicalLine {
            line: first.line,
            column: first.column,
            tokens,
        };
        out.push(line);
        if out.len() > MAX_COMMANDS {
            return Err(ParseError::new(
                number,
                1,
                "the script holds too many commands",
            ));
        }
    }

    if !pending.is_empty() {
        let tokens = tokenize(&pending)?;
        if let Some(first) = tokens.first() {
            let line = LogicalLine {
                line: first.line,
                column: first.column,
                tokens,
            };
            out.push(line);
        }
    }
    Ok(out)
}

/// Drop everything from the first unquoted `#`.
fn strip_comment(raw: &str, number: u32) -> Result<Vec<Located>, ParseError> {
    let mut out = Vec::new();
    let mut in_quote = false;
    for (offset, value) in raw.chars().enumerate() {
        let column = u32::try_from(offset + 1).unwrap_or(u32::MAX);
        #[cfg(test)]
        SCANNED_PHYSICAL_LINE_CHARS.with(|count| count.set(count.get().saturating_add(1)));
        if value == '\0' {
            return Err(ParseError::new(
                number,
                column,
                "the script holds a NUL byte",
            ));
        }
        if value == '#' && !in_quote {
            break;
        }
        if out.len() == MAX_LOGICAL_LINE_CHARS {
            return Err(ParseError::new(
                number,
                column,
                "the command line is too long",
            ));
        }
        if value == '"' {
            in_quote = !in_quote;
        }
        out.push(Located {
            value,
            line: number,
            column,
        });
    }
    if in_quote {
        let column = out.last().map_or(1, |c| c.column);
        return Err(ParseError::new(
            number,
            column,
            "a quoted argument is not closed",
        ));
    }
    Ok(out)
}

#[cfg(test)]
std::thread_local! {
    static SCANNED_PHYSICAL_LINE_CHARS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Split a trailing `&` off the line.
fn split_continuation(mut body: Vec<Located>) -> (Vec<Located>, bool) {
    trim_end(&mut body);
    if body.last().is_some_and(|c| c.value == '&') {
        body.pop();
        (body, true)
    } else {
        (body, false)
    }
}

fn trim_end(body: &mut Vec<Located>) {
    while body.last().is_some_and(|c| c.value.is_whitespace()) {
        body.pop();
    }
}

fn trim_start(body: &mut Vec<Located>) {
    let keep = body
        .iter()
        .position(|c| !c.value.is_whitespace())
        .unwrap_or(body.len());
    body.drain(..keep);
}

/// Cut a logical line into tokens.
fn tokenize(body: &[Located]) -> Result<Vec<Token>, ParseError> {
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < body.len() {
        if body[index].value.is_whitespace() {
            index += 1;
            continue;
        }
        let start = body[index];
        let mut text = String::new();
        let quoted = start.value == '"';
        let mut in_quote = false;
        while index < body.len() {
            let here = body[index];
            if here.value == '"' {
                in_quote = !in_quote;
                index += 1;
                continue;
            }
            if here.value.is_whitespace() && !in_quote {
                break;
            }
            text.push(here.value);
            index += 1;
        }
        if in_quote {
            return Err(ParseError::new(
                start.line,
                start.column,
                "a quoted argument is not closed",
            ));
        }
        out.push(Token {
            text,
            quoted,
            line: start.line,
            column: start.column,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{logical_lines, MAX_LOGICAL_LINE_CHARS, SCANNED_PHYSICAL_LINE_CHARS};

    #[test]
    fn an_oversized_physical_line_stops_scanning_at_the_limit() {
        let source = format!("expand {}", "x".repeat(4 * 1024 * 1024));
        SCANNED_PHYSICAL_LINE_CHARS.with(|count| count.set(0));

        let error = logical_lines(&source).expect_err("oversized line refused");

        assert!(error.message.contains("too long"));
        SCANNED_PHYSICAL_LINE_CHARS.with(|count| {
            assert!(
                count.get() <= MAX_LOGICAL_LINE_CHARS + 1,
                "scanned {} characters before refusing",
                count.get()
            );
        });
    }
}
