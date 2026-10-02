//! A unified patch this crate writes applies cleanly.
//!
//! The applier below is deliberately small and strict: it reads the range
//! headers, checks every context and removed line against the original, and
//! fails on the first mismatch. A patch that survives it is a patch another
//! tool can apply.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_report::input::{Importance, RowKind, TextCell, TextRow};
use ca_report::options::{OutputOptions, PatchFormat, ReportMeta, TextLayout, TextReportOptions};
use ca_report::NeverCancel;

/// Apply a unified patch to `original`, or say why it does not apply.
fn apply_unified(original: &[&str], patch: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut lines = patch.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if !line.starts_with("@@ ") {
            return Err(format!("unexpected line outside a hunk: {line}"));
        }
        let body = line
            .strip_prefix("@@ ")
            .and_then(|rest| rest.strip_suffix(" @@"))
            .ok_or_else(|| format!("malformed hunk header: {line}"))?;
        let mut parts = body.split(' ');
        let old = parts.next().ok_or("hunk header has no old range")?;
        let new = parts.next().ok_or("hunk header has no new range")?;
        if parts.next().is_some() {
            return Err(format!("hunk header has extra fields: {line}"));
        }
        let (old_start, old_count) = parse_range(old.strip_prefix('-').ok_or("no minus")?)?;
        let (_, new_count) = parse_range(new.strip_prefix('+').ok_or("no plus")?)?;

        let start_index = if old_count == 0 {
            old_start
        } else {
            old_start
                .checked_sub(1)
                .ok_or("a hunk starts before the first line")?
        };
        if start_index < cursor {
            return Err("hunks are out of order".into());
        }
        while cursor < start_index {
            out.push(
                (*original
                    .get(cursor)
                    .ok_or("a hunk starts past the end of the original")?)
                .to_owned(),
            );
            cursor += 1;
        }

        let mut consumed = 0usize;
        let mut produced = 0usize;
        while consumed < old_count || produced < new_count {
            let body = lines.next().ok_or("the patch ended inside a hunk")?;
            let (marker, text) = body.split_at(1);
            match marker {
                " " => {
                    let have = original
                        .get(cursor)
                        .ok_or("a context line runs past the original")?;
                    if *have != text {
                        return Err(format!("context mismatch: {have} against {text}"));
                    }
                    out.push(text.to_owned());
                    cursor += 1;
                    consumed += 1;
                    produced += 1;
                }
                "-" => {
                    let have = original
                        .get(cursor)
                        .ok_or("a removed line runs past the original")?;
                    if *have != text {
                        return Err(format!("removal mismatch: {have} against {text}"));
                    }
                    cursor += 1;
                    consumed += 1;
                }
                "+" => {
                    out.push(text.to_owned());
                    produced += 1;
                }
                other => return Err(format!("unknown body marker: {other}")),
            }
        }
    }
    while cursor < original.len() {
        out.push(original[cursor].to_owned());
        cursor += 1;
    }
    Ok(out)
}

fn parse_range(text: &str) -> Result<(usize, usize), String> {
    let mut parts = text.split(',');
    let start = parts
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| format!("bad range start: {text}"))?;
    let count = match parts.next() {
        Some(value) => value
            .parse::<usize>()
            .map_err(|error| format!("bad range count: {error}"))?,
        None => 1,
    };
    Ok((start, count))
}

/// Build rows from two whole sides by pairing equal lines and marking the rest.
///
/// The pairing is deliberate, not computed: the test fixes both sides so the
/// patch under test is known.
fn rows(pairs: &[(Option<&str>, Option<&str>)]) -> Vec<TextRow> {
    let mut left_number = 0u64;
    let mut right_number = 0u64;
    pairs
        .iter()
        .map(|(left, right)| {
            let left = left.map(|text| {
                left_number += 1;
                TextCell::new(left_number, text)
            });
            let right = right.map(|text| {
                right_number += 1;
                TextCell::new(right_number, text)
            });
            let kind = match (&left, &right) {
                (Some(left), Some(right)) if left.text == right.text => RowKind::Same,
                (Some(_), Some(_)) => RowKind::Changed,
                (Some(_), None) => RowKind::LeftOnly,
                _ => RowKind::RightOnly,
            };
            TextRow {
                kind,
                importance: kind.is_difference().then_some(Importance::Important),
                left,
                right,
            }
        })
        .collect()
}

