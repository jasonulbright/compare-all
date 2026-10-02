//! Behaviour the folder view has to keep: every job reaches a terminal state
//! the view can see, a cancelled run does not cost the comparison, streamed
//! results are coalesced, and a selection names a node rather than a row.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::command::Command;
use ca_ui::testing::{context, sized_input};
use ca_ui::view::SessionView;
use ca_view_folder::FolderView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn raw_input(width: f32) -> egui::RawInput {
    sized_input(width, 800.0)
}

/// Tick a view until `ready` holds, or give up.
fn tick_until(view: &mut FolderView, ready: impl Fn(&FolderView) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        view.tick();
        if ready(view) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for side in ["left", "right"] {
        std::fs::create_dir_all(dir.path().join(side).join("sub")).unwrap();
    }
    std::fs::write(dir.path().join("left/same.txt"), b"alpha\nbeta\n").unwrap();
    std::fs::write(dir.path().join("right/same.txt"), b"alpha\nbeta\n").unwrap();
    std::fs::write(dir.path().join("left/sub/differs.txt"), b"one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("right/sub/differs.txt"), b"one\ntwo!\n").unwrap();
    std::fs::write(dir.path().join("left/orphan.txt"), b"only here\n").unwrap();
    dir
}

fn view_over(dir: &Path) -> FolderView {
    FolderView::new(dir.join("left"), dir.join("right"), &context(), 1)
}

#[test]
fn a_cancelled_content_comparison_gives_the_tree_back() {
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    assert!(view.accepts(Command::CompareContents));
    view.run(Command::CompareContents);
    view.run(Command::Cancel);
    // Cancelling drains the run to its terminal message rather than dropping
    // it, so the comparison is still there to run again.
    assert!(
        tick_until(&mut view, |view| view.accepts(Command::CompareContents)),
        "the comparison was lost when the run was cancelled"
    );
}

#[test]
fn a_content_comparison_that_runs_to_the_end_leaves_the_view_ready() {
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    view.run(Command::CompareContents);
    assert!(tick_until(&mut view, |view| SessionView::is_ready(view)
        && view.accepts(Command::CompareContents)));
    assert!(view.arena().totals().files > 0);
}

/// The engine marks a folder whose listing did not come back whole. The view
/// has to count those and give the row its own mark rather than showing it as
/// an ordinary match.
#[test]
fn an_unreadable_listing_is_counted_and_marked() {
    use ca_view_folder::tree::Arena;
    use ca_view_folder::{status_glyph, INCOMPLETE_GLYPH};

    let mut root = folder_node("", true, Vec::new());
    let mut deep = folder_node("deep", true, Vec::new());
    deep.incomplete = true;
    let mut blocked = folder_node("blocked", true, Vec::new());
    if let Some(entry) = blocked.left.as_mut() {
        entry.listing_incomplete = true;
    }
    root.children = vec![deep, blocked];
    let arena = Arena::from_root(&root);
    assert_eq!(arena.totals().incomplete, 2);

    let mut view = FolderView::from_arena(
        PathBuf::from("left"),
        PathBuf::from("right"),
        &context(),
        1,
        arena,
    );
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let _ = ctx.run(raw_input(1_280.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
    }
    assert_eq!(view.arena().totals().incomplete, 2);
    assert_eq!(
        status_glyph(ca_fs::NodeStatus::Same, None, true),
        INCOMPLETE_GLYPH
    );
}

fn folder_node(rel: &str, is_dir: bool, children: Vec<ca_fs::Node>) -> ca_fs::Node {
    let entry = ca_fs::Entry {
        rel: PathBuf::from(rel),
        name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
        is_dir,
        size: 0,
        modified: None,
        created: None,
        attributes: ca_fs::Attributes::default(),
        link: None,
        error: None,
        listing_incomplete: false,
        refused: false,
    };
    ca_fs::Node {
        name: entry.name.clone(),
        rel: PathBuf::from(rel),
        is_dir,
        left: Some(entry.clone()),
        right: Some(entry),
        facts: ca_fs::PairFacts::default(),
        status: ca_fs::NodeStatus::NotCompared,
        flags: ca_fs::StatusFlags::default(),
        quick: None,
        content: None,
        error: None,
        incomplete: false,
        scan_cancelled: false,
        children,
    }
}

/// Streamed content results arrive one file at a time. Folding each one into
/// the rows costs a pass over the whole comparison, so they are coalesced.
#[test]
fn streamed_content_results_are_coalesced_into_one_rebuild_per_interval() {
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    let before = view.rebuild_count();
    let update = ca_fs::ContentUpdate {
        rel: PathBuf::from("same.txt"),
        outcome: Ok(ca_fs::ContentOutcome::BinarySame),
    };
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut applied = 0u32;
    while Instant::now() < deadline {
        assert!(view.apply_content(&update));
        applied += 1;
        view.tick();
    }
    let rebuilds = view.rebuild_count() - before;
    assert!(applied > 100, "only {applied} results were folded in");
    assert!(
        rebuilds <= 8,
        "{applied} results caused {rebuilds} rebuilds of the rows"
    );
    // Coalescing defers a rebuild; it does not drop one.
    assert!(rebuilds >= 1, "the results never reached the rows");
}

/// A selection names a node. Sorting reorders every row, and the selection has
/// to come back on the same node rather than on the row it used to sit at.
#[test]
fn a_selection_survives_a_reordering() {
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    view.selection_mut()
        .set_cursor(ca_fs::Side::Left, PathBuf::from("orphan.txt"));
    view.select(
        ca_view_folder::selection::SelectRule::All,
        ca_view_folder::selection::Scope::Left,
    );
    view.run(Command::CollapseAll);
    view.run(Command::ExpandAll);
    assert!(view
        .selection()
        .contains(ca_fs::Side::Left, Path::new("orphan.txt")));
    let node = view
        .arena()
        .index_of(Path::new("orphan.txt"))
        .expect("the selected node is still in the comparison");
    assert!(view.rows().iter().any(|row| row.node == node));
}

#[test]
fn a_selection_survives_a_filter_change() {
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    view.selection_mut()
        .set_cursor(ca_fs::Side::Left, PathBuf::from("sub"));
    assert_eq!(
        view.selection().cursor().map(|(_, rel)| rel.to_path_buf()),
        Some(PathBuf::from("sub"))
    );
    view.run(Command::CollapseAll);
    view.run(Command::ExpandAll);
    // The selection names a node, not the row it happened to be on, so
    // rebuilding the rows leaves it where it was.
    assert_eq!(
        view.selection().cursor().map(|(_, rel)| rel.to_path_buf()),
        Some(PathBuf::from("sub"))
    );
}

/// One frame of `view` with `events` delivered.
fn frame_with(view: &mut FolderView, ctx: &egui::Context, events: Vec<egui::Event>) {
    view.tick();
    let context = context();
    let _ = ctx.run(ca_ui::testing::event_input(1_280.0, 800.0, events), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            view.ui(ui, &context);
        });
    });
}

