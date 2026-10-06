//! Running commands against real folder trees.
//!
//! Every test works inside its own temporary folder and turns the recycle bin
//! off, so nothing reaches a real user folder or the trash.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use ca_script::{run_script, Session, SessionOptions, Substitution};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temporary folder");
        std::fs::create_dir_all(dir.path().join("left")).expect("left");
        std::fs::create_dir_all(dir.path().join("right")).expect("right");
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn left(&self) -> PathBuf {
        self.dir.path().join("left")
    }

    fn right(&self) -> PathBuf {
        self.dir.path().join("right")
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, text).expect("write");
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(rel)).expect("read")
    }

    fn exists(&self, rel: &str) -> bool {
        self.dir.path().join(rel).exists()
    }

    fn session(&self) -> Session {
        Session::new(SessionOptions {
            journal_directory: self.dir.path().join("journals"),
            working_directory: self.dir.path().to_path_buf(),
            recycle_bin: false,
            ..SessionOptions::default()
        })
    }

    fn load_line(&self) -> String {
        format!(
            "load \"{}\" \"{}\"",
            self.left().display(),
            self.right().display()
        )
    }
}

fn run(fixture: &Fixture, session: &mut Session, body: &str) -> ca_script::Outcome {
    let source = format!("{}\n{body}\n", fixture.load_line());
    run_script(&source, &Substitution::none(), session).expect("the run finishes")
}

// -- load and select ---------------------------------------------------------

#[cfg(windows)]
#[test]
fn a_report_refuses_to_overwrite_a_target_that_cannot_be_replaced() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("report.txt", "previous report");
    let target = fixture.root().join("report.txt");
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&target)
        .unwrap();
    let mut session = fixture.session();
    let outcome = run(
        &fixture,
        &mut session,
        "folder-report layout:summary output-to:report.txt",
    );
    assert_eq!(fixture.read("report.txt"), "previous report");
    assert!(!outcome.is_clean());
    assert!(
        !std::fs::read_dir(fixture.root()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".compare-all-"))
    );
}

#[test]
fn load_opens_two_base_folders() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let mut session = fixture.session();
    let outcome = run(&fixture, &mut session, "select all");
    assert!(outcome.is_clean());
    assert_eq!(session.left.as_deref(), Some(fixture.left().as_path()));
    assert_eq!(session.selection.len(), 1);
}

#[test]
fn load_creates_a_missing_folder_when_told_to() {
    let fixture = Fixture::new();
    let target = fixture.root().join("new-right");
    let source = format!(
        "load create:right \"{}\" \"{}\"\n",
        fixture.left().display(),
        target.display()
    );
    let mut session = fixture.session();
    run_script(&source, &Substitution::none(), &mut session).expect("runs");
    assert!(target.is_dir());
}

#[test]
fn a_failing_load_ends_the_run() {
    let fixture = Fixture::new();
    let source = format!(
        "load \"{}\" \"{}\"\nbeep\n",
        fixture.left().display(),
        fixture.root().join("absent").display()
    );
    let mut session = fixture.session();
    let error =
        run_script(&source, &Substitution::none(), &mut session).expect_err("the run stops");
    assert!(matches!(error, ca_script::RunError::LoadFailed(_)));
}

#[test]
fn a_remote_address_is_not_supported_yet() {
    let fixture = Fixture::new();
    let source = format!(
        "load \"{}\" \"ftp://host/path\"\n",
        fixture.left().display()
    );
    let mut session = fixture.session();
    let error =
        run_script(&source, &Substitution::none(), &mut session).expect_err("the run stops");
    assert!(error.to_string().contains("not supported yet"), "{error}");
}

#[test]
fn a_subfolder_is_reached_only_after_expand() {
    let fixture = Fixture::new();
    fixture.write("left/sub/a.txt", "one");
    let mut session = fixture.session();
    run(&fixture, &mut session, "select files");
    assert_eq!(session.selection.len(), 0);
    run(&fixture, &mut session, "expand all\nselect files");
    assert_eq!(session.selection.len(), 1);
    run(
        &fixture,
        &mut session,
        "expand all\ncollapse all\nselect files",
    );
    assert_eq!(session.selection.len(), 0);
}

#[test]
fn select_masks_narrow_the_selection() {
    let fixture = Fixture::new();
    fixture.write("left/same.txt", "one");
    fixture.write("right/same.txt", "one");
    fixture.write("left/only-left.txt", "one");
    fixture.write("right/only-right.txt", "one");
    let mut session = fixture.session();
    run(&fixture, &mut session, "select orphan.files");
    assert_eq!(session.selection.len(), 2);
    run(&fixture, &mut session, "select left.orphan.files");
    assert_eq!(session.selection.len(), 1);
    run(&fixture, &mut session, "select exact.files");
    assert_eq!(session.selection.len(), 1);
}

// -- file operations ---------------------------------------------------------

#[test]
fn copy_writes_the_other_side() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let mut session = fixture.session();
    let outcome = run(&fixture, &mut session, "select all\ncopy left->right");
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(fixture.read("right/a.txt"), "one");
}

#[test]
fn move_removes_the_source() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let mut session = fixture.session();
    run(&fixture, &mut session, "select all\nmove left->right");
    assert!(!fixture.exists("left/a.txt"));
    assert_eq!(fixture.read("right/a.txt"), "one");
}

#[test]
fn copyto_flattens_by_default() {
    let fixture = Fixture::new();
    fixture.write("left/sub/a.txt", "one");
    let target = fixture.root().join("out");
    let mut session = fixture.session();
    let body = format!(
        "expand all\nselect left.files\ncopyto left \"{}\"",
        target.display()
    );
    run(&fixture, &mut session, &body);
    assert!(target.join("a.txt").is_file());
}

