//! Every field of the merge settings changes a merge result.
//!
//! Each test sets one field, re-runs the merge over a small fixture written to
//! a temporary folder, and asserts a result the default settings do not
//! produce.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::settings::SessionSettings;
use ca_ui::testing::{context, sized_input};
use ca_ui::view::{SessionView, Titles};
use ca_view_merge::jobs::MergePaths;
use ca_view_merge::MergeView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WINDOW: [f32; 2] = [1_280.0, 800.0];

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

struct Session {
    view: MergeView,
    ctx: egui::Context,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}

impl Session {
    fn open(left: &[u8], center: &[u8], right: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", left),
            center: Some(write(dir.path(), "center.txt", center)),
            right: write(dir.path(), "right.txt", right),
            output: Some(dir.path().join("merged.txt")),
        };
        let mut session = Self {
            view: MergeView::over(paths, Titles::default(), &context(), 11),
            ctx: egui::Context::default(),
            dir,
        };
        session.settle();
        session
    }

    fn text(left: &str, center: &str, right: &str) -> Self {
        Self::open(left.as_bytes(), center.as_bytes(), right.as_bytes())
    }

    fn frame(&mut self) {
        self.view.tick();
        let view = &mut self.view;
        let held = context();
        let _ = self.ctx.run(sized_input(WINDOW[0], WINDOW[1]), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &held);
            });
        });
    }

    /// Paint frames until the merge is ready again, which is how the test
    /// waits for the worker rather than for a clock.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.frame();
            if self.view.is_ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the merge never became ready");
    }

    fn settings(&self) -> ca_session::settings::TextMergeSettings {
        let Some(SessionSettings::TextMerge(merge)) = self.view.settings() else {
            panic!("the view reports another kind");
        };
        merge
    }

    fn apply(&mut self, settings: ca_session::settings::TextMergeSettings) {
        self.view
            .apply_settings(&SessionSettings::TextMerge(settings));
        self.settle();
    }

    fn conflicts(&self) -> u32 {
        self.view.model().totals().conflicts
    }
}

/// Two opposing changes two lines apart are one conflict under the stored
/// separation and two separate changes without it.
#[test]
fn the_conflict_separation_changes_the_merge() {
    let mut session = Session::text("A\nb\nc\nd\ne\n", "a\nb\nc\nd\ne\n", "a\nb\nC\nd\ne\n");
    let mut settings = session.settings();
    settings.conflicts.separation_lines = 4;
    session.apply(settings.clone());
    let joined = session.conflicts();

    settings.conflicts.separation_lines = 0;
    session.apply(settings);
    assert_ne!(
        joined,
        session.conflicts(),
        "the separation distance never reached the merge engine"
    );
}

#[test]
fn asking_for_changed_lines_only_changes_the_merge() {
    let mut session = Session::text("A\nb\nc\nd\ne\n", "a\nb\nc\nd\ne\n", "a\nb\nC\nd\ne\n");
    let mut settings = session.settings();
    settings.conflicts.separation_lines = 4;
    session.apply(settings.clone());
    let joined = session.conflicts();

    settings.conflicts.same_lines_only = true;
    session.apply(settings);
    assert_ne!(joined, session.conflicts());
}

#[test]
fn comparing_line_endings_changes_the_merge() {
    let mut session = Session::open(b"a\r\nb\r\n", b"a\nb\n", b"a\nb\n");
    let before = session.view.model().totals();
    let mut settings = session.settings();
    settings.importance.compare_line_endings = true;
    session.apply(settings);
    assert_ne!(
        before,
        session.view.model().totals(),
        "the line ending test never reached the merge engine"
    );
}

#[test]
fn an_alignment_algorithm_reaches_the_merge() {
    let mut session = Session::text("a\nb\nc\nd\n", "b\nc\nd\ne\n", "b\nc\nd\nf\n");
    let before = session.view.model().totals();
    let mut settings = session.settings();
    settings.alignment.algorithm = ca_session::settings::common::AlignmentAlgorithm::Unaligned;
    session.apply(settings);
    assert_ne!(before, session.view.model().totals());
}

