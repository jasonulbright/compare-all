//! Patch output of the text report.
//!
//! A patch is consumed by another tool, so it is always plain text and carries
//! no heading: a heading line would make the file fail to apply.
//!
//! One hunk is held while it is built. The hunk ends as soon as the configured
//! count of matching lines follows a difference, so the held rows are bounded
//! by that count plus the length of one run of differences.

use crate::cancel::Cancel;
use crate::doc;
use crate::error::{ReportError, Result};
use crate::escape::text_single_line;
use crate::input::{RowKind, TextRow};
use crate::options::{PatchFormat, ReportMeta, TextReportOptions};
use std::collections::VecDeque;
use std::io::Write;

/// Build the patch and write it.
pub(crate) fn write_patch<W, I>(
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
    let format = match options.patch_format {
        PatchFormat::Context => PatchFormat::Context,
        PatchFormat::Unified => PatchFormat::Unified,
        PatchFormat::Normal => PatchFormat::Normal,
        PatchFormat::Unknown(ref value) => {
            return Err(ReportError::Unsupported(format!(
                "unknown patch format: {value}"
            )))
        }
    };
    let context = match format {
        PatchFormat::Normal => 0,
        _ => options.resolved_context_lines(),
    };
    match format {
        PatchFormat::Unified => {
            writeln!(out, "--- {}", text_single_line(&meta.left_label))?;
            writeln!(out, "+++ {}", text_single_line(&meta.right_label))?;
        }
        PatchFormat::Context => {
            writeln!(out, "*** {}", text_single_line(&meta.left_label))?;
            writeln!(out, "--- {}", text_single_line(&meta.right_label))?;
        }
        _ => {}
    }

    let mut builder = Builder::new(context, options.ignore_unimportant);
    let mut counter = 0u64;
    for row in rows {
        doc::poll_cancel(cancel, &mut counter)?;
        builder.push(row, |hunk, before| emit(out, hunk, before, &format))?;
    }
    builder.finish(|hunk, before| emit(out, hunk, before, &format))?;
    Ok(())
}

/// Line numbers of the last line on each side before the current hunk.
#[derive(Debug, Clone, Copy, Default)]
struct Before {
    left: u64,
    right: u64,
}

struct Builder {
    context: u32,
    ignore_unimportant: bool,
    before_rows: VecDeque<TextRow>,
    hunk: Vec<TextRow>,
    trailing: u32,
    before: Before,
    hunk_before: Before,
}

impl Builder {
    fn new(context: u32, ignore_unimportant: bool) -> Self {
        Self {
            context,
            ignore_unimportant,
            before_rows: VecDeque::new(),
            hunk: Vec::new(),
            trailing: 0,
            before: Before::default(),
            hunk_before: Before::default(),
        }
    }

    fn push<F>(&mut self, row: TextRow, mut flush: F) -> Result<()>
    where
        F: FnMut(&[TextRow], Before) -> Result<()>,
    {
        let difference = row.counts_as_difference(self.ignore_unimportant);
        if difference {
            if self.hunk.is_empty() {
                self.hunk_before = self.before_at_window_start();
                while let Some(kept) = self.before_rows.pop_front() {
                    self.hunk.push(kept);
                }
            }
            self.trailing = 0;
            self.hunk.push(row);
            return Ok(());
        }
        if self.hunk.is_empty() {
            self.advance(&row);
            self.before_rows.push_back(row);
            while self.before_rows.len() > self.context as usize {
                if let Some(dropped) = self.before_rows.pop_front() {
                    drop(dropped);
                }
            }
            return Ok(());
        }
        self.trailing += 1;
        self.hunk.push(row);
        if self.trailing > self.context {
            self.close(&mut flush)?;
        }
        Ok(())
    }

