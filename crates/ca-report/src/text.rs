//! Text comparison reports.

use crate::cancel::Cancel;
use crate::doc;
use crate::error::{ReportError, Result};
use crate::escape::{text_single_line, write_csv_record, write_html, write_xml};
use crate::gate::{Emit, Gate, Keep};
use crate::input::{Importance, RowKind, TextCell, TextRow};
use crate::options::{OutputOptions, ReportMeta, TextDisplayFilter, TextLayout, TextReportOptions};
use crate::patch;
use crate::plain::{fit, pad, COLUMN_WIDTH};
use std::io::Write;

/// Title used when the caller supplies none.
const FALLBACK_TITLE: &str = "Text Compare Report";

/// Counts one text report accumulates while it writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextCounts {
    /// Rows both sides share.
    pub same: u64,
    /// Rows present on both sides that differ in a way that matters.
    pub different: u64,
    /// Rows present on both sides that differ in a way that does not matter.
    pub unimportant: u64,
    /// Rows present on the left only.
    pub left_only: u64,
    /// Rows present on the right only.
    pub right_only: u64,
    /// Lines the left side holds.
    pub left_lines: u64,
    /// Lines the right side holds.
    pub right_lines: u64,
}

impl TextCounts {
    fn record(&mut self, row: &TextRow) {
        if row.left.is_some() {
            self.left_lines += 1;
        }
        if row.right.is_some() {
            self.right_lines += 1;
        }
        match row.kind {
            RowKind::Same => self.same += 1,
            RowKind::LeftOnly => self.left_only += 1,
            RowKind::RightOnly => self.right_only += 1,
            RowKind::Changed => {
                if row.importance == Some(Importance::Unimportant) {
                    self.unimportant += 1;
                } else {
                    self.different += 1;
                }
            }
        }
    }

    /// Rows that count as a difference.
    #[must_use]
    pub const fn differences(self, ignore_unimportant: bool) -> u64 {
        let base = self.different + self.left_only + self.right_only;
        if ignore_unimportant {
            base
        } else {
            base + self.unimportant
        }
    }

    /// Rows the report looked at.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.same + self.different + self.unimportant + self.left_only + self.right_only
    }

    fn rows(self, ignore_unimportant: bool) -> [(&'static str, u64); 9] {
        [
            ("rows", self.total()),
            ("same", self.same),
            ("different", self.different),
            ("unimportant", self.unimportant),
            ("left only", self.left_only),
            ("right only", self.right_only),
            ("left lines", self.left_lines),
            ("right lines", self.right_lines),
            ("differences", self.differences(ignore_unimportant)),
        ]
    }
}

fn keep_of(filter: &TextDisplayFilter) -> Keep {
    match filter {
        TextDisplayFilter::Mismatches => Keep::Mismatches,
        TextDisplayFilter::Matches => Keep::Matches,
        TextDisplayFilter::Context => Keep::Context,
        _ => Keep::All,
    }
}

/// Write a text comparison report.
///
/// Rows are consumed one at a time and written straight to `out`, so the
/// memory the report holds does not grow with the length of the comparison.
/// The patch layout is the one exception: a patch hunk is written only once it
/// is complete, so one hunk is held.
///
/// # Errors
///
/// Returns [`ReportError::Io`] when the writer refuses the bytes,
/// [`ReportError::Cancelled`] when the caller raises the flag, and
/// [`ReportError::Unsupported`] for an option the chosen document cannot carry.
pub fn write_text_report<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &TextReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TextRow>,
{
    match options.layout {
        TextLayout::Summary => summary(out, meta, options, output, rows, cancel),
        TextLayout::Statistics => statistics(out, options, rows, cancel),
        TextLayout::Patch => patch::write_patch(out, meta, options, rows, cancel),
        TextLayout::Xml => xml(out, meta, options, rows, cancel),
        TextLayout::Interleaved => interleaved(out, meta, options, output, rows, cancel),
        TextLayout::Unknown(ref value) => Err(ReportError::Unsupported(format!(
            "unknown text report layout: {value}"
        ))),
        TextLayout::SideBySide => side_by_side(out, meta, options, output, rows, cancel),
    }
}

fn is_difference(row: &TextRow, options: &TextReportOptions) -> bool {
    row.counts_as_difference(options.ignore_unimportant)
}

