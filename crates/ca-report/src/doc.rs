//! Document scaffolding shared by every report.
//!
//! One HTML document carries its whole style sheet inline, loads nothing and
//! runs no script, so a saved report opens from any folder without a network.
//! The one exception is the custom scheme, which the caller asks for by naming
//! an external style sheet.

use crate::cancel::{Cancel, CANCEL_STRIDE};
use crate::error::{ReportError, Result};
use crate::escape::{write_html, write_xml};
use crate::input::{Importance, RowKind};
use crate::options::{HtmlScheme, OutputOptions, PageOrientation, ReportMeta, Wrap};
use crate::palette::ReportPalette;
use std::io::Write;

/// Class written on a row of an HTML report.
pub(crate) fn row_class(kind: RowKind, importance: Option<Importance>) -> &'static str {
    match kind {
        RowKind::Same => "same",
        RowKind::LeftOnly => "left-only",
        RowKind::RightOnly => "right-only",
        RowKind::Changed => {
            if importance == Some(Importance::Unimportant) {
                "unimportant"
            } else {
                "diff"
            }
        }
    }
}

/// Stop the render when the caller raised the flag.
///
/// The counter is polled on a stride, so the bound on rows written after the
/// flag rises is [`CANCEL_STRIDE`].
pub(crate) fn poll_cancel(cancel: &dyn Cancel, counter: &mut u64) -> Result<()> {
    *counter = counter.wrapping_add(1);
    if (*counter).is_multiple_of(CANCEL_STRIDE) && cancel.is_cancelled() {
        return Err(ReportError::Cancelled);
    }
    Ok(())
}

/// Stop the render when the caller raised the flag, without a stride.
pub(crate) fn poll_cancel_now(cancel: &dyn Cancel) -> Result<()> {
    if cancel.is_cancelled() {
        Err(ReportError::Cancelled)
    } else {
        Ok(())
    }
}

/// Reject an option combination the chosen document cannot carry.
///
/// # Errors
///
/// Returns [`ReportError::Unsupported`] when character wrapping is asked of an
/// HTML document, which has no page to break a word across.
pub(crate) fn validate_output(options: &OutputOptions) -> Result<()> {
    if options.is_html() && matches!(options.wrap, Wrap::Character) {
        return Err(ReportError::Unsupported(
            "character wrapping applies to printer output only".into(),
        ));
    }
    Ok(())
}

fn wrap_rule(wrap: &Wrap) -> &'static str {
    match wrap {
        Wrap::Word | Wrap::Character => "pre-wrap",
        _ => "pre",
    }
}

fn page_size(orientation: &PageOrientation) -> &'static str {
    match orientation {
        PageOrientation::Landscape => "landscape",
        _ => "portrait",
    }
}

