//! Badly formatted and almost-JSON files through the line comparison,
//! Prettify for Comparison and Compare Structure of a real Text Compare view.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_diff::NeverCancel;
use ca_ui::{command::Command, report::Payload, report::RowKind, view::SessionView};
use ca_view_text::{
    prettify::{self, StructuredFormat},
    sidecopy::Side,
    structure::{self, Format},
    TextView,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MIB: usize = 1024 * 1024;
const CHANGED_PATH: &str = r#"$["nested"]["deep"]["list"][2]["k"]"#;

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/messy_json")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Lf,
    Crlf,
    Cr,
    Mixed,
    Bom,
    TrailingWhitespace,
    EveryWhitespace,
}

fn shaped(bytes: &[u8], layout: Layout) -> Vec<u8> {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    match layout {
        Layout::Lf => text.into_bytes(),
        Layout::Crlf => text.replace('\n', "\r\n").into_bytes(),
        Layout::Cr => text.replace('\n', "\r").into_bytes(),
        Layout::Mixed => {
            let mut out = String::new();
            for (index, line) in text.split_inclusive('\n').enumerate() {
                let line = line.strip_suffix('\n').unwrap_or(line);
                out.push_str(line);
                out.push_str(["\r\n", "\n", "\r"][index % 3]);
            }
            out.into_bytes()
        }
        Layout::Bom => [b"\xef\xbb\xbf".as_slice(), bytes].concat(),
        Layout::TrailingWhitespace => text
            .split('\n')
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{line} \t ")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes(),
        Layout::EveryWhitespace => text.replace('\n', " \t\r\n\t ").into_bytes(),
    }
}

fn settle(view: &mut TextView) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !view.is_settled() && Instant::now() < deadline {
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(view.is_settled(), "the comparison did not settle");
}

struct Pair {
    _directory: tempfile::TempDir,
    paths: (PathBuf, PathBuf),
    bytes: (Vec<u8>, Vec<u8>),
    view: TextView,
}

impl Pair {
    fn open(left_name: &str, left: &[u8], right_name: &str, right: &[u8]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let left_path = directory.path().join(format!("left-{left_name}"));
        let right_path = directory.path().join(format!("right-{right_name}"));
        std::fs::write(&left_path, left).unwrap();
        std::fs::write(&right_path, right).unwrap();
        let mut view = TextView::new(
            left_path.clone(),
            right_path.clone(),
            &ca_ui::testing::context(),
            1,
        );
        settle(&mut view);
        let pair = Self {
            _directory: directory,
            paths: (left_path, right_path),
            bytes: (left.to_vec(), right.to_vec()),
            view,
        };
        let _ = pair.sections();
        pair
    }

    /// The line comparison's section count; parsing it proves the diff ran.
    fn sections(&self) -> usize {
        let field = &self.view.status_fields()[0];
        field
            .strip_suffix(" difference section(s)")
            .and_then(|count| count.parse().ok())
            .unwrap_or_else(|| panic!("unexpected status {field:?}"))
    }

    fn texts(&self) -> (String, String) {
        (
            self.view.pane(Side::Left).buffer().text(),
            self.view.pane(Side::Right).buffer().text(),
        )
    }

    fn read_only(&self) -> (bool, bool) {
        (
            self.view.pane(Side::Left).is_read_only(),
            self.view.pane(Side::Right).is_read_only(),
        )
    }

    fn assert_files_unchanged(&self) {
        assert_eq!(std::fs::read(&self.paths.0).unwrap(), self.bytes.0);
        assert_eq!(std::fs::read(&self.paths.1).unwrap(), self.bytes.1);
    }