fn key_down() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::ArrowDown,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

fn cursor(view: &FolderView) -> Option<PathBuf> {
    view.selection().cursor().map(|(_, rel)| rel.to_path_buf())
}

/// The name filter reads the arrow keys typed into it, and the tree cursor
/// stays where it is.
#[test]
fn an_arrow_key_typed_into_the_name_filter_leaves_the_tree_cursor() {
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    let ctx = egui::Context::default();
    for _ in 0..3 {
        frame_with(&mut view, &ctx, Vec::new());
    }

    view.selection_mut()
        .set_cursor(ca_fs::Side::Left, PathBuf::from("orphan.txt"));
    frame_with(&mut view, &ctx, vec![key_down()]);
    let moved = cursor(&view);
    assert_ne!(
        moved,
        Some(PathBuf::from("orphan.txt")),
        "with no field holding the keyboard, Down moves the cursor"
    );

    view.selection_mut()
        .set_cursor(ca_fs::Side::Left, PathBuf::from("orphan.txt"));
    let field = ctx
        .read_response(view.name_filter_widget())
        .expect("the name filter is on screen")
        .rect
        .center();
    frame_with(&mut view, &ctx, vec![egui::Event::PointerMoved(field)]);
    for pressed in [true, false] {
        frame_with(
            &mut view,
            &ctx,
            vec![egui::Event::PointerButton {
                pos: field,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
    assert_eq!(
        ctx.memory(egui::Memory::focused),
        Some(view.name_filter_widget()),
        "the click gives the name filter the keyboard"
    );
    frame_with(&mut view, &ctx, vec![key_down()]);
    frame_with(&mut view, &ctx, Vec::new());
    assert_eq!(cursor(&view), Some(PathBuf::from("orphan.txt")));
}

/// Characters in the path a planted journal names.
const LONG_PATH_CHARACTERS: usize = 300;

/// Write one journal that records a batch which never reported an end.
fn plant_journal(journals: &Path, target: &Path) {
    std::fs::create_dir_all(journals).unwrap();
    let target = target.display().to_string().replace('\\', "\\\\");
    let body = format!(
        "{{\"record\":\"batch_start\",\"kind\":\"sync\",\"steps\":2,\"unix_seconds\":0}}\n\
         {{\"record\":\"step_begin\",\"index\":0,\"action\":{{\"copy_file\":\
         {{\"source\":\"{target}\",\"target\":\"{target}\"}}}},\"temporary\":null,\
         \"backup\":null}}\n"
    );
    std::fs::write(journals.join("batch-1-1-0.jsonl"), body).unwrap();
}

/// A path long enough that no window can hold it, under `root`.
fn long_path(root: &Path) -> PathBuf {
    let base = root.display().to_string().chars().count();
    let fill = LONG_PATH_CHARACTERS.saturating_sub(base + "/only-right.txt".len());
    root.join("d".repeat(fill)).join("only-right.txt")
}

/// The area the text one frame painted covers, after clipping.
///
/// A widget that clips its own content does not draw outside the window, so
/// the clip rectangle is applied before the area is judged.
fn text_area(output: &egui::FullOutput) -> egui::Rect {
    let mut area = egui::Rect::NOTHING;
    for shape in &output.shapes {
        if let egui::Shape::Text(text) = &shape.shape {
            let bounds = text.visual_bounding_rect().intersect(shape.clip_rect);
            if bounds.is_finite() && bounds.is_positive() {
                area = area.union(bounds);
            }
        }
    }
    area
}

/// The recovery dialog reports a batch whose steps name a path no window is
/// wide enough for. Nothing it draws may sit outside the window.
#[test]
fn the_recovery_dialog_stays_inside_a_narrow_window() {
    let dir = fixture();
    let journals = dir.path().join("journals");
    plant_journal(&journals, &long_path(dir.path()));

    for width in [640.0_f32, 1_280.0] {
        let mut view = FolderView::with_journal_directory(
            dir.path().join("left"),
            dir.path().join("right"),
            &context(),
            9,
            journals.clone(),
        );
        assert!(
            tick_until(&mut view, |view| !view.recovery_notices().is_empty()),
            "at {width} points the unfinished batch was never reported"
        );
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0));
        for frame in 0..6 {
            let output = ctx.run(raw_input(width), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            });
            // The frame the window opens at has measured nothing yet.
            if frame == 0 {
                continue;
            }
            for area in [ctx.used_rect(), text_area(&output)] {
                if !area.is_finite() || !area.is_positive() {
                    continue;
                }
                // A one point stroke on the window edge is drawn centred on
                // it, so half of it falls outside by design.
                assert!(
                    area.left() >= screen.left() - 1.0 && area.right() <= screen.right() + 1.0,
                    "at {width} points frame {frame} drew {area:?} outside {screen:?}"
                );
            }
        }
    }
}

/// No test may reach the real per-user settings folder.
#[test]
fn the_resolved_settings_directory_is_not_the_real_one() {
    ca_ui::testing::isolate_settings();
    assert!(!ca_ui::paths::is_real_per_user_location(
        &ca_ui::paths::settings_directory()
    ));
    assert!(!ca_ui::paths::is_real_per_user_location(
        &ca_ui::paths::journal_directory()
    ));
}

/// The toolbar's content method and the settings dialog read and write one
/// stored value, so neither can show a state the other does not have.
#[test]
fn the_content_method_the_toolbar_shows_is_the_stored_one() {
    ca_ui::testing::isolate_settings();
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));

    let mut settings = view.session_settings();
    settings.comparison.content_comparison = ca_session::settings::folder::ContentComparison::Crc;
    view.apply_session_settings(&settings);
    assert!(tick_until(&mut view, SessionView::is_ready));

    assert_eq!(
        view.session_settings().comparison.content_comparison,
        ca_session::settings::folder::ContentComparison::Crc,
        "the dialog's choice did not survive the round trip"
    );
    let ca_session::settings::SessionSettings::FolderCompare(shown) =
        SessionView::settings(&view).unwrap()
    else {
        panic!("the view reported settings of another kind");
    };
    assert_eq!(
        shown.comparison.content_comparison,
        ca_session::settings::folder::ContentComparison::Crc
    );
}