fn side_by_side<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &TextReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TextRow>,
{
    doc::validate_output(output)?;
    let html = output.is_html();
    if html {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        out.write_all(b"<table>\n<tr>")?;
        if options.line_numbers {
            out.write_all(b"<th class=\"num\">#</th>")?;
        }
        out.write_all(b"<th>")?;
        write_html(out, &meta.left_label)?;
        out.write_all(b"</th>")?;
        if options.line_numbers {
            out.write_all(b"<th class=\"num\">#</th>")?;
        }
        out.write_all(b"<th>")?;
        write_html(out, &meta.right_label)?;
        out.write_all(b"</th></tr>\n")?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
        writeln!(
            out,
            "{} | {}",
            pad(&text_single_line(&meta.left_label), COLUMN_WIDTH),
            text_single_line(&meta.right_label)
        )?;
        writeln!(
            out,
            "{}-+-{}",
            "-".repeat(COLUMN_WIDTH),
            "-".repeat(COLUMN_WIDTH)
        )?;
    }

    let mut gate = Gate::new(keep_of(&options.display), options.resolved_context_lines());
    let mut counter = 0u64;
    let mut counts = TextCounts::default();
    {
        let mut emit = |emit: Emit<TextRow>| -> Result<()> {
            match emit {
                Emit::Gap(count) => write_gap(out, html, count),
                Emit::Row(row) => {
                    if html {
                        html_side_by_side_row(out, &row, options)
                    } else {
                        text_side_by_side_row(out, &row, options, output)
                    }
                }
            }
        };
        for row in rows {
            doc::poll_cancel(cancel, &mut counter)?;
            counts.record(&row);
            let difference = is_difference(&row, options);
            gate.push(row, difference, &mut emit)?;
        }
        gate.finish(&mut emit)?;
    }

    if html {
        out.write_all(b"</table>\n")?;
        write_counts_html(out, counts, options)?;
        doc::close_html(out)?;
    } else {
        writeln!(out)?;
        write_counts_text(out, counts, options)?;
    }
    Ok(())
}

fn write_gap<W: Write + ?Sized>(out: &mut W, html: bool, count: u64) -> Result<()> {
    if html {
        writeln!(
            out,
            "<tr class=\"same\"><td colspan=\"4\">... {count} row(s) not shown ...</td></tr>"
        )?;
    } else {
        writeln!(out, "... {count} row(s) not shown ...")?;
    }
    Ok(())
}

fn html_cell<W: Write + ?Sized>(out: &mut W, cell: Option<&TextCell>, strike: bool) -> Result<()> {
    out.write_all(b"<td>")?;
    if let Some(cell) = cell {
        if strike {
            out.write_all(b"<span class=\"strike\">")?;
        }
        for (differs, run) in cell.runs() {
            if differs {
                out.write_all(b"<span class=\"inline\">")?;
            }
            write_html(out, run)?;
            if differs {
                out.write_all(b"</span>")?;
            }
        }
        if strike {
            out.write_all(b"</span>")?;
        }
    }
    out.write_all(b"</td>")?;
    Ok(())
}

fn html_number<W: Write + ?Sized>(out: &mut W, cell: Option<&TextCell>) -> Result<()> {
    match cell {
        Some(cell) => write!(out, "<td class=\"num\">{}</td>", cell.number)?,
        None => out.write_all(b"<td class=\"num\"></td>")?,
    }
    Ok(())
}

fn html_side_by_side_row<W: Write + ?Sized>(
    out: &mut W,
    row: &TextRow,
    options: &TextReportOptions,
) -> Result<()> {
    write!(
        out,
        "<tr class=\"{}\">",
        doc::row_class(row.kind, row.importance)
    )?;
    if options.line_numbers {
        html_number(out, row.left.as_ref())?;
    }
    html_cell(out, row.left.as_ref(), false)?;
    if options.line_numbers {
        html_number(out, row.right.as_ref())?;
    }
    html_cell(out, row.right.as_ref(), false)?;
    out.write_all(b"</tr>\n")?;
    Ok(())
}

