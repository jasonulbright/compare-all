//! Clicks delivered to a control found by what it announces.
//!
//! A frame run with AccessKit on reports every widget with its role, its
//! label, its state and its bounds. A control is found in that report by its
//! accessible label, never by where its text was painted, and is then pressed
//! and released through ordinary pointer events at the centre of its bounds.
//! A control that is drawn differently but keeps its label and its behavior
//! keeps passing.

use crate::command::Command;
use crate::toolbar::{self, Item, Layout};
use crate::view::{SessionView, ViewAction, ViewContext};
use egui::accesskit::{Role, Toggled};
use std::cell::{Cell, RefCell};

/// Time between two frames the probe runs.
const FRAME_SECONDS: f64 = 1.0 / 60.0;

/// Height of the window a toolbar is drawn in.
const BAR_WINDOW_HEIGHT: f32 = 600.0;

/// Width of the window a toolbar is drawn in, whatever room the bar is given,
/// so the overflow menu has room to open.
const BAR_WINDOW_WIDTH: f32 = 2_400.0;

/// The label the shared overflow button carries.
pub const OVERFLOW_LABEL: &str = "More";

/// Room that puts every toolbar item on the bar.
pub const WIDE_ROOM: f32 = 2_000.0;

/// Room that moves every toolbar item into the overflow menu.
pub const NARROW_ROOM: f32 = 40.0;

/// One widget as the accessibility tree reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    /// What a screen reader announces.
    pub label: String,
    /// What kind of widget it is.
    pub role: Role,
    /// Where it was laid out.
    pub rect: egui::Rect,
    /// False when the widget refuses input.
    pub enabled: bool,
    /// The state of a toggle or a check box.
    pub toggled: Option<bool>,
}

impl Control {
    /// True for a widget a pointer press acts on: a button, a toggle, a check
    /// box, a radio button or a link.
    #[must_use]
    pub fn is_pressable(&self) -> bool {
        matches!(
            self.role,
            Role::Button | Role::CheckBox | Role::RadioButton | Role::Link
        )
    }
}