    /// Run a projection command. A refusal must leave the source view as it
    /// was; a success must show read-only panes.
    fn project(&mut self, command: Command) -> Result<(), String> {
        let before = self.texts();
        let sections = self.sections();
        assert!(self.view.accepts(command), "{command:?} unavailable");
        self.view.run(command);
        settle(&mut self.view);
        self.assert_files_unchanged();
        let message = self.view.message().unwrap_or_default().to_owned();
        if message.starts_with("Could not format")
            || message.starts_with("Structure comparison refused")
        {
            assert_eq!(self.texts(), before, "the source text stays on screen");
            assert_eq!(self.read_only(), (false, false));
            assert!(self.view.structural_summary().is_none());
            assert_eq!(self.sections(), sections, "the line comparison stays");
            assert!(self.view.accepts(command), "{command:?} stays available");
            return Err(message);
        }
        assert_eq!(self.read_only(), (true, true), "{message}");
        Ok(())
    }

    /// Toggle a projection off and check the source text came back.
    fn restore(&mut self, command: Command, source: &(String, String)) {
        self.view.run(command);
        settle(&mut self.view);
        assert_eq!(&self.texts(), source);
        assert_eq!(self.read_only(), (false, false));
        self.assert_files_unchanged();
    }
}

/// Both files hold the same document in different layouts.
fn same_document(left_name: &str, left: &[u8], right_name: &str, right: &[u8]) {
    let mut pair = Pair::open(left_name, left, right_name, right);
    let source = pair.texts();

    pair.project(Command::PrettifyForComparison)
        .unwrap_or_else(|error| panic!("{left_name} / {right_name}: {error}"));
    assert_eq!(pair.sections(), 0, "{left_name} / {right_name}");
    let (formatted_left, formatted_right) = pair.texts();
    assert_eq!(
        formatted_left, formatted_right,
        "{left_name} / {right_name}"
    );
    pair.restore(Command::PrettifyForComparison, &source);

    pair.project(Command::CompareStructure)
        .unwrap_or_else(|error| panic!("{left_name} / {right_name}: {error}"));
    let summary = pair.view.structural_summary().unwrap();
    assert_eq!(
        (summary.added, summary.removed, summary.changed),
        (0, 0, 0),
        "{left_name} / {right_name}"
    );
    assert_eq!(pair.sections(), 0);
    pair.restore(Command::CompareStructure, &source);
}

#[test]
fn ugly_layouts_of_one_document_compare_equal_after_prettify_and_by_structure() {
    let canonical = fixture("canonical.json");
    for name in [
        "tabs_and_spaces.json",
        "split_tokens.json",
        "token_per_line.json",
        "comma_first.json",
        "one_line.json",
        "blank_lines.json",
        "escaped_strings.json",
    ] {
        same_document("canonical.json", &canonical, name, &fixture(name));
    }
}

#[test]
fn line_endings_byte_order_marks_and_legal_whitespace_do_not_change_the_document() {
    let one_line = fixture("one_line.json");
    for (name, layout) in [
        ("tabs_and_spaces.json", Layout::Crlf),
        ("split_tokens.json", Layout::Cr),
        ("comma_first.json", Layout::Mixed),
        ("split_tokens.json", Layout::Bom),
        ("blank_lines.json", Layout::TrailingWhitespace),
        ("token_per_line.json", Layout::EveryWhitespace),
    ] {
        let shaped = shaped(&fixture(name), layout);
        same_document(
            "one_line.json",
            &one_line,
            &format!("{layout:?}-{name}"),
            &shaped,
        );
    }
}

#[test]
fn two_ugly_layouts_of_one_document_compare_equal() {
    same_document(
        "tabs.json",
        &shaped(&fixture("tabs_and_spaces.json"), Layout::Crlf),
        "tokens.json",
        &shaped(
            &shaped(&fixture("token_per_line.json"), Layout::Bom),
            Layout::Lf,
        ),
    );
    same_document(
        "comma.json",
        &shaped(&fixture("comma_first.json"), Layout::Mixed),
        "split.json",
        &shaped(&fixture("split_tokens.json"), Layout::EveryWhitespace),
    );
}

#[test]
fn a_very_long_line_beside_short_lines_compares_equal() {
    let long = "\u{e9}x".repeat(48 * 1024);
    let one_line = String::from_utf8(fixture("one_line.json")).unwrap();
    let left = one_line.replacen('{', &format!("{{\"long\":\"{long}\","), 1);
    let tokens = String::from_utf8(fixture("token_per_line.json")).unwrap();
    let right = tokens.replacen("{\n", &format!("{{\n\"long\"\n:\n\"{long}\"\n,\n"), 1);
    assert!(left.len() > 64 * 1024);
    same_document(
        "long.json",
        left.as_bytes(),
        "tokens.json",
        right.as_bytes(),
    );
}