fn write_style<W: Write + ?Sized>(
    out: &mut W,
    options: &OutputOptions,
    palette: &ReportPalette,
) -> Result<()> {
    let wrap = wrap_rule(&options.wrap);
    let word_break = if matches!(options.wrap, Wrap::Character) {
        "break-all"
    } else {
        "normal"
    };
    write!(
        out,
        "<style>\n\
@page {{ size: {page}; }}\n\
body {{ background: {bg}; color: {fg}; font-family: monospace; font-size: 10pt; margin: 8pt; }}\n\
h1 {{ font-size: 14pt; margin: 0 0 4pt 0; }}\n\
p.sides {{ margin: 0 0 8pt 0; }}\n\
table {{ border-collapse: collapse; width: 100%; table-layout: fixed; }}\n\
th {{ background: {hdrbg}; color: {hdrfg}; border: 1px solid {border}; text-align: left; padding: 1pt 3pt; }}\n\
td {{ border: 1px solid {border}; padding: 1pt 3pt; vertical-align: top; white-space: {wrap}; word-break: {word_break}; overflow-wrap: anywhere; }}\n\
td.num {{ text-align: right; color: {hdrfg}; background: {hdrbg}; width: 5em; }}\n\
tr.same td {{ background: {samebg}; }}\n\
tr.diff td {{ background: {diffbg}; color: {difffg}; }}\n\
tr.unimportant td {{ background: {unimpbg}; color: {unimpfg}; }}\n\
tr.left-only td {{ background: {leftbg}; }}\n\
tr.right-only td {{ background: {rightbg}; }}\n\
span.inline {{ background: {inline}; }}\n\
span.strike {{ text-decoration: line-through; }}\n\
td.mark {{ width: 2em; text-align: center; }}\n\
dl.counts {{ margin: 0; }}\n\
dl.counts dt {{ font-weight: bold; margin-top: 4pt; }}\n\
img.difference {{ border: 1px solid {border}; max-width: 100%; }}\n\
</style>\n",
        page = page_size(&options.print.orientation),
        bg = palette.background,
        fg = palette.text,
        hdrbg = palette.header_background,
        hdrfg = palette.header_text,
        border = palette.border,
        samebg = palette.same_background,
        diffbg = palette.difference_background,
        difffg = palette.difference_text,
        unimpbg = palette.unimportant_background,
        unimpfg = palette.unimportant_text,
        leftbg = palette.left_orphan_background,
        rightbg = palette.right_orphan_background,
        inline = palette.inline_background,
        wrap = wrap,
        word_break = word_break,
    )?;
    Ok(())
}

/// Open an HTML document and write the heading block.
pub(crate) fn open_html<W: Write + ?Sized>(
    out: &mut W,
    meta: &ReportMeta,
    options: &OutputOptions,
    fallback_title: &str,
) -> Result<()> {
    validate_output(options)?;
    let palette = options.effective_palette();
    let title = meta.title.as_deref().unwrap_or(fallback_title);
    out.write_all(
        b"<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>",
    )?;
    write_html(out, title)?;
    out.write_all(b"</title>\n")?;
    match options.html {
        HtmlScheme::Custom { ref stylesheet, .. } => {
            out.write_all(b"<link rel=\"stylesheet\" href=\"")?;
            write_html(out, stylesheet)?;
            out.write_all(b"\">\n")?;
        }
        _ => write_style(out, options, &palette)?,
    }
    out.write_all(b"</head>\n<body>\n<h1>")?;
    write_html(out, title)?;
    out.write_all(b"</h1>\n<p class=\"sides\">")?;
    write_html(out, &meta.left_label)?;
    out.write_all(b" | ")?;
    write_html(out, &meta.right_label)?;
    if let Some(generated) = meta.generated.as_deref() {
        out.write_all(b"<br>")?;
        write_html(out, generated)?;
    }
    out.write_all(b"</p>\n")?;
    Ok(())
}

/// Close an HTML document.
pub(crate) fn close_html<W: Write + ?Sized>(out: &mut W) -> Result<()> {
    out.write_all(b"</body>\n</html>\n")?;
    Ok(())
}

/// Write the heading block of a plain text document.
pub(crate) fn open_text<W: Write + ?Sized>(
    out: &mut W,
    meta: &ReportMeta,
    fallback_title: &str,
) -> Result<()> {
    let title = meta.title.as_deref().unwrap_or(fallback_title);
    let title = crate::escape::text_single_line(title);
    writeln!(out, "{title}")?;
    writeln!(out, "{}", "=".repeat(title.chars().count().max(1)))?;
    writeln!(
        out,
        "Left:  {}",
        crate::escape::text_single_line(&meta.left_label)
    )?;
    writeln!(
        out,
        "Right: {}",
        crate::escape::text_single_line(&meta.right_label)
    )?;
    if let Some(generated) = meta.generated.as_deref() {
        writeln!(
            out,
            "Generated: {}",
            crate::escape::text_single_line(generated)
        )?;
    }
    writeln!(out)?;
    Ok(())
}