/// The widgets one frame reported, read from its AccessKit update.
#[must_use]
pub fn controls(output: &egui::FullOutput) -> Vec<Control> {
    let Some(update) = output.platform_output.accesskit_update.as_ref() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (_, node) in &update.nodes {
        let (Some(label), Some(bounds)) = (node.label(), node.bounds()) else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let rect = egui::Rect::from_min_max(
            egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
            egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
        );
        found.push(Control {
            label: label.to_owned(),
            role: node.role(),
            rect,
            enabled: !node.is_disabled(),
            toggled: match node.toggled() {
                Some(Toggled::True) => Some(true),
                Some(Toggled::False) => Some(false),
                Some(Toggled::Mixed) | None => None,
            },
        });
    }
    found.sort_by(|a, b| {
        (a.rect.top(), a.rect.left())
            .partial_cmp(&(b.rect.top(), b.rect.left()))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    found
}

/// An egui context with AccessKit on, run one frame at a time.
pub struct Probe {
    ctx: egui::Context,
    size: egui::Vec2,
    time: f64,
    controls: Vec<Control>,
}

impl Probe {
    /// A probe over a window of the stated size.
    #[must_use]
    pub fn new(width: f32, height: f32) -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        Self {
            ctx,
            size: egui::vec2(width, height),
            time: 0.0,
            controls: Vec::new(),
        }
    }

    /// The context the frames run in.
    #[must_use]
    pub const fn context(&self) -> &egui::Context {
        &self.ctx
    }

    /// Run one frame carrying `events`.
    pub fn frame(
        &mut self,
        events: Vec<egui::Event>,
        run: &mut impl FnMut(&egui::Context),
    ) -> egui::FullOutput {
        self.time += FRAME_SECONDS;
        let input = egui::RawInput {
            events,
            time: Some(self.time),
            ..super::sized_input(self.size.x, self.size.y)
        };
        let output = self.ctx.run(input, |ctx| run(ctx));
        self.controls = controls(&output);
        output
    }

    /// Run one frame with no input.
    pub fn idle(&mut self, run: &mut impl FnMut(&egui::Context)) {
        let _ = self.frame(Vec::new(), run);
    }

    /// The widgets the last frame reported.
    #[must_use]
    pub fn controls(&self) -> &[Control] {
        &self.controls
    }

    /// Every label the last frame reported, for a failure message.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.controls
            .iter()
            .map(|control| control.label.clone())
            .collect()
    }

    /// Every control the last frame reported under `label`.
    #[must_use]
    pub fn find_all(&self, label: &str) -> Vec<Control> {
        self.controls
            .iter()
            .filter(|control| control.label == label)
            .cloned()
            .collect()
    }

    /// The one control the last frame reported under `label`.
    ///
    /// # Errors
    ///
    /// Names the label when no control or more than one control carries it.
    pub fn find(&self, label: &str) -> Result<Control, String> {
        let mut found = self.find_all(label);
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(format!(
                "no control is labelled {label:?}; the frame reported {:?}",
                self.labels()
            )),
            count => Err(format!("{count} controls are labelled {label:?}")),
        }
    }

    /// The one control the last frame reported under `label` inside `group`.
    ///
    /// `group` is the rectangle around a set of controls, such as the ones one
    /// menu holds, and tells apart two controls of the same name.
    ///
    /// # Errors
    ///
    /// Names the label when no control or more than one control inside the
    /// group carries it.
    pub fn find_within(&self, label: &str, group: egui::Rect) -> Result<Control, String> {
        let mut found: Vec<Control> = self
            .find_all(label)
            .into_iter()
            .filter(|control| group.contains(control.rect.center()))
            .collect();
        match found.len() {
            1 => Ok(found.remove(0)),
            count => Err(format!(
                "{count} controls inside {group:?} are labelled {label:?}"
            )),
        }
    }

    /// Press and release the one control labelled `label` inside `group`.
    ///
    /// # Errors
    ///
    /// Names the label when no control or more than one control inside the
    /// group carries it.
    pub fn click_within(
        &mut self,
        label: &str,
        group: egui::Rect,
        run: &mut impl FnMut(&egui::Context),
    ) -> Result<Control, String> {
        let control = self.find_within(label, group)?;
        self.press_at(control.rect.center(), run);
        Ok(control)
    }

    /// Press and release the one control labelled `label`.
    ///
    /// # Errors
    ///
    /// Names the label when no control or more than one control carries it.
    pub fn click(
        &mut self,
        label: &str,
        run: &mut impl FnMut(&egui::Context),
    ) -> Result<Control, String> {
        let control = self.find(label)?;
        self.press_at(control.rect.center(), run);
        Ok(control)
    }

    /// Press and release the primary button at `point`, then move the pointer
    /// off the window.
    ///
    /// Three frames run: the press, the release, and one with no pointer, so a
    /// tooltip the press raised is gone before the caller looks again.
    pub fn press_at(&mut self, point: egui::Pos2, run: &mut impl FnMut(&egui::Context)) {
        let button = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = self.frame(vec![egui::Event::PointerMoved(point), button(true)], run);
        let _ = self.frame(vec![button(false)], run);
        let _ = self.frame(vec![egui::Event::PointerGone], run);
    }
}

/// A frame body that draws `view` in the central panel and keeps the actions
/// it returns.
pub fn view_frame<'a, V: SessionView + ?Sized>(
    view: &'a mut V,
    context: &'a ViewContext,
    actions: &'a mut Vec<ViewAction>,
) -> impl FnMut(&egui::Context) + 'a {
    move |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            actions.extend(view.ui(ui, context));
        });
    }
}

/// Tick `view` and draw one frame of it in the central panel, returning the
/// actions it asked for.
pub fn draw_view<V: SessionView + ?Sized>(view: &mut V, ctx: &egui::Context) -> Vec<ViewAction> {
    let shared = super::context();
    let mut actions = Vec::new();
    view.tick();
    egui::CentralPanel::default().show(ctx, |ui| {
        actions = view.ui(ui, &shared);
    });
    actions
}

/// Lay `view` out twice, so every control has a place to be found at.
pub fn settle<V: SessionView + ?Sized>(probe: &mut Probe, view: &mut V) {
    let mut run = |ctx: &egui::Context| {
        let _ = draw_view(view, ctx);
    };
    probe.idle(&mut run);
    probe.idle(&mut run);
}

