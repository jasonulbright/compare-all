//! The pack browser beside the Options dialog, drawn as the program draws it:
//! both are windows on one screen.
//!
//! The browser must be a window of its own, never a part of the Options
//! window, so the Options page keeps painting wherever the browser is moved
//! and the browser opens at a size that lists every pack.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

mod pack_net;

use ca_session::options::ProgramOptions;
use ca_session::AdminPolicies;
use ca_ui::options::OptionsDialog;
use ca_ui::pack::browser::{CLOSE_LABEL, OPEN_LABEL};
use ca_ui::testing::probe::{painted_texts, Control, Probe};
use ca_ui::theme::{palette, Variant};
use egui::accesskit::NodeId;
use pack_net::FakeTransport;
use pack_net::{good_zip, serve_index, serve_pack, services};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const PACKS: usize = 9;
const OPTIONS: &str = "Options";
const BROWSER: &str = "Theme packs";

/// Labels of the controls on the Appearance page.
const PAGE_LABELS: [&str; 7] = [
    "Theme",
    "Apply changes to open views at once",
    "Editor font size",
    "Byte pane font size",
    "Listing font size",
    "Merge input pane font size",
    OPEN_LABEL,
];

struct Screen {
    _dir: tempfile::TempDir,
    dialog: RefCell<OptionsDialog>,
    probe: Probe,
    tree: Vec<Node>,
}

struct Node {
    id: NodeId,
    children: Vec<NodeId>,
    window: Option<String>,
}