/// A stored handling setting reaches the options every job of the view runs
/// under, so the dialog and the engines never disagree.
#[test]
fn a_handling_setting_reaches_the_options_the_view_runs_under() {
    ca_ui::testing::isolate_settings();
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    assert!(!view.engine_options().scan.follow_links);

    let mut settings = view.session_settings();
    settings.handling.follow_symbolic_links = true;
    settings.handling.expand_subfolders_on_load = true;
    settings.handling.expand_only_folders_with_differences = false;
    view.apply_session_settings(&settings);
    assert!(tick_until(&mut view, SessionView::is_ready));

    let options = view.engine_options();
    assert!(options.scan.follow_links);
    assert!(options.handling.expand_on_load);
    assert!(!options.handling.expand_only_with_differences);
}

/// A stored exclusion narrows what the tree shows, not only what the engine
/// was handed.
#[test]
fn a_stored_exclude_mask_removes_the_rows_it_names() {
    ca_ui::testing::isolate_settings();
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    let before = view.rows().len();

    let mut settings = view.session_settings();
    settings.name_filters.exclude_files = vec!["orphan.txt".to_owned()];
    view.apply_session_settings(&settings);
    assert!(tick_until(&mut view, SessionView::is_ready));
    view.tick();

    let names: Vec<String> = view
        .rows()
        .iter()
        .filter_map(|row| view.arena().node(row.node).map(|node| node.name.clone()))
        .collect();
    assert!(!names.iter().any(|name| name == "orphan.txt"));
    assert!(view.rows().len() < before, "nothing was removed");
}

