//! Table comparison reports.

use crate::cancel::Cancel;
use crate::doc;
use crate::error::{ReportError, Result};
use crate::escape::{text_single_line, write_html};
use crate::gate::{Emit, Gate};
use crate::hex::{keep_of, reject_unknown_filter};
use crate::input::{CellStatus, Importance, RowKind, TableHeader, TableRow};
use crate::options::{OutputOptions, ReportMeta, TableLayout, TableReportOptions};
use crate::plain::pad;
use std::io::Write;

/// Title used when the caller supplies none.
const FALLBACK_TITLE: &str = "Table Compare Report";

/// Characters one cell occupies in a plain text table report.
const CELL_WIDTH: usize = 18;

/// Counts one table report accumulates while it writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TableCounts {
    /// Rows both sides share.
    pub same: u64,
    /// Rows that differ in a way that matters.
    pub different: u64,
    /// Rows that differ in a way that does not matter.
    pub unimportant: u64,
    /// Rows present on the left only.
    pub left_only: u64,
    /// Rows present on the right only.
    pub right_only: u64,
    /// Cells that differ in a way that matters.
    pub cells_different: u64,
}

impl TableCounts {
    fn record(&mut self, row: &TableRow) {
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
        self.cells_different += row
            .cells
            .iter()
            .filter(|cell| {
                matches!(
                    cell.status,
                    CellStatus::Different | CellStatus::LeftOnly | CellStatus::RightOnly
                )
            })
            .count() as u64;
    }

