//! Every stored byte comparison setting changes what a comparison produces,
//! and the toolbar edits the same stored values the settings page does.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::settings::binary::{HexAlignment, HexCharEncoding, HexCompareSettings};
use ca_ui::testing::{context, isolate_settings};
use ca_ui::view::SessionView;
use ca_view_hex::chars::CharEncoding;
use ca_view_hex::jobs::{self, HexData, HexMessage, SidePayload};
use ca_view_hex::model::Side;
use ca_view_hex::HexView;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Two sides whose difference falls in the middle, so an insertion exists for
/// a pairing pass to find.
fn pair() -> (SidePayload, SidePayload) {
    let left: Vec<u8> = (0u8..64).collect();
    let mut right = left.clone();
    right.splice(32..32, [0xAA, 0xBB, 0xCC, 0xDD]);
    (
        SidePayload::from_bytes(left),
        SidePayload::from_bytes(right),
    )
}

/// Runs one comparison to completion and returns what it produced.
fn compare(settings: &HexCompareSettings) -> HexData {
    let (left, right) = pair();
    let options = ca_view_hex::settings::options_of(settings);
    let mut job = jobs::spawn_bytes(left, right, options, Arc::new(|| {}));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        for message in job.drain() {
            if let HexMessage::Ready(data) = message {
                return *data;
            }
        }
        assert!(Instant::now() < deadline, "the comparison did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The bytes of every row, in row order, which is what the panes draw.
fn row_widths(data: &HexData) -> Vec<u32> {
    let mut widths = Vec::new();
    let mut index = 0;
    while let Some(row) = data.model.row(index) {
        let left = row.left.map_or(0, |span| span.len);
        let right = row.right.map_or(0, |span| span.len);
        widths.push(left.max(right));
        index += 1;
    }
    widths
}

#[test]
fn the_row_width_changes_the_layout() {
    let mut narrow = HexCompareSettings::default();
    narrow.comparison.bytes_per_row = 8;
    let mut wide = HexCompareSettings::default();
    wide.comparison.bytes_per_row = 32;
    let narrow_rows = row_widths(&compare(&narrow));
    let wide_rows = row_widths(&compare(&wide));
    assert!(narrow_rows.len() > wide_rows.len());
    assert!(narrow_rows.iter().all(|width| *width <= 8));
    assert!(wide_rows.iter().any(|width| *width > 8));
}

#[test]
fn the_pairing_rule_changes_what_the_comparison_finds() {
    let mut complete = HexCompareSettings::default();
    complete.comparison.alignment = HexAlignment::Complete;
    let mut none = HexCompareSettings::default();
    none.comparison.alignment = HexAlignment::None;
    let paired = compare(&complete);
    let positional = compare(&none);
    assert_eq!(paired.alignment, ca_diff::ByteAlignment::Complete);
    assert_eq!(positional.alignment, ca_diff::ByteAlignment::None);
    assert_ne!(
        paired.model.hunks(),
        positional.model.hunks(),
        "a pass that finds the insertion does not produce the positional layout"
    );
}

#[test]
fn the_character_encoding_changes_what_the_character_area_shows() {
    let mut ansi = HexCompareSettings::default();
    ansi.comparison.char_encoding = HexCharEncoding::Ansi;
    let mut ascii = HexCompareSettings::default();
    ascii.comparison.char_encoding = HexCharEncoding::Ascii;
    let upper = 0xE9_u8;
    assert_ne!(
        ca_view_hex::settings::char_encoding(&ansi.comparison.char_encoding).character(upper),
        ca_view_hex::settings::char_encoding(&ascii.comparison.char_encoding).character(upper)
    );
    assert_eq!(
        ca_view_hex::settings::char_encoding(&HexCharEncoding::Unknown(serde_json::json!(
            "future"
        ))),
        CharEncoding::Ansi,
        "an encoding this build has no table for still opens"
    );
}

/// A saved session applies its settings right after the view opens, while
/// the first read of the files still runs.
#[test]
fn settings_applied_before_the_first_read_lands_keep_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.bin");
    let right = dir.path().join("right.bin");
    std::fs::write(&left, vec![7u8; 4096]).unwrap();
    let mut changed = vec![7u8; 4096];
    changed[100] = 9;
    std::fs::write(&right, &changed).unwrap();

    let mut view = HexView::new(left, right, &context(), 1);
    let mut wanted = HexCompareSettings::default();
    wanted.comparison.bytes_per_row = 8;
    wanted.comparison.alignment = HexAlignment::None;
    view.apply_session_settings(&wanted);

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        view.tick();
        if view.is_ready() {
            break;
        }
        assert!(Instant::now() < deadline, "the comparison did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(view.buffer(Side::Left).len(), 4096);
    assert_eq!(view.buffer(Side::Right).len(), 4096);
    assert_eq!(view.bytes_per_row(), 8);
    assert!(view.model().row_count() >= 4096 / 8);
    let Some(ca_session::settings::SessionSettings::HexCompare(read_back)) = view.settings() else {
        panic!("the view answers for its own session kind");
    };
    assert_eq!(read_back.comparison.alignment, HexAlignment::None);
}

#[test]
fn the_view_reads_and_writes_one_set_of_stored_values() {
    let _settings_dir = isolate_settings();
    let context = context();
    let mut view = HexView::from_data(
        PathBuf::from("left.bin"),
        PathBuf::from("right.bin"),
        &context,
        0,
        compare(&HexCompareSettings::default()),
    );

    let mut wanted = HexCompareSettings::default();
    wanted.comparison.alignment = HexAlignment::Fast;
    wanted.comparison.bytes_per_row = 24;
    wanted.comparison.char_encoding = HexCharEncoding::Ascii;
    view.apply_settings(&ca_session::settings::SessionSettings::HexCompare(
        wanted.clone(),
    ));

    let Some(ca_session::settings::SessionSettings::HexCompare(read_back)) = view.settings() else {
        panic!("the view answers for its own session kind");
    };
    assert_eq!(read_back.comparison.alignment, HexAlignment::Fast);
    assert_eq!(read_back.comparison.bytes_per_row, 24);
    assert_eq!(read_back.comparison.char_encoding, HexCharEncoding::Ascii);
}
