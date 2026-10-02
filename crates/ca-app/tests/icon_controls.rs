//! Kind identity and tab-close behavior reach the painted icon controls.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_app::{registry, App};
use ca_ui::testing::{context, probe::Probe};

#[test]
fn every_registered_view_reports_its_kind_including_temporary_views() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("input.txt");
    std::fs::write(&file, "one\n").unwrap();
    for (kind, _) in registry::VIEWS {
        let view = registry::construct(kind, file.clone(), file.clone(), &context(), 1).unwrap();
        assert_eq!(view.kind().as_ref(), Some(kind));
        let request = ca_ui::OpenRequest::new(kind.clone(), file.clone(), file.clone())
            .over_temporaries(vec![dir.path().join("temporary-copy")]);
        let wrapped = registry::open(&request, &context(), 2).unwrap();
        assert_eq!(wrapped.kind().as_ref(), Some(kind));
    }
}

#[test]
fn tab_buttons_activate_and_close_the_tab_they_name() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, "one\n").unwrap();
    std::fs::write(&right, "two\n").unwrap();
    let mut app = App::in_directories(dir.path().to_path_buf(), dir.path().join("journal"));
    app.open(
        &ca_ui::OpenRequest::new(ca_session::SessionKind::TextCompare, left, right),
        &context(),
    );
    let compare = app.active_title().unwrap();
    app.open_home(&context());
    assert_eq!(app.tab_count(), 2);
    let home = app.active_title().unwrap();
    let mut probe = Probe::new(1280.0, 800.0);
    {
        let mut run = |ctx: &egui::Context| app.frame(ctx);
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe.click(&compare, &mut run).unwrap();
    }
    assert_eq!(app.active_title().as_deref(), Some(compare.as_str()));
    {
        let mut run = |ctx: &egui::Context| app.frame(ctx);
        probe.idle(&mut run);
        // The active comparison also draws a Home toolbar button below the tab strip.
        let tab = probe
            .find_all(&home)
            .into_iter()
            .min_by(|a, b| a.rect.min.y.total_cmp(&b.rect.min.y))
            .unwrap();
        probe.press_at(tab.rect.center(), &mut run);
    }
    assert_eq!(app.active_title().as_deref(), Some(home.as_str()));
    {
        let mut run = |ctx: &egui::Context| app.frame(ctx);
        probe.idle(&mut run);
        let mut closes = probe.find_all("Close Tab");
        assert_eq!(closes.len(), 2);
        closes.sort_by(|a, b| a.rect.min.x.total_cmp(&b.rect.min.x));
        probe.press_at(closes[0].rect.center(), &mut run);
    }
    assert_eq!(app.tab_count(), 1);
    assert_eq!(app.active_title(), Some(home));
}

#[test]
fn close_icon_closes_the_tab_it_belongs_to() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::in_directories(dir.path().to_path_buf(), dir.path().join("journal"));
    app.open_home(&context());
    app.open_home(&context());
    let mut probe = Probe::new(1280.0, 800.0);
    let mut run = |ctx: &egui::Context| app.frame(ctx);
    probe.idle(&mut run);
    probe.idle(&mut run);
    let buttons = probe.find_all("Close Tab");
    assert_eq!(buttons.len(), 2);
    probe.press_at(buttons[1].rect.center(), &mut run);
    assert_eq!(app.tab_count(), 1);
}
