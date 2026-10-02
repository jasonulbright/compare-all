//! A toolbar menu opened through an icon button keeps menu behavior: a choice
//! closes it, and it still opens from inside the collapsed toolbar.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use ca_ui::testing::probe::{Probe, NARROW_ROOM, OVERFLOW_LABEL};
use ca_ui::toolbar::{self, Item, Layout};
use std::cell::Cell;

fn select_menu(ui: &mut egui::Ui, chosen: &Cell<bool>) {
    ca_ui::widgets::icon_menu(ui, "Select", ca_ui::icons::Icon::Select, |ui| {
        if ui.button("All files").clicked() {
            chosen.set(true);
            ui.close_menu();
        }
    });
}

#[test]
fn a_combo_choice_in_the_collapsed_toolbar_reaches_its_control() {
    let chosen = Cell::new(false);
    let mut run = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.allocate_ui(egui::vec2(NARROW_ROOM, ui.available_height()), |ui| {
                toolbar::show(
                    ui,
                    egui::Id::new("combo-overflow"),
                    &[Item::widget("filter", 90.0)],
                    &Layout::built_in(),
                    |ui, _| {
                        let response = egui::ComboBox::from_id_salt("choice")
                            .selected_text("Current filter")
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(chosen.get(), "Changed files").clicked() {
                                    chosen.set(true);
                                }
                            });
                        response.response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::ComboBox,
                                true,
                                "Current filter",
                            )
                        });
                    },
                );
            });
        });
    };
    let mut probe = Probe::new(800.0, 600.0);
    probe.idle(&mut run);
    probe.idle(&mut run);
    probe.click(OVERFLOW_LABEL, &mut run).unwrap();
    probe.idle(&mut run);
    probe.click("Current filter", &mut run).unwrap();
    probe.idle(&mut run);
    probe.click("Changed files", &mut run).unwrap();
    assert!(chosen.get(), "{:?}", probe.labels());
}

#[test]
fn a_choice_in_an_icon_menu_closes_the_menu() {
    let chosen = Cell::new(false);
    let mut run = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| select_menu(ui, &chosen));
    };
    let mut probe = Probe::new(800.0, 600.0);
    probe.idle(&mut run);
    probe.click("Select", &mut run).unwrap();
    probe.idle(&mut run);
    probe.click("All files", &mut run).unwrap();
    probe.idle(&mut run);
    assert!(chosen.get());
    assert!(probe.find("All files").is_err(), "{:?}", probe.labels());
}

#[test]
fn an_icon_menu_in_the_collapsed_toolbar_opens_and_takes_a_choice() {
    let items = [Item::widget("select", 90.0)];
    let chosen = Cell::new(false);
    let collapsed = Cell::new(false);
    let mut run = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.allocate_ui(egui::vec2(NARROW_ROOM, ui.available_height()), |ui| {
                let outcome = toolbar::show(
                    ui,
                    egui::Id::new("collapsed-menu"),
                    &items,
                    &Layout::built_in(),
                    |ui, _| select_menu(ui, &chosen),
                );
                collapsed.set(outcome.collapsed);
            });
        });
    };
    let mut probe = Probe::new(800.0, 600.0);
    probe.idle(&mut run);
    probe.idle(&mut run);
    assert!(collapsed.get());
    probe.click(OVERFLOW_LABEL, &mut run).unwrap();
    probe.idle(&mut run);
    probe.click("Select", &mut run).unwrap();
    probe.idle(&mut run);
    probe.click("All files", &mut run).unwrap();
    assert!(chosen.get(), "{:?}", probe.labels());
}
