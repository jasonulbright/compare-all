//! Folder comparison reports.

use crate::cancel::Cancel;
use crate::doc;
use crate::error::{ReportError, Result};
use crate::escape::{text_single_line, write_html, write_xml};
use crate::input::{EntryStatus, FolderRow, SideFacts};
use crate::options::{
    FolderColumn, FolderDisplayFilter, FolderLayout, FolderReportOptions, OutputOptions, ReportMeta,
};
use crate::plain::pad;
use std::io::Write;

/// Title used when the caller supplies none.
const FALLBACK_TITLE: &str = "Folder Compare Report";

/// Characters the name column occupies in a plain text folder report.
const NAME_WIDTH: usize = 40;

/// Characters one value column occupies in a plain text folder report.
const VALUE_WIDTH: usize = 20;

/// Counts one folder report accumulates while it writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderCounts {
    /// Entries both sides share unchanged.
    pub same: u64,
    /// Entries that differ, with no side newer.
    pub different: u64,
    /// Entries newer on the left.
    pub left_newer: u64,
    /// Entries newer on the right.
    pub right_newer: u64,
    /// Entries present on the left only.
    pub left_orphans: u64,
    /// Entries present on the right only.
    pub right_orphans: u64,
    /// Entries that could not be compared.
    pub errors: u64,
    /// Entries the comparison has not reached.
    pub not_compared: u64,
    /// Folders seen.
    pub folders: u64,
    /// Files seen.
    pub files: u64,
}

impl FolderCounts {
    fn record(&mut self, row: &FolderRow) {
        // Status totals count files only, as the folder view does; a folder's
        // status restates the files under it.
        if row.is_dir {
            self.folders += 1;
            return;
        }
        self.files += 1;
        match row.status {
            EntryStatus::Same => self.same += 1,
            EntryStatus::Different => self.different += 1,
            EntryStatus::LeftNewer => self.left_newer += 1,
            EntryStatus::RightNewer => self.right_newer += 1,
            EntryStatus::LeftOrphan => self.left_orphans += 1,
            EntryStatus::RightOrphan => self.right_orphans += 1,
            EntryStatus::Error | EntryStatus::KindMismatch => self.errors += 1,
            EntryStatus::NotCompared => self.not_compared += 1,
        }
    }

    /// Entries that count as a mismatch.
    #[must_use]
    pub const fn differences(self) -> u64 {
        self.different + self.left_newer + self.right_newer + self.left_orphans + self.right_orphans
    }

