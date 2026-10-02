//! Every stored image comparison setting changes what a comparison produces,
//! and the view reports the same stored values a settings page edits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_session::settings::binary::{PictureCompareSettings, PictureDisplayMode, PictureSide};
use ca_session::settings::common::ReplacementItem;
use ca_ui::testing::isolate_settings;
use ca_ui::view::SessionView;
use ca_view_picture::jobs::{self, Message, Outcome, Settings};
use ca_view_picture::settings::options_of;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn run(left: &Path, right: &Path, settings: Settings) -> Outcome {
    let mut job = jobs::spawn_load(
        left.to_path_buf(),
        right.to_path_buf(),
        settings,
        Arc::new(|| {}),
    );
    let deadline = Instant::now() + common::BUDGET;
    loop {
        for message in job.drain() {
            match message {
                Message::Ready(outcome) => return *outcome,
                Message::Failed(text) | Message::SideFailed(_, text) => {
                    panic!("the comparison failed: {text}")
                }
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "the comparison did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Two images differing by a small amount in every pixel, so a tolerance
/// decides whether the difference matters.
fn near_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("near-left.png");
    let right = dir.join("near-right.png");
    common::write_png(&left, 8, 6, [100, 100, 100, 255], None);
    common::write_png(&right, 8, 6, [110, 100, 100, 255], None);
    (left, right)
}

/// Two images of different sizes, so bringing them to one scale changes the
/// result.
fn sized_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("large.png");
    let right = dir.join("small.png");
    common::write_png(&left, 16, 12, [10, 20, 30, 255], None);
    common::write_png(&right, 8, 6, [10, 20, 30, 255], None);
    (left, right)
}

#[test]
fn the_tolerance_decides_whether_a_difference_matters() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = near_pair(dir.path());

    let strict = run(
        &left,
        &right,
        options_of(&PictureCompareSettings::default()),
    );
    let mut lenient = PictureCompareSettings::default();
    lenient.comparison.tolerance = 32;
    let relaxed = run(&left, &right, options_of(&lenient));

    assert!(strict.totals.different > 0);
    assert_eq!(relaxed.totals.different, 0);
    assert!(relaxed.totals.similar > 0);
}

#[test]
fn the_display_mode_changes_the_pixels_the_pane_draws() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());

    let tolerance = run(
        &left,
        &right,
        options_of(&PictureCompareSettings::default()),
    );
    let mut blended = PictureCompareSettings::default();
    blended.comparison.display_mode = PictureDisplayMode::Blend;
    blended.comparison.blend_percent = 25;
    let blend = run(&left, &right, options_of(&blended));
    assert_ne!(tolerance.result, blend.result);

    let mut single = PictureCompareSettings::default();
    single.comparison.display_mode = PictureDisplayMode::SingleSide;
    single.comparison.single_side = PictureSide::Right;
    let right_only = run(&left, &right, options_of(&single));
    let mut single_left = single.clone();
    single_left.comparison.single_side = PictureSide::Left;
    let left_only = run(&left, &right, options_of(&single_left));
    assert_ne!(right_only.result, left_only.result);
}

#[test]
fn ignoring_unimportant_differences_changes_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = near_pair(dir.path());

    let mut counted = PictureCompareSettings::default();
    counted.comparison.tolerance = 32;
    let mut ignored = counted.clone();
    ignored.comparison.ignore_unimportant = true;

    let with = run(&left, &right, options_of(&counted));
    let without = run(&left, &right, options_of(&ignored));
    assert!(with.totals.similar > 0);
    assert_ne!(
        with.result, without.result,
        "a difference held to be unimportant renders as a match"
    );
}

#[test]
fn bringing_the_two_to_one_scale_changes_the_result_size() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = sized_pair(dir.path());

    let plain = run(
        &left,
        &right,
        options_of(&PictureCompareSettings::default()),
    );
    let mut scaled = PictureCompareSettings::default();
    scaled.comparison.auto_scale = true;
    let enlarged = run(&left, &right, options_of(&scaled));

    assert!(plain.totals.right_only > 0 || plain.totals.left_only > 0);
    assert_eq!(enlarged.totals.left_only, 0);
    assert_eq!(enlarged.totals.right_only, 0);
}