#[test]
fn copyto_without_a_source_item_does_not_create_its_target() {
    for (select_right, expected_failures) in [(false, 1), (true, 0)] {
        let fixture = Fixture::new();
        if select_right {
            fixture.write("right/b.txt", "right only");
        }
        let target = fixture.root().join("out");
        let mut session = fixture.session();
        let selection = if select_right {
            "select right.files\n"
        } else {
            ""
        };
        let body = format!("{selection}copyto left \"{}\"", target.display());

        let outcome = run(&fixture, &mut session, &body);

        assert_eq!(outcome.failures, expected_failures);
        assert!(
            !target.exists(),
            "a command with no left-side source item must not create its target"
        );
    }
}

#[test]
fn copyto_with_path_base_keeps_the_whole_path() {
    let fixture = Fixture::new();
    fixture.write("left/sub/a.txt", "one");
    let target = fixture.root().join("out");
    let mut session = fixture.session();
    let body = format!(
        "expand all\nselect left.files\ncopyto left path:base \"{}\"",
        target.display()
    );
    run(&fixture, &mut session, &body);
    assert!(target.join("sub").join("a.txt").is_file());
}

/// A name the target folder already holds is a conflict, as a copy onto an
/// existing file is: a run that has nobody to ask leaves the item, and
/// `option confirm:yes-to-all` replaces it.
#[test]
fn copyto_onto_an_occupied_name_replaces_it_only_with_yes_to_all() {
    for (confirm, expected) in [("", "kept"), ("option confirm:yes-to-all\n", "one")] {
        let fixture = Fixture::new();
        fixture.write("left/a.txt", "one");
        fixture.write("out/a.txt", "kept");
        let target = fixture.root().join("out");
        let mut session = fixture.session();
        let body = format!(
            "{confirm}select left.files\ncopyto left \"{}\"",
            target.display()
        );
        run(&fixture, &mut session, &body);
        assert_eq!(fixture.read("out/a.txt"), expected, "{confirm:?}");
        assert_eq!(fixture.read("left/a.txt"), "one");
    }
}

#[test]
fn moveto_removes_the_source() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let target = fixture.root().join("out");
    let mut session = fixture.session();
    let body = format!("select left.files\nmoveto left \"{}\"", target.display());
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(!fixture.exists("left/a.txt"));
    assert!(target.join("a.txt").is_file());
}

#[test]
fn delete_removes_the_named_side() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/a.txt", "one");
    let mut session = fixture.session();
    run(
        &fixture,
        &mut session,
        "select all\ndelete recyclebin=no left",
    );
    assert!(!fixture.exists("left/a.txt"));
    assert!(fixture.exists("right/a.txt"));
}

#[test]
fn rename_gives_new_names() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let mut session = fixture.session();
    run(&fixture, &mut session, "select left.files\nrename *.bak");
    assert!(fixture.exists("left/a.bak"));
}

#[test]
fn rename_with_an_expression_gives_new_names() {
    let fixture = Fixture::new();
    fixture.write("left/abcdef.txt", "one");
    let mut session = fixture.session();
    run(
        &fixture,
        &mut session,
        "select left.files\nrename regexpr \"(...)(...)\\.txt\" \"$2$1.txt\"",
    );
    assert!(fixture.exists("left/defabc.txt"), "{:?}", fixture.root());
}

#[test]
fn touch_writes_a_chosen_timestamp() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let mut session = fixture.session();
    run(
        &fixture,
        &mut session,
        "select left.files\ntouch \"left:2001-02-03 04:05:06\"",
    );
    let modified = std::fs::metadata(fixture.left().join("a.txt"))
        .expect("metadata")
        .modified()
        .expect("modified");
    let seconds = ca_script::clock::unix_seconds(modified);
    assert_eq!(ca_script::clock::format_date(seconds), "2001-02-03");
}

#[test]
fn far_cutoff_dates_are_refused_without_panicking() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/a.txt", "one");
    let mut session = fixture.session();
    let seconds = ca_script::clock::parse_timestamp("40000-01-01", 0)
        .expect("the test date has a valid calendar representation");
    let unrepresentable = ca_script::clock::system_time(seconds).is_none();

    let outcome = run(
        &fixture,
        &mut session,
        "filter cutoff:>40000-01-01\nselect all.files\ntouch \"left:40000-01-01\"",
    );

    if unrepresentable {
        assert!(!outcome.is_clean(), "an unrepresentable date is refused");
    }
    assert!(fixture.exists("left/a.txt"));
}

#[test]
fn far_touch_timestamps_are_refused_without_panicking() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/a.txt", "one");
    let mut session = fixture.session();
    let seconds = ca_script::clock::parse_timestamp("40000-01-01", 0)
        .expect("the test date has a valid calendar representation");
    let unrepresentable = ca_script::clock::system_time(seconds).is_none();

    let outcome = run(
        &fixture,
        &mut session,
        "select all.files\ntouch \"left:40000-01-01\"",
    );

    if unrepresentable {
        assert!(!outcome.is_clean(), "an unrepresentable date is refused");
    }
    assert!(fixture.exists("left/a.txt"));
}

#[test]
fn touch_copies_timestamps_between_the_sides() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/a.txt", "two");
    let mut session = fixture.session();
    run(
        &fixture,
        &mut session,
        "select all\ntouch \"left:2001-02-03 04:05:06\"\nselect all\ntouch left->right",
    );
    let modified = std::fs::metadata(fixture.right().join("a.txt"))
        .expect("metadata")
        .modified()
        .expect("modified");
    let seconds = ca_script::clock::unix_seconds(modified);
    assert_eq!(ca_script::clock::format_date(seconds), "2001-02-03");
}

// -- compare and criteria ----------------------------------------------------

#[test]
fn compare_reads_the_contents_of_the_selection() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/a.txt", "two");
    let mut session = fixture.session();
    let outcome = run(&fixture, &mut session, "select all\ncompare binary");
    assert!(outcome.is_clean(), "{outcome:?}");
    let tree = session.tree.as_ref().expect("a tree");
    let node = &tree.children[0];
    assert_eq!(node.status, ca_fs::NodeStatus::Different);
}