fn text_side_by_side_row<W: Write + ?Sized>(
    out: &mut W,
    row: &TextRow,
    options: &TextReportOptions,
    output: &OutputOptions,
) -> Result<()> {
    let number_width = if options.line_numbers { 7usize } else { 0 };
    let body_width = COLUMN_WIDTH.saturating_sub(number_width).max(1);
    let left = fit(
        row.left.as_ref().map_or("", |cell| cell.text.as_str()),
        body_width,
        &output.wrap,
    );
    let right = fit(
        row.right.as_ref().map_or("", |cell| cell.text.as_str()),
        body_width,
        &output.wrap,
    );
    let lines = left.len().max(right.len());
    for index in 0..lines {
        let mut left_text = String::new();
        let mut right_text = String::new();
        if options.line_numbers {
            let left_number = if index == 0 {
                row.left.as_ref().map(|cell| cell.number)
            } else {
                None
            };
            let right_number = if index == 0 {
                row.right.as_ref().map(|cell| cell.number)
            } else {
                None
            };
            left_text.push_str(&number_field(left_number));
            right_text.push_str(&number_field(right_number));
        }
        left_text.push_str(left.get(index).map_or("", String::as_str));
        right_text.push_str(right.get(index).map_or("", String::as_str));
        let marker = if index == 0 { row.kind.marker() } else { ' ' };
        writeln!(
            out,
            "{} {marker} {}",
            pad(&left_text, COLUMN_WIDTH),
            right_text.trim_end()
        )?;
    }
    Ok(())
}

fn number_field(number: Option<u64>) -> String {
    match number {
        Some(number) => format!("{number:>5}: "),
        None => " ".repeat(7),
    }
}

fn interleaved<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &TextReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TextRow>,
{
    doc::validate_output(output)?;
    let html = output.is_html();
    if html {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        out.write_all(b"<table>\n<tr><th class=\"mark\">&nbsp;</th><th>Line</th></tr>\n")?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
    }

    let mut gate = Gate::new(keep_of(&options.display), options.resolved_context_lines());
    let mut counter = 0u64;
    let mut counts = TextCounts::default();
    {
        let mut emit = |emit: Emit<TextRow>| -> Result<()> {
            match emit {
                Emit::Gap(count) => write_gap(out, html, count),
                Emit::Row(row) => interleaved_row(out, &row, options, output, html),
            }
        };
        for row in rows {
            doc::poll_cancel(cancel, &mut counter)?;
            counts.record(&row);
            let difference = is_difference(&row, options);
            gate.push(row, difference, &mut emit)?;
        }
        gate.finish(&mut emit)?;
    }

    if html {
        out.write_all(b"</table>\n")?;
        write_counts_html(out, counts, options)?;
        doc::close_html(out)?;
    } else {
        writeln!(out)?;
        write_counts_text(out, counts, options)?;
    }
    Ok(())
}

