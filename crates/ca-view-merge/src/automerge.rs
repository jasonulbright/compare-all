//! A merge that runs with no window, for the command line.
//!
//! The steps are the ones the view takes: read and merge on the calling
//! thread, apply the switches, then write the output through the same save the
//! view uses. The caller learns the exit code, or that the conflicts that
//! remain need the window.

use crate::jobs::{self, MergeDecoding, MergePaths};
use crate::model::{DisplayRules, Pane};
use crate::output::{self, MarkerLabels, Outcome, EXIT_CONFLICTS_NOT_WRITTEN, EXIT_ERROR};
use crate::settings;
use ca_session::settings::TextMergeSettings;
use ca_ui::save::text::SaveConsent;
use ca_ui::save::{RealFileSystem, SaveOutcome};
use ca_ui::worker::Cancel;
use std::path::Path;

/// The command line switches that change an automatic merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Switches {
    /// Resolve every conflict that remains from the left version.
    pub favor_left: bool,
    /// Resolve every conflict that remains from the right version.
    pub favor_right: bool,
    /// Write the output with conflict markers rather than refuse to write it.
    pub force: bool,
    /// Count a conflict whose changes are all unimportant as no conflict.
    pub ignore_unimportant: bool,
    /// Open the merge window when conflicts remain.
    pub review_conflicts: bool,
}

/// How an automatic merge ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The merge is over; the process exits with `code`.
    Done {
        /// The documented exit code.
        code: i32,
        /// What happened, for a caller that has somewhere to show it.
        message: String,
    },
    /// Conflicts remain and the switches ask for the window.
    Review,
}