#[test]
fn one_change_hidden_in_an_ugly_layout_is_the_only_difference() {
    let mut pair = Pair::open(
        "tabs.json",
        &shaped(&fixture("tabs_and_spaces.json"), Layout::Crlf),
        "comma.json",
        &fixture("changed_deep.json"),
    );
    let source = pair.texts();

    pair.project(Command::PrettifyForComparison).unwrap();
    assert_eq!(pair.sections(), 1);
    let (left, right) = pair.texts();
    assert_eq!(left.lines().count(), right.lines().count());
    let differing: Vec<_> = left
        .lines()
        .zip(right.lines())
        .filter(|(left, right)| left != right)
        .collect();
    assert_eq!(differing.len(), 1);
    assert!(differing[0].0.trim_start() == "\"k\": \"v\"");
    assert!(differing[0].1.trim_start() == "\"k\": \"w\"");
    pair.restore(Command::PrettifyForComparison, &source);

    pair.project(Command::CompareStructure).unwrap();
    let summary = pair.view.structural_summary().unwrap();
    assert_eq!((summary.added, summary.removed, summary.changed), (0, 0, 1));
    let (_meta, payload) = pair.view.report_payload();
    let Payload::Text(payload) = payload else {
        panic!("text report expected")
    };
    let changed: Vec<_> = payload
        .iter()
        .filter(|row| row.kind == RowKind::Changed)
        .collect();
    assert_eq!(changed.len(), 1);
    let row = &changed[0];
    assert!(row.left.as_ref().unwrap().text.contains(CHANGED_PATH));
    assert!(row.left.as_ref().unwrap().text.contains("\"v\""));
    assert!(row.right.as_ref().unwrap().text.contains("\"w\""));
    pair.restore(Command::CompareStructure, &source);
}