#[test]
fn a_rules_based_comparison_ignores_a_white_space_difference() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one two\n");
    fixture.write("right/a.txt", "one   two\n");
    let mut session = fixture.session();
    run(
        &fixture,
        &mut session,
        "criteria ignore-unimportant\nselect all\ncompare rules-based",
    );
    let tree = session.tree.as_ref().expect("a tree");
    assert_eq!(tree.children[0].status, ca_fs::NodeStatus::Same);
}

#[test]
fn criteria_replaces_the_whole_set() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    run(&fixture, &mut session, "criteria timestamp:2sec size owner");
    assert!(session.criteria.owner);
    assert_eq!(
        session.criteria.timestamp.and_then(|t| t.tolerance_seconds),
        Some(2)
    );
    run(&fixture, &mut session, "criteria binary");
    assert!(!session.criteria.owner);
}

#[test]
fn default_criteria_mirror_same_size_files_when_the_source_is_newer() {
    let fixture = Fixture::new();
    fixture.write("left/same-size.txt", "bbbb");
    fixture.write("right/same-size.txt", "aaaa");
    let mut session = fixture.session();
    let outcome = run(
        &fixture,
        &mut session,
        concat!(
            "option confirm:yes-to-all\n",
            "criteria size\n",
            "load <default>\n",
            "load \"left\" \"right\"\n",
            "select left.files\n",
            "touch \"left:2001-02-03 04:05:06\"\n",
            "select right.files\n",
            "touch \"right:2000-02-03 04:05:06\"\n",
            "sync mirror:left->right",
        ),
    );
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(fixture.read("right/same-size.txt"), "bbbb");
    assert_eq!(
        session
            .criteria
            .timestamp
            .and_then(|stamp| stamp.tolerance_seconds),
        Some(2)
    );
}

#[test]
fn unsupported_attribute_filter_fails_before_a_mirror_can_delete_files() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "source");
    fixture.write("right/indexed-off.dat", "keep");
    let mut session = fixture.session();
    session.options.stop_on_error = true;
    let source = format!(
        "{}\nfilter attrib:-i\nsync mirror:left->right\n",
        fixture.load_line()
    );
    let error = ca_script::run_script(&source, &Substitution::none(), &mut session)
        .expect_err("an unsupported attribute filter stops the run");
    assert!(error.to_string().contains("not supported"), "{error}");
    assert!(fixture.exists("right/indexed-off.dat"));
}

// -- filters -----------------------------------------------------------------

#[test]
fn a_name_filter_keeps_items_out_of_the_comparison() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("left/b.log", "one");
    let mut session = fixture.session();
    run(&fixture, &mut session, "filter \"*.txt\"\nselect all");
    assert_eq!(session.selection.len(), 1);
}

#[test]
fn a_size_filter_keeps_items_out_of_the_comparison() {
    let fixture = Fixture::new();
    fixture.write("left/small.txt", "a");
    fixture.write("left/large.txt", &"a".repeat(4096));
    let mut session = fixture.session();
    run(&fixture, &mut session, "filter size:>1KB\nselect all");
    assert_eq!(
        session.selection,
        [PathBuf::from("small.txt")].into_iter().collect()
    );
}

// -- sync --------------------------------------------------------------------

#[test]
fn sync_update_copies_toward_the_target() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/b.txt", "two");
    let mut session = fixture.session();
    run(&fixture, &mut session, "sync update:left->right");
    assert!(fixture.exists("right/a.txt"));
    assert!(fixture.exists("right/b.txt"));
}

#[test]
fn sync_mirror_removes_orphans_on_the_target() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/b.txt", "two");
    let mut session = fixture.session();
    run(&fixture, &mut session, "sync mirror:left->right");
    assert!(fixture.exists("right/a.txt"));
    assert!(!fixture.exists("right/b.txt"));
}

#[test]
fn a_declined_sync_step_is_reported_as_a_failed_command() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "new!");
    fixture.write("right/a.txt", "old!");
    let mut session = fixture.session();
    let outcome = run(
        &fixture,
        &mut session,
        concat!(
            "log normal run.log\n",
            "option confirm:prompt\n",
            "criteria timestamp size\n",
            "select left.files\n",
            "touch \"left:2001-02-03 04:05:06\"\n",
            "select right.files\n",
            "touch \"right:2000-02-03 04:05:06\"\n",
            "sync update:left->right",
        ),
    );
    assert_eq!(outcome.failures, 1, "{outcome:?}");
    assert_eq!(fixture.read("right/a.txt"), "old!");
    assert!(fixture
        .read("run.log")
        .contains("left undone: conflict declined"));
}

// -- snapshot ----------------------------------------------------------------

#[test]
fn snapshot_writes_a_listing() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let target = fixture.root().join("snap.cass");
    let mut session = fixture.session();
    let body = format!("snapshot save-crc left output:\"{}\"", target.display());
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(target.is_file());
}

#[test]
fn snapshot_applies_session_filters_unless_no_filters_is_requested() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "text");
    fixture.write("left/b.log", "log");
    let filtered = fixture.root().join("filtered.cass");
    let unfiltered = fixture.root().join("unfiltered.cass");
    let mut session = fixture.session();
    let body = format!(
        "filter \"*.txt\"\nsnapshot left output:\"{}\"\nsnapshot no-filters left output:\"{}\"",
        filtered.display(),
        unfiltered.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    let filtered = ca_vfs::Snapshot::load(&filtered).expect("filtered snapshot");
    let unfiltered = ca_vfs::Snapshot::load(&unfiltered).expect("unfiltered snapshot");
    assert_eq!(
        filtered
            .entries
            .iter()
            .map(|record| record.path.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["a.txt"])
    );
    assert_eq!(
        unfiltered
            .entries
            .iter()
            .map(|record| record.path.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["a.txt", "b.log"])
    );
}

