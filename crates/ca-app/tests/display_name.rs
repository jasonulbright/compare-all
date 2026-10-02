//! Display names are independent of executable and storage identifiers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[test]
fn window_and_about_use_the_display_name() {
    assert_eq!(
        ca_app::product_title(),
        format!("Compare All {}", ca_app::VERSION)
    );
    let directory = tempfile::tempdir().unwrap();
    let mut app = ca_app::App::in_directories(
        directory.path().join("settings"),
        directory.path().join("journals"),
    );
    let mut probe = ca_ui::testing::probe::Probe::new(1000.0, 700.0);
    let mut draw = |ctx: &egui::Context| app.frame(ctx);
    probe.idle(&mut draw);
    probe.click("Help", &mut draw).unwrap();
    let output = probe.frame(Vec::new(), &mut draw);
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == ca_app::product_title())));
}

#[test]
fn installer_display_names_and_publisher_do_not_include_the_version() {
    let installer = include_str!("../../../installer/compare-all.wxs");
    assert!(installer.contains("Name=\"Compare All\""));
    assert!(installer.contains("Manufacturer=\"Jason Ulbright\""));
    assert!(installer.contains("Id=\"StartMenuLink\" Name=\"Compare All\""));
    assert!(installer.contains("Id=\"DesktopLink\" Name=\"Compare All\""));
    assert!(installer.contains("Name=\"compare-all\""));
    assert!(installer.contains("Key=\"Software\\compare-all\""));
}
