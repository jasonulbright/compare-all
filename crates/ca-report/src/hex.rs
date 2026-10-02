//! Hex comparison reports.

use crate::cancel::Cancel;
use crate::doc;
use crate::error::{ReportError, Result};
use crate::escape::{write_csv_record, write_html};
use crate::gate::{Emit, Gate, Keep};
use crate::input::{HexRow, RowKind};
use crate::options::{DisplayFilter, HexLayout, HexReportOptions, OutputOptions, ReportMeta};
use crate::plain::pad;
use std::fmt::Write as _;
use std::io::Write;

/// Title used when the caller supplies none.
const FALLBACK_TITLE: &str = "Hex Compare Report";

/// Counts one hex report accumulates while it writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HexCounts {
    /// Rows both sides share.
    pub same_rows: u64,
    /// Rows that differ.
    pub different_rows: u64,
    /// Bytes the left side holds.
    pub left_bytes: u64,
    /// Bytes the right side holds.
    pub right_bytes: u64,
}

impl HexCounts {
    fn record(&mut self, row: &HexRow) {
        self.left_bytes += row.left.len() as u64;
        self.right_bytes += row.right.len() as u64;
        if row.kind == RowKind::Same {
            self.same_rows += 1;
        } else {
            self.different_rows += 1;
        }
    }

    fn rows(self) -> [(&'static str, u64); 5] {
        [
            ("rows", self.same_rows + self.different_rows),
            ("same rows", self.same_rows),
            ("different rows", self.different_rows),
            ("left bytes", self.left_bytes),
            ("right bytes", self.right_bytes),
        ]
    }
}

pub(crate) fn keep_of(filter: &DisplayFilter) -> Keep {
    match filter {
        DisplayFilter::Mismatches => Keep::Mismatches,
        DisplayFilter::Matches => Keep::Matches,
        _ => Keep::All,
    }
}

pub(crate) fn reject_unknown_filter(filter: &DisplayFilter) -> Result<()> {
    if let DisplayFilter::Unknown(value) = filter {
        return Err(ReportError::Unsupported(format!(
            "unknown display filter: {value}"
        )));
    }
    Ok(())
}

/// Render bytes as pairs of hexadecimal digits, padded to a fixed count.
fn hex_bytes(bytes: &[u8], width: usize) -> String {
    let mut text = String::with_capacity(width * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        let _ = write!(text, "{byte:02X}");
    }
    pad(&text, width * 3 - 1)
}

/// Render bytes as printable characters, with a period for the rest.
fn ascii_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            }
        })
        .collect()
}

/// Write a hex comparison report.
///
/// # Errors
///
/// Returns [`ReportError::Io`] when the writer refuses the bytes,
/// [`ReportError::Cancelled`] when the caller raises the flag, and
/// [`ReportError::Unsupported`] for an option the chosen document cannot carry.
pub fn write_hex_report<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &HexReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = HexRow>,
{
    reject_unknown_filter(&options.display)?;
    doc::validate_output(output)?;
    let width = options.resolved_bytes_per_row() as usize;
    match options.layout {
        HexLayout::Summary => summary(out, meta, output, rows, cancel),
        HexLayout::Unknown(ref value) => Err(ReportError::Unsupported(format!(
            "unknown hex report layout: {value}"
        ))),
        HexLayout::Interleaved => body(out, meta, options, output, rows, cancel, width, true),
        HexLayout::SideBySide => body(out, meta, options, output, rows, cancel, width, false),
    }
}