    fn before_at_window_start(&self) -> Before {
        let mut before = self.before;
        for row in &self.before_rows {
            if let Some(cell) = row.left.as_ref() {
                before.left = cell.number.saturating_sub(1);
                break;
            }
        }
        let mut right = self.before.right;
        for row in &self.before_rows {
            if let Some(cell) = row.right.as_ref() {
                right = cell.number.saturating_sub(1);
                break;
            }
        }
        before.right = right;
        before
    }

    fn advance(&mut self, row: &TextRow) {
        if let Some(cell) = row.left.as_ref() {
            self.before.left = cell.number;
        }
        if let Some(cell) = row.right.as_ref() {
            self.before.right = cell.number;
        }
    }

    fn close<F>(&mut self, flush: &mut F) -> Result<()>
    where
        F: FnMut(&[TextRow], Before) -> Result<()>,
    {
        if self.hunk.is_empty() {
            return Ok(());
        }
        let keep = self.hunk.len() - (self.trailing.saturating_sub(self.context)) as usize;
        let extra: Vec<TextRow> = self.hunk.drain(keep..).collect();
        flush(&self.hunk, self.hunk_before)?;
        let emitted: Vec<TextRow> = self.hunk.drain(..).collect();
        for row in &emitted {
            self.advance(row);
        }
        self.before_rows.clear();
        for row in extra {
            self.advance(&row);
            self.before_rows.push_back(row);
            while self.before_rows.len() > self.context as usize {
                self.before_rows.pop_front();
            }
        }
        self.trailing = 0;
        Ok(())
    }

    fn finish<F>(&mut self, mut flush: F) -> Result<()>
    where
        F: FnMut(&[TextRow], Before) -> Result<()>,
    {
        self.close(&mut flush)
    }
}

fn emit<W: Write + ?Sized>(
    out: &mut W,
    hunk: &[TextRow],
    before: Before,
    format: &PatchFormat,
) -> Result<()> {
    match format {
        PatchFormat::Unified => emit_unified(out, hunk, before),
        PatchFormat::Context => emit_context(out, hunk, before),
        _ => emit_normal(out, hunk, before),
    }
}

fn left_lines(hunk: &[TextRow]) -> Vec<&str> {
    hunk.iter()
        .filter_map(|row| row.left.as_ref().map(|cell| cell.text.as_str()))
        .collect()
}

fn right_lines(hunk: &[TextRow]) -> Vec<&str> {
    hunk.iter()
        .filter_map(|row| row.right.as_ref().map(|cell| cell.text.as_str()))
        .collect()
}

fn first_left(hunk: &[TextRow]) -> Option<u64> {
    hunk.iter()
        .find_map(|row| row.left.as_ref().map(|cell| cell.number))
}

fn first_right(hunk: &[TextRow]) -> Option<u64> {
    hunk.iter()
        .find_map(|row| row.right.as_ref().map(|cell| cell.number))
}

fn emit_unified<W: Write + ?Sized>(out: &mut W, hunk: &[TextRow], before: Before) -> Result<()> {
    let old_count = left_lines(hunk).len() as u64;
    let new_count = right_lines(hunk).len() as u64;
    let old_start = first_left(hunk).unwrap_or(before.left);
    let new_start = first_right(hunk).unwrap_or(before.right);
    writeln!(
        out,
        "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
    )?;
    let mut index = 0usize;
    while index < hunk.len() {
        if hunk[index].kind == RowKind::Same {
            let line = hunk[index]
                .left
                .as_ref()
                .or(hunk[index].right.as_ref())
                .map_or("", |cell| cell.text.as_str());
            write_content_line(out, " ", line)?;
            index += 1;
            continue;
        }
        let start = index;
        while index < hunk.len() && hunk[index].kind != RowKind::Same {
            index += 1;
        }
        let run = &hunk[start..index];
        for line in left_lines(run) {
            write_content_line(out, "-", line)?;
        }
        for line in right_lines(run) {
            write_content_line(out, "+", line)?;
        }
    }
    Ok(())
}