/// Open an XML document and write the heading element.
pub(crate) fn open_xml<W: Write + ?Sized>(
    out: &mut W,
    root: &str,
    meta: &ReportMeta,
    fallback_title: &str,
) -> Result<()> {
    out.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    write!(out, "<{root}>\n  <title>")?;
    write_xml(out, meta.title.as_deref().unwrap_or(fallback_title))?;
    out.write_all(b"</title>\n  <left>")?;
    write_xml(out, &meta.left_label)?;
    out.write_all(b"</left>\n  <right>")?;
    write_xml(out, &meta.right_label)?;
    out.write_all(b"</right>\n")?;
    if let Some(generated) = meta.generated.as_deref() {
        out.write_all(b"  <generated>")?;
        write_xml(out, generated)?;
        out.write_all(b"</generated>\n")?;
    }
    Ok(())
}

/// Close an XML document.
pub(crate) fn close_xml<W: Write + ?Sized>(out: &mut W, root: &str) -> Result<()> {
    writeln!(out, "</{root}>")?;
    Ok(())
}

/// Write one XML element holding an escaped text value.
pub(crate) fn xml_element<W: Write + ?Sized>(
    out: &mut W,
    indent: &str,
    name: &str,
    value: &str,
) -> Result<()> {
    write!(out, "{indent}<{name}>")?;
    write_xml(out, value)?;
    writeln!(out, "</{name}>")?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{close_html, open_html, open_text, row_class, validate_output};
    use crate::input::{Importance, RowKind};
    use crate::options::{OutputOptions, ReportMeta, Wrap};

    fn render(options: &OutputOptions) -> String {
        let mut out = Vec::new();
        let meta = ReportMeta::new("left.txt", "right.txt").with_title("Title");
        open_html(&mut out, &meta, options, "Report").expect("open");
        close_html(&mut out).expect("close");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn an_html_document_carries_its_style_inline() {
        let html = render(&OutputOptions::html_color());
        assert!(html.contains("<style>"));
        assert!(!html.contains("<link"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("http://"));
    }

    #[test]
    fn a_custom_scheme_names_the_external_sheet() {
        let html = render(&OutputOptions::html_custom("theme.css"));
        assert!(html.contains("<link rel=\"stylesheet\" href=\"theme.css\">"));
        assert!(!html.contains("<style>"));
    }

    #[test]
    fn a_hostile_stylesheet_name_is_escaped() {
        let html = render(&OutputOptions::html_custom("\"><script>x()</script>"));
        assert!(!html.contains("<script>"), "{html}");
    }

    #[test]
    fn landscape_reaches_the_page_rule() {
        let mut options = OutputOptions::html_color();
        options.print.orientation = crate::options::PageOrientation::Landscape;
        assert!(render(&options).contains("size: landscape"));
    }

    #[test]
    fn character_wrapping_is_refused_for_html() {
        let mut options = OutputOptions::html_color();
        options.wrap = Wrap::Character;
        assert!(validate_output(&options).is_err());
    }

    #[test]
    fn a_plain_text_heading_has_no_time_unless_supplied() {
        let mut out = Vec::new();
        let meta = ReportMeta::new("a", "b");
        open_text(&mut out, &meta, "Report").expect("open");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(!text.contains("Generated"));
        assert!(text.starts_with("Report\n======\n"));
    }

    #[test]
    fn a_supplied_time_reaches_the_heading() {
        let mut out = Vec::new();
        let mut meta = ReportMeta::new("a", "b");
        meta.generated = Some("2024-01-01 00:00:00".into());
        open_text(&mut out, &meta, "Report").expect("open");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("Generated: 2024-01-01 00:00:00"));
    }

    #[test]
    fn row_classes_split_the_four_kinds() {
        assert_eq!(row_class(RowKind::Same, None), "same");
        assert_eq!(row_class(RowKind::Changed, None), "diff");
        assert_eq!(
            row_class(RowKind::Changed, Some(Importance::Unimportant)),
            "unimportant"
        );
        assert_eq!(row_class(RowKind::LeftOnly, None), "left-only");
    }
}
