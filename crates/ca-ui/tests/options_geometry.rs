//! Every options page fits the window it is given.
//!
//! The narrow width is the smallest window the program opens at. A page that
//! drew past it would put a control out of reach, so each page is laid out at
//! both widths and what it painted is measured.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_session::options::ProgramOptions;
use ca_session::AdminPolicies;
use ca_ui::options::OptionsDialog;
use ca_ui::testing::sized_input;
use ca_ui::theme::{palette, Variant};

/// The smallest window width the program opens at.
const NARROW: f32 = 640.0;

/// The width the window opens at by default.
const WIDE: f32 = 1_280.0;

/// The area every shape one frame painted covers.
fn painted_area(output: &egui::FullOutput) -> egui::Rect {
    let mut area = egui::Rect::NOTHING;
    for shape in &output.shapes {
        let bounds = shape.shape.visual_bounding_rect();
        if bounds.is_finite() && bounds.is_positive() {
            area = area.union(bounds);
        }
    }
    area
}

/// Lay out one page at `width` and report what it painted.
fn draw_page(width: f32, page: &str) -> egui::Rect {
    let table = palette(Variant::Dark);
    let mut dialog = OptionsDialog::new(&ProgramOptions::default(), AdminPolicies::default(), 1);
    dialog.select_page_named(page);
    let ctx = egui::Context::default();
    // The first frame lays out with nothing measured, so a later frame is the
    // one judged.
    for _ in 0..2 {
        let _ = ctx.run(sized_input(width, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = dialog.body(ui, &table);
            });
        });
    }
    let output = ctx.run(sized_input(width, 800.0), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = dialog.body(ui, &table);
        });
    });
    painted_area(&output)
}

#[test]
fn every_page_fits_both_widths() {
    let dialog = OptionsDialog::new(&ProgramOptions::default(), AdminPolicies::default(), 1);
    for page in dialog.pages() {
        for width in [NARROW, WIDE] {
            let area = draw_page(width, page.name);
            assert!(
                area.right() <= width + 1.0,
                "{} drew to {} at {width} points",
                page.name,
                area.right()
            );
        }
    }
}

#[test]
fn every_color_page_fits_both_widths_in_each_group() {
    let table = palette(Variant::Dark);
    for group in ca_session::options::ColorGroup::ALL {
        for width in [NARROW, WIDE] {
            let mut dialog =
                OptionsDialog::new(&ProgramOptions::default(), AdminPolicies::default(), 1);
            dialog.select_page_named("File Views");
            dialog.set_color_group(*group);
            dialog.set_color_variant(Variant::Light);
            let ctx = egui::Context::default();
            let mut area = egui::Rect::NOTHING;
            for _ in 0..3 {
                let output = ctx.run(sized_input(width, 800.0), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let _ = dialog.body(ui, &table);
                    });
                });
                area = painted_area(&output);
            }
            assert!(
                area.right() <= width + 1.0,
                "the {} colors drew to {} at {width} points",
                group.id(),
                area.right()
            );
        }
    }
}

#[test]
fn every_command_bar_page_fits_both_widths() {
    let table = palette(Variant::Dark);
    for view in ca_ui::command::MenuView::ALL {
        for width in [NARROW, WIDE] {
            let mut dialog =
                OptionsDialog::new(&ProgramOptions::default(), AdminPolicies::default(), 1);
            dialog.select_page_named("Commands");
            dialog.set_command_view(*view);
            let ctx = egui::Context::default();
            let mut area = egui::Rect::NOTHING;
            for _ in 0..3 {
                let output = ctx.run(sized_input(width, 800.0), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let _ = dialog.body(ui, &table);
                    });
                });
                area = painted_area(&output);
            }
            assert!(
                area.right() <= width + 1.0,
                "the {} commands drew to {} at {width} points",
                view.id(),
                area.right()
            );
        }
    }
}

#[test]
fn an_open_with_entry_being_edited_fits_both_widths() {
    let table = palette(Variant::Dark);
    for width in [NARROW, WIDE] {
        let mut dialog =
            OptionsDialog::new(&ProgramOptions::default(), AdminPolicies::default(), 1);
        dialog.select_page_named("Open With");
        dialog.add_open_with_entry();
        let ctx = egui::Context::default();
        let mut area = egui::Rect::NOTHING;
        for _ in 0..3 {
            let output = ctx.run(sized_input(width, 800.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = dialog.body(ui, &table);
                });
            });
            area = painted_area(&output);
        }
        assert!(
            area.right() <= width + 1.0,
            "the entry drew to {} at {width} points",
            area.right()
        );
    }
}

#[test]
fn the_restore_wizard_fits_both_widths() {
    for width in [NARROW, WIDE] {
        let mut wizard = ca_ui::options::RestoreDialog::new(1);
        wizard.set(ca_session::RestoreCategory::Sessions, true);
        let ctx = egui::Context::default();
        let mut area = egui::Rect::NOTHING;
        for _ in 0..3 {
            let output = ctx.run(sized_input(width, 800.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = wizard.body(ui);
                });
            });
            area = painted_area(&output);
        }
        assert!(
            area.right() <= width + 1.0,
            "the wizard drew to {} at {width} points",
            area.right()
        );
    }
}