#[test]
fn escaped_text_compares_by_value_and_json_inside_strings_is_left_alone() {
    let mut pair = Pair::open(
        "a.json",
        br#"{"s": "{\"a\": [1, 2]}", "e": "\u00e9\t\"\\\/", "p": "\ud83d\ude00"}"#,
        "b.json",
        "{\"s\":\"{\\\"a\\\":[1,2]}\",\n\"e\":\"\u{e9}\\t\\\"\\\\/\",\n\"p\":\"\u{1f600}\"}"
            .as_bytes(),
    );
    let source = pair.texts();
    pair.project(Command::PrettifyForComparison).unwrap();
    assert_eq!(pair.sections(), 1);
    let (left, right) = pair.texts();
    assert!(left.contains(r#""s": "{\"a\": [1, 2]}""#), "{left}");
    assert!(right.contains(r#""s": "{\"a\":[1,2]}""#), "{right}");
    pair.restore(Command::PrettifyForComparison, &source);
    pair.project(Command::CompareStructure).unwrap();
    let summary = pair.view.structural_summary().unwrap();
    assert_eq!((summary.added, summary.removed, summary.changed), (0, 0, 1));
}

/// One file is not strict JSON. Both projections refuse, name the side and
/// keep the line comparison.
fn refused_on(name: &str, bad: &[u8], bad_on_left: bool) -> (String, String) {
    let good = fixture("canonical.json");
    let mut pair = if bad_on_left {
        Pair::open(name, bad, "canonical.json", &good)
    } else {
        Pair::open("canonical.json", &good, name, bad)
    };
    let side = if bad_on_left {
        "left side: "
    } else {
        "right side: "
    };
    let prettify = pair
        .project(Command::PrettifyForComparison)
        .err()
        .unwrap_or_else(|| panic!("{name}: Prettify accepted it"));
    let structure = pair
        .project(Command::CompareStructure)
        .err()
        .unwrap_or_else(|| panic!("{name}: Compare Structure accepted it"));
    for message in [&prettify, &structure] {
        let reason = message
            .split_once(side)
            .unwrap_or_else(|| panic!("{name}: {message}"))
            .1;
        assert!(reason.len() > 8, "{name}: {message}");
    }
    (prettify, structure)
}

#[test]
fn almost_json_is_refused_by_both_projections_with_the_side_and_reason() {
    for (name, prettify_reason, structure_reason) in [
        ("comment_line.json", "expected '\"'", "expected '\"'"),
        (
            "comment_block.json",
            "expected a JSON value",
            "expected a JSON value",
        ),
        (
            "trailing_comma_array.json",
            "expected a JSON value",
            "expected a JSON value",
        ),
        (
            "trailing_comma_object.json",
            "expected '\"'",
            "expected '\"'",
        ),
        ("single_quotes.json", "expected '\"'", "expected '\"'"),
        ("unquoted_keys.json", "expected '\"'", "expected '\"'"),
        ("nan.json", "expected a JSON value", "expected a JSON value"),
        (
            "infinity.json",
            "expected a JSON value",
            "expected a JSON value",
        ),
        (
            "negative_infinity.json",
            "invalid JSON number",
            "invalid JSON number",
        ),
        (
            "leading_zeros.json",
            "invalid JSON number",
            "invalid JSON number",
        ),
        (
            "hex_number.json",
            "invalid JSON number",
            "invalid JSON number",
        ),
        (
            "python_literals.json",
            "expected a JSON value",
            "expected a JSON value",
        ),
        (
            "trailing_garbage.json",
            "unexpected text after",
            "unexpected text after",
        ),
        (
            "concatenated.json",
            "unexpected text after",
            "unexpected text after",
        ),
        (
            "truncated.json",
            "unterminated JSON string",
            "unterminated JSON string",
        ),
        (
            "lone_surrogate.json",
            "invalid JSON string",
            "invalid JSON string",
        ),
    ] {
        let bytes = fixture(&format!("near/{name}"));
        for bad_on_left in [true, false] {
            let (prettify, structure) = refused_on(name, &bytes, bad_on_left);
            assert!(prettify.contains(prettify_reason), "{name}: {prettify}");
            assert!(structure.contains(structure_reason), "{name}: {structure}");
        }
    }
}

#[test]
fn json_lines_in_a_json_file_is_refused_and_a_jsonl_pair_stays_a_text_comparison() {
    let lines = fixture("near/lines.jsonl");
    let (prettify, structure) = refused_on("lines.json", &lines, true);
    assert!(prettify.contains("unexpected text after the JSON value"));
    assert!(structure.contains("unexpected text after the JSON value"));

    let other = String::from_utf8(lines.clone())
        .unwrap()
        .replace("\"b\"", "\"B\"");
    let pair = Pair::open("a.jsonl", &lines, "b.jsonl", other.as_bytes());
    assert_eq!(pair.sections(), 1);
    assert!(!pair.view.accepts(Command::PrettifyForComparison));
    assert!(!pair.view.accepts(Command::CompareStructure));
}

#[test]
fn empty_whitespace_only_and_illegal_whitespace_files_are_refused() {
    for (name, bytes, reason) in [
        ("empty.json", Vec::new(), "expected a JSON value"),
        (
            "blank.json",
            b"  \n\t\r\n \r ".to_vec(),
            "expected a JSON value",
        ),
        (
            "form_feed.json",
            b"{\"a\":\x0c1}".to_vec(),
            "expected a JSON value",
        ),
        (
            "vertical_tab.json",
            b"{\"a\":\x0b1}".to_vec(),
            "expected a JSON value",
        ),
        (
            "no_break_space.json",
            "{\"a\":\u{a0}1}".as_bytes().to_vec(),
            "expected a JSON value",
        ),
        (
            "line_separator.json",
            "{\"a\":1\u{2028}}".as_bytes().to_vec(),
            "invalid JSON number",
        ),
        (
            "inner_bom.json",
            "{\"a\":\u{feff}1}".as_bytes().to_vec(),
            "expected a JSON value",
        ),
        (
            "raw_tab_in_string.json",
            b"{\"a\":\"x\ty\"}".to_vec(),
            "invalid JSON string",
        ),
    ] {
        for bad_on_left in [true, false] {
            let (prettify, structure) = refused_on(name, &bytes, bad_on_left);
            assert!(prettify.contains(reason), "{name}: {prettify}");
            assert!(structure.contains(reason), "{name}: {structure}");
        }
    }
}

/// A document of exactly `total` bytes whose size is one long string.
fn padded(total: usize) -> String {
    let prefix = "{\n\t\"pad\"\n:\n\"";
    let suffix = "\"\n,\t\"a\"\n:\n[ 1\n, 2 ]\n}";
    let pad = total - prefix.len() - suffix.len();
    format!("{prefix}{}{suffix}", "x".repeat(pad))
}

/// A document of exactly `total` bytes whose size is legal whitespace.
fn spaced(total: usize) -> String {
    let prefix = "{\n\t\"pad\"\n:\n\"x\"";
    let suffix = "\n,\t\"a\"\n:\n[ 1\n, 2 ]\n}";
    let filler = " \t\r\n".repeat(total);
    let pad = total - prefix.len() - suffix.len();
    format!("{prefix}{}{suffix}", &filler[..pad])
}

#[test]
fn the_structural_input_limit_holds_exactly_at_four_mebibytes_for_ugly_input() {
    let at_limit = spaced(4 * MIB);
    assert_eq!(at_limit.len(), 4 * MIB);
    let compared = structure::compare(&at_limit, &at_limit, Format::Json, &NeverCancel).unwrap();
    assert_eq!(
        (
            compared.summary.added,
            compared.summary.removed,
            compared.summary.changed
        ),
        (0, 0, 0)
    );
    // The storage estimate counts the input twelve times, so a document made
    // mostly of one string is refused by the estimate below the input limit.
    let error = structure::compare(&padded(4 * MIB), "{}", Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert_eq!(
        error,
        "left side: structure exceeds the 64 MiB storage estimate"
    );
    let over = spaced(4 * MIB + 1);
    let error = structure::compare(&over, "{}", Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert_eq!(
        error,
        "left side: input exceeds the 4 MiB structural comparison limit"
    );
    let error = structure::compare("{}", &over, Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert_eq!(
        error,
        "right side: input exceeds the 4 MiB structural comparison limit"
    );
}

#[test]
fn the_prettify_input_limit_holds_exactly_at_sixteen_mebibytes_for_ugly_input() {
    let at_limit = padded(16 * MIB);
    let formatted = prettify::format(&at_limit, StructuredFormat::Json).unwrap();
    assert!(formatted.starts_with("{\n  \"pad\": \"xxx"));
    let error = prettify::format(&padded(16 * MIB + 1), StructuredFormat::Json)
        .err()
        .unwrap();
    assert!(error.contains("16 MiB formatting limit"), "{error}");
}

#[test]
fn an_ugly_file_over_the_structural_limit_still_prettifies_in_the_view() {
    // The long string keeps a line of its own on both sides, so the layouts
    // differ only on short lines.
    let left = padded(4 * MIB + 64);
    let right = left
        .replacen("{\n\t\"pad\"\n:\n", "{ \"pad\" :\n", 1)
        .replacen("\n,\t\"a\"\n:\n[ 1\n, 2 ]\n}", "\n, \"a\": [1,\n2] }\n", 1);
    assert_ne!(left, right);
    assert!(right.len() > 4 * MIB);
    let mut pair = Pair::open("big.json", left.as_bytes(), "other.json", right.as_bytes());
    let source = pair.texts();
    pair.project(Command::PrettifyForComparison).unwrap();
    assert_eq!(pair.sections(), 0);
    pair.restore(Command::PrettifyForComparison, &source);
    let refusal = pair.project(Command::CompareStructure).err().unwrap();
    assert!(
        refusal.ends_with("left side: input exceeds the 4 MiB structural comparison limit"),
        "{refusal}"
    );
}