#[test]
fn read_only_bases_block_operations_reports_snapshots_and_logs_through_dot_dot_paths() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "left");
    fixture.write("right/b.txt", "right");
    let alternate = |name: &str| fixture.right().join("..").join("left").join(name);
    let mut session = fixture.session();
    session.options.left_read_only = true;

    let destinations = vec![alternate("copy-destination")];
    #[cfg(windows)]
    let destinations = {
        let mut destinations = destinations;
        let normal = alternate("copy-destination-case");
        destinations.push(PathBuf::from(normal.to_string_lossy().to_uppercase()));
        destinations.push(PathBuf::from(format!(r"\\?\{}", normal.display())));
        destinations
    };
    for destination in destinations {
        let copy = run(
            &fixture,
            &mut session,
            &format!(
                "select right.files\ncopyto rt path:none \"{}\"",
                destination.display()
            ),
        );
        assert!(!copy.is_clean(), "{copy:?}");
        assert!(
            !destination.exists(),
            "the locked destination is not created: {}",
            destination.display()
        );
    }

    let report = alternate("report.txt");
    let reported = run(
        &fixture,
        &mut session,
        &format!(
            "folder-report layout:summary output-to:\"{}\"",
            report.display()
        ),
    );
    assert!(!reported.is_clean(), "{reported:?}");
    assert!(!report.exists());

    let snapshot = alternate("listing.cass");
    let captured = run(
        &fixture,
        &mut session,
        &format!("snapshot left output:\"{}\"", snapshot.display()),
    );
    assert!(!captured.is_clean(), "{captured:?}");
    assert!(!snapshot.exists());

    let log = alternate("run.log");
    let logged = run(
        &fixture,
        &mut session,
        &format!("log normal \"{}\"", log.display()),
    );
    assert!(!logged.is_clean(), "{logged:?}");
    assert!(!log.exists());
}

#[test]
fn snapshot_records_the_contents_of_an_archive_as_folders() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let target = fixture.root().join("snap.cass");
    let mut session = fixture.session();
    let body = format!(
        "snapshot expand-archives left output:\"{}\"",
        target.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(outcome.not_supported.is_empty(), "{outcome:?}");
    assert!(target.is_file());
}

// -- reports -----------------------------------------------------------------

#[test]
fn a_folder_report_is_written() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/a.txt", "two");
    let target = fixture.root().join("folder.txt");
    let mut session = fixture.session();
    let body = format!(
        "folder-report layout:side-by-side title:\"My Report\" output-to:\"{}\"",
        target.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    let text = std::fs::read_to_string(&target).expect("report");
    assert!(text.contains("a.txt"), "{text}");
}

#[test]
fn a_text_report_is_written_for_a_named_pair() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one\n");
    fixture.write("right/a.txt", "two\n");
    let target = fixture.root().join("text.txt");
    let mut session = fixture.session();
    let body = format!(
        "text-report layout:side-by-side output-to:\"{}\" \"{}\" \"{}\"",
        target.display(),
        fixture.left().join("a.txt").display(),
        fixture.right().join("a.txt").display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(target.is_file());
}

#[test]
fn a_hex_report_is_written_for_the_selection() {
    let fixture = Fixture::new();
    fixture.write("left/a.bin", "one");
    fixture.write("right/a.bin", "two");
    let target = fixture.root().join("hex.txt");
    let mut session = fixture.session();
    let body = format!(
        "select all\nhex-report layout:side-by-side output-to:\"{}\"",
        target.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(target.is_file());
}

/// A small picture, encoded as a portable network graphic.
fn png(width: u32, height: u32, shift: u8) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        let red = u8::try_from((x * 17) % 256)
            .unwrap_or(0)
            .wrapping_add(shift);
        let green = u8::try_from((y * 29) % 256).unwrap_or(0);
        image::Rgba([red, green, 0, 255])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encode");
    bytes
}

