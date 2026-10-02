//! Every stored table comparison setting changes what a comparison produces,
//! and the view reports the same stored values a settings page edits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::settings::common::{EncodingChoice, FileFormatChoice};
use ca_session::settings::table::{
    ColumnHandling as StoredHandling, TableCompareSettings, TablePairing,
};
use ca_view_table::jobs::{self, TableData, TableMessage, TableSettings};
use ca_view_table::session_options::options_of;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Two comma separated files whose only differences are capitalization, extra
/// blanks and a small numeric change.
fn write_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("left.csv");
    let right = dir.join("right.csv");
    std::fs::write(&left, "name,size,note\nalpha,10,first\nbeta,20,second\n").unwrap();
    std::fs::write(&right, "name,size,note\nALPHA, 10 ,first\nbeta,21,second\n").unwrap();
    (left, right)
}

fn run(left: &Path, right: &Path, settings: TableSettings) -> TableData {
    let mut job = jobs::spawn_load(
        left.to_path_buf(),
        right.to_path_buf(),
        settings,
        Arc::new(|| {}),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        for message in job.drain() {
            match message {
                TableMessage::Ready(data) => return *data,
                TableMessage::Failed(reason) => panic!("the comparison failed: {reason}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "the comparison did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn different_cells(data: &TableData) -> u64 {
    data.comparison.totals.cells_different
}

#[test]
fn the_inherited_column_treatment_changes_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = write_pair(dir.path());
    let strict = run(&left, &right, options_of(&TableCompareSettings::default()));

    let mut lenient = TableCompareSettings::default();
    lenient.columns.default_handling.ignore_character_case = true;
    lenient.columns.default_handling.ignore_whitespace = true;
    lenient.columns.default_handling.numeric_tolerance = 5.0;
    let relaxed = run(&left, &right, options_of(&lenient));

    assert!(
        different_cells(&relaxed) < different_cells(&strict),
        "a wider treatment finds fewer important differences"
    );
}

#[test]
fn a_per_column_treatment_changes_only_the_column_it_names() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = write_pair(dir.path());
    let strict = run(&left, &right, options_of(&TableCompareSettings::default()));

    let mut named = TableCompareSettings::default();
    named.columns.per_column = vec![StoredHandling {
        column: Some(1),
        use_default: false,
        numeric_tolerance: 5.0,
        ..StoredHandling::default()
    }];
    let relaxed = run(&left, &right, options_of(&named));
    assert!(different_cells(&relaxed) < different_cells(&strict));

    let mut elsewhere = TableCompareSettings::default();
    elsewhere.columns.per_column = vec![StoredHandling {
        column: Some(2),
        use_default: false,
        numeric_tolerance: 5.0,
        ..StoredHandling::default()
    }];
    assert_eq!(
        different_cells(&run(&left, &right, options_of(&elsewhere))),
        different_cells(&strict),
        "a tolerance on another column changes nothing"
    );
}

#[test]
fn an_explicit_pair_list_decides_which_columns_are_compared() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = write_pair(dir.path());

    let mut paired = TableCompareSettings::default();
    paired.columns.pairing = TablePairing::Custom {
        pairs: vec![(Some(0), Some(2)), (Some(2), None)],
        unknown: std::collections::BTreeMap::new(),
    };
    let built = run(&left, &right, options_of(&paired));
    assert_eq!(built.schema.columns.len(), 2);
    assert_eq!(built.schema.columns[0].left, Some(0));
    assert_eq!(built.schema.columns[0].right, Some(2));
    assert_eq!(built.schema.columns[1].right, None);

    let plain = run(&left, &right, options_of(&TableCompareSettings::default()));
    assert_ne!(plain.schema.columns.len(), built.schema.columns.len());
}

#[test]
fn a_named_format_decides_how_the_lines_are_split() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = write_pair(dir.path());

    let mut pinned = TableCompareSettings::default();
    let bars = FileFormatChoice::Named {
        name: "bar-separated".to_owned(),
        unknown: std::collections::BTreeMap::new(),
    };
    pinned.format.left_format = bars.clone();
    pinned.format.right_format = bars;
    let split_on_bars = run(&left, &right, options_of(&pinned));
    let detected = run(&left, &right, options_of(&TableCompareSettings::default()));
    assert_eq!(
        split_on_bars.schema.columns.len(),
        1,
        "no line holds a bar, so every line is one cell"
    );
    assert!(detected.schema.columns.len() > 1);
}

#[test]
fn a_named_encoding_decides_how_the_bytes_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.csv");
    let right = dir.path().join("right.csv");
    // Automatic detection reads this as Windows-1252. Pinning UTF-8 must
    // change the encoding even though the same bytes are also valid input to
    // the single byte code page.
    std::fs::write(&left, b"name\nvalue\n").unwrap();
    std::fs::write(&right, b"name\n\xE9\n").unwrap();

    let mut pinned = TableCompareSettings::default();
    pinned.format.right_encoding = EncodingChoice::Named {
        name: "utf-8".to_owned(),
        unknown: std::collections::BTreeMap::new(),
    };
    let as_code_page = run(&left, &right, options_of(&pinned));
    let detected = run(&left, &right, options_of(&TableCompareSettings::default()));
    assert_ne!(
        as_code_page.right.facts.encoding, detected.right.facts.encoding,
        "the pinned side is decoded with the encoding the settings name"
    );
}