/// Press the control labelled `label` on `view` and return the actions the
/// view asked for while the press ran.
///
/// # Errors
///
/// Names the label when no single control carries it.
pub fn press_view<V: SessionView + ?Sized>(
    probe: &mut Probe,
    view: &mut V,
    label: &str,
) -> Result<Vec<ViewAction>, String> {
    let mut actions = Vec::new();
    let mut run = |ctx: &egui::Context| actions.extend(draw_view(view, ctx));
    probe.idle(&mut run);
    probe.click(label, &mut run)?;
    probe.idle(&mut run);
    Ok(actions)
}

/// Press the control labelled `label` inside `group` on `view` and return the
/// actions the view asked for while the press ran.
///
/// # Errors
///
/// Names the label when no single control inside the group carries it.
pub fn press_view_within<V: SessionView + ?Sized>(
    probe: &mut Probe,
    view: &mut V,
    label: &str,
    group: egui::Rect,
) -> Result<Vec<ViewAction>, String> {
    let mut actions = Vec::new();
    let mut run = |ctx: &egui::Context| actions.extend(draw_view(view, ctx));
    probe.idle(&mut run);
    probe.click_within(label, group, &mut run)?;
    probe.idle(&mut run);
    Ok(actions)
}

/// Press the toggle labelled `label` on `view` twice and check that each press
/// flips the setting `read` returns and that the control announces it.
///
/// # Errors
///
/// Says which press did not flip the setting or which state was announced
/// wrongly.
pub fn toggle_view<V: SessionView + ?Sized>(
    probe: &mut Probe,
    view: &mut V,
    label: &str,
    read: impl Fn(&V) -> bool,
) -> Result<(), String> {
    for press in 1..=2 {
        let before = read(view);
        let announced = {
            let mut run = |ctx: &egui::Context| {
                let _ = draw_view(view, ctx);
            };
            probe.idle(&mut run);
            probe.find(label)?.toggled
        };
        if announced != Some(before) {
            return Err(format!(
                "{label} announces {announced:?} while the setting is {before}"
            ));
        }
        let _ = press_view(probe, view, label)?;
        if read(view) == before {
            return Err(format!(
                "press {press} of {label} left the setting at {before}"
            ));
        }
    }
    Ok(())
}

/// The smallest rectangle holding every control whose label is in `labels`.
///
/// A label more than one control carries is left out, because a button of the
/// same name elsewhere in the view would stretch the rectangle over it.
#[must_use]
pub fn region_of(controls: &[Control], labels: &[&str]) -> Option<egui::Rect> {
    let carried = |label: &str| {
        controls
            .iter()
            .filter(|control| control.label == label)
            .count()
    };
    controls
        .iter()
        .filter(|control| labels.contains(&control.label.as_str()))
        .filter(|control| carried(&control.label) == 1)
        .map(|control| control.rect)
        .reduce(egui::Rect::union)
}

/// Pressable controls inside `region` whose label is not in `labels`.
#[must_use]
pub fn strangers(controls: &[Control], labels: &[&str], region: egui::Rect) -> Vec<Control> {
    controls
        .iter()
        .filter(|control| control.is_pressable())
        .filter(|control| region.contains(control.rect.center()))
        .filter(|control| !labels.contains(&control.label.as_str()))
        .cloned()
        .collect()
}

/// Pressable controls on the toolbar whose label is neither a command item of
/// `items` nor one of `slot_labels`.
///
/// The toolbar is the rectangle around every control the last frame of
/// `probe` reported under one of those labels.
///
/// # Errors
///
/// Says so when the frame reported none of the labels.
pub fn toolbar_strangers(
    probe: &Probe,
    items: &[Item],
    slot_labels: &[&str],
) -> Result<Vec<Control>, String> {
    let mut known: Vec<&str> = command_items(items)
        .into_iter()
        .map(|(_, label, _)| label)
        .collect();
    known.extend_from_slice(slot_labels);
    let region = region_of(probe.controls(), &known)
        .ok_or_else(|| format!("no toolbar control was reported: {:?}", probe.labels()))?;
    Ok(strangers(probe.controls(), &known, region))
}

/// How a toolbar item is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The item sits on the bar.
    Bar,
    /// The item sits behind the overflow button.
    Overflow,
}