#[test]
fn a_data_report_reads_two_table_files() {
    let fixture = Fixture::new();
    fixture.write("left/rows.csv", "id,name\n1,Ann\n2,Bob\n");
    fixture.write("right/rows.csv", "id,name\n1,Ann\n3,Cy\n");
    let target = fixture.root().join("data.txt");
    let mut session = fixture.session();
    let body = format!(
        "data-report layout:side-by-side options:line-numbers output-to:\"{}\" \"{}\" \"{}\"",
        target.display(),
        fixture.left().join("rows.csv").display(),
        fixture.right().join("rows.csv").display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    let text = std::fs::read_to_string(&target).expect("report");
    assert!(text.contains("name"), "{text}");
    assert!(text.contains("Bob"), "{text}");
    assert!(text.contains("Cy"), "{text}");
}

#[test]
fn a_data_report_summary_counts_the_rows() {
    let fixture = Fixture::new();
    fixture.write("left/rows.csv", "id,name\n1,Ann\n2,Bob\n");
    fixture.write("right/rows.csv", "id,name\n1,Ann\n3,Cy\n");
    let target = fixture.root().join("data-summary.txt");
    let mut session = fixture.session();
    let body = format!(
        "data-report layout:summary output-to:\"{}\" \"{}\" \"{}\"",
        target.display(),
        fixture.left().join("rows.csv").display(),
        fixture.right().join("rows.csv").display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    let text = std::fs::read_to_string(&target).expect("report");
    assert!(text.contains("rows"), "{text}");
}

#[test]
fn a_picture_report_reads_two_pictures() {
    let fixture = Fixture::new();
    std::fs::write(fixture.left().join("a.png"), png(8, 8, 0)).expect("left picture");
    std::fs::write(fixture.right().join("a.png"), png(8, 8, 40)).expect("right picture");
    let target = fixture.root().join("picture.txt");
    let mut session = fixture.session();
    let body = format!(
        "picture-report layout:side-by-side output-to:\"{}\" \"{}\" \"{}\"",
        target.display(),
        fixture.left().join("a.png").display(),
        fixture.right().join("a.png").display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    let text = std::fs::read_to_string(&target).expect("report");
    assert!(text.contains("8 x 8"), "{text}");
    assert!(text.contains("PNG"), "{text}");
}

#[test]
fn a_picture_report_takes_the_selected_pair() {
    let fixture = Fixture::new();
    std::fs::write(fixture.left().join("a.png"), png(4, 4, 0)).expect("left picture");
    std::fs::write(fixture.right().join("a.png"), png(4, 4, 9)).expect("right picture");
    let target = fixture.root().join("picture-summary.txt");
    let mut session = fixture.session();
    let body = format!(
        "select all\npicture-report layout:summary output-to:\"{}\"",
        target.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(target.is_file());
}

#[test]
fn a_file_report_chooses_the_report_from_the_file_format() {
    let fixture = Fixture::new();
    fixture.write("left/rows.csv", "id,name\n1,Ann\n");
    fixture.write("right/rows.csv", "id,name\n1,Bob\n");
    std::fs::write(fixture.left().join("a.png"), png(4, 4, 0)).expect("left picture");
    std::fs::write(fixture.right().join("a.png"), png(4, 4, 30)).expect("right picture");
    fixture.write("left/notes.txt", "one\n");
    fixture.write("right/notes.txt", "two\n");
    let mut session = fixture.session();

    let table_target = fixture.root().join("file-table.txt");
    let body = format!(
        "file-report layout:side-by-side output-to:\"{}\" \"{}\" \"{}\"",
        table_target.display(),
        fixture.left().join("rows.csv").display(),
        fixture.right().join("rows.csv").display()
    );
    assert!(run(&fixture, &mut session, &body).is_clean());
    let table_text = std::fs::read_to_string(&table_target).expect("table report");
    assert!(table_text.contains("Table Compare Report"), "{table_text}");

    let picture_target = fixture.root().join("file-picture.txt");
    let body = format!(
        "file-report layout:side-by-side output-to:\"{}\" \"{}\" \"{}\"",
        picture_target.display(),
        fixture.left().join("a.png").display(),
        fixture.right().join("a.png").display()
    );
    assert!(run(&fixture, &mut session, &body).is_clean());
    let picture_text = std::fs::read_to_string(&picture_target).expect("picture report");
    assert!(
        picture_text.contains("Picture Compare Report"),
        "{picture_text}"
    );

    let text_target = fixture.root().join("file-text.txt");
    let body = format!(
        "file-report layout:side-by-side output-to:\"{}\" \"{}\" \"{}\"",
        text_target.display(),
        fixture.left().join("notes.txt").display(),
        fixture.right().join("notes.txt").display()
    );
    assert!(run(&fixture, &mut session, &body).is_clean());
    let text = std::fs::read_to_string(&text_target).expect("text report");
    assert!(text.contains("Text Compare Report"), "{text}");
}

#[test]
fn the_reports_with_no_engine_say_so() {
    let fixture = Fixture::new();
    let target = fixture.root().join("out.txt");
    for word in ["media-report", "registry-report", "version-report"] {
        let mut session = fixture.session();
        let body = format!("{word} layout:summary output-to:\"{}\"", target.display());
        let outcome = run(&fixture, &mut session, &body);
        assert_eq!(outcome.not_supported, vec![word.to_string()], "{word}");
    }
}

#[test]
fn printing_and_the_clipboard_say_they_have_no_engine() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let outcome = run(
        &fixture,
        &mut session,
        "folder-report layout:summary output-to:printer",
    );
    assert_eq!(outcome.not_supported, vec!["folder-report".to_string()]);
}

// -- log ---------------------------------------------------------------------

#[test]
fn dry_run_preserves_report_and_log_targets() {
    let fixture = Fixture::new();
    fixture.write("report.txt", "existing report");
    fixture.write("log.txt", "existing log");
    let mut session = fixture.session();
    session.options.dry_run = true;
    let outcome = run(&fixture, &mut session,
        "log normal log.txt\nfolder-report layout:summary output-to:report.txt\nfolder-report layout:summary output-to:new/report.txt");
    assert!(outcome.is_clean());
    assert_eq!(fixture.read("report.txt"), "existing report");
    assert_eq!(fixture.read("log.txt"), "existing log");
    assert!(!fixture.exists("new"));
}

#[test]
fn dry_run_does_not_create_a_missing_load_folder() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    session.options.dry_run = true;
    let _ = run_script(
        "load create:right left new-right",
        &Substitution::none(),
        &mut session,
    );
    assert!(!fixture.exists("new-right"));
}

#[test]
fn the_log_records_each_command_and_each_failure() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let target = fixture.root().join("run.log");
    let mut session = fixture.session();
    session.log.set_fixed_time(Some(0));
    let body = format!(
        "log normal \"{}\"\nselect all\ncopy left->right",
        target.display()
    );
    run(&fixture, &mut session, &body);
    let text = std::fs::read_to_string(&target).expect("log");
    assert!(
        text.contains("1970-01-01 00:00:00  command  select all.all.all"),
        "{text}"
    );
    assert!(text.contains("note"), "{text}");
    assert!(!text.contains("item "), "{text}");
}

#[test]
fn a_verbose_log_records_each_item() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let target = fixture.root().join("run.log");
    let mut session = fixture.session();
    session.log.set_fixed_time(Some(0));
    let body = format!(
        "log verbose \"{}\"\nselect all\ncopy left->right",
        target.display()
    );
    run(&fixture, &mut session, &body);
    let text = std::fs::read_to_string(&target).expect("log");
    assert!(text.contains("item"), "{text}");
}