#[test]
fn a_turn_changes_which_pixels_are_compared() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("marked.png");
    let right = dir.path().join("plain.png");
    common::write_png(&left, 8, 8, [10, 20, 30, 255], Some((1, 6)));
    common::write_png(&right, 8, 8, [10, 20, 30, 255], Some((1, 6)));

    let plain = run(
        &left,
        &right,
        options_of(&PictureCompareSettings::default()),
    );
    let mut turned = PictureCompareSettings::default();
    turned.comparison.left_quarter_turns = 1;
    let rotated = run(&left, &right, options_of(&turned));
    assert_eq!(plain.totals.different, 0);
    assert!(rotated.totals.different > 0);

    let mut flipped = PictureCompareSettings::default();
    flipped.comparison.right_flip_horizontal = true;
    assert!(run(&left, &right, options_of(&flipped)).totals.different > 0);
}

#[test]
fn a_color_substitution_makes_a_difference_unimportant() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("orange.png");
    let right = dir.path().join("green.png");
    common::write_png(&left, 4, 4, [255, 165, 0, 255], None);
    common::write_png(&right, 4, 4, [0, 128, 0, 255], None);

    let plain = run(
        &left,
        &right,
        options_of(&PictureCompareSettings::default()),
    );
    assert!(plain.totals.different > 0);

    let mut declared = PictureCompareSettings::default();
    declared.replacements.items = vec![ReplacementItem {
        find: "#FFA500FF".to_owned(),
        replace_with: "#008000FF".to_owned(),
        ..ReplacementItem::default()
    }];
    let covered = run(&left, &right, options_of(&declared));
    assert_eq!(covered.totals.different, 0);
    assert!(covered.totals.similar > 0);
}

#[test]
fn the_alpha_toggles_decide_what_a_transparent_pixel_means() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("clear-left.png");
    let right = dir.path().join("clear-right.png");
    common::write_png(&left, 4, 4, [255, 0, 0, 0], None);
    common::write_png(&right, 4, 4, [0, 0, 255, 0], None);

    let mut equal = PictureCompareSettings::default();
    equal.comparison.transparent_pixels_equal = true;
    assert_eq!(run(&left, &right, options_of(&equal)).totals.different, 0);

    let mut counted = PictureCompareSettings::default();
    counted.comparison.transparent_pixels_equal = false;
    assert!(run(&left, &right, options_of(&counted)).totals.different > 0);

    let opaque_left = dir.path().join("opaque-left.png");
    let opaque_right = dir.path().join("opaque-right.png");
    common::write_png(&opaque_left, 4, 4, [10, 20, 30, 255], None);
    common::write_png(&opaque_right, 4, 4, [10, 20, 30, 128], None);
    let mut alpha = PictureCompareSettings::default();
    alpha.comparison.transparent_pixels_equal = false;
    assert!(
        run(&opaque_left, &opaque_right, options_of(&alpha))
            .totals
            .different
            > 0
    );
    alpha.comparison.ignore_alpha = true;
    assert_eq!(
        run(&opaque_left, &opaque_right, options_of(&alpha))
            .totals
            .different,
        0
    );
}

#[test]
fn the_view_reads_and_writes_one_set_of_stored_values() {
    let _settings_dir = isolate_settings();
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let context = ca_ui::testing::context();
    let mut view = ca_view_picture::PictureView::new(left, right, &context, 0);

    let mut wanted = PictureCompareSettings::default();
    wanted.comparison.display_mode = PictureDisplayMode::MismatchRange;
    wanted.comparison.tolerance = 9;
    wanted.comparison.ignore_unimportant = true;
    wanted.comparison.ignore_alpha = true;
    wanted.comparison.transparent_pixels_equal = false;
    wanted.comparison.auto_scale = true;
    wanted.comparison.single_side = PictureSide::Right;
    wanted.comparison.left_quarter_turns = 3;
    wanted.comparison.right_flip_vertical = true;
    view.apply_settings(&ca_session::settings::SessionSettings::PictureCompare(
        wanted.clone(),
    ));

    let Some(ca_session::settings::SessionSettings::PictureCompare(back)) =
        SessionView::settings(&view)
    else {
        panic!("the view answers for its own session kind");
    };
    assert_eq!(back.comparison, wanted.comparison);

    // A toolbar command edits the same stored values the dialog shows.
    view.rotate(ca_image::compare::Side::Left, true);
    view.set_auto_scale(false);
    let Some(ca_session::settings::SessionSettings::PictureCompare(after)) =
        SessionView::settings(&view)
    else {
        panic!("the view answers for its own session kind");
    };
    assert_eq!(after.comparison.left_quarter_turns, 0);
    assert!(!after.comparison.auto_scale);
}
