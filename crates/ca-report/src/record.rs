//! Reports over named values.
//!
//! Version, media and registry comparisons all produce a list of named values
//! with two sides, and all three offer the same two layouts and the same
//! display filters, so one renderer serves them.

use crate::cancel::Cancel;
use crate::doc;
use crate::error::{ReportError, Result};
use crate::escape::{text_single_line, write_html};
use crate::gate::{Emit, Gate};
use crate::hex::{keep_of, reject_unknown_filter};
use crate::input::{Importance, RecordRow, RowKind};
use crate::options::{OutputOptions, PairLayout, RecordReportOptions, ReportMeta};
use crate::plain::pad;
use std::io::Write;

/// Which comparison the records come from.
///
/// The kind names the report and the column headings; it changes nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// File version information.
    Version,
    /// Media metadata.
    Media,
    /// Registry keys and values.
    Registry,
}

impl RecordKind {
    /// Title used when the caller supplies none.
    #[must_use]
    pub const fn fallback_title(self) -> &'static str {
        match self {
            Self::Version => "Version Compare Report",
            Self::Media => "Media Compare Report",
            Self::Registry => "Registry Compare Report",
        }
    }

    /// Heading of the group column.
    #[must_use]
    pub const fn group_heading(self) -> &'static str {
        match self {
            Self::Version => "Block",
            Self::Media => "Stream",
            Self::Registry => "Key",
        }
    }
}

/// Counts one record report accumulates while it writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordCounts {
    /// Records both sides share.
    pub same: u64,
    /// Records that differ in a way that matters.
    pub different: u64,
    /// Records that differ in a way that does not matter.
    pub unimportant: u64,
    /// Records present on the left only.
    pub left_only: u64,
    /// Records present on the right only.
    pub right_only: u64,
}

