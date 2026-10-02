//! The embedded artwork, master selection and slot vocabulary stay in agreement.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_ui::icons::{command_icon, session_icon, toolbar_icon, Icon};
use ca_ui::toolbar::ToolbarView;
use std::{collections::BTreeSet, path::Path};

#[test]
fn every_declared_toolbar_command_uses_its_command_icon() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let declarations =
        regex::Regex::new(r#"Item::(?:command|toggle)\(\s*"([^"]+)"\s*,\s*Command::(\w+)"#)
            .unwrap();
    let mut count = 0;
    for (view, path) in [
        (ToolbarView::Text, "ca-view-text/src/lib.rs"),
        (ToolbarView::Folder, "ca-view-folder/src/lib.rs"),
        (ToolbarView::Hex, "ca-view-hex/src/lib.rs"),
        (ToolbarView::Table, "ca-view-table/src/lib.rs"),
        (ToolbarView::Picture, "ca-view-picture/src/lib.rs"),
        (ToolbarView::Merge, "ca-view-merge/src/lib.rs"),
        (ToolbarView::FolderMerge, "ca-view-folder/src/merge_view.rs"),
        (ToolbarView::Records, "ca-view-records/src/view.rs"),
    ] {
        let source = std::fs::read_to_string(root.join(path)).unwrap();
        let mut seen = 0;
        for declaration in declarations.captures_iter(&source) {
            let command = ca_ui::Command::ALL
                .iter()
                .copied()
                .find(|command| format!("{command:?}") == declaration[2])
                .unwrap();
            assert_eq!(
                toolbar_icon(view, &declaration[1]),
                command_icon(command),
                "{path}: {}",
                &declaration[1]
            );
            seen += 1;
        }
        assert!(seen > 0, "{path}");
        count += seen;
    }
    assert!(count >= 60, "covered {count} declarations");
}

#[test]
fn slot_icons_are_resolved_for_the_view_that_declares_them() {
    for view in ToolbarView::ALL {
        let items: Vec<_> = view
            .built_in()
            .iter()
            .filter(|entry| !entry.name.starts_with("separator-"))
            .map(|entry| ca_ui::toolbar::Item::widget(entry.name, 20.0))
            .collect();
        let context = egui::Context::default();
        let _ = context.run(ca_ui::testing::sized_input(2400.0, 800.0), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ca_ui::toolbar::show_for(
                    *view,
                    ui,
                    egui::Id::new(view.id()),
                    &items,
                    &ca_ui::toolbar::Layout::built_in(),
                    |ui, name| {
                        let icon = ui
                            .ctx()
                            .data(|data| {
                                data.get_temp::<Option<Icon>>(egui::Id::new("toolbar-icon"))
                            })
                            .flatten();
                        assert_eq!(icon, toolbar_icon(*view, name), "{} {name}", view.id());
                        ui.label(name);
                    },
                );
            });
        });
    }
    assert_eq!(
        toolbar_icon(ToolbarView::Table, "row-numbers"),
        Some(Icon::LineNumbers)
    );
}

#[test]
fn an_unavailable_icon_only_texture_draws_a_clickable_text_fallback() {
    let chosen = std::cell::Cell::new(false);
    let mut probe = ca_ui::testing::probe::Probe::new(800.0, 600.0);
    probe.context().set_pixels_per_point(40.0);
    let mut run = |context: &egui::Context| {
        egui::CentralPanel::default().show(context, |ui| {
            if ca_ui::widgets::icon_only(ui, "Close panel", Icon::Close, None).clicked() {
                chosen.set(true);
            }
        });
    };
    probe.idle(&mut run);
    let output = probe.frame(Vec::new(), &mut run);
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Close panel")));
    probe.click("Close panel", &mut run).unwrap();
    assert!(chosen.get());
}