    fn rows(self) -> [(&'static str, u64); 10] {
        [
            ("folders", self.folders),
            ("files", self.files),
            ("same", self.same),
            ("different", self.different),
            ("left newer", self.left_newer),
            ("right newer", self.right_newer),
            ("left only", self.left_orphans),
            ("right only", self.right_orphans),
            ("errors", self.errors),
            ("differences", self.differences()),
        ]
    }
}

/// Whether the filter keeps one entry.
fn keeps(filter: &FolderDisplayFilter, status: EntryStatus) -> Result<bool> {
    use EntryStatus::{
        Different, Error, KindMismatch, LeftNewer, LeftOrphan, NotCompared, RightNewer,
        RightOrphan, Same,
    };
    let keep = match filter {
        FolderDisplayFilter::All => true,
        FolderDisplayFilter::Mismatches => status != Same,
        FolderDisplayFilter::NoOrphans => !status.is_orphan(),
        FolderDisplayFilter::MismatchesNoOrphans => status != Same && !status.is_orphan(),
        FolderDisplayFilter::Orphans => status.is_orphan(),
        FolderDisplayFilter::LeftNewer => {
            matches!(
                status,
                LeftNewer | Different | KindMismatch | NotCompared | Error
            )
        }
        FolderDisplayFilter::RightNewer => {
            matches!(
                status,
                RightNewer | Different | KindMismatch | NotCompared | Error
            )
        }
        FolderDisplayFilter::LeftNewerOrphans => matches!(
            status,
            LeftNewer | LeftOrphan | Different | KindMismatch | NotCompared | Error
        ),
        FolderDisplayFilter::RightNewerOrphans => matches!(
            status,
            RightNewer | RightOrphan | Different | KindMismatch | NotCompared | Error
        ),
        FolderDisplayFilter::LeftOrphans => status == LeftOrphan,
        FolderDisplayFilter::RightOrphans => status == RightOrphan,
        FolderDisplayFilter::Matches => status == Same,
        FolderDisplayFilter::Unknown(value) => {
            return Err(ReportError::Unsupported(format!(
                "unknown folder report filter: {value}"
            )))
        }
    };
    Ok(keep)
}

fn value(facts: &SideFacts, column: FolderColumn) -> String {
    if !facts.present {
        return String::new();
    }
    let owned = match column {
        FolderColumn::Size => return facts.size.map(|size| size.to_string()).unwrap_or_default(),
        FolderColumn::Timestamp => facts.timestamp.as_deref(),
        FolderColumn::Crc => facts.crc.as_deref(),
        FolderColumn::Version => facts.version.as_deref(),
        FolderColumn::Revision => facts.revision.as_deref(),
        FolderColumn::Vcs => facts.vcs.as_deref(),
        FolderColumn::Attributes => facts.attributes.as_deref(),
        FolderColumn::Owner => facts.owner.as_deref(),
        FolderColumn::Group => facts.group.as_deref(),
    };
    owned.unwrap_or_default().to_owned()
}

/// Write a folder comparison report.
///
/// Rows are consumed one at a time, so a report over a very large tree holds
/// only the row it is writing.
///
/// # Errors
///
/// Returns [`ReportError::Io`] when the writer refuses the bytes,
/// [`ReportError::Cancelled`] when the caller raises the flag, and
/// [`ReportError::Unsupported`] for an option the chosen document cannot carry.
pub fn write_folder_report<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &FolderReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = FolderRow>,
{
    if options.include_file_links
        && !(matches!(options.layout, FolderLayout::SideBySide) && output.is_html())
    {
        return Err(ReportError::Unsupported(
            "file links need a side-by-side HTML folder report".into(),
        ));
    }
    match options.layout {
        FolderLayout::Summary => summary(out, meta, options, output, rows, cancel),
        FolderLayout::Xml => xml(out, meta, options, rows, cancel),
        FolderLayout::Unknown(ref name) => Err(ReportError::Unsupported(format!(
            "unknown folder report layout: {name}"
        ))),
        FolderLayout::SideBySide => side_by_side(out, meta, options, output, rows, cancel),
    }
}

fn side_by_side<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &FolderReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = FolderRow>,
{
    doc::validate_output(output)?;
    let columns = options.columns.selected();
    let html = output.is_html();
    if html {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        out.write_all(b"<table>\n<tr><th>Name</th>")?;
        for column in &columns {
            write!(out, "<th>{} (left)</th>", column.heading())?;
        }
        out.write_all(b"<th>Status</th>")?;
        for column in &columns {
            write!(out, "<th>{} (right)</th>", column.heading())?;
        }
        out.write_all(b"</tr>\n")?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
        let mut heading = pad("Name", NAME_WIDTH);
        for column in &columns {
            heading.push_str(&pad(column.heading(), VALUE_WIDTH));
        }
        heading.push_str(&pad("Status", 14));
        for column in &columns {
            heading.push_str(&pad(column.heading(), VALUE_WIDTH));
        }
        writeln!(out, "{}", heading.trim_end())?;
        writeln!(
            out,
            "{}",
            "-".repeat(heading.trim_end().chars().count().max(1))
        )?;
    }

    let mut counts = FolderCounts::default();
    let mut counter = 0u64;
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        counts.record(&row);
        if !keeps(&options.display, row.status)? {
            continue;
        }
        if html {
            html_row(out, &row, &columns, options)?;
        } else {
            text_row(out, &row, &columns)?;
        }
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

fn indented_name(row: &FolderRow) -> String {
    let mut name = "  ".repeat(row.depth as usize);
    name.push_str(&text_single_line(&row.name));
    if row.is_dir {
        name.push('/');
    }
    name
}

/// A row link is a relative address of another report. A link with a scheme
/// (`javascript:`, `data:`) or a network path would run script or leave the
/// report folder when clicked; browsers drop tabs and line breaks inside a
/// scheme, so any colon before the path counts.
fn is_relative_link(link: &str) -> bool {
    let head = link.split(['/', '\\', '?', '#']).next().unwrap_or_default();
    !head.contains(':') && !link.starts_with("//") && !link.starts_with("\\\\")
}

fn html_row<W: Write + ?Sized>(
    out: &mut W,
    row: &FolderRow,
    columns: &[FolderColumn],
    options: &FolderReportOptions,
) -> Result<()> {
    let class = match row.status {
        EntryStatus::Same => "same",
        EntryStatus::LeftOrphan => "left-only",
        EntryStatus::RightOrphan => "right-only",
        EntryStatus::NotCompared => "unimportant",
        _ => "diff",
    };
    write!(out, "<tr class=\"{class}\"><td>")?;
    match (
        options.include_file_links,
        row.link.as_deref().filter(|link| is_relative_link(link)),
    ) {
        (true, Some(link)) => {
            out.write_all(b"<a href=\"")?;
            write_html(out, link)?;
            out.write_all(b"\">")?;
            write_html(out, &indented_name(row))?;
            out.write_all(b"</a>")?;
        }
        _ => write_html(out, &indented_name(row))?,
    }
    out.write_all(b"</td>")?;
    for column in columns {
        out.write_all(b"<td>")?;
        write_html(out, &value(&row.left, *column))?;
        out.write_all(b"</td>")?;
    }
    write!(out, "<td>{}</td>", row.status.label())?;
    for column in columns {
        out.write_all(b"<td>")?;
        write_html(out, &value(&row.right, *column))?;
        out.write_all(b"</td>")?;
    }
    out.write_all(b"</tr>\n")?;
    Ok(())
}

fn text_row<W: Write + ?Sized>(
    out: &mut W,
    row: &FolderRow,
    columns: &[FolderColumn],
) -> Result<()> {
    let mut line = pad(&indented_name(row), NAME_WIDTH);
    for column in columns {
        line.push_str(&pad(
            &text_single_line(&value(&row.left, *column)),
            VALUE_WIDTH,
        ));
    }
    line.push_str(&pad(row.status.label(), 14));
    for column in columns {
        line.push_str(&pad(
            &text_single_line(&value(&row.right, *column)),
            VALUE_WIDTH,
        ));
    }
    writeln!(out, "{}", line.trim_end())?;
    Ok(())
}

fn summary<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &FolderReportOptions,
    output: &OutputOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = FolderRow>,
{
    doc::validate_output(output)?;
    let html = output.is_html();
    if html {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        out.write_all(b"<table>\n<tr><th>Name</th><th>Status</th></tr>\n")?;
    } else {
        doc::open_text(out, meta, FALLBACK_TITLE)?;
    }
    let mut counts = FolderCounts::default();
    let mut counter = 0u64;
    let summary_filter = if options.display == FolderDisplayFilter::All {
        FolderDisplayFilter::Mismatches
    } else {
        options.display.clone()
    };
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        counts.record(&row);
        if !keeps(&summary_filter, row.status)? {
            continue;
        }
        if html {
            write!(
                out,
                "<tr class=\"{}\"><td>",
                doc::row_class(
                    match row.status {
                        EntryStatus::LeftOrphan => crate::input::RowKind::LeftOnly,
                        EntryStatus::RightOrphan => crate::input::RowKind::RightOnly,
                        _ => crate::input::RowKind::Changed,
                    },
                    None
                )
            )?;
            write_html(out, &row.relative_path)?;
            writeln!(out, "</td><td>{}</td></tr>", row.status.label())?;
        } else {
            writeln!(
                out,
                "{} {}",
                pad(&text_single_line(&row.relative_path), NAME_WIDTH),
                row.status.label()
            )?;
        }
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

fn xml<W, I>(
    out: &mut W,
    meta: &ReportMeta,
    options: &FolderReportOptions,
    rows: I,
    cancel: &dyn Cancel,
) -> Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = FolderRow>,
{
    doc::open_xml(out, "folder-report", meta, FALLBACK_TITLE)?;
    out.write_all(b"  <entries>\n")?;
    let columns = options.columns.selected();
    let mut counts = FolderCounts::default();
    let mut counter = 0u64;
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        counts.record(&row);
        if !keeps(&options.display, row.status)? {
            continue;
        }
        write!(
            out,
            "    <entry kind=\"{}\" status=\"{}\" depth=\"{}\">\n      <path>",
            if row.is_dir { "folder" } else { "file" },
            row.status.element_value(),
            row.depth
        )?;
        write_xml(out, &row.relative_path)?;
        out.write_all(b"</path>\n      <name>")?;
        write_xml(out, &row.name)?;
        out.write_all(b"</name>\n")?;
        for (side, facts) in [("left", &row.left), ("right", &row.right)] {
            writeln!(out, "      <{side} present=\"{}\">", facts.present)?;
            for column in &columns {
                doc::xml_element(out, "        ", column.element(), &value(facts, *column))?;
            }
            writeln!(out, "      </{side}>")?;
        }
        out.write_all(b"    </entry>\n")?;
    }
    out.write_all(b"  </entries>\n  <counts>\n")?;
    for (name, count) in counts.rows() {
        writeln!(
            out,
            "    <count name=\"{}\">{count}</count>",
            name.replace(' ', "-")
        )?;
    }
    out.write_all(b"  </counts>\n")?;
    doc::close_xml(out, "folder-report")
}

fn write_counts_html<W: Write + ?Sized>(out: &mut W, counts: FolderCounts) -> Result<()> {
    out.write_all(b"<dl class=\"counts\">\n")?;
    for (name, count) in counts.rows() {
        writeln!(out, "<dt>{name}</dt><dd>{count}</dd>")?;
    }
    out.write_all(b"</dl>\n")?;
    Ok(())
}

fn write_counts_text<W: Write + ?Sized>(out: &mut W, counts: FolderCounts) -> Result<()> {
    for (name, count) in counts.rows() {
        writeln!(out, "{}{count}", pad(name, 14))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::write_folder_report;
    use crate::cancel::NeverCancel;
    use crate::input::{EntryStatus, FolderRow, SideFacts};
    use crate::options::{
        FolderColumns, FolderDisplayFilter, FolderLayout, FolderReportOptions, OutputOptions,
        ReportMeta,
    };

    fn side(size: u64, stamp: &str) -> SideFacts {
        SideFacts {
            present: true,
            size: Some(size),
            timestamp: Some(stamp.into()),
            ..SideFacts::default()
        }
    }

    fn rows() -> Vec<FolderRow> {
        vec![
            FolderRow {
                depth: 0,
                name: "same.txt".into(),
                relative_path: "same.txt".into(),
                is_dir: false,
                status: EntryStatus::Same,
                left: side(10, "2024-01-01 00:00:00"),
                right: side(10, "2024-01-01 00:00:00"),
                link: None,
            },
            FolderRow {
                depth: 0,
                name: "changed.txt".into(),
                relative_path: "changed.txt".into(),
                is_dir: false,
                status: EntryStatus::LeftNewer,
                left: side(20, "2024-02-01 00:00:00"),
                right: side(18, "2024-01-01 00:00:00"),
                link: Some("changed.html".into()),
            },
            FolderRow {
                depth: 0,
                name: "only-left.txt".into(),
                relative_path: "only-left.txt".into(),
                is_dir: false,
                status: EntryStatus::LeftOrphan,
                left: side(5, "2024-01-01 00:00:00"),
                right: SideFacts::absent(),
                link: None,
            },
        ]
    }

    fn render(options: &FolderReportOptions, output: &OutputOptions) -> String {
        render_rows(options, output, rows())
    }

    fn render_rows(
        options: &FolderReportOptions,
        output: &OutputOptions,
        rows: Vec<FolderRow>,
    ) -> String {
        let mut out = Vec::new();
        write_folder_report(
            &mut out,
            &ReportMeta::new("C:/left", "C:/right"),
            options,
            output,
            rows,
            &NeverCancel,
        )
        .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    fn named_row(name: &str, status: EntryStatus) -> FolderRow {
        FolderRow {
            depth: 0,
            name: name.into(),
            relative_path: name.into(),
            is_dir: false,
            status,
            left: side(1, "2024-01-01 00:00:00"),
            right: side(1, "2024-01-01 00:00:00"),
            link: None,
        }
    }

    #[test]
    fn the_side_by_side_layout_carries_the_default_columns() {
        let html = render(
            &FolderReportOptions::default(),
            &OutputOptions::html_color(),
        );
        assert!(html.contains("Size (left)"));
        assert!(html.contains("Modified (right)"));
        assert_eq!(html.matches("<tr class=").count(), 3);
    }

    #[test]
    fn clearing_the_columns_leaves_the_name_and_status() {
        let options = FolderReportOptions {
            columns: FolderColumns::none(),
            ..FolderReportOptions::default()
        };
        let html = render(&options, &OutputOptions::html_color());
        assert!(!html.contains("Size"));
        assert!(html.contains("<th>Status</th>"));
    }

    #[test]
    fn the_orphan_filter_keeps_one_row() {
        let options = FolderReportOptions {
            display: FolderDisplayFilter::LeftOrphans,
            ..FolderReportOptions::default()
        };
        let html = render(&options, &OutputOptions::html_color());
        assert_eq!(html.matches("<tr class=").count(), 1);
        assert!(html.contains("only-left.txt"));
    }

    #[test]
    fn the_no_orphan_filter_drops_the_orphan() {
        let options = FolderReportOptions {
            display: FolderDisplayFilter::MismatchesNoOrphans,
            ..FolderReportOptions::default()
        };
        let html = render(&options, &OutputOptions::html_color());
        assert!(!html.contains("only-left.txt"));
        assert!(html.contains("changed.txt"));
    }

    #[test]
    fn report_display_filters_match_the_folder_view_filters() {
        use ca_fs::display::DisplayFilter as ViewFilter;

        let filters = [
            (FolderDisplayFilter::All, ViewFilter::ShowAll),
            (FolderDisplayFilter::Mismatches, ViewFilter::ShowDifferences),
            (FolderDisplayFilter::NoOrphans, ViewFilter::ShowNoOrphans),
            (
                FolderDisplayFilter::MismatchesNoOrphans,
                ViewFilter::ShowDifferencesNoOrphans,
            ),
            (FolderDisplayFilter::Orphans, ViewFilter::ShowOrphans),
            (FolderDisplayFilter::LeftNewer, ViewFilter::ShowLeftNewer),
            (FolderDisplayFilter::RightNewer, ViewFilter::ShowRightNewer),
            (
                FolderDisplayFilter::LeftNewerOrphans,
                ViewFilter::ShowLeftNewerAndLeftOrphans,
            ),
            (
                FolderDisplayFilter::RightNewerOrphans,
                ViewFilter::ShowRightNewerAndRightOrphans,
            ),
            (
                FolderDisplayFilter::LeftOrphans,
                ViewFilter::ShowLeftOrphans,
            ),
            (
                FolderDisplayFilter::RightOrphans,
                ViewFilter::ShowRightOrphans,
            ),
            (FolderDisplayFilter::Matches, ViewFilter::ShowSame),
        ];
        let statuses = [
            EntryStatus::NotCompared,
            EntryStatus::Same,
            EntryStatus::Different,
            EntryStatus::LeftNewer,
            EntryStatus::RightNewer,
            EntryStatus::LeftOrphan,
            EntryStatus::RightOrphan,
            EntryStatus::KindMismatch,
            EntryStatus::Error,
        ];
        for (report_filter, view_filter) in filters {
            for status in statuses {
                let view_status = match status {
                    EntryStatus::NotCompared => ca_fs::compare::NodeStatus::NotCompared,
                    EntryStatus::Same => ca_fs::compare::NodeStatus::Same,
                    EntryStatus::Different => ca_fs::compare::NodeStatus::Different,
                    EntryStatus::LeftNewer => ca_fs::compare::NodeStatus::LeftNewer,
                    EntryStatus::RightNewer => ca_fs::compare::NodeStatus::RightNewer,
                    EntryStatus::LeftOrphan => ca_fs::compare::NodeStatus::LeftOrphan,
                    EntryStatus::RightOrphan => ca_fs::compare::NodeStatus::RightOrphan,
                    EntryStatus::KindMismatch => ca_fs::compare::NodeStatus::KindMismatch,
                    EntryStatus::Error => ca_fs::compare::NodeStatus::Error,
                };
                assert_eq!(
                    super::keeps(&report_filter, status).unwrap(),
                    view_filter.shows(view_status),
                    "{report_filter:?} / {status:?}"
                );
            }
        }
    }

    #[test]
    fn summary_mismatch_filter_lists_error_and_not_compared_rows() {
        let options = FolderReportOptions {
            layout: FolderLayout::Summary,
            display: FolderDisplayFilter::Mismatches,
            ..FolderReportOptions::default()
        };
        let mut input = rows();
        input.push(named_row("locked.txt", EntryStatus::Error));
        input.push(named_row("pending.txt", EntryStatus::NotCompared));
        let text = render_rows(&options, &OutputOptions::plain_text(), input);
        assert!(text.contains("locked.txt"), "{text}");
        assert!(text.contains("pending.txt"), "{text}");
        assert!(!text.contains("same.txt"), "{text}");
    }

    #[test]
    fn a_link_is_written_only_when_asked_for() {
        let options = FolderReportOptions {
            include_file_links: true,
            ..FolderReportOptions::default()
        };
        let html = render(&options, &OutputOptions::html_color());
        assert!(html.contains("<a href=\"changed.html\">"));
        let plain = render(
            &FolderReportOptions::default(),
            &OutputOptions::html_color(),
        );
        assert!(!plain.contains("<a href"));
    }

    #[test]
    fn a_link_with_a_scheme_or_another_host_is_not_written() {
        let options = FolderReportOptions {
            include_file_links: true,
            ..FolderReportOptions::default()
        };
        for link in [
            "javascript:alert(3)",
            " JavaScript:alert(3)",
            "java\tscript:alert(3)",
            "data:text/html,x",
            "//example.test/x.html",
            r"\\example.test\x.html",
        ] {
            let mut input = rows();
            input[1].link = Some(link.to_owned());
            let html = render_rows(&options, &OutputOptions::html_color(), input);
            assert!(!html.contains("<a href"), "{link:?}: {html}");
            assert!(html.contains("changed.txt"), "{link:?}");
        }
    }

    #[test]
    fn counts_cover_files_only_and_a_kind_mismatch_is_an_error() {
        let options = FolderReportOptions {
            layout: FolderLayout::Xml,
            ..FolderReportOptions::default()
        };
        let mut input = rows();
        input.push(FolderRow {
            depth: 0,
            name: "sub".into(),
            relative_path: "sub".into(),
            is_dir: true,
            status: EntryStatus::Different,
            left: side(0, "2024-01-01 00:00:00"),
            right: side(0, "2024-01-01 00:00:00"),
            link: None,
        });
        input.push(FolderRow {
            depth: 0,
            name: "kind".into(),
            relative_path: "kind".into(),
            is_dir: false,
            status: EntryStatus::KindMismatch,
            left: side(1, "2024-01-01 00:00:00"),
            right: side(0, "2024-01-01 00:00:00"),
            link: None,
        });
        let xml = render_rows(&options, &OutputOptions::plain_text(), input);
        assert!(xml.contains("<count name=\"folders\">1</count>"), "{xml}");
        assert!(xml.contains("<count name=\"different\">0</count>"), "{xml}");
        assert!(xml.contains("<count name=\"errors\">1</count>"), "{xml}");
        assert!(
            xml.contains("<count name=\"differences\">2</count>"),
            "{xml}"
        );
    }
    #[test]
    fn a_link_outside_a_side_by_side_html_report_is_refused() {
        let options = FolderReportOptions {
            include_file_links: true,
            layout: FolderLayout::Summary,
            ..FolderReportOptions::default()
        };
        let mut out = Vec::new();
        assert!(write_folder_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &options,
            &OutputOptions::html_color(),
            rows(),
            &NeverCancel,
        )
        .is_err());
    }

    #[test]
    fn the_xml_layout_names_each_entry() {
        let options = FolderReportOptions {
            layout: FolderLayout::Xml,
            ..FolderReportOptions::default()
        };
        let text = render(&options, &OutputOptions::plain_text());
        assert_eq!(text.matches("<entry ").count(), 3);
        assert!(text.contains("status=\"left-orphan\""));
        assert!(text.ends_with("</folder-report>\n"));
    }

    #[test]
    fn the_summary_layout_lists_the_differences_only() {
        let options = FolderReportOptions {
            layout: FolderLayout::Summary,
            ..FolderReportOptions::default()
        };
        let text = render(&options, &OutputOptions::plain_text());
        assert!(!text.contains("same.txt"));
        assert!(text.contains("changed.txt"));
        assert!(text.contains("differences"));
    }

    #[test]
    fn a_hostile_file_name_cannot_forge_markup() {
        let mut hostile = rows();
        let name = "<img src=x onerror=alert(1)>".to_owned();
        hostile[0].name.clone_from(&name);
        hostile[0].relative_path = name;
        let mut out = Vec::new();
        write_folder_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &FolderReportOptions::default(),
            &OutputOptions::html_color(),
            hostile,
            &NeverCancel,
        )
        .expect("render");
        let html = String::from_utf8(out).expect("utf-8");
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains("&lt;img"));
    }
}