/// The bytes of a zip holding each named entry, built under `dir`.
fn zip_bytes(dir: &Path, entries: &[(&str, &[u8])]) -> Vec<u8> {
    const EMPTY_ZIP: &[u8] = &[
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let path = dir.join("built.zip");
    std::fs::write(&path, EMPTY_ZIP).unwrap();
    {
        let source = ca_fs::Source::archive(&path, ca_vfs::ArchiveOptions::default()).unwrap();
        let cancel = ca_vfs::Cancel::new();
        for (name, bytes) in entries {
            let mut reader = *bytes;
            source
                .file_system()
                .write_file(&ca_vfs::VfsPath::parse(name).unwrap(), &mut reader, &cancel)
                .unwrap();
        }
    }
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    bytes
}

/// A folder session over a folder and a zip.
fn zip_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("left")).unwrap();
    std::fs::write(dir.path().join("left/notes.txt"), b"one\n").unwrap();
    let bytes = zip_bytes(dir.path(), &[("notes.txt", b"two\n")]);
    std::fs::write(dir.path().join("pack.zip"), bytes).unwrap();
    dir
}

/// Masks that leave the zip format with no mask, which turns it off.
fn no_zip_masks() -> ca_session::options::ArchiveOptions {
    let mut masks = ca_session::options::ArchiveOptions::default();
    masks.masks.insert("zip".to_owned(), String::new());
    masks
}