#[allow(clippy::too_many_arguments)]
fn body<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &HexReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
    width: usize,
    interleaved: bool,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = HexRow>,
{
    let html = output.is_html();
    if html {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        out.write_all(b"<table>\n<tr>")?;
        if interleaved {
            out.write_all(b"<th class=\"mark\">&nbsp;</th><th>Address</th><th>Bytes</th>")?;
        } else {
            if options.line_numbers {
                out.write_all(b"<th class=\"num\">Address</th>")?;
            }
            out.write_all(b"<th>Left</th>")?;
            if options.line_numbers {
                out.write_all(b"<th class=\"num\">Address</th>")?;
            }
            out.write_all(b"<th>Right</th>")?;
        }
        out.write_all(b"</tr>\n")?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
    }

    let mut gate = Gate::new(keep_of(&options.display), 0);
    let mut counts = HexCounts::default();
    let mut counter = 0u64;
    {
        let mut emit = |emit: Emit<HexRow>| -> Result<()> {
            match emit {
                Emit::Gap(count) => {
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
                Emit::Row(row) => {
                    if interleaved {
                        write_interleaved(out, &row, width, html)
                    } else if html {
                        write_html_row(out, &row, width, options)
                    } else {
                        write_text_row(out, &row, width, options)
                    }
                }
            }
        };
        for row in rows {
            doc::poll_cancel(cancel, &mut counter)?;
            counts.record(&row);
            let difference = row.kind != RowKind::Same;
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

fn address(offset: Option<u64>) -> String {
    offset.map_or_else(String::new, |offset| format!("{offset:08X}"))
}

fn write_html_row<W: Write + ?Sized>(
    out: &mut W,
    row: &HexRow,
    width: usize,
    options: &HexReportOptions,
) -> Result<()> {
    write!(out, "<tr class=\"{}\">", doc::row_class(row.kind, None))?;
    if options.line_numbers {
        write!(out, "<td class=\"num\">{}</td>", address(row.left_offset))?;
    }
    out.write_all(b"<td>")?;
    write_html(
        out,
        &format!(
            "{}  {}",
            hex_bytes(&row.left, width),
            ascii_bytes(&row.left)
        ),
    )?;
    out.write_all(b"</td>")?;
    if options.line_numbers {
        write!(out, "<td class=\"num\">{}</td>", address(row.right_offset))?;
    }
    out.write_all(b"<td>")?;
    write_html(
        out,
        &format!(
            "{}  {}",
            hex_bytes(&row.right, width),
            ascii_bytes(&row.right)
        ),
    )?;
    out.write_all(b"</td></tr>\n")?;
    Ok(())
}

fn write_text_row<W: Write + ?Sized>(
    out: &mut W,
    row: &HexRow,
    width: usize,
    options: &HexReportOptions,
) -> Result<()> {
    let left_address = if options.line_numbers {
        format!("{}: ", pad(&address(row.left_offset), 8))
    } else {
        String::new()
    };
    let right_address = if options.line_numbers {
        format!("{}: ", pad(&address(row.right_offset), 8))
    } else {
        String::new()
    };
    writeln!(
        out,
        "{left_address}{}  {} {} {right_address}{}  {}",
        hex_bytes(&row.left, width),
        pad(&ascii_bytes(&row.left), width),
        row.kind.marker(),
        hex_bytes(&row.right, width),
        ascii_bytes(&row.right)
    )?;
    Ok(())
}

fn write_interleaved<W: Write + ?Sized>(
    out: &mut W,
    row: &HexRow,
    width: usize,
    html: bool,
) -> Result<()> {
    for (marker, offset, bytes) in [
        ('<', row.left_offset, &row.left),
        ('>', row.right_offset, &row.right),
    ] {
        if bytes.is_empty() {
            continue;
        }
        let marker = if row.kind == RowKind::Same {
            ' '
        } else {
            marker
        };
        if html {
            write!(
                out,
                "<tr class=\"{}\"><td class=\"mark\">{marker}</td><td class=\"num\">{}</td><td>",
                doc::row_class(row.kind, None),
                address(offset)
            )?;
            write_html(
                out,
                &format!("{}  {}", hex_bytes(bytes, width), ascii_bytes(bytes)),
            )?;
            out.write_all(b"</td></tr>\n")?;
        } else {
            writeln!(
                out,
                "{marker} {}: {}  {}",
                pad(&address(offset), 8),
                hex_bytes(bytes, width),
                ascii_bytes(bytes)
            )?;
        }
        if row.kind == RowKind::Same {
            break;
        }
    }
    Ok(())
}

fn summary<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = HexRow>,
{
    let mut counts = HexCounts::default();
    let mut counter = 0u64;
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        counts.record(&row);
    }
    if output.is_html() {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        write_counts_html(out, counts)?;
        doc::close_html(out)?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
        write_counts_text(out, counts)?;
    }
    Ok(())
}

fn write_counts_html<W: Write + ?Sized>(out: &mut W, counts: HexCounts) -> Result<()> {
    out.write_all(b"<dl class=\"counts\">\n")?;
    for (name, value) in counts.rows() {
        writeln!(out, "<dt>{name}</dt><dd>{value}</dd>")?;
    }
    out.write_all(b"</dl>\n")?;
    Ok(())
}

fn write_counts_text<W: Write + ?Sized>(out: &mut W, counts: HexCounts) -> Result<()> {
    for (name, value) in counts.rows() {
        writeln!(out, "{}{value}", pad(name, 16))?;
    }
    Ok(())
}

/// Write the counts of a hex comparison as comma separated values.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write_hex_statistics<W: Write + ?Sized>(out: &mut W, counts: HexCounts) -> Result<()> {
    write_csv_record(out, &["metric", "count"])?;
    for (name, value) in counts.rows() {
        write_csv_record(out, &[name, &value.to_string()])?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{ascii_bytes, hex_bytes, write_hex_report};
    use crate::cancel::NeverCancel;
    use crate::input::{HexRow, RowKind};
    use crate::options::{DisplayFilter, HexLayout, HexReportOptions, OutputOptions, ReportMeta};

    fn rows() -> Vec<HexRow> {
        vec![
            HexRow {
                kind: RowKind::Same,
                left_offset: Some(0),
                right_offset: Some(0),
                left: vec![0x41, 0x42],
                right: vec![0x41, 0x42],
            },
            HexRow {
                kind: RowKind::Changed,
                left_offset: Some(2),
                right_offset: Some(2),
                left: vec![0x00, 0xff],
                right: vec![0x01, 0xfe],
            },
        ]
    }

    fn render(options: &HexReportOptions, output: &OutputOptions) -> String {
        let mut out = Vec::new();
        write_hex_report(
            &mut out,
            &ReportMeta::new("left.bin", "right.bin"),
            options,
            output,
            rows(),
            &NeverCancel,
        )
        .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn bytes_render_as_upper_case_pairs() {
        assert_eq!(hex_bytes(&[0x0a, 0xff], 2), "0A FF");
        assert_eq!(ascii_bytes(&[0x41, 0x00, 0x7f]), "A..");
    }

    #[test]
    fn the_side_by_side_layout_writes_addresses_when_asked() {
        let options = HexReportOptions {
            line_numbers: true,
            bytes_per_row: 2,
            ..HexReportOptions::default()
        };
        let html = render(&options, &OutputOptions::html_color());
        assert!(html.contains("00000002"));
        assert_eq!(html.matches("<tr class=").count(), 2);
    }

    #[test]
    fn addresses_are_left_out_when_not_asked_for() {
        let options = HexReportOptions {
            bytes_per_row: 2,
            ..HexReportOptions::default()
        };
        assert!(!render(&options, &OutputOptions::html_color()).contains("00000002"));
    }

    #[test]
    fn the_mismatch_filter_keeps_the_differing_row() {
        let options = HexReportOptions {
            display: DisplayFilter::Mismatches,
            bytes_per_row: 2,
            ..HexReportOptions::default()
        };
        let html = render(&options, &OutputOptions::html_color());
        assert!(!html.contains("41 42"), "{html}");
        assert!(html.contains("01 FE"));
    }

    #[test]
    fn the_interleaved_layout_writes_a_line_per_side() {
        let options = HexReportOptions {
            layout: HexLayout::Interleaved,
            bytes_per_row: 2,
            ..HexReportOptions::default()
        };
        let text = render(&options, &OutputOptions::plain_text());
        assert!(text.contains("< 00000002"), "{text}");
        assert!(text.contains("> 00000002"), "{text}");
    }

    #[test]
    fn the_summary_layout_writes_counts_only() {
        let options = HexReportOptions {
            layout: HexLayout::Summary,
            ..HexReportOptions::default()
        };
        let text = render(&options, &OutputOptions::plain_text());
        assert!(text.contains("different rows"));
        assert!(!text.contains("41 42"));
    }
}