    fn rows(self) -> [(&'static str, u64); 7] {
        [
            (
                "rows",
                self.same + self.different + self.unimportant + self.left_only + self.right_only,
            ),
            ("same", self.same),
            ("different", self.different),
            ("unimportant", self.unimportant),
            ("left only", self.left_only),
            ("right only", self.right_only),
            ("cells different", self.cells_different),
        ]
    }
}

/// Write a table comparison report.
///
/// # Errors
///
/// Returns [`ReportError::Io`] when the writer refuses the bytes,
/// [`ReportError::Cancelled`] when the caller raises the flag, and
/// [`ReportError::Unsupported`] for an option the chosen document cannot carry.
pub fn write_table_report<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    header: &TableHeader,
    options: &TableReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TableRow>,
{
    reject_unknown_filter(&options.display)?;
    doc::validate_output(output)?;
    match options.layout {
        TableLayout::Summary => summary(out, meta, header, output, rows, cancel),
        TableLayout::Unknown(ref value) => Err(ReportError::Unsupported(format!(
            "unknown table report layout: {value}"
        ))),
        TableLayout::Interleaved => body(out, meta, header, options, output, rows, cancel, true),
        TableLayout::SideBySide => body(out, meta, header, options, output, rows, cancel, false),
    }
}

fn write_sheet_heading<W: Write + ?Sized>(
    out: &mut W,
    header: &TableHeader,
    options: &TableReportOptions,
    html: bool,
) -> Result<()> {
    let Some(sheet) = header.sheet.as_deref() else {
        return Ok(());
    };
    let scope = if options.current_sheet_only {
        " (current sheet)"
    } else {
        ""
    };
    if html {
        out.write_all(b"<h2>")?;
        write_html(out, sheet)?;
        writeln!(out, "{scope}</h2>")?;
    } else {
        writeln!(out, "Sheet: {}{scope}", text_single_line(sheet))?;
        writeln!(out)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn body<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    header: &TableHeader,
    options: &TableReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
    interleaved: bool,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TableRow>,
{
    let html = output.is_html();
    if html {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
    }
    write_sheet_heading(out, header, options, html)?;
    if html {
        out.write_all(b"<table>\n<tr>")?;
        if options.line_numbers {
            out.write_all(b"<th class=\"num\">#</th>")?;
        }
        if interleaved {
            out.write_all(b"<th class=\"mark\">&nbsp;</th>")?;
        }
        for column in &header.columns {
            out.write_all(b"<th>")?;
            write_html(out, column)?;
            out.write_all(b"</th>")?;
        }
        out.write_all(b"</tr>\n")?;
    } else {
        let mut line = String::new();
        if options.line_numbers {
            line.push_str(&pad("#", 12));
        }
        line.push_str(&pad(" ", 2));
        for column in &header.columns {
            line.push_str(&pad(&text_single_line(column), CELL_WIDTH));
        }
        writeln!(out, "{}", line.trim_end())?;
        writeln!(
            out,
            "{}",
            "-".repeat(line.trim_end().chars().count().max(1))
        )?;
    }

    let mut gate = Gate::new(keep_of(&options.display), 0);
    let mut counts = TableCounts::default();
    let mut counter = 0u64;
    {
        let mut emit = |emit: Emit<TableRow>| -> Result<()> {
            match emit {
                Emit::Gap(count) => {
                    if html {
                        writeln!(
                            out,
                            "<tr class=\"same\"><td colspan=\"99\">... {count} row(s) not shown ...</td></tr>"
                        )?;
                    } else {
                        writeln!(out, "... {count} row(s) not shown ...")?;
                    }
                    Ok(())
                }
                Emit::Row(row) => {
                    if interleaved {
                        interleaved_row(out, &row, options, html)
                    } else {
                        side_by_side_row(out, &row, options, html)
                    }
                }
            }
        };
        for row in rows {
            doc::poll_cancel(cancel, &mut counter)?;
            counts.record(&row);
            let difference = row.kind.is_difference()
                && !(options.ignore_unimportant && row.importance == Some(Importance::Unimportant));
            gate.push(row, difference, &mut emit)?;
        }
        gate.finish(&mut emit)?;
    }

    if html {
        out.write_all(b"</table>\n")?;
        write_counts_html(out, counts)?;
        doc::close_html(out)?;
    } else {
        writeln!(out)?;
        write_counts_text(out, counts)?;
    }
    Ok(())
}

fn numbers(row: &TableRow) -> String {
    match (row.left_number, row.right_number) {
        (Some(left), Some(right)) => format!("{left}/{right}"),
        (Some(left), None) => format!("{left}/-"),
        (None, Some(right)) => format!("-/{right}"),
        (None, None) => "-/-".into(),
    }
}

fn cell_class(status: CellStatus) -> &'static str {
    match status {
        CellStatus::Same => "same",
        CellStatus::Different => "diff",
        CellStatus::Unimportant => "unimportant",
        CellStatus::LeftOnly => "left-only",
        CellStatus::RightOnly => "right-only",
    }
}

fn side_by_side_row<W: Write + ?Sized>(
    out: &mut W,
    row: &TableRow,
    options: &TableReportOptions,
    html: bool,
) -> Result<()> {
    if html {
        write!(
            out,
            "<tr class=\"{}\">",
            doc::row_class(row.kind, row.importance)
        )?;
        if options.line_numbers {
            write!(out, "<td class=\"num\">{}</td>", numbers(row))?;
        }
        for cell in &row.cells {
            write!(out, "<td class=\"{}\">", cell_class(cell.status))?;
            write_html(out, &cell.left)?;
            if cell.left != cell.right {
                out.write_all(b" &rarr; ")?;
                write_html(out, &cell.right)?;
            }
            out.write_all(b"</td>")?;
        }
        out.write_all(b"</tr>\n")?;
        return Ok(());
    }
    let mut line = String::new();
    if options.line_numbers {
        line.push_str(&pad(&numbers(row), 12));
    }
    line.push(row.kind.marker());
    line.push(' ');
    for cell in &row.cells {
        let text = if cell.left == cell.right {
            cell.left.clone()
        } else {
            format!("{} -> {}", cell.left, cell.right)
        };
        line.push_str(&pad(&text_single_line(&text), CELL_WIDTH));
    }
    writeln!(out, "{}", line.trim_end())?;
    Ok(())
}

fn interleaved_row<W: Write + ?Sized>(
    out: &mut W,
    row: &TableRow,
    options: &TableReportOptions,
    html: bool,
) -> Result<()> {
    let sides: &[(char, bool)] = if row.kind == RowKind::Same {
        &[(' ', true)]
    } else {
        &[('<', true), ('>', false)]
    };
    for (marker, is_left) in sides {
        if *is_left && row.left_number.is_none() && row.kind != RowKind::Same {
            continue;
        }
        if !*is_left && row.right_number.is_none() {
            continue;
        }
        if html {
            write!(
                out,
                "<tr class=\"{}\">",
                doc::row_class(row.kind, row.importance)
            )?;
            if options.line_numbers {
                write!(
                    out,
                    "<td class=\"num\">{}</td>",
                    if *is_left {
                        row.left_number
                    } else {
                        row.right_number
                    }
                    .map_or_else(String::new, |number| number.to_string())
                )?;
            }
            write!(out, "<td class=\"mark\">{marker}</td>")?;
            for cell in &row.cells {
                write!(out, "<td class=\"{}\">", cell_class(cell.status))?;
                write_html(out, if *is_left { &cell.left } else { &cell.right })?;
                out.write_all(b"</td>")?;
            }
            out.write_all(b"</tr>\n")?;
        } else {
            let mut line = String::new();
            if options.line_numbers {
                line.push_str(&pad(
                    &if *is_left {
                        row.left_number
                    } else {
                        row.right_number
                    }
                    .map_or_else(String::new, |number| number.to_string()),
                    12,
                ));
            }
            line.push(*marker);
            line.push(' ');
            for cell in &row.cells {
                line.push_str(&pad(
                    &text_single_line(if *is_left { &cell.left } else { &cell.right }),
                    CELL_WIDTH,
                ));
            }
            writeln!(out, "{}", line.trim_end())?;
        }
    }
    Ok(())
}

fn summary<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    header: &TableHeader,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = TableRow>,
{
    let mut counts = TableCounts::default();
    let mut counter = 0u64;
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        counts.record(&row);
    }
    let html = output.is_html();
    if html {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
    }
    write_sheet_heading(out, header, &TableReportOptions::default(), html)?;
    if html {
        write_counts_html(out, counts)?;
        doc::close_html(out)?;
    } else {
        write_counts_text(out, counts)?;
    }
    Ok(())
}

fn write_counts_html<W: Write + ?Sized>(out: &mut W, counts: TableCounts) -> Result<()> {
    out.write_all(b"<dl class=\"counts\">\n")?;
    for (name, value) in counts.rows() {
        writeln!(out, "<dt>{name}</dt><dd>{value}</dd>")?;
    }
    out.write_all(b"</dl>\n")?;
    Ok(())
}

fn write_counts_text<W: Write + ?Sized>(out: &mut W, counts: TableCounts) -> Result<()> {
    for (name, value) in counts.rows() {
        writeln!(out, "{}{value}", pad(name, 16))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::write_table_report;
    use crate::cancel::NeverCancel;
    use crate::input::{CellStatus, Importance, RowKind, TableCell, TableHeader, TableRow};
    use crate::options::{
        DisplayFilter, OutputOptions, ReportMeta, TableLayout, TableReportOptions,
    };

    fn header() -> TableHeader {
        TableHeader {
            sheet: Some("Sheet1".into()),
            columns: vec!["id".into(), "name".into()],
        }
    }

    fn rows() -> Vec<TableRow> {
        vec![
            TableRow {
                left_number: Some(1),
                right_number: Some(1),
                kind: RowKind::Same,
                importance: None,
                cells: vec![
                    TableCell {
                        status: CellStatus::Same,
                        left: "1".into(),
                        right: "1".into(),
                    },
                    TableCell {
                        status: CellStatus::Same,
                        left: "ann".into(),
                        right: "ann".into(),
                    },
                ],
            },
            TableRow {
                left_number: Some(2),
                right_number: Some(2),
                kind: RowKind::Changed,
                importance: Some(Importance::Important),
                cells: vec![
                    TableCell {
                        status: CellStatus::Same,
                        left: "2".into(),
                        right: "2".into(),
                    },
                    TableCell {
                        status: CellStatus::Different,
                        left: "bob".into(),
                        right: "rob".into(),
                    },
                ],
            },
        ]
    }

    fn render(options: &TableReportOptions, output: &OutputOptions) -> String {
        let mut out = Vec::new();
        write_table_report(
            &mut out,
            &ReportMeta::new("left.csv", "right.csv"),
            &header(),
            options,
            output,
            rows(),
            &NeverCancel,
        )
        .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn the_sheet_name_reaches_the_heading() {
        let html = render(&TableReportOptions::default(), &OutputOptions::html_color());
        assert!(html.contains("<h2>Sheet1</h2>"));
    }

    #[test]
    fn the_current_sheet_option_is_stated_in_the_heading() {
        let options = TableReportOptions {
            current_sheet_only: true,
            ..TableReportOptions::default()
        };
        assert!(render(&options, &OutputOptions::html_color()).contains("(current sheet)"));
    }

    #[test]
    fn a_differing_cell_carries_its_own_class() {
        let html = render(&TableReportOptions::default(), &OutputOptions::html_color());
        assert!(
            html.contains("<td class=\"diff\">bob &rarr; rob</td>"),
            "{html}"
        );
    }

    #[test]
    fn the_interleaved_layout_writes_a_row_per_side() {
        let options = TableReportOptions {
            layout: TableLayout::Interleaved,
            ..TableReportOptions::default()
        };
        let text = render(&options, &OutputOptions::plain_text());
        assert!(text.contains("< "), "{text}");
        assert!(text.contains("> "), "{text}");
        assert!(text.contains("bob"));
        assert!(text.contains("rob"));
    }

    #[test]
    fn the_mismatch_filter_drops_the_matching_row() {
        let options = TableReportOptions {
            display: DisplayFilter::Mismatches,
            ..TableReportOptions::default()
        };
        let html = render(&options, &OutputOptions::html_color());
        assert!(!html.contains("ann"), "{html}");
    }

    #[test]
    fn the_summary_layout_writes_counts_only() {
        let options = TableReportOptions {
            layout: TableLayout::Summary,
            ..TableReportOptions::default()
        };
        let text = render(&options, &OutputOptions::plain_text());
        assert!(text.contains("cells different"));
        assert!(!text.contains("bob"));
    }

    #[test]
    fn row_numbers_are_written_when_asked_for() {
        let options = TableReportOptions {
            line_numbers: true,
            ..TableReportOptions::default()
        };
        assert!(render(&options, &OutputOptions::html_color()).contains("2/2"));
    }
}