#[test]
fn the_row_pairing_changes_how_the_rows_line_up() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.csv");
    let right = dir.path().join("right.csv");
    std::fs::write(&left, "a\nb\nc\nd\n").unwrap();
    std::fs::write(&right, "a\nx\nb\nc\nd\n").unwrap();

    let mut positional = TableCompareSettings::default();
    positional.rows.algorithm = ca_session::settings::common::AlignmentAlgorithm::Unaligned;
    let unaligned = run(&left, &right, options_of(&positional));
    let aligned = run(&left, &right, options_of(&TableCompareSettings::default()));
    assert_ne!(
        unaligned.comparison.totals.same, aligned.comparison.totals.same,
        "a pass that finds the inserted row does not produce the positional pairing"
    );
}

#[test]
fn a_toolbar_command_and_the_settings_page_read_one_state() {
    let _settings_dir = ca_ui::testing::isolate_settings();
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = write_pair(dir.path());
    let context = ca_ui::testing::context();
    let mut view = ca_view_table::TableView::new(left, right, &context, 0);

    let mut wanted = TableCompareSettings::default();
    wanted.rows.algorithm = ca_session::settings::common::AlignmentAlgorithm::Patience;
    wanted.rows.skew_tolerance = 17;
    wanted.columns.default_handling.ignore_character_case = true;
    ca_ui::view::SessionView::apply_settings(
        &mut view,
        &ca_session::settings::SessionSettings::TableCompare(wanted),
    );

    let Some(ca_session::settings::SessionSettings::TableCompare(back)) =
        ca_ui::view::SessionView::settings(&view)
    else {
        panic!("the view answers for its own session kind");
    };
    assert_eq!(
        back.rows.algorithm,
        ca_session::settings::common::AlignmentAlgorithm::Patience
    );
    assert_eq!(back.rows.skew_tolerance, 17);
    assert!(back.columns.default_handling.ignore_character_case);

    view.set_key_column(1, true);
    view.set_sort_rows_before_alignment(true);
    let Some(ca_session::settings::SessionSettings::TableCompare(after)) =
        ca_ui::view::SessionView::settings(&view)
    else {
        panic!("the view answers for its own session kind");
    };
    assert!(after.rows.sort_before_alignment);
    assert!(after
        .columns
        .per_column
        .iter()
        .any(|entry| entry.column == Some(1) && entry.key));
}
