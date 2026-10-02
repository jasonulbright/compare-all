#![allow(clippy::unwrap_used, missing_docs)]

use ca_ui::testing::{context, probe::Probe};
use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

#[test]
fn structural_menu_runs_and_restores_both_formats_by_accessible_label() {
    for (extension, left, right, title) in [
        (
            "json",
            r#"{"b":2,"a":1}"#,
            r#"{"a":1,"b":3}"#,
            "JSON Structure Compare",
        ),
        (
            "xml",
            "<r><x>one</x></r>",
            "<r><x>two</x></r>",
            "XML Structure Compare",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join(format!("left.{extension}"));
        let b = directory.path().join(format!("right.{extension}"));
        std::fs::write(&a, left).unwrap();
        std::fs::write(&b, right).unwrap();
        let mut app = ca_app::App::in_directories(
            directory.path().join("settings"),
            directory.path().join("journal"),
        );
        app.open_kind(
            &ca_session::SessionKind::TextCompare,
            a.clone(),
            b,
            &context(),
        );
        let app = RefCell::new(app);
        let mut probe = Probe::new(1600.0, 900.0);
        let mut run = |ctx: &egui::Context| app.borrow_mut().frame(ctx);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !app.borrow().active_is_ready() && Instant::now() < deadline {
            probe.idle(&mut run);
            std::thread::sleep(Duration::from_millis(2));
        }
        probe.idle(&mut run);
        probe.click("View", &mut run).unwrap();
        assert!(probe.find("Compare Structure").unwrap().enabled);
        probe.click("Compare Structure", &mut run).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut shown = String::new();
        while !shown.contains(title) && Instant::now() < deadline {
            shown = painted_text(&mut probe, &mut run);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(shown.contains(title), "{shown}");
        assert!(shown.contains("Changed"));
        probe.click("View", &mut run).unwrap();
        probe.click("Compare Structure", &mut run).unwrap();
        assert!(painted_text(&mut probe, &mut run).contains("Showing the original text"));
        assert_eq!(std::fs::read_to_string(a).unwrap(), left);
    }
}

fn painted_text(probe: &mut Probe, run: &mut impl FnMut(&egui::Context)) -> String {
    let output = probe.frame(Vec::new(), run);
    output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                Some(text.galley.text())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