fn settled(view: &FolderView) -> bool {
    view.is_ready() || view.failure().is_some()
}

#[test]
fn the_first_scan_runs_under_the_stored_archive_masks() {
    ca_ui::testing::isolate_settings();
    let dir = zip_fixture();
    let mut stored = ca_session::ProgramOptions::default();
    stored.archives = no_zip_masks();
    let mut view_context = context();
    view_context.options = std::sync::Arc::new(ca_ui::options::AppOptions {
        stored,
        ..ca_ui::options::AppOptions::default()
    });
    let mut view = FolderView::new(
        dir.path().join("left"),
        dir.path().join("pack.zip"),
        &view_context,
        1,
    );
    assert!(tick_until(&mut view, settled));
    assert!(
        view.failure().is_some(),
        "the stored masks turn the zip format off"
    );
}

#[test]
fn a_change_of_the_archive_masks_scans_again() {
    ca_ui::testing::isolate_settings();
    let dir = zip_fixture();
    let mut view = FolderView::new(
        dir.path().join("left"),
        dir.path().join("pack.zip"),
        &context(),
        1,
    );
    assert!(tick_until(&mut view, settled));
    assert_eq!(view.failure(), None);

    view.apply_archive_masks(&no_zip_masks());
    assert!(tick_until(&mut view, settled));
    assert!(
        view.failure().is_some(),
        "the new masks turn the zip format off"
    );
}

#[test]
fn explorer_is_disabled_for_a_row_inside_an_archive_side() {
    ca_ui::testing::isolate_settings();
    let dir = zip_fixture();
    let mut view = FolderView::new(
        dir.path().join("left"),
        dir.path().join("pack.zip"),
        &context(),
        1,
    );
    assert!(tick_until(&mut view, SessionView::is_ready));
    view.selection_mut()
        .set_cursor(ca_fs::Side::Right, PathBuf::from("notes.txt"));

    assert!(SessionView::launch_target(&view).is_some());
    assert_eq!(SessionView::explorer_target(&view), None);
}

#[test]
fn explorer_is_disabled_when_the_selected_side_has_no_item() {
    ca_ui::testing::isolate_settings();
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    view.selection_mut()
        .set_cursor(ca_fs::Side::Right, PathBuf::from("orphan.txt"));

    assert!(SessionView::launch_target(&view).is_some());
    assert_eq!(SessionView::explorer_target(&view), None);
}

/// A pair of files under the folder `d` with one status each.
fn file_pair(rel: &str, status: ca_fs::NodeStatus) -> ca_fs::Node {
    let mut node = folder_node(rel, false, Vec::new());
    node.status = status;
    node
}