fn range_text(start: Option<u64>, count: usize, before: u64) -> String {
    match (start, count) {
        (Some(start), 1) => format!("{start}"),
        (Some(start), count) if count > 1 => {
            let end = start + count as u64 - 1;
            format!("{start},{end}")
        }
        _ => format!("{before}"),
    }
}

/// Write one patch content line. Control characters are file data and must be
/// retained; only embedded line breaks are folded so one row stays one patch
/// record.
fn write_content_line<W: Write + ?Sized>(out: &mut W, prefix: &str, line: &str) -> Result<()> {
    out.write_all(prefix.as_bytes())?;
    let bytes = line.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\r' || *byte == b'\n' {
            out.write_all(&bytes[start..index])?;
            out.write_all(b" ")?;
            start = index + 1;
        }
    }
    out.write_all(&bytes[start..])?;
    out.write_all(b"\n")?;
    Ok(())
}

fn emit_context<W: Write + ?Sized>(out: &mut W, hunk: &[TextRow], before: Before) -> Result<()> {
    out.write_all(b"***************\n")?;
    let left = left_lines(hunk);
    let right = right_lines(hunk);
    writeln!(
        out,
        "*** {} ****",
        range_text(first_left(hunk), left.len(), before.left)
    )?;
    for row in hunk {
        let Some(cell) = row.left.as_ref() else {
            continue;
        };
        let lead = match row.kind {
            RowKind::Same => "  ",
            RowKind::LeftOnly => "- ",
            _ => "! ",
        };
        write_content_line(out, lead, &cell.text)?;
    }
    writeln!(
        out,
        "--- {} ----",
        range_text(first_right(hunk), right.len(), before.right)
    )?;
    for row in hunk {
        let Some(cell) = row.right.as_ref() else {
            continue;
        };
        let lead = match row.kind {
            RowKind::Same => "  ",
            RowKind::RightOnly => "+ ",
            _ => "! ",
        };
        write_content_line(out, lead, &cell.text)?;
    }
    Ok(())
}

