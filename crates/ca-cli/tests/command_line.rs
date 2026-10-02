//! Switch parsing, the quick comparison exit codes, and script runs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use ca_cli::args::{Invocation, QuickKind, QuickRun};
use ca_cli::request::Pane;
use ca_cli::{exit, help, parse, quick, script};

fn desktop(arguments: &[&str]) -> ca_cli::DesktopRequest {
    match parse(arguments.iter().map(|a| (*a).to_string())) {
        Ok(Invocation::Desktop(request)) => *request,
        other => panic!("{other:?}"),
    }
}

// -- switches ----------------------------------------------------------------

#[test]
fn the_three_switch_prefixes_are_all_accepted() {
    for text in ["/expandall", "-expandall", "--expandall"] {
        assert!(desktop(&[text]).expand_all, "{text}");
    }
}

#[test]
fn switch_names_ignore_letter_case() {
    assert!(desktop(&["/ExpandAll"]).expand_all);
}

#[test]
fn help_is_asked_for_in_three_ways() {
    for text in ["/?", "/h", "/help", "--help", "-h"] {
        assert_eq!(
            parse([text.to_string()]).expect("parses"),
            Invocation::Help,
            "{text}"
        );
    }
}

#[test]
fn positional_paths_fill_the_four_panes() {
    let request = desktop(&["a", "b", "c", "d"]);
    assert_eq!(request.path(Pane::Left), Some(Path::new("a")));
    assert_eq!(request.path(Pane::Right), Some(Path::new("b")));
    assert_eq!(request.path(Pane::Center), Some(Path::new("c")));
    assert_eq!(request.path(Pane::Output), Some(Path::new("d")));
}

#[test]
fn a_fifth_path_is_refused() {
    assert!(parse(["a", "b", "c", "d", "e"].map(String::from)).is_err());
}

#[test]
fn the_center_switch_fills_the_third_pane() {
    let request = desktop(&["a", "b", "/center=base.txt"]);
    assert_eq!(request.paths.len(), 3);
    assert_eq!(request.path(Pane::Left), Some(Path::new("a")));
    assert_eq!(request.path(Pane::Right), Some(Path::new("b")));
    assert_eq!(request.path(Pane::Center), Some(Path::new("base.txt")));
    assert_eq!(request.path(Pane::Output), None);
}

#[test]
fn the_center_switch_can_stand_alone_or_precede_positional_paths() {
    let alone = desktop(&["/center=base.txt"]);
    assert_eq!(alone.paths.len(), 3);
    assert_eq!(alone.path(Pane::Left), Some(Path::new("")));
    assert_eq!(alone.path(Pane::Right), Some(Path::new("")));
    assert_eq!(alone.path(Pane::Center), Some(Path::new("base.txt")));

    let before_paths = desktop(&["/center=base.txt", "a", "b"]);
    assert_eq!(before_paths.path(Pane::Left), Some(Path::new("a")));
    assert_eq!(before_paths.path(Pane::Right), Some(Path::new("b")));
    assert_eq!(before_paths.path(Pane::Center), Some(Path::new("base.txt")));
}

#[test]
fn center_switch_does_not_add_a_fifth_pane_or_bypass_the_path_limit() {
    assert!(matches!(
        parse(["a", "b", "c", "d", "e", "/center=base.txt"].map(String::from)),
        Err(ca_cli::args::ArgError::TooManyPaths(5))
    ));

    let request = desktop(&["a", "b", "old-base.txt", "out.txt", "/center=new-base.txt"]);
    assert_eq!(request.paths.len(), 4);
    assert_eq!(request.path(Pane::Center), Some(Path::new("new-base.txt")));
    assert_eq!(request.path(Pane::Output), Some(Path::new("out.txt")));
}