#[test]
fn icon_files_are_exactly_the_embedded_masters_and_follow_the_svg_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/ui-icons");
    let elements = [
        "svg", "g", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon",
    ];
    let attributes = [
        "xmlns",
        "width",
        "height",
        "viewBox",
        "x",
        "y",
        "cx",
        "cy",
        "rx",
        "ry",
        "r",
        "x1",
        "y1",
        "x2",
        "y2",
        "d",
        "points",
        "fill",
        "stroke",
        "stroke-width",
        "stroke-linecap",
        "stroke-linejoin",
        "stroke-dasharray",
        "fill-rule",
    ];
    let tags = regex::Regex::new(r"</?([a-z]+)\b").unwrap();
    let attrs = regex::Regex::new(r#"([a-zA-Z][a-zA-Z0-9-]*)="([^"]*)""#).unwrap();
    assert_eq!(Icon::ALL.len(), 141);
    for (master, count) in [(16, 130), (24, 73)] {
        let expected: BTreeSet<_> = Icon::ALL
            .iter()
            .filter(|icon| icon.bytes(master).is_some())
            .map(|icon| format!("{}.svg", icon.id()))
            .collect();
        let actual: BTreeSet<_> = std::fs::read_dir(root.join(master.to_string()))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(expected.len(), count);
        for icon in Icon::ALL {
            let Some(bytes) = icon.bytes(master) else {
                continue;
            };
            let text = std::str::from_utf8(bytes).unwrap();
            assert!(text.ends_with('\n') && !text.contains('\r') && !text.starts_with('\u{feff}'));
            assert!(text.starts_with(&format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{master}\" height=\"{master}\" viewBox=\"0 0 {master} {master}\">\n")));
            assert!(!text.contains("<!--") && !text.contains("<?"));
            for capture in tags.captures_iter(text) {
                assert!(elements.contains(&&capture[1]));
            }
            for capture in attrs.captures_iter(text) {
                assert!(
                    attributes.contains(&&capture[1]),
                    "{}: {}",
                    icon.id(),
                    &capture[1]
                );
                if matches!(&capture[1], "fill" | "stroke") {
                    assert!(matches!(&capture[2], "#000" | "none"));
                }
            }
            resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).unwrap();
            assert_eq!(
                bytes,
                std::fs::read(
                    root.join(master.to_string())
                        .join(format!("{}.svg", icon.id()))
                )
                .unwrap()
            );
            for size in [16.0, 24.0, 32.0, 48.0] {
                assert!(
                    icon.texture(&egui::Context::default(), size).is_some(),
                    "{} at {size}",
                    icon.id()
                );
            }
        }
    }
}

#[test]
fn masters_follow_pixel_multiples_and_textures_are_reused_across_tints() {
    let both = Icon::StateSame;
    for (px, master) in [(16, 16), (20, 16), (24, 24), (32, 16), (48, 24), (36, 24)] {
        assert_eq!(both.master(px), master);
    }
    assert_eq!(Icon::Rules.master(16), 24);
    assert_eq!(Icon::Close.master(24), 16);
    let ctx = egui::Context::default();
    let first = both.texture(&ctx, 24.0).unwrap();
    assert_eq!(first.id(), both.texture(&ctx, 24.0).unwrap().id());
    let second = both.texture(&ctx, 16.0).unwrap();
    assert_ne!(first.id(), second.id());
    ctx.set_pixels_per_point(2.0);
    // Display scale takes effect at the start of a frame.
    let _ = ctx.run(ca_ui::testing::raw_input(), |_| {});
    assert_ne!(first.id(), both.texture(&ctx, 24.0).unwrap().id());
    assert!(both.texture(&ctx, f32::NAN).is_none());
}

#[test]
fn every_toolbar_entry_has_its_icon_or_is_an_explicit_unconverted_slot() {
    let plain = [
        "filter",
        "structure",
        "method",
        "name-filter",
        "files",
        "alignment",
        "encoding",
        "width",
        "hex-addresses",
        "font",
        "mode",
        "tolerance",
        "panes",
    ];
    for view in ToolbarView::ALL {
        for item in view.built_in() {
            let icon = toolbar_icon(*view, item.name);
            if item.name.starts_with("separator-") || plain.contains(&item.name) {
                assert_eq!(icon, None, "{} {}", view.id(), item.name);
            } else {
                assert!(icon.is_some(), "{} {}", view.id(), item.name);
            }
        }
    }
    for (slot, command) in [
        ("report", ca_ui::Command::CompareReport),
        ("copy", ca_ui::Command::CopyToOtherSide),
        ("reload", ca_ui::Command::Reload),
        ("swap", ca_ui::Command::SwapSides),
        ("take-both", ca_ui::Command::TakeLeftThenRight),
    ] {
        assert_eq!(
            toolbar_icon(ToolbarView::Merge, slot),
            command_icon(command)
        );
    }
    for kind in ca_session::SessionKind::ALL {
        assert!(session_icon(kind).has_16());
    }
    assert_eq!(
        session_icon(&ca_session::SessionKind::Unknown("future".into())),
        Icon::SessionUnknown
    );
}