fn interleaved_row<W: Write + ?Sized>(
    out: &mut W,
    row: &TextRow,
    options: &TextReportOptions,
    output: &OutputOptions,
    html: bool,
) -> Result<()> {
    let class = doc::row_class(row.kind, row.importance);
    if row.kind == RowKind::Same {
        if let Some(cell) = row.left.as_ref().or(row.right.as_ref()) {
            write_interleaved_line(out, class, ' ', cell, false, options, output, html)?;
        }
        return Ok(());
    }
    if let Some(cell) = row.left.as_ref() {
        write_interleaved_line(
            out,
            class,
            '<',
            cell,
            options.strikeout_left_diffs,
            options,
            output,
            html,
        )?;
    }
    if let Some(cell) = row.right.as_ref() {
        write_interleaved_line(
            out,
            class,
            '>',
            cell,
            options.strikeout_right_diffs,
            options,
            output,
            html,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_interleaved_line<W: Write + ?Sized>(
    out: &mut W,
    class: &str,
    marker: char,
    cell: &TextCell,
    strike: bool,
    options: &TextReportOptions,
    output: &OutputOptions,
    html: bool,
) -> Result<()> {
    if html {
        write!(
            out,
            "<tr class=\"{class}\"><td class=\"mark\">{marker}</td>"
        )?;
        html_cell(out, Some(cell), strike)?;
        out.write_all(b"</tr>\n")?;
        return Ok(());
    }
    let width = COLUMN_WIDTH * 2;
    for (index, piece) in fit(&cell.text, width, &output.wrap).into_iter().enumerate() {
        let lead = if index == 0 { marker } else { ' ' };
        if options.line_numbers && index == 0 {
            writeln!(out, "{lead} {:>5}: {piece}", cell.number)?;
        } else if options.line_numbers {
            writeln!(out, "{lead}        {piece}")?;
        } else {
            writeln!(out, "{lead} {piece}")?;
        }
    }
    Ok(())
}

fn summary<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &TextReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TextRow>,
{
    doc::validate_output(output)?;
    let mut counts = TextCounts::default();
    let mut counter = 0u64;
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        counts.record(&row);
    }
    if output.is_html() {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        write_counts_html(out, counts, options)?;
        doc::close_html(out)?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
        write_counts_text(out, counts, options)?;
    }
    Ok(())
}

fn write_counts_html<W: Write + ?Sized>(
    out: &mut W,
    counts: TextCounts,
    options: &TextReportOptions,
) -> Result<()> {
    out.write_all(b"<dl class=\"counts\">\n")?;
    for (name, value) in counts.rows(options.ignore_unimportant) {
        writeln!(out, "<dt>{name}</dt><dd>{value}</dd>")?;
    }
    out.write_all(b"</dl>\n")?;
    Ok(())
}

fn write_counts_text<W: Write + ?Sized>(
    out: &mut W,
    counts: TextCounts,
    options: &TextReportOptions,
) -> Result<()> {
    for (name, value) in counts.rows(options.ignore_unimportant) {
        writeln!(out, "{}{value}", pad(name, 14))?;
    }
    Ok(())
}

fn statistics<W, I>(
    out: &mut W,
    options: &TextReportOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TextRow>,
{
    let mut counts = TextCounts::default();
    let mut counter = 0u64;
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        counts.record(&row);
    }
    write_csv_record(out, &["metric", "count"])?;
    for (name, value) in counts.rows(options.ignore_unimportant) {
        write_csv_record(out, &[name, &value.to_string()])?;
    }
    Ok(())
}

fn xml<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &TextReportOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TextRow>,
{
    doc::open_xml(out, "text-report", meta, FALLBACK_TITLE)?;
    out.write_all(b"  <lines>\n")?;
    let mut gate = Gate::new(keep_of(&options.display), options.resolved_context_lines());
    let mut counter = 0u64;
    let mut counts = TextCounts::default();
    {
        let mut emit = |emit: Emit<TextRow>| -> Result<()> {
            match emit {
                Emit::Gap(count) => {
                    writeln!(out, "    <skipped rows=\"{count}\"/>")?;
                    Ok(())
                }
                Emit::Row(row) => xml_row(out, &row),
            }
        };
        for row in rows {
            doc::poll_cancel(cancel, &mut counter)?;
            counts.record(&row);
            let difference = is_difference(&row, options);
            gate.push(row, difference, &mut emit)?;
        }
        gate.finish(&mut emit)?;
    }
    out.write_all(b"  </lines>\n  <counts>\n")?;
    for (name, value) in counts.rows(options.ignore_unimportant) {
        writeln!(
            out,
            "    <count name=\"{}\">{value}</count>",
            name.replace(' ', "-")
        )?;
    }
    out.write_all(b"  </counts>\n")?;
    doc::close_xml(out, "text-report")
}

fn xml_row<W: Write + ?Sized>(out: &mut W, row: &TextRow) -> Result<()> {
    let kind = match row.kind {
        RowKind::Same => "same",
        RowKind::Changed => "changed",
        RowKind::LeftOnly => "left-only",
        RowKind::RightOnly => "right-only",
    };
    let importance = match row.importance {
        Some(Importance::Important) => " importance=\"important\"",
        Some(Importance::Unimportant) => " importance=\"unimportant\"",
        None => "",
    };
    writeln!(out, "    <line kind=\"{kind}\"{importance}>")?;
    if let Some(cell) = row.left.as_ref() {
        write!(out, "      <left number=\"{}\">", cell.number)?;
        write_xml(out, &cell.text)?;
        out.write_all(b"</left>\n")?;
    }
    if let Some(cell) = row.right.as_ref() {
        write!(out, "      <right number=\"{}\">", cell.number)?;
        write_xml(out, &cell.text)?;
        out.write_all(b"</right>\n")?;
    }
    out.write_all(b"    </line>\n")?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{write_text_report, TextCounts};
    use crate::cancel::{AtomicCancel, NeverCancel};
    use crate::input::{Importance, RowKind, TextCell, TextRow};
    use crate::options::{
        OutputOptions, ReportMeta, TextDisplayFilter, TextLayout, TextReportOptions,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    fn rows() -> Vec<TextRow> {
        vec![
            TextRow {
                kind: RowKind::Same,
                importance: None,
                left: Some(TextCell::new(1, "alpha")),
                right: Some(TextCell::new(1, "alpha")),
            },
            TextRow {
                kind: RowKind::Changed,
                importance: Some(Importance::Important),
                left: Some(TextCell::new(2, "beta")),
                right: Some(TextCell::new(2, "BETA")),
            },
            TextRow {
                kind: RowKind::LeftOnly,
                importance: Some(Importance::Important),
                left: Some(TextCell::new(3, "gamma")),
                right: None,
            },
        ]
    }

    fn render(layout: TextLayout, output: &OutputOptions) -> String {
        let mut out = Vec::new();
        let options = TextReportOptions {
            layout,
            line_numbers: true,
            ..TextReportOptions::default()
        };
        write_text_report(
            &mut out,
            &ReportMeta::new("left.txt", "right.txt"),
            &options,
            output,
            rows(),
            &NeverCancel,
        )
        .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn the_side_by_side_html_layout_writes_one_row_per_line() {
        let html = render(TextLayout::SideBySide, &OutputOptions::html_color());
        assert_eq!(html.matches("<tr class=").count(), 3);
        assert!(html.contains("class=\"diff\""));
        assert!(html.contains("class=\"left-only\""));
    }

    #[test]
    fn the_plain_text_layout_marks_each_kind() {
        let text = render(TextLayout::SideBySide, &OutputOptions::plain_text());
        assert!(text.contains(" * "), "{text}");
        assert!(text.contains(" < "), "{text}");
    }

    #[test]
    fn the_statistics_layout_is_comma_separated_values() {
        let text = render(TextLayout::Statistics, &OutputOptions::plain_text());
        assert!(text.starts_with("\"metric\",\"count\"\r\n"));
        assert!(text.contains("\"different\",\"1\""));
    }

    #[test]
    fn the_xml_layout_is_well_formed_enough_to_carry_every_row() {
        let text = render(TextLayout::Xml, &OutputOptions::plain_text());
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert_eq!(text.matches("<line ").count(), 3);
        assert!(text.ends_with("</text-report>\n"));
    }

    #[test]
    fn the_summary_layout_writes_counts_only() {
        let text = render(TextLayout::Summary, &OutputOptions::plain_text());
        assert!(!text.contains("alpha"));
        assert!(text.contains("different"));
    }

    #[test]
    fn an_unknown_layout_is_refused() {
        let mut out = Vec::new();
        let options = TextReportOptions {
            layout: TextLayout::Unknown(serde_json::Value::String("over-under".into())),
            ..TextReportOptions::default()
        };
        let error = write_text_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &options,
            &OutputOptions::plain_text(),
            rows(),
            &NeverCancel,
        );
        assert!(error.is_err());
    }

    #[test]
    fn the_mismatch_filter_drops_the_matching_rows() {
        let mut out = Vec::new();
        let options = TextReportOptions {
            display: TextDisplayFilter::Mismatches,
            ..TextReportOptions::default()
        };
        write_text_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &options,
            &OutputOptions::plain_text(),
            rows(),
            &NeverCancel,
        )
        .expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(!text.contains("alpha"), "{text}");
        assert!(text.contains("beta"));
    }

    #[test]
    fn the_same_input_twice_writes_the_same_bytes() {
        let first = render(TextLayout::SideBySide, &OutputOptions::html_color());
        let second = render(TextLayout::SideBySide, &OutputOptions::html_color());
        assert_eq!(first, second);
    }

    #[test]
    fn a_raised_flag_stops_a_long_report() {
        let flag = AtomicBool::new(true);
        let cancel = AtomicCancel::new(&flag);
        flag.store(true, Ordering::Relaxed);
        let long = (0..10_000u64).map(|index| TextRow {
            kind: RowKind::Same,
            importance: None,
            left: Some(TextCell::new(index + 1, "line")),
            right: Some(TextCell::new(index + 1, "line")),
        });
        let mut out = Vec::new();
        let result = write_text_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &TextReportOptions::default(),
            &OutputOptions::plain_text(),
            long,
            &cancel,
        );
        assert!(matches!(result, Err(crate::error::ReportError::Cancelled)));
    }

    #[test]
    fn counts_add_up() {
        let mut counts = TextCounts::default();
        for row in rows() {
            counts.record(&row);
        }
        assert_eq!(counts.total(), 3);
        assert_eq!(counts.differences(false), 2);
        assert_eq!(counts.left_lines, 3);
        assert_eq!(counts.right_lines, 2);
    }
}