#[test]
fn a_password_in_a_location_reaches_neither_the_log_nor_the_error() {
    let fixture = Fixture::new();
    let target = fixture.root().join("run.log");
    let mut session = fixture.session();
    session.log.set_fixed_time(Some(0));
    let source = format!(
        "log verbose \"{}\"\nload \"ftp://alice:hunter2@127.0.0.1:1/pub\" \"{}\"\n",
        target.display(),
        fixture.right().display()
    );
    let error = run_script(&source, &Substitution::none(), &mut session).unwrap_err();
    let text = std::fs::read_to_string(&target).expect("log");
    assert!(text.contains("ftp://alice:***@127.0.0.1:1/pub"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!error.to_string().contains("hunter2"), "{error}");
}

#[test]
fn append_adds_to_the_log_instead_of_replacing_it() {
    let fixture = Fixture::new();
    let target = fixture.root().join("run.log");
    std::fs::write(&target, "earlier\n").expect("seed");
    let mut session = fixture.session();
    session.log.set_fixed_time(Some(0));
    let body = format!("log normal append:\"{}\"\nbeep", target.display());
    run(&fixture, &mut session, &body);
    let text = std::fs::read_to_string(&target).expect("log");
    assert!(text.starts_with("earlier"), "{text}");
    assert!(text.contains("beep"), "{text}");
}

#[test]
fn a_log_level_of_none_writes_nothing() {
    let fixture = Fixture::new();
    let target = fixture.root().join("run.log");
    let mut session = fixture.session();
    let body = format!("log none \"{}\"\nbeep", target.display());
    run(&fixture, &mut session, &body);
    assert!(!target.exists());
}

// -- run behaviour -----------------------------------------------------------

#[test]
fn dry_run_touches_nothing() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let mut session = Session::new(SessionOptions {
        dry_run: true,
        journal_directory: fixture.root().join("journals"),
        working_directory: fixture.root().to_path_buf(),
        ..SessionOptions::default()
    });
    let outcome = run(&fixture, &mut session, "select all\ncopy left->right");
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(!fixture.exists("right/a.txt"));
    assert!(!fixture.root().join("journals").exists());
}

#[test]
fn a_failing_command_lets_the_run_carry_on() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let mut session = fixture.session();
    let outcome = run(&fixture, &mut session, "copy left->right\nselect all");
    assert_eq!(outcome.failures, 1);
    assert!(!outcome.stopped);
    assert_eq!(outcome.steps.len(), 3);
}

#[test]
fn stop_on_error_ends_the_run_at_the_first_failure() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let source = format!(
        "{}\noption stop-on-error\ncopy left->right\nselect all\n",
        fixture.load_line()
    );
    let mut session = fixture.session();
    let error =
        run_script(&source, &Substitution::none(), &mut session).expect_err("the run stops");
    assert!(matches!(error, ca_script::RunError::Stopped(_)));
}

#[test]
fn a_command_that_needs_a_selection_says_so() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let outcome = run(&fixture, &mut session, "delete left");
    let error = outcome.steps.last().and_then(|s| s.error.clone());
    assert!(
        error
            .as_deref()
            .is_some_and(|text| text.contains("selection")),
        "{error:?}"
    );
}

#[test]
fn a_journal_is_written_for_a_batch_that_changes_the_disk() {
    const FILES: usize = 512;
    let fixture = Fixture::new();
    for index in 0..FILES {
        fixture.write(&format!("left/file-{index:04}.txt"), "one");
    }
    let journals = fixture.root().join("journals");
    let copied_file = fixture.right().join("file-0000.txt");
    let mut session = fixture.session();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = run(&fixture, &mut session, "select all\ncopy left->right");
        let _ = send.send(outcome.is_clean());
    });

    // Hold the journal open as soon as the executor creates it, before the
    // successful batch retires the file. Its records remain readable after
    // the executor removes its directory entry.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut journal = None;
    let mut completed = None;
    while std::time::Instant::now() < deadline && journal.is_none() {
        if let Ok(entries) = std::fs::read_dir(&journals) {
            journal = entries.filter_map(Result::ok).find_map(|entry| {
                (entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "jsonl"))
                .then(|| std::fs::File::open(entry.path()).ok())
                .flatten()
            });
        }
        if journal.is_none() {
            completed = receive.try_recv().ok();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    let mut journal = journal.expect("the write batch opened a journal");
    let clean = completed.unwrap_or_else(|| {
        // Each copy forces its data and journal records to stable storage.
        // Wait for completion rather than imposing a disk-speed deadline.
        receive.recv().expect("the write batch finished")
    });
    assert!(clean);

    let mut records = String::new();
    std::io::Read::read_to_string(&mut journal, &mut records).expect("read journal records");
    assert!(records.contains("\"record\":\"batch_start\""), "{records}");
    assert!(records.contains("\"record\":\"step_begin\""), "{records}");
    assert!(records.contains("\"record\":\"batch_end\""), "{records}");
    assert!(copied_file.is_file());
}

#[test]
fn numbered_arguments_reach_the_commands() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let subst = Substitution::fixed(
        vec![
            fixture.left().display().to_string(),
            fixture.right().display().to_string(),
        ],
        std::collections::BTreeMap::new(),
        0,
    );
    let mut session = fixture.session();
    run_script(
        "load \"%1\" \"%2\"\nselect all\ncopy left->right\n",
        &subst,
        &mut session,
    )
    .expect("runs");
    assert_eq!(fixture.read("right/a.txt"), "one");
}

// -- sources other than a local folder ---------------------------------------

/// The 22 bytes of an end of central directory record, which is a zip holding
/// nothing.
const EMPTY_ZIP: &[u8] = &[
    0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// A zip at `rel` holding one named entry.
fn zip_with(fixture: &Fixture, rel: &str, name: &str, content: &[u8]) -> PathBuf {
    let path = fixture.root().join(rel);
    std::fs::write(&path, EMPTY_ZIP).expect("zip");
    let source = ca_fs::Source::archive(&path, ca_vfs::ArchiveOptions::default()).expect("open");
    let mut reader = content;
    source
        .file_system()
        .write_file(
            &ca_vfs::VfsPath::parse(name).expect("name"),
            &mut reader,
            &ca_vfs::Cancel::new(),
        )
        .expect("write");
    path
}

#[test]
fn load_reads_an_archive_as_one_side_of_the_comparison() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let archive = zip_with(&fixture, "right.zip", "a.txt", b"one");
    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files",
        fixture.left().display(),
        archive.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(session.selection.len(), 1);
}