impl Finish {
    fn done(code: i32, message: impl Into<String>) -> Self {
        Self::Done {
            code,
            message: message.into(),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Merge the files `paths` names under `stored` settings and `switches`.
///
/// This reads and writes files on the calling thread, so it belongs to a
/// process that has no window yet.
#[must_use]
pub fn run(paths: &MergePaths, stored: &TextMergeSettings, switches: Switches) -> Finish {
    let Some(target) = paths.output.clone() else {
        return Finish::done(EXIT_ERROR, "An automatic merge needs an output file.");
    };
    let options = settings::options_from(stored);
    let decoding = MergeDecoding {
        left: settings::decode_from(&stored.format.left_encoding),
        right: settings::decode_from(&stored.format.right_encoding),
    };
    let data = match jobs::read_and_merge(paths, &options, &decoding, &Cancel::new(), &|_| {}) {
        Ok(Some(data)) => data,
        Ok(None) => return Finish::done(EXIT_ERROR, "The merge stopped before it finished."),
        Err(reason) => return Finish::done(EXIT_ERROR, reason),
    };
    let mut model = data.model;
    model.set_rules(DisplayRules {
        ignore_unimportant: switches.ignore_unimportant,
        ignore_same_changes: false,
        ..DisplayRules::default()
    });
    if switches.favor_left {
        model.favor(Pane::Left);
    } else if switches.favor_right {
        model.favor(Pane::Right);
    }
    let remaining = model.totals().conflicts_remaining;
    if remaining > 0 && switches.review_conflicts {
        return Finish::Review;
    }
    if remaining > 0 && !switches.force {
        return Finish::done(
            EXIT_CONFLICTS_NOT_WRITTEN,
            format!("{remaining} conflict(s) remain. The output was not written."),
        );
    }
    let markers = (remaining > 0).then(|| MarkerLabels {
        left: file_name(&paths.left),
        center: paths
            .center
            .as_deref()
            .map_or_else(|| "center".to_owned(), file_name),
        right: file_name(&paths.right),
    });
    if output::contains_lossy_input(&model, &data.sources) {
        return Finish::done(
            EXIT_ERROR,
            "The output includes replacement characters from an input that did not decode cleanly. It was not written.",
        );
    }
    let outcome = output::save(
        &RealFileSystem,
        &target,
        &model,
        &data.sources.left,
        markers.as_ref(),
        data.output_baseline,
        SaveConsent::default(),
    );
    let written = |message: &str| {
        Finish::done(
            output::exit_code(Outcome {
                conflicts_remaining: remaining,
                written: true,
                failed: false,
            }),
            message,
        )
    };
    match outcome {
        SaveOutcome::Saved(_) => written("The output was written."),
        SaveOutcome::SavedWithConflict { backup, .. } => written(&format!(
            "The output was written. Another version was kept at {}.",
            backup.display()
        )),
        SaveOutcome::ChangedOnDisk => Finish::done(
            EXIT_ERROR,
            "The output file changed on disk during the merge. It was not written.",
        ),
        SaveOutcome::WouldLose(reason) => Finish::done(EXIT_ERROR, reason),
        SaveOutcome::NotWritable => Finish::done(EXIT_ERROR, "The output file cannot be replaced."),
        SaveOutcome::Failed(reason) => {
            Finish::done(EXIT_ERROR, format!("The output was not written: {reason}"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{run, Finish, Switches};
    use crate::jobs::MergePaths;
    use crate::output::{EXIT_CONFLICTS, EXIT_CONFLICTS_NOT_WRITTEN, EXIT_ERROR, EXIT_SUCCESS};
    use ca_session::settings::TextMergeSettings;
    use std::path::Path;

    fn paths(dir: &Path, left: &str, center: &str, right: &str) -> MergePaths {
        let write = |name: &str, text: &str| {
            let path = dir.join(name);
            std::fs::write(&path, text.as_bytes()).unwrap();
            path
        };
        MergePaths {
            left: write("left.txt", left),
            center: Some(write("center.txt", center)),
            right: write("right.txt", right),
            output: Some(dir.join("out.txt")),
        }
    }

    fn code(finish: &Finish) -> i32 {
        match finish {
            Finish::Done { code, .. } => *code,
            Finish::Review => panic!("the merge asked for the window"),
        }
    }

    const CONFLICT: (&str, &str, &str) = ("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");

    #[test]
    fn a_clean_merge_writes_the_output_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(
            dir.path(),
            "A\nb\nc\nd\ne\nf\n",
            "a\nb\nc\nd\ne\nf\n",
            "a\nb\nc\nd\ne\nF\n",
        );
        let finish = run(&paths, &TextMergeSettings::default(), Switches::default());
        assert_eq!(code(&finish), EXIT_SUCCESS);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "A\nb\nc\nd\ne\nF\n"
        );
    }

    #[test]
    fn a_conflict_without_force_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (left, center, right) = CONFLICT;
        let paths = paths(dir.path(), left, center, right);
        let finish = run(&paths, &TextMergeSettings::default(), Switches::default());
        assert_eq!(code(&finish), EXIT_CONFLICTS_NOT_WRITTEN);
        assert!(!dir.path().join("out.txt").exists());
    }

    #[test]
    fn force_writes_markers_and_reports_the_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let (left, center, right) = CONFLICT;
        let paths = paths(dir.path(), left, center, right);
        let switches = Switches {
            force: true,
            ..Switches::default()
        };
        let finish = run(&paths, &TextMergeSettings::default(), switches);
        assert_eq!(code(&finish), EXIT_CONFLICTS);
        let written = std::fs::read_to_string(dir.path().join("out.txt")).unwrap();
        assert!(written.contains("<<<<<<< left.txt"), "{written}");
        assert!(written.contains(">>>>>>> right.txt"), "{written}");
    }

    #[test]
    fn an_automatic_merge_keeps_an_unterminated_line_apart_from_an_added_line() {
        for (ending, written) in [("\n", "a\nb\nc\n"), ("\r", "a\rb\rc\r")] {
            let dir = tempfile::tempdir().unwrap();
            let base = format!("a{ending}b");
            let right = format!("a{ending}b{ending}c{ending}");
            let paths = paths(dir.path(), &base, &base, &right);
            let finish = run(&paths, &TextMergeSettings::default(), Switches::default());
            assert_eq!(code(&finish), EXIT_SUCCESS);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
                written
            );
        }
    }

    #[test]
    fn force_writes_each_marker_on_a_line_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path(), "a\nb\nL\n", "a\nb", "a\nb\nR\n");
        let switches = Switches {
            force: true,
            ..Switches::default()
        };
        let finish = run(&paths, &TextMergeSettings::default(), switches);
        assert_eq!(code(&finish), EXIT_CONFLICTS);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "a\nb\n<<<<<<< left.txt\nL\n||||||| center.txt\n=======\nR\n>>>>>>> right.txt\n"
        );
    }

    #[test]
    fn favoring_a_side_resolves_the_conflicts_from_it() {
        for (switches, expected) in [
            (
                Switches {
                    favor_left: true,
                    ..Switches::default()
                },
                "a\nL\nc\n",
            ),
            (
                Switches {
                    favor_right: true,
                    ..Switches::default()
                },
                "a\nR\nc\n",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (left, center, right) = CONFLICT;
            let paths = paths(dir.path(), left, center, right);
            let finish = run(&paths, &TextMergeSettings::default(), switches);
            assert_eq!(code(&finish), EXIT_SUCCESS);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn review_conflicts_asks_for_the_window_only_when_conflicts_remain() {
        let dir = tempfile::tempdir().unwrap();
        let (left, center, right) = CONFLICT;
        let conflicted = paths(dir.path(), left, center, right);
        let switches = Switches {
            review_conflicts: true,
            ..Switches::default()
        };
        assert_eq!(
            run(&conflicted, &TextMergeSettings::default(), switches),
            Finish::Review
        );
        assert!(!dir.path().join("out.txt").exists());
        let clean = tempfile::tempdir().unwrap();
        let clean_paths = paths(clean.path(), "A\nb\n", "a\nb\n", "a\nb\n");
        let finish = run(&clean_paths, &TextMergeSettings::default(), switches);
        assert_eq!(code(&finish), EXIT_SUCCESS);
    }

    /// Two changes that differ only in trailing whitespace conflict, and
    /// ignoring unimportant differences takes them out of the conflict count.
    #[test]
    fn ignoring_unimportant_differences_merges_a_whitespace_only_conflict() {
        let mut settings = TextMergeSettings::default();
        settings.importance.trailing_whitespace_important = false;
        let dir = tempfile::tempdir().unwrap();
        let conflicted = paths(dir.path(), "a\nb \nc\n", "a\nb\nc\n", "a\nb  \nc\n");
        assert_eq!(
            code(&run(&conflicted, &settings, Switches::default())),
            EXIT_CONFLICTS_NOT_WRITTEN
        );
        let switches = Switches {
            ignore_unimportant: true,
            ..Switches::default()
        };
        assert_eq!(code(&run(&conflicted, &settings, switches)), EXIT_SUCCESS);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "a\nb\nc\n"
        );
    }

    #[test]
    fn a_merge_with_no_output_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut named = paths(dir.path(), "a\n", "a\n", "a\n");
        named.output = None;
        assert_eq!(
            code(&run(
                &named,
                &TextMergeSettings::default(),
                Switches::default()
            )),
            EXIT_ERROR
        );
    }
}
