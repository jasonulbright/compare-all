//! Registry, version and media comparison views.
//!
//! The three comparisons share one display: two panes side by side over one
//! row model, keys and values aligned by name, each key painted by what it
//! holds and each value by how its two sides relate. They differ in the engine
//! that reads a side and in the settings that decide importance, which
//! [`flavor::Flavor`] names.
//!
//! Reading, comparing and flattening all run on a worker. The frame thread
//! reads the result the worker posted and paints it. The version and media
//! views read only. The registry view edits: each command adds a step to a
//! plan that the panes show at once, and Save writes the plan to an export
//! file or, after a confirmation, to a live key. See [`view::editing`].

pub mod editor;
pub mod flavor;
pub mod history;
pub mod jobs;
pub mod model;
pub mod search;
pub mod session_options;
pub mod text;
pub mod thumbnail;
pub mod view;

#[cfg(test)]
pub(crate) mod testing;

pub use flavor::Flavor;
pub use view::RecordsView;

use ca_session::settings::SessionSettings;
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::view::{CommandState, SessionView, ViewAction, ViewContext};
use std::path::PathBuf;

/// Declares one public view type over the shared record view.
macro_rules! record_view {
    ($(#[$doc:meta])* $name:ident, $flavor:expr) => {
        $(#[$doc])*
        pub struct $name(RecordsView);

        impl $name {
            /// A tab over the two sides, with the comparison already started.
            #[must_use]
            pub fn new(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
                Self(RecordsView::new($flavor, left, right, context, instance))
            }

            /// Which kind of session this view answers for.
            #[must_use]
            pub const fn kind() -> SessionKind {
                $flavor.kind()
            }

            /// The shared view behind this tab.
            #[must_use]
            pub const fn view(&self) -> &RecordsView {
                &self.0
            }

            /// The shared view behind this tab.
            pub const fn view_mut(&mut self) -> &mut RecordsView {
                &mut self.0
            }
        }

        impl ca_ui::view::ViewFactory for $name {
            fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
                Self::new(left, right, context, instance)
            }
        }

        impl SessionView for $name {
            fn kind(&self) -> Option<ca_session::SessionKind> { self.0.kind() }
            fn title(&self) -> String {
                self.0.title()
            }

            fn menu_view(&self) -> ca_ui::command::MenuView {
                self.0.menu_view()
            }

            fn tick(&mut self) {
                self.0.tick();
            }

            fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
                self.0.ui(ui, context)
            }

            fn commands(&self) -> Vec<CommandState> {
                self.0.commands()
            }

            fn accepts(&self, command: Command) -> bool {
                self.0.accepts(command)
            }

            fn run(&mut self, command: Command) {
                self.0.run(command);
            }

            fn apply_settings(&mut self, settings: &SessionSettings) {
                self.0.apply_settings(settings);
            }

            fn settings(&self) -> Option<SessionSettings> {
                self.0.settings()
            }

            fn holds_temporaries(&self) -> bool {
                self.0.holds_temporaries()
            }

            fn is_ready(&self) -> bool {
                self.0.is_ready()
            }

            fn notice(&self) -> Option<String> {
                self.0.notice()
            }

            fn launch_target(&self) -> Option<ca_ui::launch::LaunchTarget> {
                self.0.launch_target()
            }

            fn explorer_target(
                &self,
            ) -> Option<(std::path::PathBuf, ca_ui::launch::Selection)> {
                self.0.explorer_target()
            }

            fn wants_close(&self) -> bool {
                self.0.wants_close()
            }

            fn may_close(&mut self) -> bool {
                self.0.may_close()
            }

            fn is_busy(&self) -> bool {
                self.0.is_busy()
            }

            fn holds_unwritten_edits(&self) -> bool {
                self.0.holds_unwritten_edits()
            }

            fn on_close(&mut self) {
                self.0.on_close();
            }
        }
    };
}

record_view!(
    /// The Registry Compare tab: two export files or two live keys, with edits
    /// that Save writes back.
    RegistryView,
    Flavor::Registry
);

record_view!(
    /// The Version Compare tab: the version resources of two Windows binaries.
    VersionView,
    Flavor::Version
);

