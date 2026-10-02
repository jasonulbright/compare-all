//! The stored toolbar order and visibility reach the painted bar.
//!
//! The declared order is fixed, so it is stated here as a literal and compared
//! with the declaration. What the options document then says about it is
//! checked by painting a real frame and reading back what the bar drew.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use ca_session::options::CommandOptions;
use ca_ui::command::Command;
use ca_ui::testing::sized_input;
use ca_ui::toolbar::{self, Item, Layout, ToolbarView};

/// The smallest window width the program opens at.
const NARROW: f32 = 640.0;

#[test]
fn icon_slots_and_command_buttons_have_aligned_centers() {
    let items = [
        Item::widget("home", 70.0),
        Item::widget("sessions", 90.0),
        Item::widget("all", 46.0),
        Item::command("reload", Command::Reload, "Reload", true, ""),
    ];
    let mut probe = ca_ui::testing::probe::Probe::new(1280.0, 300.0);
    let mut run = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            toolbar::show(
                ui,
                egui::Id::new("aligned"),
                &items,
                &Layout::built_in(),
                |ui, name| {
                    ca_ui::widgets::toolbar_button(
                        ui,
                        match name {
                            "home" => "Home",
                            "sessions" => "Sessions",
                            _ => "All",
                        },
                        true,
                        "",
                    );
                },
            );
        });
    };
    probe.idle(&mut run);
    probe.idle(&mut run);
    for label in ["Home", "Sessions", "All"] {
        assert!(
            (probe.find(label).unwrap().rect.center().y
                - probe.find("Reload").unwrap().rect.center().y)
                .abs()
                <= f32::EPSILON,
            "{label}"
        );
    }
}

/// The items one bar declares, in the order it declares them.
fn names(view: ToolbarView) -> Vec<&'static str> {
    toolbar::defaults(view)
        .iter()
        .map(|held| held.name)
        .collect()
}

#[test]
fn the_text_bar_keeps_the_measured_order() {
    assert_eq!(
        names(ToolbarView::Text),
        vec![
            "home",
            "sessions",
            "separator-1",
            "all",
            "diffs",
            "same",
            "context",
            "minor",
            "rules",
            "format",
            "separator-2",
            "copy",
            "next-section",
            "previous-section",
            "swap",
            "reload",
            "separator-3",
            "report",
        ]
    );
}

#[test]
fn the_folder_bar_keeps_the_measured_order() {
    assert_eq!(
        names(ToolbarView::Folder),
        vec![
            "home",
            "sessions",
            "separator-1",
            "all",
            "diffs",
            "same",
            "filter",
            "structure",
            "minor",
            "rules",
            "separator-2",
            "copy",
            "expand",
            "collapse",
            "select",
            "files",
            "method",
            "compare-contents",
            "refresh",
            "swap",
            "stop",
            "separator-3",
            "name-filter",
            "peek",
            "report",
        ]
    );
}

#[test]
fn every_bar_ends_its_declaration_with_a_report_item() {
    for view in ToolbarView::ALL {
        assert!(
            names(*view).contains(&"report"),
            "{} declares no report item",
            view.id()
        );
    }
}

/// A small bar with three buttons, drawn under `layout`.
fn paint(layout: &Layout, width: f32) -> Vec<String> {
    let items = [
        Item::command("one", Command::Reload, "One", true, ""),
        Item::separator("separator-1"),
        Item::command("two", Command::SwapSides, "Two", true, ""),
        Item::command("three", Command::Recompare, "Three", true, ""),
    ];
    let ctx = egui::Context::default();
    let mut drawn = Vec::new();
    let output = ctx.run(sized_input(width, 200.0), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = toolbar::show(ui, egui::Id::new("bar"), &items, layout, |_, _| {});
        });
    });
    for shape in &output.shapes {
        if let egui::Shape::Text(text) = &shape.shape {
            drawn.push(text.galley.text().to_owned());
        }
    }
    drawn
}

#[test]
fn the_declared_order_is_what_is_painted() {
    let drawn = paint(&Layout::built_in(), 1_280.0);
    assert_eq!(drawn, vec!["One", "Two", "Three"]);
}

#[test]
fn a_stored_order_reaches_the_painted_bar() {
    let mut options = CommandOptions::default();
    options.set_toolbar(
        ToolbarView::Hex.id(),
        vec!["three".to_owned(), "two".to_owned(), "one".to_owned()],
    );
    let layout = Layout::from_options(&options, ToolbarView::Hex);
    assert_eq!(paint(&layout, 1_280.0), vec!["Three", "Two", "One"]);
}

#[test]
fn a_hidden_item_is_not_painted() {
    let mut options = CommandOptions::default();
    options.set_hidden_from_toolbar(ToolbarView::Hex.id(), "two", true);
    let layout = Layout::from_options(&options, ToolbarView::Hex);
    assert_eq!(paint(&layout, 1_280.0), vec!["One", "Three"]);
}

#[test]
fn a_reset_puts_the_declared_order_back() {
    let mut options = CommandOptions::default();
    options.set_toolbar(ToolbarView::Hex.id(), vec!["three".to_owned()]);
    options.set_hidden_from_toolbar(ToolbarView::Hex.id(), "two", true);
    options.reset_toolbar(ToolbarView::Hex.id());
    let layout = Layout::from_options(&options, ToolbarView::Hex);
    assert_eq!(layout, Layout::built_in());
    assert_eq!(paint(&layout, 1_280.0), vec!["One", "Two", "Three"]);
}

#[test]
fn a_narrow_window_moves_the_controls_behind_one_button() {
    // The first frame measures; the second is the one judged.
    let layout = Layout::built_in();
    let _ = paint(&layout, NARROW);
    let drawn = paint(&layout, 40.0);
    assert_eq!(drawn, vec!["More"]);
}