#[test]
fn the_title_switches_have_two_spellings_each() {
    let numbered = desktop(&[
        "/title1=one",
        "/title2=two",
        "/title3=three",
        "/title4=four",
    ]);
    let named = desktop(&[
        "/lefttitle=one",
        "/righttitle=two",
        "/centertitle=three",
        "/outputtitle=four",
    ]);
    assert_eq!(numbered.titles, named.titles);
    assert_eq!(numbered.title(Pane::Left), Some("one"));
    assert_eq!(numbered.title(Pane::Output), Some("four"));
}

#[test]
fn the_vcs_switches_have_two_spellings_each() {
    let numbered = desktop(&["/vcs1=one", "/vcs2=two", "/vcs3=three", "/vcs4=four"]);
    let named = desktop(&[
        "/vcsleft=one",
        "/vcsright=two",
        "/vcscenter=three",
        "/vcsoutput=four",
    ]);
    assert_eq!(numbered.vcs_paths, named.vcs_paths);
    assert_eq!(numbered.vcs_path(Pane::Center), Some("three"));
}

#[test]
fn the_read_only_switches_lock_the_named_sides() {
    assert!(desktop(&["/ro"]).read_only.left && desktop(&["/readonly"]).read_only.right);
    for text in ["/ro1", "/lro", "/leftreadonly"] {
        let lock = desktop(&[text]).read_only;
        assert!(lock.left && !lock.right, "{text}");
    }
    for text in ["/ro2", "/rro", "/rightreadonly"] {
        let lock = desktop(&[text]).read_only;
        assert!(lock.right && !lock.left, "{text}");
    }
}

#[test]
fn the_merge_switches_are_read() {
    let request = desktop(&[
        "/automerge",
        "/favorleft",
        "/favorright",
        "/iu",
        "/force",
        "/reviewconflicts",
        "/mergeoutput=out.txt",
        "/nobackups",
        "/savetarget=save.txt",
    ]);
    assert!(request.automerge.enabled);
    assert!(request.automerge.favor_left);
    assert!(request.automerge.favor_right);
    assert!(request.automerge.ignore_unimportant);
    assert!(request.automerge.force);
    assert!(request.automerge.review_conflicts);
    assert_eq!(request.merge_output, Some(PathBuf::from("out.txt")));
    assert_eq!(request.save_target, Some(PathBuf::from("save.txt")));
    assert!(request.no_backups);
}

#[test]
fn the_view_type_switch_has_two_spellings() {
    assert_eq!(
        desktop(&["/fv=Text Compare"]).file_viewer.as_deref(),
        Some("Text Compare")
    );
    assert_eq!(
        desktop(&["/fileviewer=Hex Compare"]).file_viewer.as_deref(),
        Some("Hex Compare")
    );
}

#[test]
fn the_filter_switch_is_read() {
    assert_eq!(
        desktop(&["/filters=*.txt;*.log"]).filters.as_deref(),
        Some("*.txt;*.log")
    );
}

#[test]
fn desktop_only_switches_are_accepted_and_named() {
    let request = desktop(&[
        "/solo",
        "/sync",
        "/edit",
        "/closescript",
        "/reviewconflicts",
    ]);
    assert!(request.solo);
    assert!(request.folder_sync);
    assert!(request.edit);
    assert!(request.close_script);
    for name in ["solo", "sync", "edit", "closescript", "reviewconflicts"] {
        assert!(
            request.desktop_only.iter().any(|item| item == name),
            "{name}"
        );
    }
}

#[test]
fn a_switch_that_needs_a_value_says_so() {
    let error = parse(["--filters".to_string()]).expect_err("an error");
    assert!(error.to_string().contains("needs a value"), "{error}");
}

#[test]
fn an_unknown_long_switch_says_so() {
    let error = parse(["--wobble".to_string()]).expect_err("an error");
    assert!(error.to_string().contains("not a switch"), "{error}");
}