#[test]
fn load_reads_a_recorded_listing_as_one_side() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let capture = fixture.root().join("left.cass");
    let mut session = fixture.session();
    let body = format!("snapshot left output:\"{}\"", capture.display());
    assert!(run(&fixture, &mut session, &body).is_clean());

    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files",
        fixture.left().display(),
        capture.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(session.selection.len(), 1);
}

#[test]
fn a_write_command_is_refused_before_it_runs_when_a_side_cannot_take_it() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let capture = fixture.root().join("left.cass");
    let mut session = fixture.session();
    let body = format!("snapshot left output:\"{}\"", capture.display());
    assert!(run(&fixture, &mut session, &body).is_clean());

    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files\ncopy left->right",
        fixture.left().display(),
        capture.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert_eq!(outcome.not_supported, vec!["copy".to_string()]);
    assert_eq!(fixture.read("left/a.txt"), "one");
}

/// Every entry name a zip holds.
fn zip_names(path: &Path) -> Vec<String> {
    let source = ca_fs::Source::archive(path, ca_vfs::ArchiveOptions::default()).expect("open");
    let result = ca_fs::scan_source(
        &source,
        &ca_fs::ScanOptions::default(),
        &ca_fs::Cancel::new(),
        &|_| {},
    )
    .expect("scan");
    let mut names: Vec<String> = result
        .entries
        .values()
        .filter(|entry| !entry.is_dir)
        .map(|entry| entry.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn copy_writes_into_a_zip_on_the_target_side() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("left/b.txt", "two");
    let archive = zip_with(&fixture, "right.zip", "a.txt", b"old");
    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files\noption confirm:yes-to-all\ncopy left->right",
        fixture.left().display(),
        archive.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(
        zip_names(&archive),
        vec!["a.txt".to_string(), "b.txt".to_string()]
    );
}

/// A copy into a zip gives the record the modification time of the file it
/// copies, an odd second that a DOS stamp alone cannot hold, so the pair the
/// copy made is the same time. A later script that rewrites the container
/// leaves the record at that time.
#[test]
fn a_copy_into_a_zip_keeps_the_time_of_the_file() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let written = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_001);
    std::fs::File::options()
        .write(true)
        .open(fixture.left().join("a.txt"))
        .expect("open")
        .set_modified(written)
        .expect("stamp");
    let archive = zip_with(&fixture, "right.zip", "keep.txt", b"keep");
    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files\ncopy left->right",
        fixture.left().display(),
        archive.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    let source = ca_fs::Source::archive(&archive, ca_vfs::ArchiveOptions::default()).expect("open");
    let entry = source
        .file_system()
        .metadata(&ca_vfs::VfsPath::parse("a.txt").expect("name"))
        .expect("the copy landed");
    assert_eq!(entry.modified, Some(written));
    drop(source);

    fixture.write("left/b.txt", "two");
    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect left.orphan.files\ncopy left->right",
        fixture.left().display(),
        archive.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(
        zip_names(&archive),
        vec![
            "a.txt".to_string(),
            "b.txt".to_string(),
            "keep.txt".to_string()
        ]
    );
    let source = ca_fs::Source::archive(&archive, ca_vfs::ArchiveOptions::default()).expect("open");
    let entry = source
        .file_system()
        .metadata(&ca_vfs::VfsPath::parse("a.txt").expect("name"))
        .expect("the copy stayed");
    assert_eq!(entry.modified, Some(written));
}

#[test]
fn delete_removes_an_entry_inside_a_zip() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let archive = zip_with(&fixture, "right.zip", "a.txt", b"one");
    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files\ndelete recyclebin=no right",
        fixture.left().display(),
        archive.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    assert!(zip_names(&archive).is_empty(), "{:?}", zip_names(&archive));
}

#[test]
fn sync_updates_a_zip_on_the_target_side() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("left/sub/c.txt", "three");
    let archive = zip_with(&fixture, "right.zip", "keep.txt", b"keep");
    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nsync create-empty update:left->right",
        fixture.left().display(),
        archive.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");
    let names = zip_names(&archive);
    assert!(names.contains(&"a.txt".to_string()), "{names:?}");
    assert!(names.contains(&"c.txt".to_string()), "{names:?}");
    assert!(names.contains(&"keep.txt".to_string()), "{names:?}");
}

#[test]
fn a_read_only_container_refuses_the_write_before_the_plan_runs() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let tar = fixture.root().join("right.tar");
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(3);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "a.txt", &b"old"[..])
        .expect("entry");
    std::fs::write(&tar, builder.into_inner().expect("tar")).expect("write");

    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files\ncopy left->right",
        fixture.left().display(),
        tar.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert_eq!(outcome.not_supported, vec!["copy".to_string()]);
}

#[test]
fn a_remote_location_with_no_stored_profile_is_refused_by_name() {
    let fixture = Fixture::new();
    let source = format!(
        "load \"{}\" \"sftp://host/path\"\n",
        fixture.left().display()
    );
    let mut session = fixture.session();
    let error =
        run_script(&source, &Substitution::none(), &mut session).expect_err("the run stops");
    let text = error.to_string();
    assert!(text.contains("sftp://host/path"), "{text}");
}

// -- a remote side -----------------------------------------------------------

/// A lookup that answers with one profile and one stored secret.
#[derive(Debug)]
struct OneRemote {
    profile: ca_vfs::RemoteProfile,
}

impl ca_script::ProfileLookup for OneRemote {
    fn profile(&self, _location: &str) -> Option<ca_vfs::RemoteProfile> {
        Some(self.profile.clone())
    }

    fn secrets(&self) -> std::sync::Arc<dyn ca_vfs::SecretStore> {
        let store = ca_vfs::MemorySecretStore::new();
        store.insert("password", "hunter2");
        std::sync::Arc::new(store)
    }
}