fn emit_normal<W: Write + ?Sized>(out: &mut W, hunk: &[TextRow], before: Before) -> Result<()> {
    let left = left_lines(hunk);
    let right = right_lines(hunk);
    let command = if left.is_empty() {
        'a'
    } else if right.is_empty() {
        'd'
    } else {
        'c'
    };
    let left_range = range_text(first_left(hunk), left.len(), before.left);
    let right_range = range_text(first_right(hunk), right.len(), before.right);
    writeln!(out, "{left_range}{command}{right_range}")?;
    for line in &left {
        write_content_line(out, "< ", line)?;
    }
    if command == 'c' {
        out.write_all(b"---\n")?;
    }
    for line in &right {
        write_content_line(out, "> ", line)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::cancel::NeverCancel;
    use crate::input::{Importance, RowKind, TextCell, TextRow};
    use crate::options::{OutputOptions, PatchFormat, ReportMeta, TextLayout, TextReportOptions};
    use crate::text::write_text_report;

    fn row(kind: RowKind, left: Option<(u64, &str)>, right: Option<(u64, &str)>) -> TextRow {
        TextRow {
            kind,
            importance: if kind == RowKind::Same {
                None
            } else {
                Some(Importance::Important)
            },
            left: left.map(|(number, text)| TextCell::new(number, text)),
            right: right.map(|(number, text)| TextCell::new(number, text)),
        }
    }

    fn sample() -> Vec<TextRow> {
        vec![
            row(RowKind::Same, Some((1, "one")), Some((1, "one"))),
            row(RowKind::Same, Some((2, "two")), Some((2, "two"))),
            row(RowKind::Changed, Some((3, "three")), Some((3, "THREE"))),
            row(RowKind::Same, Some((4, "four")), Some((4, "four"))),
            row(RowKind::Same, Some((5, "five")), Some((5, "five"))),
        ]
    }

    fn render(format: PatchFormat, rows: Vec<TextRow>) -> String {
        let mut out = Vec::new();
        let options = TextReportOptions {
            layout: TextLayout::Patch,
            patch_format: format,
            context_lines: 1,
            ..TextReportOptions::default()
        };
        write_text_report(
            &mut out,
            &ReportMeta::new("left.txt", "right.txt"),
            &options,
            &OutputOptions::plain_text(),
            rows,
            &NeverCancel,
        )
        .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn the_normal_format_writes_a_change_command() {
        let text = render(PatchFormat::Normal, sample());
        assert!(text.contains("3c3\n< three\n---\n> THREE\n"), "{text}");
    }

    #[test]
    fn the_normal_format_writes_a_delete_command() {
        let rows = vec![
            row(RowKind::Same, Some((1, "one")), Some((1, "one"))),
            row(RowKind::LeftOnly, Some((2, "gone")), None),
        ];
        let text = render(PatchFormat::Normal, rows);
        assert!(text.contains("2d1\n< gone\n"), "{text}");
    }

    #[test]
    fn the_normal_format_writes_an_add_command() {
        let rows = vec![
            row(RowKind::Same, Some((1, "one")), Some((1, "one"))),
            row(RowKind::RightOnly, None, Some((2, "new"))),
        ];
        let text = render(PatchFormat::Normal, rows);
        assert!(text.contains("1a2\n> new\n"), "{text}");
    }

    #[test]
    fn the_unified_format_writes_a_range_header() {
        let text = render(PatchFormat::Unified, sample());
        assert!(text.starts_with("--- left.txt\n+++ right.txt\n"));
        assert!(text.contains("@@ -2,3 +2,3 @@"), "{text}");
        assert!(text.contains("-three\n+THREE\n"), "{text}");
    }

    #[test]
    fn patch_content_preserves_control_characters_and_applies_exactly() {
        let left = "page one\u{c}\nesc \u{1b}[31mred\n";
        let right = "page one\u{c}\nesc \u{1b}[32mgreen\n";
        let rows = vec![
            row(
                RowKind::Same,
                Some((1, "page one\u{c}")),
                Some((1, "page one\u{c}")),
            ),
            row(
                RowKind::Changed,
                Some((2, "esc \u{1b}[31mred")),
                Some((2, "esc \u{1b}[32mgreen")),
            ),
        ];
        let text = render(PatchFormat::Unified, rows);
        assert!(text.contains(" page one\u{c}\n"), "{text:?}");
        assert!(text.contains("-esc \u{1b}[31mred\n"), "{text:?}");
        assert!(text.contains("+esc \u{1b}[32mgreen\n"), "{text:?}");

        let parsed = ca_diff::parse_patch(&text).expect("patch parses");
        let applied = ca_diff::apply_patch(left, &parsed.files[0]);
        assert!(applied.rejected.is_empty(), "{applied:?}");
        assert_eq!(applied.text, right);
    }

    #[test]
    fn the_context_format_writes_both_blocks() {
        let text = render(PatchFormat::Context, sample());
        assert!(text.contains("***************"));
        assert!(text.contains("*** 2,4 ****"), "{text}");
        assert!(text.contains("--- 2,4 ----"), "{text}");
        assert!(text.contains("! three"));
        assert!(text.contains("! THREE"));
    }

    #[test]
    fn an_unknown_patch_format_is_refused() {
        let mut out = Vec::new();
        let options = TextReportOptions {
            layout: TextLayout::Patch,
            patch_format: PatchFormat::Unknown(serde_json::Value::String("ed".into())),
            ..TextReportOptions::default()
        };
        assert!(write_text_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &options,
            &OutputOptions::plain_text(),
            sample(),
            &NeverCancel,
        )
        .is_err());
    }

    #[test]
    fn a_patch_carries_no_heading() {
        let text = render(PatchFormat::Normal, sample());
        assert!(!text.contains("Text Compare Report"), "{text}");
    }
}