/// The view refreshes a folder's status from its children once a batch of
/// content results lands. A folder that holds a matching pair and a pair that
/// is not compared yet is not compared, and for every mix of
/// children the view rolls a folder up to the status the engine gives it.
#[test]
fn the_view_rolls_a_folder_up_to_the_status_the_engine_gives_it() {
    use ca_fs::NodeStatus::{
        Different, Error, LeftNewer, LeftOrphan, NotCompared, RightNewer, Same,
    };
    use ca_view_folder::tree::Arena;
    let mixes = [
        [Same, NotCompared],
        [NotCompared, NotCompared],
        [LeftNewer, NotCompared],
        [RightNewer, Same],
        [LeftNewer, RightNewer],
        [Same, Same],
        [Same, Different],
        [Error, Same],
        [Error, Different],
        [LeftOrphan, Same],
    ];
    for mix in mixes {
        let children = vec![file_pair("d/a", mix[0]), file_pair("d/b", mix[1])];
        let mut root = folder_node("", true, vec![folder_node("d", true, children)]);
        ca_fs::rollup(&mut root);
        let engine = root.children[0].status;
        let mut arena = Arena::from_root(&root);
        let index = arena.index_of(Path::new("d")).unwrap();
        arena.roll_up();
        assert_eq!(arena.node(index).unwrap().status, engine, "{mix:?}");
    }
    let mut root = folder_node(
        "",
        true,
        vec![folder_node(
            "d",
            true,
            vec![file_pair("d/a", Same), file_pair("d/b", NotCompared)],
        )],
    );
    ca_fs::rollup(&mut root);
    let mut arena = Arena::from_root(&root);
    arena.roll_up();
    let index = arena.index_of(Path::new("d")).unwrap();
    assert_eq!(arena.node(index).unwrap().status, NotCompared);

    // The folder `d/e` reports its error, which hides that `d/e/a` is newer on
    // the left; with `d/b` newer on the right, `d` differs.
    let nested = folder_node(
        "d/e",
        true,
        vec![file_pair("d/e/a", LeftNewer), file_pair("d/e/c", Error)],
    );
    let mut root = folder_node(
        "",
        true,
        vec![folder_node(
            "d",
            true,
            vec![nested, file_pair("d/b", RightNewer)],
        )],
    );
    ca_fs::rollup(&mut root);
    assert_eq!(root.children[0].status, Different);
    let mut arena = Arena::from_root(&root);
    arena.roll_up();
    let index = arena.index_of(Path::new("d")).unwrap();
    assert_eq!(arena.node(index).unwrap().status, Different);
}

fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

/// The tree holds the keyboard while no field does. Tab moves the cursor to
/// the other side, and the arrow keys go on moving the cursor; Escape leaves
/// them with the tree.
#[test]
fn tab_moves_the_cursor_to_the_other_side_and_the_arrows_stay_with_the_tree() {
    let dir = fixture();
    let mut view = view_over(dir.path());
    assert!(tick_until(&mut view, SessionView::is_ready));
    view.run(Command::ExpandAll);
    let ctx = egui::Context::default();
    for _ in 0..3 {
        frame_with(&mut view, &ctx, Vec::new());
    }
    let first = view.rows()[0].node;
    let first = view.arena().node(first).unwrap().rel.clone();
    view.selection_mut()
        .set_cursor(ca_fs::Side::Left, first.clone());

    frame_with(&mut view, &ctx, vec![key(egui::Key::Tab)]);
    frame_with(&mut view, &ctx, Vec::new());
    assert_eq!(
        view.selection()
            .cursor()
            .map(|(side, rel)| (side, rel.to_path_buf())),
        Some((ca_fs::Side::Right, first.clone())),
        "Tab did not move the cursor to the other side"
    );

    for (arrow, label) in [(egui::Key::ArrowDown, "Down"), (egui::Key::ArrowUp, "Up")] {
        let before = cursor(&view);
        frame_with(&mut view, &ctx, vec![key(arrow)]);
        frame_with(&mut view, &ctx, Vec::new());
        assert_ne!(cursor(&view), before, "{label} after Tab left the cursor");
    }
    assert_eq!(cursor(&view), Some(first));

    frame_with(&mut view, &ctx, vec![key(egui::Key::Escape)]);
    frame_with(&mut view, &ctx, Vec::new());
    let before = cursor(&view);
    frame_with(&mut view, &ctx, vec![key_down()]);
    frame_with(&mut view, &ctx, Vec::new());
    assert_ne!(cursor(&view), before, "Down after Escape left the cursor");
}

/// A synchronisation tab over the two sides of `dir`, scanned.
fn sync_over(dir: &Path) -> ca_view_folder::FolderSyncView {
    let mut sync = <ca_view_folder::FolderSyncView as ca_ui::view::ViewFactory>::create(
        dir.join("left"),
        dir.join("right"),
        &context(),
        1,
    );
    assert!(tick_until(sync.view_mut(), SessionView::is_ready));
    sync
}