impl RecordCounts {
    fn record(&mut self, row: &RecordRow) {
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

    fn rows(self, ignore_unimportant: bool) -> [(&'static str, u64); 6] {
        let differences = self.different
            + self.left_only
            + self.right_only
            + if ignore_unimportant {
                0
            } else {
                self.unimportant
            };
        [
            (
                "records",
                self.same + self.different + self.unimportant + self.left_only + self.right_only,
            ),
            ("same", self.same),
            ("different", self.different),
            ("left only", self.left_only),
            ("right only", self.right_only),
            ("differences", differences),
        ]
    }
}

/// Write a report over named values.
///
/// # Errors
///
/// Returns [`ReportError::Io`] when the writer refuses the bytes,
/// [`ReportError::Cancelled`] when the caller raises the flag, and
/// [`ReportError::Unsupported`] for an option the chosen document cannot carry.
pub fn write_record_report<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    kind: RecordKind,
    options: &RecordReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = RecordRow>,
{
    reject_unknown_filter(&options.display)?;
    doc::validate_output(output)?;
    let side_by_side = match options.layout {
        PairLayout::SideBySide => true,
        PairLayout::Summary => false,
        PairLayout::Unknown(ref value) => {
            return Err(ReportError::Unsupported(format!(
                "unknown report layout: {value}"
            )))
        }
    };
    let html = output.is_html();
    let title = kind.fallback_title();
    if html {
        doc::open_html(out, meta, output, title)?;
    } else {
        doc::open_text(out, meta, title)?;
    }
    if side_by_side {
        if html {
            write!(
                out,
                "<table>\n<tr><th>{}</th><th>Name</th><th>",
                kind.group_heading()
            )?;
            write_html(out, &meta.left_label)?;
            out.write_all(b"</th><th>")?;
            write_html(out, &meta.right_label)?;
            out.write_all(b"</th></tr>\n")?;
        } else {
            writeln!(
                out,
                "{}{}{}Right",
                pad(kind.group_heading(), 20),
                pad("Name", 24),
                pad("Left", 24),
            )?;
            writeln!(out, "{}", "-".repeat(92))?;
        }
    }

    let mut gate = Gate::new(keep_of(&options.display), 0);
    let mut counts = RecordCounts::default();
    let mut counter = 0u64;
    {
        let mut emit = |emit: Emit<RecordRow>| -> Result<()> {
            match emit {
                Emit::Gap(count) => {
                    if !side_by_side {
                        return Ok(());
                    }
                    if html {
                        writeln!(
                            out,
                            "<tr class=\"same\"><td colspan=\"4\">... {count} record(s) not shown ...</td></tr>"
                        )?;
                    } else {
                        writeln!(out, "... {count} record(s) not shown ...")?;
                    }
                    Ok(())
                }
                Emit::Row(row) => {
                    if !side_by_side {
                        return Ok(());
                    }
                    write_row(out, &row, html)
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
        if side_by_side {
            out.write_all(b"</table>\n")?;
        }
        out.write_all(b"<dl class=\"counts\">\n")?;
        for (name, value) in counts.rows(options.ignore_unimportant) {
            writeln!(out, "<dt>{name}</dt><dd>{value}</dd>")?;
        }
        out.write_all(b"</dl>\n")?;
        doc::close_html(out)?;
    } else {
        writeln!(out)?;
        for (name, value) in counts.rows(options.ignore_unimportant) {
            writeln!(out, "{}{value}", pad(name, 14))?;
        }
    }
    Ok(())
}

fn write_row<W: Write + ?Sized>(out: &mut W, row: &RecordRow, html: bool) -> Result<()> {
    let group = row.group.as_deref().unwrap_or_default();
    if html {
        write!(
            out,
            "<tr class=\"{}\"><td>",
            doc::row_class(row.kind, row.importance)
        )?;
        write_html(out, group)?;
        out.write_all(b"</td><td>")?;
        write_html(out, &row.name)?;
        out.write_all(b"</td><td>")?;
        write_html(out, row.left.as_deref().unwrap_or_default())?;
        out.write_all(b"</td><td>")?;
        write_html(out, row.right.as_deref().unwrap_or_default())?;
        out.write_all(b"</td></tr>\n")?;
        return Ok(());
    }
    writeln!(
        out,
        "{}{}{}{}",
        pad(&text_single_line(group), 20),
        pad(&text_single_line(&row.name), 24),
        pad(
            &text_single_line(row.left.as_deref().unwrap_or_default()),
            24
        ),
        text_single_line(row.right.as_deref().unwrap_or_default())
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{write_record_report, RecordKind};
    use crate::cancel::NeverCancel;
    use crate::input::{Importance, RecordRow, RowKind};
    use crate::options::{
        DisplayFilter, OutputOptions, PairLayout, RecordReportOptions, ReportMeta,
    };

    fn rows() -> Vec<RecordRow> {
        vec![
            RecordRow {
                name: "FileVersion".into(),
                group: Some("StringFileInfo".into()),
                kind: RowKind::Same,
                importance: None,
                left: Some("1.0".into()),
                right: Some("1.0".into()),
            },
            RecordRow {
                name: "ProductVersion".into(),
                group: Some("StringFileInfo".into()),
                kind: RowKind::Changed,
                importance: Some(Importance::Important),
                left: Some("1.0".into()),
                right: Some("2.0".into()),
            },
        ]
    }

    fn render(kind: RecordKind, options: &RecordReportOptions, output: &OutputOptions) -> String {
        let mut out = Vec::new();
        write_record_report(
            &mut out,
            &ReportMeta::new("left", "right"),
            kind,
            options,
            output,
            rows(),
            &NeverCancel,
        )
        .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn each_kind_names_its_own_group_column() {
        assert!(render(
            RecordKind::Registry,
            &RecordReportOptions::default(),
            &OutputOptions::html_color()
        )
        .contains("<th>Key</th>"));
        assert!(render(
            RecordKind::Media,
            &RecordReportOptions::default(),
            &OutputOptions::html_color()
        )
        .contains("<th>Stream</th>"));
    }

    #[test]
    fn the_side_by_side_layout_writes_a_row_per_record() {
        let html = render(
            RecordKind::Version,
            &RecordReportOptions::default(),
            &OutputOptions::html_color(),
        );
        assert_eq!(html.matches("<tr class=").count(), 2);
        assert!(html.contains("Version Compare Report"));
    }

    #[test]
    fn the_summary_layout_writes_counts_only() {
        let options = RecordReportOptions {
            layout: PairLayout::Summary,
            ..RecordReportOptions::default()
        };
        let text = render(RecordKind::Version, &options, &OutputOptions::plain_text());
        assert!(!text.contains("ProductVersion"));
        assert!(text.contains("differences"));
    }

    #[test]
    fn the_match_filter_keeps_the_matching_record() {
        let options = RecordReportOptions {
            display: DisplayFilter::Matches,
            ..RecordReportOptions::default()
        };
        let text = render(RecordKind::Version, &options, &OutputOptions::plain_text());
        assert!(text.contains("FileVersion"));
        assert!(!text.contains("ProductVersion"));
    }

    #[test]
    fn an_unknown_filter_is_refused() {
        let options = RecordReportOptions {
            display: DisplayFilter::Unknown(serde_json::Value::String("display-new".into())),
            ..RecordReportOptions::default()
        };
        let mut out = Vec::new();
        assert!(write_record_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            RecordKind::Version,
            &options,
            &OutputOptions::plain_text(),
            rows(),
            &NeverCancel,
        )
        .is_err());
    }
}