fn unified(rows: Vec<TextRow>, context: u32) -> String {
    let options = TextReportOptions {
        layout: TextLayout::Patch,
        patch_format: PatchFormat::Unified,
        context_lines: context,
        ..TextReportOptions::default()
    };
    let mut out = Vec::new();
    ca_report::write_text_report(
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

fn check(pairs: &[(Option<&str>, Option<&str>)], context: u32) {
    let left: Vec<&str> = pairs.iter().filter_map(|(left, _)| *left).collect();
    let right: Vec<&str> = pairs.iter().filter_map(|(_, right)| *right).collect();
    let patch = unified(rows(pairs), context);
    let applied = apply_unified(&left, &patch)
        .unwrap_or_else(|error| panic!("the patch did not apply: {error}\n{patch}"));
    assert_eq!(applied, right, "patch:\n{patch}");
}

#[test]
fn a_changed_line_applies() {
    check(
        &[
            (Some("one"), Some("one")),
            (Some("two"), Some("TWO")),
            (Some("three"), Some("three")),
        ],
        1,
    );
}

#[test]
fn a_removed_line_applies() {
    check(
        &[
            (Some("one"), Some("one")),
            (Some("gone"), None),
            (Some("three"), Some("three")),
        ],
        1,
    );
}

#[test]
fn an_added_line_applies() {
    check(
        &[
            (Some("one"), Some("one")),
            (None, Some("new")),
            (Some("three"), Some("three")),
        ],
        1,
    );
}

#[test]
fn two_separated_hunks_apply() {
    let mut pairs: Vec<(Option<&str>, Option<&str>)> = Vec::new();
    pairs.push((Some("a"), Some("a")));
    pairs.push((Some("b"), Some("B")));
    for _ in 0..12 {
        pairs.push((Some("filler"), Some("filler")));
    }
    pairs.push((Some("y"), Some("Y")));
    pairs.push((Some("z"), Some("z")));
    check(&pairs, 2);
}

#[test]
fn two_adjacent_hunks_merge_into_one() {
    let pairs = [
        (Some("a"), Some("a")),
        (Some("b"), Some("B")),
        (Some("c"), Some("c")),
        (Some("d"), Some("D")),
        (Some("e"), Some("e")),
    ];
    let patch = unified(rows(&pairs), 2);
    assert_eq!(patch.matches("@@ ").count(), 1, "{patch}");
    check(&pairs, 2);
}

#[test]
fn a_change_at_the_first_line_applies() {
    check(&[(Some("one"), Some("ONE")), (Some("two"), Some("two"))], 3);
}

#[test]
fn a_change_at_the_last_line_applies() {
    check(&[(Some("one"), Some("one")), (Some("two"), Some("TWO"))], 3);
}

#[test]
fn a_file_with_no_difference_produces_an_empty_patch() {
    let patch = unified(rows(&[(Some("one"), Some("one"))]), 3);
    assert_eq!(patch, "--- left.txt\n+++ right.txt\n");
    check(&[(Some("one"), Some("one"))], 3);
}

#[test]
fn control_characters_survive_patch_generation_and_application() {
    let pairs = [
        (Some("page one\u{000C}"), Some("page one\u{000C}")),
        (Some("red\u{001B}[31m"), Some("green\u{001B}[32m")),
    ];
    check(&pairs, 1);
}

#[test]
fn the_applier_rejects_a_patch_whose_context_does_not_match() {
    let patch = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n other\n-two\n+TWO\n";
    assert!(apply_unified(&["one", "two"], patch).is_err());
}