/// What one click on a toolbar reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolbarClick {
    /// Every command the bar reported while the click ran, in frame order.
    pub commands: Vec<Command>,
    /// Where the item was when it was pressed.
    pub reach: Reach,
}

/// Draw `items` under `layout` in a bar `room` points wide and click the
/// control labelled `label`, opening the overflow menu first when the bar
/// collapsed into it.
///
/// Slots are left empty, so only the command items are drawn.
///
/// # Errors
///
/// Names the label when no single control carries it.
pub fn click_toolbar(
    items: &[Item],
    layout: &Layout,
    room: f32,
    label: &str,
) -> Result<ToolbarClick, String> {
    let reported: RefCell<Vec<Command>> = RefCell::new(Vec::new());
    let collapsed = Cell::new(false);
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let size = egui::vec2(room, ui.available_height());
            ui.allocate_ui(size, |ui| {
                let outcome =
                    toolbar::show(ui, egui::Id::new("probe-toolbar"), items, layout, |_, _| {});
                collapsed.set(outcome.collapsed);
                reported.borrow_mut().extend(outcome.command);
            });
        });
    };
    let mut probe = Probe::new(BAR_WINDOW_WIDTH, BAR_WINDOW_HEIGHT);
    // The first frame measures the bar; the overflow decision reads it.
    probe.idle(&mut draw);
    probe.idle(&mut draw);
    let reach = if collapsed.get() {
        probe.click(OVERFLOW_LABEL, &mut draw)?;
        probe.idle(&mut draw);
        Reach::Overflow
    } else {
        Reach::Bar
    };
    reported.borrow_mut().clear();
    probe.click(label, &mut draw)?;
    let commands = reported.borrow().clone();
    Ok(ToolbarClick { commands, reach })
}

/// The items with every command item switched on.
///
/// Whether an item is on follows the state of the view; switching every one
/// on lets a test press each of them without first bringing the view into the
/// state that allows it.
#[must_use]
pub fn all_enabled(items: &[Item]) -> Vec<Item> {
    items
        .iter()
        .cloned()
        .map(|mut item| {
            if let Item::Command { enabled, .. } = &mut item {
                *enabled = true;
            }
            item
        })
        .collect()
}

/// Every command item of `items`, as its stable name, its label and the
/// command it declares.
#[must_use]
pub fn command_items(items: &[Item]) -> Vec<(&'static str, &'static str, Command)> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Command {
                name,
                label,
                command,
                ..
            } => Some((*name, *label, *command)),
            _ => None,
        })
        .collect()
}

/// A command item as a test states it: its stable name, the label a user
/// presses, and the command that press has to run.
pub type Expected = (&'static str, &'static str, Command);

/// Check a toolbar's command items against what a test states they are.
///
/// `items` has to declare exactly the command items of `expected`, in that
/// order. Each label of `expected` is then pressed on a fresh bar `room`
/// points wide, from where `reach` says it sits, and has to report the stated
/// command exactly once. A command moved to another label, an item added
/// without a line in `expected`, and a button the builder wires to the wrong
/// command all fail.
///
/// # Errors
///
/// Lists every difference between the declaration and `expected`, and every
/// label that could not be found, was found in the wrong place, or reported
/// anything other than its stated command once.
pub fn every_command_reports(
    items: &[Item],
    expected: &[Expected],
    room: f32,
    reach: Reach,
) -> Result<(), String> {
    let mut problems = Vec::new();
    let declared = command_items(items);
    for index in 0..declared.len().max(expected.len()) {
        let (held, stated) = (declared.get(index), expected.get(index));
        if held != stated {
            problems.push(format!(
                "item {index}: the view declares {held:?}, the test states {stated:?}"
            ));
        }
    }
    let layout = Layout::built_in();
    for (name, label, command) in expected {
        match click_toolbar(items, &layout, room, label) {
            Ok(click) if click.reach != reach => {
                problems.push(format!("{name}: reached from {:?}", click.reach));
            }
            Ok(click) if click.commands == [*command] => {}
            Ok(click) => problems.push(format!(
                "{name}: pressing {label:?} has to run {command:?}, the bar reported {:?}",
                click.commands
            )),
            Err(error) => problems.push(format!("{name}: {error}")),
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}