#[test]
fn arguments_after_the_end_of_switches_marker_are_paths() {
    let request = desktop(&["/solo", "--", "--x.txt", "--y.txt"]);
    assert_eq!(
        request.paths,
        [
            std::path::PathBuf::from("--x.txt"),
            std::path::PathBuf::from("--y.txt")
        ]
    );
    assert!(request.solo);
}

#[test]
fn a_unix_path_is_not_read_as_a_switch() {
    let request = desktop(&["/home/user/left", "/home/user/right"]);
    assert_eq!(request.paths.len(), 2);
}

// -- scripts -----------------------------------------------------------------

#[test]
fn a_script_argument_collects_the_paths_after_it() {
    match parse(
        ["@run.txt", "/silent", "one", "two"]
            .iter()
            .map(|a| (*a).to_string()),
    )
    .expect("parses")
    {
        Invocation::Script(run) => {
            assert_eq!(run.file, PathBuf::from("run.txt"));
            assert_eq!(run.arguments, vec!["one".to_string(), "two".to_string()]);
            assert!(run.silent);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn two_script_arguments_are_refused() {
    assert!(parse(["@one.txt", "@two.txt"].map(String::from)).is_err());
}

#[test]
fn a_missing_script_file_returns_its_own_code() {
    let run = ca_cli::ScriptRun {
        file: PathBuf::from("no-such-script.txt"),
        arguments: Vec::new(),
        silent: true,
        close_script: false,
        read_only: ca_cli::ReadOnly::default(),
        dry_run: false,
    };
    assert_eq!(script::run(&run), exit::SCRIPT_NOT_LOADED);
}

#[test]
fn a_script_with_a_syntax_error_returns_its_own_code() {
    let dir = tempfile::tempdir().expect("temporary folder");
    let file = dir.path().join("run.txt");
    std::fs::write(&file, "wobble\n").expect("write");
    let run = ca_cli::ScriptRun {
        file,
        arguments: Vec::new(),
        silent: true,
        close_script: false,
        read_only: ca_cli::ReadOnly::default(),
        dry_run: false,
    };
    assert_eq!(script::run(&run), exit::SCRIPT_SYNTAX);
}

#[test]
fn a_script_that_cannot_open_its_folders_returns_its_own_code() {
    let dir = tempfile::tempdir().expect("temporary folder");
    let file = dir.path().join("run.txt");
    std::fs::write(&file, "load \"no-such-left\" \"no-such-right\"\n").expect("write");
    let run = ca_cli::ScriptRun {
        file,
        arguments: Vec::new(),
        silent: true,
        close_script: false,
        read_only: ca_cli::ReadOnly::default(),
        dry_run: false,
    };
    assert_eq!(script::run(&run), exit::SCRIPT_PATHS);
}

#[test]
fn a_script_that_runs_cleanly_returns_success() {
    let dir = tempfile::tempdir().expect("temporary folder");
    let file = dir.path().join("run.txt");
    std::fs::write(&file, "beep\n").expect("write");
    let run = ca_cli::ScriptRun {
        file,
        arguments: Vec::new(),
        silent: true,
        close_script: false,
        read_only: ca_cli::ReadOnly::default(),
        dry_run: false,
    };
    assert_eq!(script::run(&run), exit::SUCCESS);
}

#[test]
fn a_dry_run_script_writes_nothing() {
    let dir = tempfile::tempdir().expect("temporary folder");
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    std::fs::create_dir_all(&left).expect("left");
    std::fs::create_dir_all(&right).expect("right");
    std::fs::write(left.join("a.txt"), "one").expect("seed");
    let file = dir.path().join("run.txt");
    std::fs::write(
        &file,
        format!(
            "load \"{}\" \"{}\"\nselect all\ncopy left->right\n",
            left.display(),
            right.display()
        ),
    )
    .expect("write");
    let run = ca_cli::ScriptRun {
        file,
        arguments: Vec::new(),
        silent: true,
        close_script: false,
        read_only: ca_cli::ReadOnly::default(),
        dry_run: true,
    };
    assert_eq!(script::run(&run), exit::SUCCESS);
    assert!(!right.join("a.txt").exists());
}

#[test]
fn a_read_only_side_refuses_a_write() {
    let dir = tempfile::tempdir().expect("temporary folder");
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    std::fs::create_dir_all(&left).expect("left");
    std::fs::create_dir_all(&right).expect("right");
    std::fs::write(left.join("a.txt"), "one").expect("seed");
    let file = dir.path().join("run.txt");
    std::fs::write(
        &file,
        format!(
            "load \"{}\" \"{}\"\nselect all\ncopy left->right\n",
            left.display(),
            right.display()
        ),
    )
    .expect("write");
    let run = ca_cli::ScriptRun {
        file,
        arguments: Vec::new(),
        silent: true,
        close_script: false,
        read_only: ca_cli::ReadOnly {
            left: false,
            right: true,
        },
        dry_run: false,
    };
    assert_eq!(script::run(&run), exit::UNKNOWN_ERROR);
    assert!(!right.join("a.txt").exists());
}

/// A run of the script in `file` that prints nothing.
fn quiet_run(file: PathBuf) -> ca_cli::ScriptRun {
    ca_cli::ScriptRun {
        file,
        arguments: Vec::new(),
        silent: true,
        close_script: false,
        read_only: ca_cli::ReadOnly::default(),
        dry_run: false,
    }
}

/// The instant 2001-02-03 04:05:06 names when it is read as UTC.
const WALL_CLOCK_AS_UTC: i64 = 981_173_106;

/// A time a script names is a wall clock in the zone of the machine, as the
/// desktop program reads it.
#[test]
fn a_touch_with_a_wall_clock_time_stamps_that_time_in_the_machine_zone() {
    let dir = tempfile::tempdir().expect("temporary folder");
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    std::fs::create_dir_all(&left).expect("left");
    std::fs::create_dir_all(&right).expect("right");
    std::fs::write(left.join("a.txt"), "one").expect("seed");
    let file = dir.path().join("run.txt");
    std::fs::write(
        &file,
        format!(
            "load \"{}\" \"{}\"\nselect left.files\ntouch \"left:2001-02-03 04:05:06\"\n",
            left.display(),
            right.display()
        ),
    )
    .expect("write");
    assert_eq!(script::run(&quiet_run(file)), exit::SUCCESS);
    let modified = std::fs::metadata(left.join("a.txt"))
        .expect("metadata")
        .modified()
        .expect("modified");
    assert_eq!(
        ca_script::clock::unix_seconds(modified),
        WALL_CLOCK_AS_UTC - i64::from(ca_fs::local_offset_seconds())
    );
}

/// The log stamps and the clock variables are wall clocks in the zone of the
/// machine.
#[test]
fn the_log_and_the_clock_variables_read_the_machine_zone() {
    let dir = tempfile::tempdir().expect("temporary folder");
    let file = dir.path().join("run.txt");
    let named = dir.path().join("run-%fn_time%.log");
    std::fs::write(
        &file,
        format!("log normal \"{}\"\noption stop-on-error\n", named.display()),
    )
    .expect("write");
    let offset = i64::from(ca_fs::local_offset_seconds());
    let wall_clock = || ca_script::clock::unix_seconds(std::time::SystemTime::now()) + offset;
    let before = wall_clock();
    assert_eq!(script::run(&quiet_run(file)), exit::SUCCESS);
    let after = wall_clock();

    let log = (before..=after)
        .map(|seconds| {
            dir.path().join(format!(
                "run-{}.log",
                ca_script::clock::format_filename_time(seconds)
            ))
        })
        .find(|path| path.exists())
        .unwrap_or_else(|| panic!("no log is named with a local time from {before} to {after}"));
    let text = std::fs::read_to_string(log).expect("read the log");
    let line = text.lines().next().expect("a logged command");
    let logged = line
        .get(..19)
        .and_then(|stamp| ca_script::clock::parse_timestamp(stamp, 0))
        .expect("a stamp starts the line");
    assert!((before..=after).contains(&logged), "{line}");
}

// -- quick comparison --------------------------------------------------------

struct Pair {
    dir: tempfile::TempDir,
}

impl Pair {
    fn new(left: &[u8], right: &[u8]) -> Self {
        let dir = tempfile::tempdir().expect("temporary folder");
        std::fs::write(dir.path().join("left.txt"), left).expect("left");
        std::fs::write(dir.path().join("right.txt"), right).expect("right");
        Self { dir }
    }

    fn run(&self, kind: QuickKind) -> u8 {
        quick::compare(&QuickRun {
            kind,
            left: self.dir.path().join("left.txt"),
            right: self.dir.path().join("right.txt"),
        })
    }
}

#[test]
fn a_binary_comparison_of_identical_files_returns_one() {
    let pair = Pair::new(b"same", b"same");
    assert_eq!(pair.run(QuickKind::Binary), exit::BINARY_SAME);
    assert_eq!(pair.run(QuickKind::Crc), exit::BINARY_SAME);
    assert_eq!(pair.run(QuickKind::Size), exit::BINARY_SAME);
}

#[test]
#[cfg(windows)]
fn the_process_compares_a_path_with_an_unpaired_utf16_surrogate() {
    use std::os::windows::ffi::OsStringExt;
    use std::process::Command;

    let directory = tempfile::tempdir().expect("temporary folder");
    let mut left = directory.path().as_os_str().to_os_string();
    left.push("\\bad-");
    left.push(std::ffi::OsString::from_wide(&[0xD800]));
    left.push(".txt");
    let right = directory.path().join("right.txt");
    std::fs::write(PathBuf::from(&left), b"same").expect("non-Unicode file");
    std::fs::write(&right, b"same").expect("right file");

    let result = Command::new(env!("CARGO_BIN_EXE_ca"))
        .arg("/qc=binary")
        .arg(&left)
        .arg(right)
        .output()
        .expect("run the command line program");

    assert_eq!(result.status.code(), Some(i32::from(exit::BINARY_SAME)));
}

#[test]
fn an_automatic_merge_reports_conflicts_and_writes_only_when_forced() {
    use std::process::Command;

    let directory = tempfile::tempdir().expect("temporary folder");
    let left = directory.path().join("left.txt");
    let right = directory.path().join("right.txt");
    let center = directory.path().join("base.txt");
    let output = directory.path().join("merged.txt");
    std::fs::write(&left, b"left\n").expect("left");
    std::fs::write(&right, b"right\n").expect("right");
    std::fs::write(&center, b"base\n").expect("base");

    let conflict = Command::new(env!("CARGO_BIN_EXE_ca"))
        .arg(&left)
        .arg(&right)
        .arg(&center)
        .arg(&output)
        .arg("/automerge")
        .output()
        .expect("run automatic merge");
    assert_eq!(
        conflict.status.code(),
        Some(i32::from(exit::CONFLICTS_NO_OUTPUT))
    );
    assert!(!output.exists());

    let forced_output = directory.path().join("forced.txt");
    let forced = Command::new(env!("CARGO_BIN_EXE_ca"))
        .arg(&left)
        .arg(&right)
        .arg(&center)
        .arg(&forced_output)
        .arg("/automerge")
        .arg("/force")
        .output()
        .expect("run forced automatic merge");
    assert_eq!(forced.status.code(), Some(i32::from(exit::CONFLICTS)));
    assert!(std::fs::read_to_string(forced_output)
        .expect("read merge result")
        .contains("<<<<<<<"));
}

#[test]
fn an_unrun_desktop_comparison_does_not_return_success() {
    use std::process::Command;

    let directory = tempfile::tempdir().expect("temporary folder");
    let left = directory.path().join("left.txt");
    let right = directory.path().join("right.txt");
    std::fs::write(&left, b"left\n").expect("left");
    std::fs::write(&right, b"right\n").expect("right");
    let result = Command::new(env!("CARGO_BIN_EXE_ca"))
        .arg(left)
        .arg(right)
        .output()
        .expect("run console command line");
    assert_eq!(result.status.code(), Some(i32::from(exit::UNKNOWN_ERROR)));
}

#[test]
fn a_binary_comparison_of_differing_files_returns_eleven() {
    let pair = Pair::new(b"one", b"other");
    assert_eq!(pair.run(QuickKind::Binary), exit::BINARY_DIFFERENT);
    assert_eq!(pair.run(QuickKind::Crc), exit::BINARY_DIFFERENT);
    assert_eq!(pair.run(QuickKind::Size), exit::BINARY_DIFFERENT);
}

#[test]
fn a_rules_based_comparison_of_identical_files_returns_two() {
    let pair = Pair::new(b"one two\n", b"one two\n");
    assert_eq!(pair.run(QuickKind::RulesBased), exit::RULES_SAME);
}

#[test]
fn files_that_differ_only_in_white_space_return_twelve() {
    let pair = Pair::new(b"one two\n", b"one   two\n");
    assert_eq!(pair.run(QuickKind::RulesBased), exit::SIMILAR);
}

#[test]
fn a_rules_based_comparison_of_differing_files_returns_thirteen() {
    let pair = Pair::new(b"one\n", b"two\n");
    assert_eq!(pair.run(QuickKind::RulesBased), exit::RULES_DIFFERENT);
}

#[test]
fn a_missing_file_on_either_side_returns_one_hundred_and_seven() {
    let dir = tempfile::tempdir().expect("temporary folder");
    std::fs::write(dir.path().join("there.txt"), b"one").expect("write");
    let absent = dir.path().join("absent.txt");
    let there = dir.path().join("there.txt");
    for (left, right) in [(&absent, &there), (&there, &absent)] {
        let code = quick::compare(&QuickRun {
            kind: QuickKind::Binary,
            left: left.clone(),
            right: right.clone(),
        });
        assert_eq!(code, exit::SCRIPT_PATHS);
    }
}

#[test]
fn the_quick_switch_reads_every_type() {
    for (text, kind) in [
        ("/qc", QuickKind::RulesBased),
        ("/qc=size", QuickKind::Size),
        ("/qc=crc", QuickKind::Crc),
        ("/qc=binary", QuickKind::Binary),
        ("/quickcompare=rules-based", QuickKind::RulesBased),
    ] {
        match parse([text, "a", "b"].iter().map(|a| (*a).to_string())).expect("parses") {
            Invocation::Quick(run) => assert_eq!(run.kind, kind, "{text}"),
            other => panic!("{other:?}"),
        }
    }
    assert!(parse(["/qc=wobble", "a", "b"].map(String::from)).is_err());
    assert!(parse(["/qc", "a"].map(String::from)).is_err());
}

// -- help --------------------------------------------------------------------

#[test]
fn the_help_names_every_exit_code_and_every_switch() {
    let text = help::text();
    for entry in exit::TABLE {
        assert!(
            text.contains(&format!("  {:<4}{}", entry.code, entry.meaning)),
            "code {}",
            entry.code
        );
    }
    for switch in [
        "/automerge",
        "/center=",
        "/closescript",
        "/dry-run",
        "/edit",
        "/expandall",
        "/favorleft",
        "/favorright",
        "/filters=",
        "/force",
        "/fv=",
        "/iu",
        "/mergeoutput=",
        "/nobackups",
        "/qc",
        "/readonly",
        "/reviewconflicts",
        "/savetarget=",
        "/silent",
        "/solo",
        "/sync",
        "/title1=",
        "/vcs1=",
    ] {
        assert!(text.contains(switch), "{switch}");
    }
    assert!(text.contains("@<script file>"));
    assert!(text.contains("ca text <left> <right>"));
}