#[test]
fn copy_writes_to_a_remote_side() {
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    let remote_dir = tempfile::tempdir().expect("remote folder");
    let server = ca_vfs::testing::ftp_server::FtpTestServer::start(
        remote_dir.path(),
        ca_vfs::testing::ftp_server::Options {
            account: Some(("operator".to_owned(), "hunter2".to_owned())),
            mlsd: true,
            ..ca_vfs::testing::ftp_server::Options::default()
        },
    );
    let mut settings = ca_vfs::remote::profile::FtpProfile::default();
    settings.login.host = "127.0.0.1".to_owned();
    settings.login.port = Some(server.port());
    settings.login.username = "operator".to_owned();
    settings.login.password = ca_vfs::SecretRef::new("password");
    settings.listing.use_mlsd = true;
    settings.connection.passive = true;

    let mut session = Session::new(SessionOptions {
        journal_directory: fixture.root().join("journals"),
        working_directory: fixture.root().to_path_buf(),
        recycle_bin: false,
        profiles: std::sync::Arc::new(OneRemote {
            profile: ca_vfs::RemoteProfile {
                name: "127.0.0.1".to_owned(),
                service: ca_vfs::ServiceProfile::Ftp(settings),
                ..ca_vfs::RemoteProfile::default()
            },
        }),
        ..SessionOptions::default()
    });
    let source = format!(
        "load \"{}\" \"ftp://127.0.0.1/\"\nexpand all\nselect all.files\ncopy left->right\n",
        fixture.left().display()
    );
    let outcome =
        run_script(&source, &Substitution::none(), &mut session).expect("the run finishes");
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(
        std::fs::read(remote_dir.path().join("a.txt")).expect("the upload landed"),
        b"one"
    );
}

#[test]
fn compare_and_a_text_report_read_both_sides_inside_archives() {
    let fixture = Fixture::new();
    let left = zip_with(&fixture, "a.zip", "b.txt", b"left side line\n");
    let right = zip_with(&fixture, "b.zip", "b.txt", b"right side line\n");
    let report = fixture.root().join("text.txt");
    let mut session = fixture.session();
    let body = format!(
        "load \"{}\" \"{}\"\nexpand all\nselect all.files\ncompare binary\ntext-report layout:side-by-side output-to:\"{}\"",
        left.display(),
        right.display(),
        report.display()
    );
    let outcome = run(&fixture, &mut session, &body);
    assert!(outcome.is_clean(), "{outcome:?}");

    let tree = session.tree.as_ref().expect("a tree");
    let node = tree
        .children
        .iter()
        .find(|node| node.name == "b.txt")
        .expect("the pair");
    assert_eq!(node.status, ca_fs::NodeStatus::Different);
    assert_eq!(node.error, None);
    let written = std::fs::read_to_string(&report).expect("the report is written");
    assert!(written.contains("left side line"), "{written}");
    assert!(written.contains("right side line"), "{written}");
}

#[cfg(windows)]
#[test]
fn attrib_sets_and_clears_the_system_flag() {
    use std::os::windows::fs::MetadataExt;

    const SYSTEM: u32 = 0x0000_0004;
    let fixture = Fixture::new();
    fixture.write("left/a.txt", "one");
    fixture.write("right/a.txt", "one");
    let flag = |side: &str| {
        std::fs::metadata(fixture.root().join(side).join("a.txt"))
            .expect("metadata")
            .file_attributes()
            & SYSTEM
    };

    let mut session = fixture.session();
    let outcome = run(&fixture, &mut session, "select all.files\nattrib +s");
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_ne!(flag("left"), 0);
    assert_ne!(flag("right"), 0);

    let mut session = fixture.session();
    let outcome = run(&fixture, &mut session, "select all.files\nattrib -s");
    assert!(outcome.is_clean(), "{outcome:?}");
    assert_eq!(flag("left"), 0);
    assert_eq!(flag("right"), 0);
}

#[cfg(windows)]
#[test]
fn a_failed_copy_clears_the_selection_before_a_following_delete() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    fixture.write("left/a.txt", "new a");
    fixture.write("left/b.txt", "new b");
    fixture.write("right/b.txt", "old b");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.root().join("right/b.txt"))
        .expect("hold the destination without sharing");

    let mut session = fixture.session();
    let outcome = run(
        &fixture,
        &mut session,
        "option confirm:yes-to-all\nselect left.files\ncopy left->right\ndelete left",
    );

    assert!(!outcome.is_clean(), "the locked destination must fail");
    drop(held);
    assert!(fixture.exists("left/a.txt"));
    assert!(fixture.exists("left/b.txt"));
    assert_eq!(fixture.read("right/b.txt"), "old b");
}

#[cfg(windows)]
#[test]
fn attrib_filter_sign_selects_only_the_requested_attribute_state() {
    use std::os::windows::fs::MetadataExt;

    const HIDDEN: u32 = 0x0000_0002;
    let fixture = Fixture::new();
    fixture.write("left/hidden.dat", "hidden");
    fixture.write("right/hidden.dat", "hidden");
    let mut session = fixture.session();
    let marked = run(&fixture, &mut session, "select left.files\nattrib +h");
    assert!(marked.is_clean(), "{marked:?}");
    assert_ne!(
        std::fs::metadata(fixture.root().join("left/hidden.dat"))
            .expect("hidden file metadata")
            .file_attributes()
            & HIDDEN,
        0
    );
    fixture.write("left/plain.txt", "plain");
    fixture.write("right/plain.txt", "plain");

    let selected = run(&fixture, &mut session, "filter attrib:-h\nselect all.files");
    assert!(selected.is_clean(), "{selected:?}");
    assert_eq!(session.selection, [PathBuf::from("plain.txt")].into(),);

    let selected = run(&fixture, &mut session, "filter attrib:+h\nselect all.files");
    assert!(selected.is_clean(), "{selected:?}");
    assert_eq!(session.selection, [PathBuf::from("hidden.dat")].into(),);
}