impl Screen {
    fn new(width: f32, height: f32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let packs = dir.path().join("packs");
        let transport = Arc::new(FakeTransport::default());
        let mut entries = Vec::new();
        for n in 0..PACKS {
            let id = format!("p{n}");
            let name = format!("Pack{n}");
            entries.push(serve_pack(&transport, &id, &name, good_zip(&id, &name)));
        }
        serve_index(&transport, &entries);
        let mut dialog =
            OptionsDialog::new(&ProgramOptions::default(), AdminPolicies::default(), 1);
        dialog.set_pack_services(Some(services(&transport, &packs)));
        dialog.select_page_named("Appearance");
        let mut screen = Self {
            _dir: dir,
            dialog: RefCell::new(dialog),
            probe: Probe::new(width, height),
            tree: Vec::new(),
        };
        for _ in 0..4 {
            let _ = screen.frame(Vec::new());
        }
        screen
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        let table = palette(Variant::Light);
        let dialog = &self.dialog;
        let mut run = |ctx: &egui::Context| {
            let _ = dialog.borrow_mut().show(ctx, &table);
        };
        let output = self.probe.frame(events, &mut run);
        self.tree = output
            .platform_output
            .accesskit_update
            .as_ref()
            .map(|update| {
                update
                    .nodes
                    .iter()
                    .map(|(id, node)| Node {
                        id: *id,
                        children: node.children().to_vec(),
                        window: (node.role() == egui::accesskit::Role::Window)
                            .then(|| node.label().unwrap_or_default().to_owned()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        output
    }

    fn press(&mut self, point: egui::Pos2, pressed: bool) -> egui::FullOutput {
        self.frame(vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            },
        ])
    }

    fn open_browser(&mut self) {
        let button = self.probe.find(OPEN_LABEL).unwrap();
        let _ = self.press(button.rect.center(), true);
        let _ = self.press(button.rect.center(), false);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let _ = self.frame(Vec::new());
            let last = format!("Install Pack{}", PACKS - 1);
            if self.probe.find(&last).is_ok() && self.probe.find(CLOSE_LABEL).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "{:?}", self.probe.labels());
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..4 {
            let _ = self.frame(Vec::new());
        }
    }

    fn window(&self, title: &str) -> Control {
        self.probe
            .controls()
            .iter()
            .find(|c| c.label == title && c.role == egui::accesskit::Role::Window)
            .cloned()
            .unwrap_or_else(|| panic!("no window {title:?}: {:?}", self.probe.labels()))
    }

    /// Drag the browser window by its title bar by `delta`, one frame per step.
    fn drag_browser(&mut self, delta: egui::Vec2) -> Vec<egui::FullOutput> {
        let title = self.window(BROWSER).rect;
        let from = egui::pos2(title.left() + 40.0, title.top() + 8.0);
        let mut outputs = vec![self.press(from, true)];
        let steps: u8 = 8;
        for step in 1..=steps {
            let to = from + delta * (f32::from(step) / f32::from(steps));
            outputs.push(self.frame(vec![egui::Event::PointerMoved(to)]));
        }
        outputs.push(self.press(from + delta, false));
        outputs
    }

    fn visible_page_labels(&self) -> Vec<&'static str> {
        PAGE_LABELS
            .into_iter()
            .filter(|label| self.probe.find(label).is_ok())
            .collect()
    }

    /// Whether the node of the window `title` has the window `ancestor` among
    /// its ancestors in the accessibility tree.
    fn nested_in(&self, title: &str, ancestor: &str) -> bool {
        let node_of = |label: &str| {
            self.tree
                .iter()
                .find(|node| node.window.as_deref() == Some(label))
                .map(|node| node.id)
        };
        let (Some(inner), Some(outer)) = (node_of(title), node_of(ancestor)) else {
            panic!("window nodes missing");
        };
        let parents: HashMap<NodeId, NodeId> = self
            .tree
            .iter()
            .flat_map(|node| node.children.iter().map(|child| (*child, node.id)))
            .collect();
        let mut at = inner;
        while let Some(parent) = parents.get(&at) {
            if *parent == outer {
                return true;
            }
            at = *parent;
        }
        false
    }
}

#[test]
fn the_browser_is_a_window_beside_the_options_window_not_inside_it() {
    let mut screen = Screen::new(1920.0, 1080.0);
    screen.open_browser();
    assert!(
        !screen.nested_in(BROWSER, OPTIONS),
        "the browser window hangs under the Options window in the accessibility tree"
    );
}

#[test]
fn the_options_page_keeps_painting_while_the_browser_moves_over_it() {
    let mut screen = Screen::new(1920.0, 1080.0);
    let before = screen.visible_page_labels();
    assert_eq!(before.len(), PAGE_LABELS.len(), "{before:?}");
    screen.open_browser();
    let options = screen.window(OPTIONS).rect;
    let browser = screen.window(BROWSER).rect;
    let delta = options.center() - browser.center();
    let page_texts = |texts: &[String]| {
        PAGE_LABELS
            .iter()
            .filter(|label| texts.iter().any(|t| t == *label))
            .count()
    };
    for output in screen.drag_browser(delta) {
        let texts = painted_texts(&output);
        assert!(
            page_texts(&texts) >= 5,
            "the Options page lost its text while the browser moved: {texts:?}"
        );
    }
    for _ in 0..3 {
        let output = screen.frame(Vec::new());
        assert!(page_texts(&painted_texts(&output)) >= 5);
    }
    assert_eq!(screen.visible_page_labels(), before);
    let moved = screen.window(BROWSER).rect;
    assert!(
        moved.intersects(screen.window(OPTIONS).rect),
        "the browser did not move over the dialog: {moved:?}"
    );
}

#[test]
fn the_browser_lists_every_pack_at_its_default_size_on_a_1080p_screen() {
    let mut screen = Screen::new(1920.0, 1080.0);
    screen.open_browser();
    let window = screen.window(BROWSER).rect;
    let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
    assert!(screen_rect.contains_rect(window), "{window:?}");
    for n in 0..PACKS {
        let button = screen.probe.find(&format!("Install Pack{n}")).unwrap();
        assert!(
            window.contains_rect(button.rect),
            "Pack{n} is scrolled out of the window {window:?}: {:?}",
            button.rect
        );
    }
    let close = screen.probe.find(CLOSE_LABEL).unwrap();
    assert!(window.contains_rect(close.rect), "{:?}", close.rect);
}

#[test]
fn a_screen_too_short_for_the_list_scrolls_inside_the_screen() {
    let mut screen = Screen::new(1280.0, 600.0);
    screen.open_browser();
    let window = screen.window(BROWSER).rect;
    let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 600.0));
    assert!(screen_rect.contains_rect(window), "{window:?}");
    let close = screen.probe.find(CLOSE_LABEL).unwrap();
    assert!(window.contains_rect(close.rect), "Close is out of reach");
    let first = screen.probe.find("Install Pack0").unwrap();
    let last_label = format!("Install Pack{}", PACKS - 1);
    let last = screen.probe.find(&last_label).unwrap();
    assert!(!window.contains_rect(last.rect), "the list does not scroll");
    let over_list = first.rect.center() - egui::vec2(150.0, 0.0);
    let _ = screen.frame(vec![egui::Event::PointerMoved(over_list)]);
    for _ in 0..30 {
        let _ = screen.frame(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            modifiers: egui::Modifiers::default(),
        }]);
    }
    let window = screen.window(BROWSER).rect;
    let last = screen.probe.find(&last_label).unwrap();
    assert!(
        window.contains_rect(last.rect),
        "the wheel did not bring the last pack into the window {window:?}: {:?}",
        last.rect
    );
    let first = screen.probe.find("Install Pack0").unwrap();
    assert!(
        !window.contains_rect(first.rect),
        "the first pack stayed in view: {:?}",
        first.rect
    );
}
