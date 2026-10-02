//! Escaping of untrusted values for each output format.
//!
//! File names and file content reach the report unchanged, so every value
//! passes through the escaper of the format that carries it. Nothing in this
//! module trusts its input.

use std::io::Write;

/// Replacement written in place of a code point the format cannot carry.
const REPLACEMENT: &str = "\u{fffd}";

/// True for a control code point that XML 1.0 forbids in character data.
///
/// Tab, line feed and carriage return are the three control code points XML
/// allows; every other code point below `U+0020` is forbidden, and so are the
/// two surrogate-adjacent non-characters `U+FFFE` and `U+FFFF`.
fn xml_forbids(ch: char) -> bool {
    match ch {
        '\t' | '\n' | '\r' => false,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{fffe}' | '\u{ffff}' => true,
        _ => false,
    }
}

/// True for a control code point that no HTML report carries literally.
///
/// The set matches the XML set so one hostile value renders the same way in
/// both documents.
fn html_forbids(ch: char) -> bool {
    xml_forbids(ch)
}

/// Write `value` as HTML text or attribute content.
///
/// The same escape set serves both contexts, so one function covers a value
/// that moves between them.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write_html<W: Write + ?Sized>(out: &mut W, value: &str) -> std::io::Result<()> {
    write_markup(out, value, html_forbids)
}

/// Write `value` as XML text or attribute content.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write_xml<W: Write + ?Sized>(out: &mut W, value: &str) -> std::io::Result<()> {
    write_markup(out, value, xml_forbids)
}

fn write_markup<W: Write + ?Sized>(
    out: &mut W,
    value: &str,
    forbids: fn(char) -> bool,
) -> std::io::Result<()> {
    let mut plain_start = 0usize;
    for (index, ch) in value.char_indices() {
        let replacement = match ch {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\'' => "&#39;",
            _ if forbids(ch) => REPLACEMENT,
            _ => continue,
        };
        if plain_start < index {
            out.write_all(&value.as_bytes()[plain_start..index])?;
        }
        out.write_all(replacement.as_bytes())?;
        plain_start = index + ch.len_utf8();
    }
    if plain_start < value.len() {
        out.write_all(&value.as_bytes()[plain_start..])?;
    }
    Ok(())
}

/// Escape `value` as HTML and return it.
#[must_use]
pub fn html(value: &str) -> String {
    let mut buffer = Vec::new();
    let _ = write_html(&mut buffer, value);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Escape `value` as XML and return it.
#[must_use]
pub fn xml(value: &str) -> String {
    let mut buffer = Vec::new();
    let _ = write_xml(&mut buffer, value);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// True when a value would be read as a formula by a spreadsheet.
fn starts_a_formula(value: &str) -> bool {
    matches!(
        value.as_bytes().first(),
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r')
    )
}

/// Build one CSV field from `value`.
///
/// A value that starts with a character a spreadsheet reads as a formula gains
/// a leading apostrophe inside the quotes, so opening the file never executes
/// it. The guard is unconditional: the reports write counts, which are never
/// negative, so no number loses its sign to it.
#[must_use]
pub fn csv_field(value: &str) -> String {
    let guarded = starts_a_formula(value);
    let mut field = String::with_capacity(value.len() + 4);
    field.push('"');
    if guarded {
        field.push('\'');
    }
    for ch in value.chars() {
        if ch == '"' {
            field.push('"');
        }
        field.push(ch);
    }
    field.push('"');
    field
}

/// Write one CSV record and its terminator.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write_csv_record<W: Write + ?Sized>(out: &mut W, fields: &[&str]) -> std::io::Result<()> {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.write_all(b",")?;
        }
        out.write_all(csv_field(field).as_bytes())?;
    }
    out.write_all(b"\r\n")
}

/// Fold a value onto one plain text line.
///
/// A line break inside a file name would otherwise forge a row in a plain text
/// report, so every break becomes one space.
#[must_use]
pub fn text_single_line(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch == '\n' || ch == '\r' || xml_forbids(ch) {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{csv_field, html, text_single_line, write_csv_record, xml};

    #[test]
    fn markup_specials_are_escaped() {
        assert_eq!(html("a<b>&\"c\'"), "a&lt;b&gt;&amp;&quot;c&#39;");
        assert_eq!(xml("a<b>&\"c\'"), "a&lt;b&gt;&amp;&quot;c&#39;");
    }

    #[test]
    fn a_script_element_in_a_file_name_stays_text() {
        let name = "<script>alert(1)</script>.txt";
        let escaped = html(name);
        assert!(!escaped.contains('<'), "no markup survives: {escaped}");
        assert!(escaped.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_forbidden_control_code_point_becomes_the_replacement() {
        assert_eq!(xml("a\u{0}b\u{1f}c"), "a\u{fffd}b\u{fffd}c");
        assert_eq!(html("a\u{0}b"), "a\u{fffd}b");
    }

    #[test]
    fn the_three_allowed_control_code_points_survive() {
        assert_eq!(xml("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(html("plain name.txt"), "plain name.txt");
    }

    #[test]
    fn csv_quotes_are_doubled() {
        assert_eq!(csv_field(r#"say "hi""#), r#""say ""hi""""#);
    }

    #[test]
    fn csv_carries_a_line_break_inside_quotes() {
        assert_eq!(csv_field("a\r\nb"), "\"a\r\nb\"");
    }

    #[test]
    fn csv_guards_a_leading_formula_character() {
        for value in ["=cmd()", "+1", "-1", "@SUM(A1)"] {
            let field = csv_field(value);
            assert!(field.starts_with("\"'"), "{value} is guarded, got {field}");
        }
    }

    #[test]
    fn csv_leaves_an_ordinary_value_unguarded() {
        assert_eq!(csv_field("report.txt"), "\"report.txt\"");
    }

    #[test]
    fn a_csv_record_ends_with_a_carriage_return_pair() {
        let mut out = Vec::new();
        write_csv_record(&mut out, &["a", "b"]).expect("vector write");
        assert_eq!(out, b"\"a\",\"b\"\r\n");
    }

    #[test]
    fn plain_text_folds_a_break_in_a_name() {
        assert_eq!(text_single_line("a\nb\rc\u{0}d"), "a b c d");
    }
}