#[test]
fn never_aligning_differences_reaches_the_merge() {
    let mut session = Session::text(
        "same\nalpha\nsame\n",
        "same\nbase\nsame\n",
        "same\nbase\nsame\n",
    );
    let before = session.view.model().rows().len();
    let mut settings = session.settings();
    settings.alignment.never_align_differences = true;
    session.apply(settings);
    assert_ne!(before, session.view.model().rows().len());
}

/// A pinned encoding decodes bytes the detector reads another way, so the
/// merge sees a different left version.
#[test]
fn a_pinned_encoding_reaches_the_merge_loader() {
    let utf16: Vec<u8> = "one\ntwo\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut session = Session::open(&utf16, b"one\ntwo\n", b"one\ntwo\n");
    let before = session.view.model().totals();
    let mut settings = session.settings();
    settings.format.left_encoding = ca_session::settings::common::EncodingChoice::Named {
        name: "UTF-8".to_owned(),
        unknown: std::collections::BTreeMap::new(),
    };
    session.apply(settings);
    assert_ne!(
        before,
        session.view.model().totals(),
        "the pinned encoding never reached the decoder"
    );
}

type ImportanceEdit = fn(&mut ca_session::settings::text::TextImportanceSettings);

/// Each importance entry the merge page offers changes which differences
/// Ignore Unimportant Differences takes out of the count.
#[test]
fn every_importance_entry_changes_what_ignore_unimportant_hides() {
    let table: [(&str, &str, ImportanceEdit, ImportanceEdit); 6] = [
        (
            "  x\n",
            "x\n",
            |_| {},
            |importance| importance.leading_whitespace_important = false,
        ),
        (
            "a  b\n",
            "a b\n",
            |_| {},
            |importance| importance.embedded_whitespace_important = false,
        ),
        (
            "x  \n",
            "x\n",
            |_| {},
            |importance| importance.trailing_whitespace_important = false,
        ),
        (
            "y\n",
            "x\n",
            |_| {},
            |importance| importance.everything_else_important = false,
        ),
        (
            "X\n",
            "x\n",
            |_| {},
            |importance| importance.character_case_important = true,
        ),
        (
            "x\n  \n",
            "x\n",
            |importance| {
                importance.leading_whitespace_important = false;
                importance.trailing_whitespace_important = false;
            },
            |importance| importance.orphan_lines_always_important = false,
        ),
    ];
    for (left, center, base, flip) in table {
        let mut session = Session::text(left, center, center);
        session
            .view
            .run(ca_ui::command::Command::ToggleIgnoreUnimportant);
        let mut settings = session.settings();
        base(&mut settings.importance);
        session.apply(settings.clone());
        let before = session.view.model().totals().differences;
        flip(&mut settings.importance);
        session.apply(settings);
        assert_ne!(
            before,
            session.view.model().totals().differences,
            "an importance entry never reached the merge for {left:?}"
        );
    }
}

/// Clear Session hands the view the default settings of its kind; the merge
/// runs again under them over the files already open, and the settings the
/// view reports name those files.
#[test]
fn clearing_the_session_returns_the_defaults_and_keeps_the_files() {
    let without_sides = |settings: ca_session::settings::TextMergeSettings| {
        ca_session::settings::TextMergeSettings {
            specs: ca_session::settings::SpecsSettings::default(),
            ..settings
        }
    };
    let mut session = Session::text("A\nb\nc\nd\ne\n", "a\nb\nc\nd\ne\n", "a\nb\nC\nd\ne\n");
    let left = session.view.paths().left.clone();
    let defaults = session.settings();
    let mut changed = defaults.clone();
    changed.conflicts.separation_lines = 0;
    session.apply(changed);
    let separate = session.conflicts();
    session.apply(ca_session::settings::TextMergeSettings::default());
    let cleared = session.settings();
    assert_eq!(cleared.specs.left, ca_ui::view::side_location(&left));
    assert_eq!(
        without_sides(cleared),
        ca_session::settings::TextMergeSettings::default()
    );
    assert_eq!(session.view.paths().left, left);
    assert_ne!(separate, session.conflicts());
    assert_eq!(
        without_sides(defaults),
        ca_session::settings::TextMergeSettings::default()
    );
}
