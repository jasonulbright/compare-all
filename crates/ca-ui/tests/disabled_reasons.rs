#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A disabled control says why it refuses input: on screen while the pointer
//! rests on it, and to a screen reader as the description of its node. Every
//! check reads the drawn frame, never the data the control was built from.

use ca_ui::command::{Command, MenuView};
use ca_ui::testing::probe::{painted_texts, Probe};
use ca_ui::toolbar::{self, Item, Layout};
use ca_ui::widgets;

const REASON: &str = "Wait for the comparison to finish";

/// The accessible label of the menu line of `command` in the text bar.
fn line_label(command: Command) -> String {
    match command.shortcut_in(MenuView::Text) {
        Some(shortcut) => format!("{}\t{shortcut}", command.label_in(MenuView::Text)),
        None => command.label_in(MenuView::Text).to_owned(),
    }
}

/// What a resting pointer on the control labelled `label` shows: the texts
/// the frame painted and the description the control's node carries.
fn rest_on(
    label: &str,
    enabled: bool,
    draw: &mut impl FnMut(&egui::Context),
) -> (Vec<String>, Option<String>) {
    let mut probe = Probe::new(800.0, 400.0);
    probe.idle(draw);
    probe.idle(draw);
    let control = probe.find(label).unwrap();
    assert_eq!(control.enabled, enabled, "{label}");
    let output = probe.hover(control.rect.center(), draw);
    let description = probe.find(label).unwrap().description;
    (painted_texts(&output), description)
}

fn shows(painted: &[String], text: &str) -> bool {
    painted.iter().any(|painted| painted == text)
}

#[test]
fn a_resting_pointer_opens_the_tooltip_of_an_enabled_button() {
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = ui.button("Plain").on_hover_text("Plain tooltip");
        });
    };
    let (painted, _) = rest_on("Plain", true, &mut draw);
    assert!(shows(&painted, "Plain tooltip"), "painted {painted:?}");
}

#[test]
fn a_disabled_menu_line_shows_its_reason_and_names_it_to_a_screen_reader() {
    let label = line_label(Command::CopyToRight);
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ =
                widgets::command_item_in(ui, MenuView::Text, Command::CopyToRight, false, REASON);
        });
    };
    let (painted, description) = rest_on(&label, false, &mut draw);
    assert!(shows(&painted, REASON), "painted {painted:?}");
    assert_eq!(description.as_deref(), Some(REASON));
}

#[test]
fn a_disabled_toolbar_button_shows_its_reason_and_names_it_to_a_screen_reader() {
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = widgets::toolbar_button_with_icon(ui, "Copy", false, REASON, None);
        });
    };
    let (painted, description) = rest_on("Copy", false, &mut draw);
    assert!(shows(&painted, REASON), "painted {painted:?}");
    assert_eq!(description.as_deref(), Some(REASON));
}

#[test]
fn a_disabled_command_of_a_declared_toolbar_shows_its_reason() {
    let items = [
        Item::command("copy", Command::CopyToOtherSide, "Copy", false, REASON),
        Item::command("swap", Command::SwapSides, "Swap", true, "Never shown"),
    ];
    let layout = Layout::built_in();
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = toolbar::show(ui, egui::Id::new("bar"), &items, &layout, |_, _| {});
        });
    };
    let (painted, description) = rest_on("Copy", false, &mut draw);
    assert!(shows(&painted, REASON), "painted {painted:?}");
    assert_eq!(description.as_deref(), Some(REASON));
    let (painted, description) = rest_on("Swap", true, &mut draw);
    assert!(!shows(&painted, "Never shown"), "painted {painted:?}");
    assert_eq!(description, None);
}

#[test]
fn a_line_this_build_does_not_offer_says_so_on_hover() {
    const PENDING: &str = "Not available in this build";
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            widgets::pending_item(ui, "Later");
        });
    };
    let (painted, description) = rest_on("Later", false, &mut draw);
    assert!(shows(&painted, PENDING), "painted {painted:?}");
    assert_eq!(description.as_deref(), Some(PENDING));
}

#[test]
fn a_control_shows_its_own_tooltip_while_enabled_and_only_its_reason_while_disabled() {
    const NORMAL: &str = "Copies the section";
    for enabled in [true, false] {
        let mut draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui
                    .add_enabled(enabled, egui::Button::new("Copy"))
                    .on_hover_text(NORMAL);
                let _ = widgets::disabled_reason(response, REASON);
            });
        };
        let (painted, description) = rest_on("Copy", enabled, &mut draw);
        assert_eq!(shows(&painted, NORMAL), enabled, "painted {painted:?}");
        assert_eq!(shows(&painted, REASON), !enabled, "painted {painted:?}");
        assert_eq!(description.as_deref(), (!enabled).then_some(REASON));
    }
}

#[test]
fn a_disabled_group_of_controls_shows_its_reason() {
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let shown = ui.add_enabled_ui(false, |ui| {
                let mut value = true;
                ui.checkbox(&mut value, "Locked option");
            });
            let _ = widgets::disabled_reason(shown.response, REASON);
        });
    };
    let (painted, _) = rest_on("Locked option", false, &mut draw);
    assert!(shows(&painted, REASON), "painted {painted:?}");
}

#[test]
fn an_enabled_menu_line_and_toolbar_button_show_no_reason() {
    let label = line_label(Command::CopyToRight);
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ =
                widgets::command_item_in(ui, MenuView::Text, Command::CopyToRight, true, REASON);
        });
    };
    let (painted, description) = rest_on(&label, true, &mut draw);
    assert!(!shows(&painted, REASON), "painted {painted:?}");
    assert_eq!(description, None);

    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = widgets::toolbar_button_with_icon(ui, "Copy", true, REASON, None);
        });
    };
    let (painted, description) = rest_on("Copy", true, &mut draw);
    assert!(!shows(&painted, REASON), "painted {painted:?}");
    assert_eq!(description, None);
}