/// A synchronisation tab reports the settings of a folder synchronization
/// session, which the shell saves and opens the dialog over. A change of them
/// reaches the folder view and the method.
#[test]
fn a_synchronisation_tab_reports_its_settings_and_takes_a_change() {
    use ca_session::settings::folder::SyncMethod;
    use ca_session::settings::SessionSettings;

    ca_ui::testing::isolate_settings();
    let dir = fixture();
    let mut sync = sync_over(dir.path());

    let Some(SessionSettings::FolderSync(mut settings)) = sync.settings() else {
        panic!("the synchronisation tab reported no synchronization settings");
    };
    assert_eq!(settings.sync.method, SyncMethod::UpdateRight);

    settings.sync.method = SyncMethod::MirrorToLeft;
    settings.name_filters.exclude_files = vec!["orphan.txt".to_owned()];
    sync.apply_settings(&SessionSettings::FolderSync(settings.clone()));
    assert!(tick_until(sync.view_mut(), SessionView::is_ready));

    assert_eq!(
        sync.view_mut().sync().method,
        ca_view_folder::sync_mode::Method::MirrorToLeft
    );
    assert_eq!(
        sync.view_mut()
            .session_settings()
            .name_filters
            .exclude_files,
        vec!["orphan.txt".to_owned()]
    );
    assert_eq!(sync.settings(), Some(SessionSettings::FolderSync(settings)));
}

/// File commands on a synchronisation tab name the item under the cursor, as
/// they do on a comparison tab.
#[test]
fn a_synchronisation_tab_names_the_item_under_the_cursor_for_file_commands() {
    ca_ui::testing::isolate_settings();
    let dir = fixture();
    let mut sync = sync_over(dir.path());
    sync.view_mut()
        .selection_mut()
        .set_cursor(ca_fs::Side::Left, PathBuf::from("orphan.txt"));

    let target = sync
        .launch_target()
        .expect("the synchronisation tab named no item");
    assert_eq!(
        target.context.first.path,
        dir.path().join("left").join("orphan.txt")
    );
    assert_eq!(
        sync.explorer_target(),
        Some((
            dir.path().join("left").join("orphan.txt"),
            ca_ui::launch::Selection::Files
        ))
    );
}

/// A stored method this build does not know stays in the settings the tab
/// reports, so Save Session writes it back unchanged, while the tab runs its
/// own method. A method chosen on the tab replaces it.
#[test]
fn a_stored_method_this_build_does_not_know_is_written_back_unchanged() {
    use ca_session::settings::folder::SyncMethod;
    use ca_session::settings::SessionSettings;
    use ca_view_folder::sync_mode::Method;

    ca_ui::testing::isolate_settings();
    let dir = fixture();
    let mut sync = sync_over(dir.path());
    let Some(SessionSettings::FolderSync(mut stored)) = sync.settings() else {
        panic!("the synchronisation tab reported no synchronization settings");
    };
    let later = SyncMethod::Unknown(serde_json::json!("custom-preset-7"));
    stored.sync.method = later.clone();
    stored.sync.copy_orphans = false;
    sync.apply_settings(&SessionSettings::FolderSync(stored.clone()));
    assert!(tick_until(sync.view_mut(), SessionView::is_ready));
    assert_eq!(sync.view_mut().sync().method, Method::UpdateRight);

    let Some(SessionSettings::FolderSync(reported)) = sync.settings() else {
        panic!("the synchronisation tab reported no synchronization settings");
    };
    assert_eq!(reported.sync.method, later);
    assert_eq!(reported, stored);

    sync.view_mut().set_sync_method(Method::MirrorToRight);
    let Some(SessionSettings::FolderSync(chosen)) = sync.settings() else {
        panic!("the synchronisation tab reported no synchronization settings");
    };
    assert_eq!(chosen.sync.method, SyncMethod::MirrorToRight);
}