record_view!(
    /// The Media Compare tab: the tags and stream facts of two media files.
    MediaView,
    Flavor::Media
);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{MediaView, RecordsView, RegistryView, VersionView};
    use crate::model::{DisplayFilter, Side};
    use crate::testing;
    use crate::view::editing::{Panel, Prompt};
    use ca_records::registry::{RegFile, ValueData, ValueKind};
    use ca_records::Limits;
    use ca_session::settings::SessionSettings;
    use ca_ui::command::Command;
    use ca_ui::report::Payload;
    use ca_ui::testing::{context, event_input, sized_input, wait_until};
    use ca_ui::theme::records::RecordClass;
    use ca_ui::view::SessionView;
    use std::path::PathBuf;
    use std::time::Duration;

    fn frame(view: &mut dyn SessionView, ctx: &egui::Context, width: f32, height: f32) {
        let _ = ctx.run(sized_input(width, height), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
    }

    /// Run frames until the view has settled, and report whether it holds a
    /// comparison.
    fn settle<V: SessionView>(view: &mut V, inner: fn(&V) -> &RecordsView) -> bool {
        let ctx = egui::Context::default();
        assert!(
            wait_until(Duration::from_secs(30), || {
                frame(view, &ctx, 1_280.0, 800.0);
                view.is_ready()
            }),
            "the comparison never settled"
        );
        inner(view).has_comparison()
    }

    fn registry(dir: &std::path::Path) -> RegistryView {
        let (left, right) = testing::registry_pair(dir);
        RegistryView::new(left, right, &context(), 1)
    }

    fn copy_from_command<V: SessionView>(view: &mut V) -> String {
        let ctx = egui::Context::default();
        let mut copied = None;
        assert!(
            wait_until(Duration::from_secs(10), || {
                view.tick();
                let output = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        view.ui(ui, &context());
                    });
                });
                copied = output
                    .platform_output
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        egui::OutputCommand::CopyText(text) => Some(text.clone()),
                        _ => None,
                    });
                copied.is_some()
            }),
            "the asynchronous copy did not reach the clipboard"
        );
        copied.unwrap()
    }

    fn row_named(view: &RecordsView, name: &str) -> usize {
        (0..view.listing().rows())
            .find(|row| view.listing().node_at(*row).unwrap().name == name)
            .unwrap_or_else(|| panic!("no row named {name}"))
    }

    #[test]
    fn two_export_files_align_by_name_with_keys_rolled_up() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let inner = view.view();
        let listing = inner.listing();
        let sample = row_named(inner, "Sample");
        assert_eq!(listing.class_at(sample), Some(RecordClass::Different));
        let changed = row_named(inner, "Changed");
        assert_eq!(listing.class_at(changed), Some(RecordClass::Different));
        let gone = row_named(inner, "Gone");
        assert_eq!(listing.class_at(gone), Some(RecordClass::Orphan));
        let gone_node = listing.node_at(gone).unwrap();
        assert!(gone_node.is_on(Side::Left) && !gone_node.is_on(Side::Right));
        let left_only = row_named(inner, "LeftOnly");
        assert_eq!(listing.class_at(left_only), Some(RecordClass::Orphan));
        let kept = row_named(inner, "Kept");
        assert_eq!(listing.class_at(kept), Some(RecordClass::Same));
        assert_eq!(view.title(), "left.reg - right.reg");
    }

    #[test]
    fn a_record_tab_exposes_both_files_to_the_shell_open_with_menu() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::version_pair(dir.path());
        let mut view = VersionView::new(left.clone(), right.clone(), &context(), 922);

        let target = SessionView::launch_target(&view).expect("the file view has two paths");

        assert_eq!(target.selection, ca_ui::launch::Selection::Files);
        assert_eq!(target.context.first.path, left);
        assert_eq!(target.context.second.expect("the right path").path, right);

        view.view_mut().set_active_side(Side::Right);
        assert_eq!(
            SessionView::explorer_target(&view),
            Some((right, ca_ui::launch::Selection::Files))
        );
    }

    #[test]
    fn the_view_lands_on_the_first_difference_and_walks_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let first = view.view().listing().cursor();
        assert!(view.view().listing().is_stop(first));
        view.run(Command::NextDifference);
        assert!(view.view().listing().cursor() > first);
        view.run(Command::PreviousDifference);
        assert_eq!(view.view().listing().cursor(), first);
    }

    #[test]
    fn the_display_filter_commands_reach_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let all = view.view().listing().rows();
        view.run(Command::ShowDifferences);
        let differences = view.view().listing().rows();
        assert!(differences < all);
        view.run(Command::ShowSame);
        assert_eq!(view.view().listing().filter(), DisplayFilter::Same);
        view.run(Command::ShowNone);
        assert_eq!(view.view().listing().filter(), DisplayFilter::None);
        assert_eq!(view.view().listing().rows(), 0);
        view.run(Command::ShowAll);
        assert_eq!(view.view().listing().rows(), all);
    }

    #[test]
    fn collapse_all_and_expand_all_change_the_rows_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let all = view.view().listing().rows();
        view.run(Command::CollapseAll);
        assert_eq!(view.view().listing().rows(), 1, "only the hive shows");
        view.run(Command::ExpandAll);
        assert_eq!(view.view().listing().rows(), all);
    }

    #[test]
    fn a_click_on_the_mark_of_a_key_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let ctx = egui::Context::default();
        frame(&mut view, &ctx, 1_280.0, 800.0);
        assert!(view.view().listing().rows() > 1);
        // The first row is the hive; its mark sits at the start of the name
        // column of the left pane.
        let rows = view.view().rows_area();
        let height = view.view().row_height();
        let pos = egui::pos2(rows.left() + 10.0, rows.top() + height / 2.0);
        let click = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        let _ = ctx.run(event_input(1_280.0, 800.0, click), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        assert_eq!(view.view().listing().cursor(), 0);
        assert_eq!(view.view().listing().rows(), 1, "the click closed the hive");
    }

    #[test]
    fn keyboard_navigation_opens_and_closes_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let inner = view.view_mut();
        inner.set_cursor(0);
        inner.navigate(egui::Key::ArrowLeft, 5);
        assert_eq!(inner.listing().rows(), 1);
        inner.navigate(egui::Key::ArrowRight, 5);
        assert!(inner.listing().rows() > 1);
        inner.navigate(egui::Key::End, 5);
        assert_eq!(inner.listing().cursor(), inner.listing().rows() - 1);
        inner.navigate(egui::Key::ArrowLeft, 5);
        let parent = inner.listing().node_at(inner.listing().cursor()).unwrap();
        assert!(parent.is_group);
    }

    #[test]
    fn two_same_files_show_no_difference() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_same(dir.path());
        let mut view = RegistryView::new(left, right, &context(), 2);
        assert!(settle(&mut view, RegistryView::view));
        let counts = view.view().listing().counts();
        assert_eq!(counts.same, 1);
        assert_eq!(counts.different + counts.left_only + counts.right_only, 0);
        assert!(view.view().listing().next_difference(0).is_none());
    }

    #[test]
    fn a_missing_file_is_reported_rather_than_painted_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = RegistryView::new(
            dir.path().join("absent-left.reg"),
            dir.path().join("absent-right.reg"),
            &context(),
            3,
        );
        assert!(!settle(&mut view, RegistryView::view));
        assert!(view.view().failure().is_some());
        assert!(view.notice().is_some());
    }

    #[test]
    fn a_file_that_is_not_an_export_is_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.reg");
        std::fs::write(&left, "not a registry export\r\n").unwrap();
        let mut view = RegistryView::new(left.clone(), left, &context(), 4);
        assert!(!settle(&mut view, RegistryView::view));
        assert!(view.view().failure().unwrap().contains("left.reg"));
    }

    #[cfg(windows)]
    #[test]
    fn an_absent_live_key_is_reported_and_nothing_is_written() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let key = format!(
            r"reg:\\HKEY_CURRENT_USER\Software\compare-all-tests\{}-{nonce}",
            std::process::id()
        );
        let mut view = RegistryView::new(PathBuf::from(&key), PathBuf::from(&key), &context(), 5);
        assert!(!settle(&mut view, RegistryView::view));
        let failure = view.view().failure().unwrap();
        assert!(failure.contains("compare-all-tests"), "{failure}");
    }

    #[test]
    fn swapping_sides_reads_them_again_the_other_way_round() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let gone = row_named(view.view(), "Gone");
        assert!(view
            .view()
            .listing()
            .node_at(gone)
            .unwrap()
            .is_on(Side::Left));
        view.run(Command::SwapSides);
        assert!(settle(&mut view, RegistryView::view));
        let gone = row_named(view.view(), "Gone");
        let node = view.view().listing().node_at(gone).unwrap();
        assert!(node.is_on(Side::Right) && !node.is_on(Side::Left));
        assert_eq!(view.view().left().file_name().unwrap(), "right.reg");
    }

    #[test]
    fn two_binaries_compare_their_version_resources() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::version_pair(dir.path());
        let mut view = VersionView::new(left, right, &context(), 6);
        assert!(settle(&mut view, VersionView::view));
        let inner = view.view();
        let file_version = row_named(inner, "FileVersion");
        assert_eq!(
            inner.listing().class_at(file_version),
            Some(RecordClass::Different)
        );
        let company = row_named(inner, "CompanyName");
        assert_eq!(inner.listing().class_at(company), Some(RecordClass::Same));
        assert!(view.accepts(Command::ToggleIgnoreUnimportant));
    }

    #[test]
    fn an_importance_setting_recompares_without_reading_again() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::version_pair(dir.path());
        let mut view = VersionView::new(left, right, &context(), 7);
        assert!(settle(&mut view, VersionView::view));
        let mut settings = view.settings().unwrap();
        let SessionSettings::VersionCompare(version) = &mut settings else {
            panic!("a version view holds version settings");
        };
        version.importance.file_version_important = false;
        view.apply_settings(&settings);
        assert!(settle(&mut view, VersionView::view));
        let inner = view.view();
        let row = row_named(inner, "FileVersion");
        assert_eq!(
            inner.listing().class_at(row),
            Some(RecordClass::Unimportant)
        );
        view.view_mut().set_ignore_unimportant(true);
        let inner = view.view();
        let row = row_named(inner, "FileVersion");
        assert_eq!(inner.listing().class_at(row), Some(RecordClass::Same));
        assert_eq!(view.settings(), Some(settings));
    }

    #[test]
    fn settings_of_another_kind_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let before = view.settings();
        view.apply_settings(&SessionSettings::defaults_for(&MediaView::kind()));
        assert_eq!(view.settings(), before);
        assert!(view.is_ready());
    }

    #[test]
    fn two_media_files_compare_their_tags() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::media_pair(dir.path());
        let mut view = MediaView::new(left, right, &context(), 8);
        assert!(settle(&mut view, MediaView::view));
        let inner = view.view();
        let title = row_named(inner, "Title");
        assert_eq!(
            inner.listing().class_at(title),
            Some(RecordClass::Different)
        );
        let artist = row_named(inner, "Artist");
        assert_eq!(inner.listing().class_at(artist), Some(RecordClass::Same));
    }

    #[test]
    fn the_report_carries_every_value_under_the_record_kind() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::media_pair(dir.path());
        let mut view = MediaView::new(left, right, &context(), 9);
        assert!(settle(&mut view, MediaView::view));
        let (meta, payload) = view.view().report_payload();
        assert_eq!(meta.title.as_deref(), Some("Media Compare Report"));
        let Payload::Record(kind, rows) = payload else {
            panic!("a media view reports records");
        };
        assert_eq!(kind, ca_ui::report::RecordKind::Media);
        assert!(rows.iter().any(|row| row.name == "Title"
            && row.left.as_deref() == Some("First")
            && row.right.as_deref() == Some("Second")));
        let mut plan_rows = Vec::new();
        let plan = ca_ui::report::ReportPlan {
            settings: ca_ui::report::ReportSettings::new(ca_ui::report::ReportKind::Media),
            meta,
            payload: Payload::Record(kind, rows),
            bytes_per_row: 0,
        };
        plan.write(&mut plan_rows, &ca_ui::worker::Cancel::new())
            .unwrap();
        let document = String::from_utf8(plan_rows).unwrap();
        assert!(document.contains("Second"));
    }

    #[test]
    fn the_command_declaration_covers_what_the_view_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let declared = view.commands();
        assert!(!declared.is_empty());
        for state in &declared {
            assert_eq!(state.enabled, view.accepts(state.command));
        }
        assert!(declared
            .iter()
            .all(|state| state.command != Command::ToggleIgnoreUnimportant));
        // Nothing is edited yet, so there is nothing to write.
        assert!(!view.accepts(Command::SaveFile));
        assert!(declared
            .iter()
            .any(|state| state.command == Command::CopyToOtherSide));
        assert_eq!(
            view.menu_view(),
            ca_ui::command::MenuView::Registry,
            "the registry bar carries the edit menu"
        );

        // The version and media views stay read only.
        let (left, right) = testing::version_pair(dir.path());
        let mut version = VersionView::new(left, right, &context(), 11);
        assert!(settle(&mut version, VersionView::view));
        for state in version.commands() {
            assert!(
                !crate::view::editing::EDIT_COMMANDS.contains(&state.command),
                "{:?}",
                state.command
            );
        }
        assert!(!version.accepts(Command::Delete));
        assert_eq!(version.menu_view(), ca_ui::command::MenuView::Version);
    }

    #[test]
    fn the_toolbar_declares_what_the_options_page_lists() {
        let names: Vec<&str> = ca_ui::toolbar::defaults(ca_ui::toolbar::ToolbarView::Records)
            .iter()
            .map(|item| item.name)
            .collect();
        for name in [
            "filter",
            "minor",
            "expand",
            "collapse",
            "reload",
            "recompare",
            "swap",
            "stop",
            "report",
            "strip",
            "details",
            "hex",
        ] {
            assert!(names.contains(&name), "{name} is not listed");
        }
    }

    #[test]
    fn the_view_toggles_and_copies_through_commands() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        assert!(view.view().shows_details());
        view.run(Command::ToggleLineDetails);
        assert!(!view.view().shows_details());
        view.run(Command::HexDetails);
        assert!(view.view().shows_hex());
        view.run(Command::Thumbnail);
        assert!(!view.view().shows_strip());
        let copied = view.view().copy_text().unwrap();
        assert!(copied.contains('\t'));
        // Every area drawn at once still paints.
        let ctx = egui::Context::default();
        view.run(Command::ToggleLineDetails);
        view.run(Command::Copy);
        let copied = copy_from_command(&mut view);
        assert_eq!(Some(copied), view.view().copy_text());
        frame(&mut view, &ctx, 1_280.0, 800.0);
        assert!(view.view().shows_details() && view.view().shows_hex());
    }

    #[test]
    fn a_narrow_window_keeps_every_toolbar_control_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let ctx = egui::Context::default();
        frame(&mut view, &ctx, 640.0, 600.0);
        let output = ctx.run(sized_input(640.0, 600.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        for shape in output.shapes {
            let rect = shape
                .shape
                .visual_bounding_rect()
                .intersect(shape.clip_rect);
            if !rect.is_finite() || rect.is_negative() {
                continue;
            }
            assert!(rect.right() <= 641.0, "a shape reaches {}", rect.right());
        }
    }

    #[test]
    fn cancelling_stops_the_work_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        view.run(Command::Cancel);
        let ctx = egui::Context::default();
        assert!(wait_until(Duration::from_secs(30), || {
            frame(&mut view, &ctx, 1_280.0, 800.0);
            view.is_ready()
        }));
    }

    #[test]
    fn a_settings_change_supersedes_the_running_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::media_pair(dir.path());
        let mut view = MediaView::new(left, right, &context(), 10);
        assert!(settle(&mut view, MediaView::view));
        let before = view.view().superseded_requests();
        view.run(Command::Recompare);
        view.run(Command::Recompare);
        assert!(view.view().superseded_requests() > before);
        assert!(settle(&mut view, MediaView::view));
    }

    #[test]
    fn closing_the_view_leaves_nothing_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        view.on_close();
        view.tick();
        assert!(view.is_ready());
        assert_eq!(view.view().listing().rows(), 0);
    }

    #[test]
    fn each_view_answers_for_its_own_kind() {
        assert_eq!(
            RegistryView::kind(),
            ca_session::SessionKind::RegistryCompare
        );
        assert_eq!(VersionView::kind(), ca_session::SessionKind::VersionCompare);
        assert_eq!(MediaView::kind(), ca_session::SessionKind::MediaCompare);
        let _ = PathBuf::new();
    }

    /// Run frames until the comparison and every write have finished.
    fn finish(view: &mut RegistryView) {
        let ctx = egui::Context::default();
        assert!(
            wait_until(Duration::from_secs(30), || {
                frame(view, &ctx, 1_280.0, 800.0);
                view.is_ready() && !view.view().is_writing()
            }),
            "the view never finished"
        );
    }

    /// Put the cursor on the row named `name` and make `side` active.
    fn point(view: &mut RegistryView, name: &str, side: Side) {
        let row = row_named(view.view(), name);
        view.view_mut().set_cursor(row);
        view.view_mut().set_active_side(side);
    }

    fn run_and_finish(view: &mut RegistryView, command: Command) {
        assert!(view.accepts(command), "{command:?} is not accepted");
        view.run(command);
        finish(view);
    }

    fn has_row(view: &RecordsView, name: &str) -> bool {
        (0..view.listing().rows()).any(|row| {
            view.listing()
                .node_at(row)
                .is_some_and(|node| node.name == name)
        })
    }

    fn set_value_form(view: &mut RegistryView, name: Option<&str>, kind: ValueKind, text: &str) {
        let Some(Panel::Value { form, .. }) = view.view_mut().panel_mut() else {
            panic!("the value editor is not open");
        };
        if let Some(name) = name {
            form.name = name.to_owned();
        }
        form.kind = kind;
        form.text = text.to_owned();
    }

    fn set_name(view: &mut RegistryView, typed: &str) {
        match view.view_mut().panel_mut() {
            Some(Panel::NewKey { name, .. } | Panel::Rename { name, .. }) => {
                *name = typed.to_owned();
            }
            other => panic!("no name editor is open: {other:?}"),
        }
    }

    #[test]
    fn a_copied_value_shows_at_once_and_undo_and_redo_walk_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "Changed", Side::Left);
        run_and_finish(&mut view, Command::CopyToRight);
        let row = row_named(view.view(), "Changed");
        assert_eq!(view.view().listing().class_at(row), Some(RecordClass::Same));
        assert!(view.view().is_modified(Side::Right));
        assert!(!view.view().is_modified(Side::Left));
        assert_eq!(
            view.view()
                .listing()
                .node_at(view.view().listing().cursor())
                .unwrap()
                .name,
            "Changed",
            "the cursor stays on the edited row"
        );
        run_and_finish(&mut view, Command::Undo);
        let row = row_named(view.view(), "Changed");
        assert_eq!(
            view.view().listing().class_at(row),
            Some(RecordClass::Different)
        );
        assert!(!view.view().is_modified(Side::Right));
        run_and_finish(&mut view, Command::Redo);
        let row = row_named(view.view(), "Changed");
        assert_eq!(view.view().listing().class_at(row), Some(RecordClass::Same));
        assert!(
            !view.accepts(Command::Reload),
            "a reload would lose the edit"
        );
    }

    /// With the editing switch of the Specs page on, no copy, delete or save
    /// runs and nothing changes. With the switch off again, a copy lands and
    /// the view reports the unwritten edit.
    #[test]
    fn the_editing_switch_turns_every_registry_edit_and_save_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        let mut settings = view.settings().unwrap();
        settings.specs_mut().unwrap().disable_editing = true;
        view.apply_settings(&settings);
        point(&mut view, "Changed", Side::Left);
        for command in [
            Command::CopyToRight,
            Command::Delete,
            Command::Rename,
            Command::SaveBoth,
        ] {
            assert!(!view.accepts(command), "{command:?}");
        }
        view.run(Command::CopyToRight);
        finish(&mut view);
        assert!(!view.view().is_modified(Side::Right));
        assert!(!view.holds_unwritten_edits());

        settings.specs_mut().unwrap().disable_editing = false;
        view.apply_settings(&settings);
        point(&mut view, "Changed", Side::Left);
        run_and_finish(&mut view, Command::CopyToRight);
        assert!(view.view().is_modified(Side::Right));
        assert!(view.holds_unwritten_edits());
    }

    #[test]
    fn a_deleted_key_leaves_the_panes_and_nothing_is_written_before_save() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let before = std::fs::read(&left).unwrap();
        let mut view = RegistryView::new(left.clone(), right, &context(), 20);
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "LeftOnly", Side::Left);
        run_and_finish(&mut view, Command::Delete);
        assert!(!has_row(view.view(), "LeftOnly"));
        assert_eq!(std::fs::read(&left).unwrap(), before);
        run_and_finish(&mut view, Command::SaveFile);
        let parsed = RegFile::parse(&std::fs::read(&left).unwrap(), &Limits::default()).unwrap();
        let text = parsed.to_text();
        assert!(parsed
            .key("HKEY_CURRENT_USER\\Software\\Sample\\LeftOnly")
            .is_none());
        assert!(
            !text.contains("[-"),
            "a removed key leaves no deletion line"
        );
        assert!(!view.view().is_modified(Side::Left));
    }

    #[test]
    fn new_key_new_value_modify_and_rename_reach_the_saved_file() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RegistryView::new(left, right.clone(), &context(), 21);
        assert!(settle(&mut view, RegistryView::view));

        point(&mut view, "Sample", Side::Right);
        run_and_finish(&mut view, Command::NewKey);
        set_name(&mut view, "Added");
        assert!(view.view_mut().commit_panel());
        finish(&mut view);
        assert!(has_row(view.view(), "Added"));

        point(&mut view, "Added", Side::Right);
        run_and_finish(&mut view, Command::NewValue);
        set_value_form(&mut view, Some("Count"), ValueKind::Dword, "0x10");
        assert!(view.view_mut().commit_panel());
        finish(&mut view);

        point(&mut view, "Count", Side::Right);
        run_and_finish(&mut view, Command::Modify);
        set_value_form(&mut view, None, ValueKind::Dword, "17");
        assert!(view.view_mut().commit_panel());
        finish(&mut view);

        point(&mut view, "Blob", Side::Right);
        run_and_finish(&mut view, Command::Rename);
        set_name(&mut view, "Blob2");
        assert!(view.view_mut().commit_panel());
        finish(&mut view);
        assert!(has_row(view.view(), "Blob2"));
        assert!(!has_row(view.view(), "Blob"));
        assert_eq!(view.view().history().ops().len(), 4);

        run_and_finish(&mut view, Command::SaveFile);
        assert!(!view.view().is_modified(Side::Right));
        let parsed = RegFile::parse(&std::fs::read(&right).unwrap(), &Limits::default()).unwrap();
        let added = parsed
            .key("HKEY_CURRENT_USER\\Software\\Sample\\Added")
            .unwrap();
        assert_eq!(added.entries[0].data, Some(ValueData::Dword(17)));
        let sample = parsed.key("HKEY_CURRENT_USER\\Software\\Sample").unwrap();
        assert!(sample
            .entries
            .iter()
            .any(|entry| entry.name.raw() == "Blob2"
                && entry.data == Some(ValueData::Binary(vec![1, 2, 3]))));
        assert!(sample
            .entries
            .iter()
            .all(|entry| entry.name.raw() != "Blob"));
    }

    #[test]
    fn malformed_data_in_the_editor_is_refused_with_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "Blob", Side::Right);
        run_and_finish(&mut view, Command::Modify);
        set_value_form(&mut view, None, ValueKind::Binary, "01 zz");
        assert!(!view.view_mut().commit_panel());
        assert!(view.view().panel_error().unwrap().contains("'z'"));
        assert!(view.view().panel().is_some(), "the editor stays open");
        set_value_form(&mut view, None, ValueKind::Dword, "99999999999");
        assert!(!view.view_mut().commit_panel());
        assert!(view.view().history().ops().is_empty());
        view.view_mut().cancel_panel();
        assert!(view.view().panel().is_none());
    }

    #[test]
    fn a_file_changed_on_disk_asks_before_it_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RegistryView::new(left, right.clone(), &context(), 22);
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "Changed", Side::Left);
        run_and_finish(&mut view, Command::CopyToRight);
        let other = "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\Other]\r\n\"Z\"=\"written by another program\"\r\n";
        std::fs::write(&right, other).unwrap();
        view.view_mut().set_active_side(Side::Right);
        run_and_finish(&mut view, Command::SaveFile);
        assert_eq!(
            view.view().prompt(),
            Some(&Prompt::DiskChanged(Side::Right))
        );
        assert_eq!(std::fs::read_to_string(&right).unwrap(), other);
        view.view_mut().overwrite_changed();
        finish(&mut view);
        let parsed = RegFile::parse(&std::fs::read(&right).unwrap(), &Limits::default()).unwrap();
        assert!(parsed.key("HKEY_CURRENT_USER\\Software\\Sample").is_some());
        assert!(!view.view().is_modified(Side::Right));
    }

    #[test]
    fn select_all_copy_and_copy_key_name_fill_the_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "Changed", Side::Left);
        view.run(Command::CopyKeyName);
        assert_eq!(
            view.view().pending_clipboard(),
            Some("HKEY_CURRENT_USER\\Software\\Sample")
        );
        assert_eq!(
            copy_from_command(&mut view),
            "HKEY_CURRENT_USER\\Software\\Sample"
        );
        view.run(Command::SelectAll);
        let selected = view.view().selection().len();
        assert!(selected > 3);
        view.run(Command::Copy);
        let copied = copy_from_command(&mut view);
        assert_eq!(copied.lines().count(), selected);
        assert!(copied
            .lines()
            .any(|line| line.starts_with("HKEY_CURRENT_USER\\Software\\Sample\\Changed\t")));
        // Delete acts on the whole selection: every item the left pane
        // shows goes, in one step.
        run_and_finish(&mut view, Command::Delete);
        assert_eq!(view.view().history().ops().len(), 1);
        assert!(!has_row(view.view(), "LeftOnly"));
        assert!(view.view().selection().is_empty());
    }

    #[test]
    fn selected_record_values_with_tabs_are_quoted_in_the_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.reg");
        let right = dir.path().join("right.reg");
        let header = "Windows Registry Editor Version 5.00\r\n\r\n";
        let body =
            format!("{header}[HKEY_CURRENT_USER\\Software\\Sample]\r\n\"Tabbed\"=\"a\tb\"\r\n");
        std::fs::write(&left, &body).unwrap();
        std::fs::write(&right, &body).unwrap();
        let mut view = RegistryView::new(left, right, &context(), 919);
        assert!(settle(&mut view, RegistryView::view));

        view.run(Command::SelectAll);
        let selected = view.view().selection().len();
        view.run(Command::Copy);
        let copied = copy_from_command(&mut view);

        assert_eq!(copied.lines().count(), selected);
        assert!(copied
            .lines()
            .any(|line| line == "HKEY_CURRENT_USER\\Software\\Sample\\Tabbed\t\"a\tb\"\t\"a\tb\""));
    }

    #[test]
    fn changing_the_record_filter_does_not_cancel_a_copy_already_requested() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        view.run(Command::SelectAll);
        let selected = view.view().selection().len();
        view.run(Command::Copy);
        view.run(Command::ShowDifferences);
        let copied = copy_from_command(&mut view);
        assert_eq!(copied.lines().count(), selected);
    }

    #[test]
    fn copy_key_name_supersedes_a_selection_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        view.run(Command::SelectAll);
        let gate = view.view_mut().gate_next_selection_copy();
        view.run(Command::Copy);
        point(&mut view, "Changed", Side::Left);
        view.run(Command::CopyKeyName);
        let wanted = "HKEY_CURRENT_USER\\Software\\Sample";
        assert_eq!(view.view().pending_clipboard(), Some(wanted));
        gate.wait();
        assert!(wait_until(Duration::from_secs(10), || {
            view.view().selection_copy_finished()
        }));
        view.tick();
        assert_eq!(view.view().pending_clipboard(), Some(wanted));
    }

    #[test]
    fn export_writes_one_key_and_export_all_writes_the_side() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "LeftOnly", Side::Left);
        let one = dir.path().join("one.reg");
        view.view_mut().export_to(&one, false);
        finish(&mut view);
        let parsed = RegFile::parse(&std::fs::read(&one).unwrap(), &Limits::default()).unwrap();
        assert_eq!(parsed.keys.len(), 1);
        let all = dir.path().join("all.reg");
        view.view_mut().export_to(&all, true);
        finish(&mut view);
        let parsed = RegFile::parse(&std::fs::read(&all).unwrap(), &Limits::default()).unwrap();
        assert_eq!(parsed.keys.len(), 2);
        assert!(view.view().message().unwrap().contains("Exported"));
    }

    #[test]
    fn work_in_flight_keeps_the_tab_busy() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        assert!(!view.is_busy());
        point(&mut view, "LeftOnly", Side::Left);
        view.view_mut()
            .export_to(&dir.path().join("one.reg"), false);
        assert!(view.view().is_writing());
        assert!(view.is_busy());
        assert!(!view.may_close());
        finish(&mut view);
        assert!(!view.is_busy());
        assert!(view.may_close());
    }

    #[test]
    fn closing_with_edits_asks_and_discard_lets_the_tab_go() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        assert!(view.may_close());
        point(&mut view, "Changed", Side::Left);
        run_and_finish(&mut view, Command::CopyToRight);
        assert!(!view.may_close());
        assert_eq!(view.view().prompt(), Some(&Prompt::Closing));
        assert!(!view.wants_close());
        view.view_mut().discard_and_close();
        assert!(view.wants_close());
    }

    #[test]
    fn a_new_value_whose_name_is_taken_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = registry(dir.path());
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "Sample", Side::Left);
        run_and_finish(&mut view, Command::NewValue);
        set_value_form(&mut view, Some("kept"), ValueKind::Sz, "x");
        assert!(!view.view_mut().commit_panel());
        assert!(view.view().panel_error().unwrap().contains("already"));
    }

    /// A key under `HKEY_CURRENT_USER\Software\compare-all-tests` that exists
    /// for one test and is deleted on every exit path, a panic included.
    #[cfg(windows)]
    struct Throwaway {
        sub: String,
    }

    #[cfg(windows)]
    impl Throwaway {
        const ROOT: &'static str = r"Software\compare-all-tests";

        fn create(values: &[(&str, &str)]) -> Option<Self> {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let sub = format!(
                r"{}\{}-{nanos}{}",
                Self::ROOT,
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            );
            let root = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
            let guard = Self { sub };
            let (key, _) = root.create_subkey(&guard.sub).ok()?;
            for (name, text) in values {
                key.set_value(name, text).ok()?;
            }
            key.create_subkey("Child").ok()?;
            Some(guard)
        }

        fn path(&self) -> String {
            format!(r"HKEY_CURRENT_USER\{}", self.sub)
        }

        fn read(&self, name: &str) -> Option<String> {
            winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
                .open_subkey(&self.sub)
                .ok()?
                .get_value(name)
                .ok()
        }
    }

    #[cfg(windows)]
    impl Drop for Throwaway {
        fn drop(&mut self) {
            let root = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
            let _ = root.delete_subkey_all(&self.sub);
            if let Ok(parent) =
                root.open_subkey_with_flags(Self::ROOT, winreg::enums::KEY_ALL_ACCESS)
            {
                if parent.enum_keys().next().is_none() {
                    let _ = root.delete_subkey(Self::ROOT);
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_live_edit_waits_for_the_confirmation_and_writes_a_restore_file_first() {
        let Some(key) = Throwaway::create(&[("A", "a")]) else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journals");
        let file = dir.path().join("wanted.reg");
        std::fs::write(
            &file,
            format!(
                "Windows Registry Editor Version 5.00\r\n\r\n[{}]\r\n\"A\"=\"b\"\r\n\"New\"=\"n\"\r\n",
                key.path()
            ),
        )
        .unwrap();
        let live = PathBuf::from(format!(r"reg:\\{}", key.path()));
        let mut view = RegistryView::new(live, file, &context(), 23);
        view.view_mut().set_journal_directory(journal.clone());
        assert!(settle(&mut view, RegistryView::view));

        point(&mut view, "A", Side::Right);
        run_and_finish(&mut view, Command::CopyToLeft);
        point(&mut view, "New", Side::Right);
        run_and_finish(&mut view, Command::CopyToLeft);
        assert_eq!(
            key.read("A").as_deref(),
            Some("a"),
            "an edit is not a write"
        );

        view.view_mut().set_active_side(Side::Left);
        run_and_finish(&mut view, Command::SaveFile);
        let Some(Prompt::ConfirmLive {
            counts, protected, ..
        }) = view.view().prompt().cloned()
        else {
            panic!("no confirmation was asked for");
        };
        assert_eq!(counts.values_set, 2);
        assert_eq!(counts.total(), 2);
        assert!(!protected);

        // A refusal writes nothing.
        view.view_mut().cancel_prompt();
        assert_eq!(key.read("A").as_deref(), Some("a"));
        assert!(!journal.exists());

        run_and_finish(&mut view, Command::SaveFile);
        view.view_mut().confirm_live();
        finish(&mut view);
        assert_eq!(key.read("A").as_deref(), Some("b"));
        assert_eq!(key.read("New").as_deref(), Some("n"));
        assert!(!view.view().is_modified(Side::Left));
        let restore: Vec<_> = std::fs::read_dir(&journal)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        assert_eq!(restore.len(), 1);
        let text = RegFile::parse(&std::fs::read(&restore[0]).unwrap(), &Limits::default())
            .unwrap()
            .to_text();
        assert!(text.contains("\"A\"=\"a\""), "{text}");
        assert!(text.contains("\"New\"=-"), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn set_as_base_keys_moves_a_live_side_to_the_chosen_key() {
        let Some(key) = Throwaway::create(&[("A", "a")]) else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let live = PathBuf::from(format!(r"reg:\\{}", key.path()));
        let mut view = RegistryView::new(live.clone(), live, &context(), 24);
        assert!(settle(&mut view, RegistryView::view));
        point(&mut view, "Child", Side::Left);
        assert!(view.accepts(Command::SetBothAsBaseKeys));
        view.run(Command::SetBothAsBaseKeys);
        assert!(settle(&mut view, RegistryView::view));
        assert!(view.view().left().to_string_lossy().ends_with(r"\Child"));
        assert!(view.view().right().to_string_lossy().ends_with(r"\Child"));
        // The key above the base is a wrapper row, which no edit may change.
        point(&mut view, "Software", Side::Left);
        assert!(!view.accepts(Command::NewValue));
        assert!(!view.accepts(Command::Rename));
    }

    #[cfg(windows)]
    #[test]
    fn up_one_level_moves_each_named_live_side_to_its_parent_key() {
        let Some(key) = Throwaway::create(&[("A", "a")]) else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let grand = format!(r"{}\Child\Grand", key.sub);
        if winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .create_subkey(&grand)
            .is_err()
        {
            println!("skipped: the throwaway key could not be created");
            return;
        }
        let start = PathBuf::from(format!(r"reg:\\{}\Child\Grand", key.path()));
        let mut view = RegistryView::new(start.clone(), start, &context(), 26);
        assert!(settle(&mut view, RegistryView::view));
        let ends = |view: &RegistryView, tail: &str| {
            (
                view.view().left().to_string_lossy().ends_with(tail),
                view.view().right().to_string_lossy().ends_with(tail),
            )
        };
        view.run(Command::UpOneLevelLeft);
        assert!(settle(&mut view, RegistryView::view));
        assert_eq!(ends(&view, r"\Child"), (true, false));
        assert_eq!(ends(&view, r"\Grand"), (false, true));
        view.run(Command::UpOneLevelRight);
        assert!(settle(&mut view, RegistryView::view));
        assert_eq!(ends(&view, r"\Child"), (true, true));
        assert!(view.accepts(Command::UpOneLevelBoth));
        view.run(Command::UpOneLevelBoth);
        assert!(settle(&mut view, RegistryView::view));
        let parent = key.path();
        assert!(view.view().left().to_string_lossy().ends_with(&parent));
        assert!(view.view().right().to_string_lossy().ends_with(&parent));
    }
}
