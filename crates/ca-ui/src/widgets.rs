//! Small shared pieces of the interface.

use crate::command::{Command, MenuView};
use crate::icons::Icon;
use egui::NumExt;

/// A button with an icon above its unchanged accessible label.
pub struct IconButton<'a> {
    label: &'a str,
    icon: Option<Icon>,
    selected: Option<bool>,
    above: bool,
    reserve: bool,
}

impl<'a> IconButton<'a> {
    /// A toolbar command or launcher entry.
    #[must_use]
    pub const fn new(label: &'a str, icon: Option<Icon>) -> Self {
        Self {
            label,
            icon,
            selected: None,
            above: true,
            reserve: false,
        }
    }
    /// A selected toggle.
    #[must_use]
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }
    /// Place the icon left of the label.
    #[must_use]
    pub const fn inline(mut self) -> Self {
        self.above = false;
        self
    }
    /// Reserve an icon column for aligned menu entries, including empty slots.
    #[must_use]
    pub const fn menu(mut self) -> Self {
        self.above = false;
        self.reserve = true;
        self
    }
}

impl egui::Widget for IconButton<'_> {
    fn ui(mut self, ui: &mut egui::Ui) -> egui::Response {
        if ui
            .ctx()
            .data(|data| data.get_temp::<bool>(egui::Id::new("toolbar-inline")))
            .unwrap_or(false)
        {
            self.above = false;
        }
        if self.icon.is_none() && !self.reserve {
            return ui.add(egui::Button::new(self.label).selected(self.selected.unwrap_or(false)));
        }
        let font = egui::TextStyle::Button.resolve(ui.style());
        let galley =
            ui.painter()
                .layout_no_wrap(self.label.to_owned(), font, ui.visuals().text_color());
        let size = if self.above { 24.0 } else { 16.0 };
        let gap = ui.spacing().icon_spacing;
        let padding = ui.spacing().button_padding;
        let content = if self.above {
            egui::vec2(galley.size().x.max(size), size + gap + galley.size().y)
        } else {
            egui::vec2(size + gap + galley.size().x, size.max(galley.size().y))
        };
        let desired = (content + padding * 2.0).at_least(ui.spacing().interact_size);
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
        response.widget_info(|| match self.selected {
            Some(state) => egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                ui.is_enabled(),
                state,
                self.label,
            ),
            None => {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), self.label)
            }
        });
        if ui.is_rect_visible(rect) {
            let visuals = ui
                .style()
                .interact_selectable(&response, self.selected.unwrap_or(false));
            ui.painter().rect(
                rect.expand(visuals.expansion),
                visuals.corner_radius,
                visuals.weak_bg_fill,
                visuals.bg_stroke,
                egui::StrokeKind::Inside,
            );
            let inner = rect.shrink2(padding);
            let (icon_rect, text_pos) = if self.above {
                (
                    egui::Rect::from_min_size(
                        egui::pos2(inner.center().x - size / 2.0, inner.min.y),
                        egui::vec2(size, size),
                    ),
                    egui::pos2(
                        inner.center().x - galley.size().x / 2.0,
                        inner.min.y + size + gap,
                    ),
                )
            } else {
                (
                    egui::Rect::from_min_size(
                        egui::pos2(inner.min.x, inner.center().y - size / 2.0),
                        egui::vec2(size, size),
                    ),
                    egui::pos2(
                        inner.min.x + size + gap,
                        inner.center().y - galley.size().y / 2.0,
                    ),
                )
            };
            if let Some(icon) = self.icon {
                icon.paint(ui.painter(), icon_rect, visuals.fg_stroke.color);
            }
            ui.painter()
                .galley_with_override_text_color(text_pos, galley, visuals.fg_stroke.color);
        }
        response
    }
}

/// An icon toggle that preserves the label and reports a changed value.
pub fn icon_toggle(ui: &mut egui::Ui, state: &mut bool, label: &str, icon: Icon) -> egui::Response {
    let mut response = ui.add(IconButton::new(label, Some(icon)).selected(*state));
    if response.clicked() {
        *state = !*state;
        response.mark_changed();
    }
    response
}

/// A toolbar popup opened through the same icon button as other commands.
pub fn icon_menu<R>(
    ui: &mut egui::Ui,
    label: &str,
    icon: Icon,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let key = egui::Id::new("toolbar-inline");
    let previous = ui.ctx().data(|data| data.get_temp::<bool>(key));
    let contents = |ui: &mut egui::Ui| {
        ui.ctx().data_mut(|data| data.insert_temp(key, true));
        let result = add(ui);
        ui.ctx().data_mut(|data| {
            if let Some(previous) = previous {
                data.insert_temp(key, previous);
            } else {
                data.remove::<bool>(key);
            }
        });
        result
    };
    // egui keeps one open popup: a popup opened inside another closes it, and
    // `close_menu` reaches only menu state. A nested entry is a submenu.
    if previous == Some(true) {
        let font = egui::TextStyle::Button.resolve(ui.style());
        let space = ui
            .painter()
            .layout_no_wrap(" ".into(), font, ui.visuals().text_color())
            .size()
            .x
            .max(1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = ((16.0 + ui.spacing().icon_spacing) / space)
            .ceil()
            .clamp(1.0, 32.0) as usize;
        let result = ui.menu_button(format!("{}{label}", " ".repeat(count)), contents);
        result.response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
        });
        icon.paint(
            ui.painter(),
            egui::Rect::from_center_size(
                egui::pos2(
                    result.response.rect.left() + ui.spacing().button_padding.x + 8.0,
                    result.response.rect.center().y,
                ),
                egui::vec2(16.0, 16.0),
            ),
            ui.style().interact(&result.response).fg_stroke.color,
        );
        return result;
    }
    let bar = ui.id();
    let mut state = egui::menu::BarState::load(ui.ctx(), bar);
    let response = ui.add(IconButton::new(label, Some(icon)));
    // A combo popup receives its click before the containing menu tests an
    // outside press; the parent must remain alive until that popup closes.
    let inner = if state.is_some() && ui.memory(egui::Memory::any_popup_open) {
        state.show(&response, contents)
    } else {
        state.bar_menu(&response, contents)
    };
    state.store(ui.ctx(), bar);
    egui::InnerResponse {
        response,
        inner: inner.map(|shown| shown.inner),
    }
}

/// An inline icon button with an unchanged accessible label.
pub fn inline_button(ui: &mut egui::Ui, label: &str, icon: Icon) -> egui::Response {
    ui.add(IconButton::new(label, Some(icon)).inline())
}

/// An icon-only button with a screen-reader label and hover text.
pub fn icon_only(
    ui: &mut egui::Ui,
    label: &str,
    icon: Icon,
    tint: Option<egui::Color32>,
) -> egui::Response {
    icon_only_with_hover(ui, label, label, icon, tint)
}

/// An icon-only control whose concise label and explanatory tooltip differ.
pub fn icon_only_with_hover(
    ui: &mut egui::Ui,
    label: &str,
    hover: &str,
    icon: Icon,
    tint: Option<egui::Color32>,
) -> egui::Response {
    if icon.texture(ui.ctx(), 16.0).is_none() {
        return ui.button(label).on_hover_text(hover);
    }
    let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let visuals = ui.style().interact(&response);
    icon.paint(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)),
        tint.unwrap_or(visuals.fg_stroke.color),
    );
    response.on_hover_text(hover)
}

/// An icon beside a message, tinted from the notice palette.
pub fn notice(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    icon: Icon,
    size: f32,
    text: &str,
) {
    let tint = match icon {
        Icon::Warning => palette.notice_warning,
        Icon::Error => palette.notice_error,
        _ => palette.notice_info,
    };
    ui.horizontal_top(|ui| {
        icon.show(ui, size, tint);
        ui.vertical(|ui| {
            wrapped_text(ui, text);
        });
    });
}

/// A message using the active runtime palette.
pub fn notice_current(ui: &mut egui::Ui, icon: Icon, size: f32, text: &str) {
    let palette = crate::options::runtime::current(ui.ctx()).tables.main;
    notice(ui, &palette, icon, size, text);
}

/// A menu entry that runs `command`, returning true when it was clicked.
///
/// An entry the current view cannot run is drawn disabled with the reason
/// attached, never hidden, so the command set a view offers stays visible.
pub fn command_item(ui: &mut egui::Ui, command: Command, enabled: bool, reason: &str) -> bool {
    command_item_in(ui, MenuView::Other, command, enabled, reason)
}

/// A menu entry drawn with the label and the keystroke the bar of `view` uses.
pub fn command_item_in(
    ui: &mut egui::Ui,
    view: MenuView,
    command: Command,
    enabled: bool,
    reason: &str,
) -> bool {
    let label = match command.shortcut_in(view) {
        Some(shortcut) => format!("{}\t{shortcut}", command.label_in(view)),
        None => command.label_in(view).to_string(),
    };
    let response = ui.add_enabled(
        enabled,
        IconButton::new(&label, crate::icons::command_icon(command)).menu(),
    );
    if !enabled {
        let _ = disabled_reason(response, reason);
        return false;
    }
    if response.clicked() {
        ui.close_menu();
        return true;
    }
    false
}

/// Attach why a disabled control refuses input.
///
/// egui draws a hover text only over an enabled widget, so the reason goes
/// through the tooltip egui draws over a disabled one. The reason is also the
/// description of the control's accessibility node, where a screen reader
/// reads it. An enabled control is returned unchanged, so a tooltip it
/// carries for its normal use still shows and the reason does not.
#[must_use]
pub fn disabled_reason(response: egui::Response, reason: &str) -> egui::Response {
    if response.enabled() || reason.is_empty() {
        return response;
    }
    describe_refusal(&response, reason);
    response.on_disabled_hover_text(reason)
}

/// Name why a control refuses input in its accessibility node.
///
/// A control that refuses input while the widget under it stays enabled,
/// such as an area that senses only hover, shows its reason through an
/// ordinary tooltip and states it here.
pub fn describe_refusal(response: &egui::Response, reason: &str) {
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_description(reason.to_owned());
    });
}

/// A menu entry for something this build does not do yet.
pub fn pending_item(ui: &mut egui::Ui, label: &str) {
    let _ = disabled_reason(
        ui.add_enabled(false, egui::Button::new(label)),
        "Not available in this build",
    );
}

/// A toolbar button that is disabled with a stated reason rather than absent.
pub fn toolbar_button(ui: &mut egui::Ui, label: &str, enabled: bool, reason: &str) -> bool {
    let icon = ui
        .ctx()
        .data(|data| data.get_temp::<Option<Icon>>(egui::Id::new("toolbar-icon")))
        .flatten();
    toolbar_button_with_icon(ui, label, enabled, reason, icon)
}

/// Give a group of toolbar controls explicit artwork and inline placement.
pub fn with_toolbar_icon<R>(
    ui: &mut egui::Ui,
    icon: Icon,
    inline: bool,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let icon_key = egui::Id::new("toolbar-icon");
    let inline_key = egui::Id::new("toolbar-inline");
    let previous = ui.ctx().data(|data| {
        (
            data.get_temp::<Option<Icon>>(icon_key),
            data.get_temp::<bool>(inline_key),
        )
    });
    ui.ctx().data_mut(|data| {
        data.insert_temp(icon_key, Some(icon));
        data.insert_temp(inline_key, inline);
    });
    let result = add(ui);
    ui.ctx().data_mut(|data| {
        if let Some(icon) = previous.0 {
            data.insert_temp(icon_key, icon);
        } else {
            data.remove::<Option<Icon>>(icon_key);
        }
        if let Some(inline) = previous.1 {
            data.insert_temp(inline_key, inline);
        } else {
            data.remove::<bool>(inline_key);
        }
    });
    result
}

/// A toolbar button with an explicit optional icon.
pub fn toolbar_button_with_icon(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    reason: &str,
    icon: Option<Icon>,
) -> bool {
    let response = ui.add_enabled(enabled, IconButton::new(label, icon));
    if enabled {
        response.clicked()
    } else {
        let _ = disabled_reason(response, reason);
        false
    }
}

/// What the user asked of a path bar this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathBarAction {
    /// Raise a picker for one side.
    Browse(crate::dialog::Target),
    /// Read both sides again from the fields as they stand.
    Reload,
}

/// Width a path field takes, which is a share of the room the bar was given.
fn field_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() * 0.35).clamp(80.0, 420.0)
}

/// The two path fields, a browse button for each, and the button that re-reads.
///
/// Laid out wrapped rather than fixed, so a narrow window pushes a field onto
/// the next line instead of clipping it. `reload_label` names the last button,
/// because what re-reading means differs between comparisons.
pub fn path_bar(
    ui: &mut egui::Ui,
    left: &mut String,
    right: &mut String,
    reload_label: &str,
) -> Option<PathBarAction> {
    path_bar_with_titles(ui, left, right, None, None, reload_label)
}

/// The two sides' paths, optional caller names, browse buttons and reload.
///
/// A caller name replaces the visible path field while the underlying path is
/// kept for loading and saving. This is useful for tools that compare
/// temporary copies under names the user recognizes.
pub fn path_bar_with_titles(
    ui: &mut egui::Ui,
    left: &mut String,
    right: &mut String,
    left_title: Option<&str>,
    right_title: Option<&str>,
    reload_label: &str,
) -> Option<PathBarAction> {
    use crate::dialog::Target;
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        let field = field_width(ui);
        ui.label("Left");
        if let Some(title) = left_title.filter(|title| !title.is_empty()) {
            ui.add_sized(
                egui::vec2(field, ui.spacing().interact_size.y),
                egui::Label::new(title).truncate(),
            );
        } else {
            ui.add(egui::TextEdit::singleline(left).desired_width(field));
        }
        if crate::widgets::inline_button(ui, "Browse", crate::icons::Icon::Browse).clicked() {
            action = Some(PathBarAction::Browse(Target::Left));
        }
        ui.label("Right");
        if let Some(title) = right_title.filter(|title| !title.is_empty()) {
            ui.add_sized(
                egui::vec2(field, ui.spacing().interact_size.y),
                egui::Label::new(title).truncate(),
            );
        } else {
            ui.add(egui::TextEdit::singleline(right).desired_width(field));
        }
        if crate::widgets::inline_button(ui, "Browse", crate::icons::Icon::Browse).clicked() {
            action = Some(PathBarAction::Browse(Target::Right));
        }
        if inline_button(ui, reload_label, Icon::Reload).clicked() {
            action = Some(PathBarAction::Reload);
        }
    });
    action
}

/// Room a toolbar keeps for the button that opens what did not fit.
const CHEVRON_WIDTH: f32 = 34.0;

/// Height reserved by a toolbar button with an icon above its label.
#[must_use]
pub fn toolbar_height(ui: &egui::Ui) -> f32 {
    (24.0
        + ui.spacing().icon_spacing
        + ui.text_style_height(&egui::TextStyle::Button)
        + ui.spacing().button_padding.y * 2.0)
        .max(ui.spacing().interact_size.y)
}

/// Place a control in a box of a stated width.
///
/// A wrapped layout chooses where to break from the size it is given before a
/// control is built. A control that sizes itself while it is built, such as a
/// drop down, is therefore placed wherever the cursor already stands and runs
/// past the edge of the line. Claiming a box of a known width first moves the
/// break in front of it.
pub fn sized<R>(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let height = if ui
        .ctx()
        .data(|data| data.get_temp::<bool>(egui::Id::new("toolbar-inline")))
        == Some(false)
    {
        toolbar_height(ui)
    } else {
        ui.spacing().interact_size.y
    };
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        add,
    )
    .inner
}

/// A toolbar that never draws outside the room it was given.
///
/// Controls are laid out wrapped, so a narrow window moves them onto further
/// lines rather than off the edge. The width the controls needed last frame is
/// remembered; once that is more than the bar has, the whole set moves into a
/// menu behind one button, which is always narrow enough to fit.
pub struct Overflow {
    id: egui::Id,
    collapsed: bool,
}

impl Overflow {
    /// Decide how `ui` should carry its toolbar this frame.
    #[must_use]
    pub fn new(ui: &egui::Ui, id: egui::Id) -> Self {
        let room = ui.available_width();
        let needed = ui
            .ctx()
            .memory(|memory| memory.data.get_temp::<f32>(id))
            .unwrap_or(0.0);
        Self {
            id,
            collapsed: room <= CHEVRON_WIDTH * 2.0 || needed > room,
        }
    }

    /// True when the controls went into the menu rather than onto the bar.
    #[must_use]
    pub const fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// Lay out the controls and report the rectangle they took.
    pub fn show(&mut self, ui: &mut egui::Ui, controls: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
        if self.collapsed {
            return ui
                .horizontal(|ui| {
                    icon_menu(ui, "More", Icon::More, |ui| {
                        ui.set_min_width(180.0);
                        controls(ui);
                    });
                })
                .response
                .rect;
        }
        let room = ui.available_width();
        let response = ui.horizontal_wrapped(controls);
        let needed = response.response.rect.width();
        // Only a measurement taken while the controls were on the bar says what
        // they need; the menu's own width says nothing about them.
        let id = self.id;
        ui.ctx().memory_mut(|memory| {
            memory
                .data
                .insert_temp(id, if needed > room { needed } else { 0.0 });
        });
        response.response.rect
    }
}

/// Thickness of a scrollbar drawn by [`horizontal_scrollbar`].
pub const SCROLLBAR: f32 = 12.0;

/// The bar under two panes that scrolls both of them sideways.
///
/// Content, viewport and offset are all in the same unit, which the caller
/// chooses: pixels for a grid, character cells for a text pane. The new offset
/// is returned when the bar was dragged, and nothing otherwise, so the caller
/// decides what a change of offset means for its own panes.
///
/// `minimum_span` keeps the thumb grabbable on a very wide comparison.
#[must_use]
pub fn horizontal_scrollbar(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    id: egui::Id,
    track: egui::Rect,
    colors: (egui::Color32, egui::Color32),
    geometry: (f32, f32, f32),
    minimum_span: f32,
) -> Option<f32> {
    let (background, thumb) = colors;
    let (content, viewport, offset) = geometry;
    if track.width() <= 0.0 || track.height() <= 0.0 {
        return None;
    }
    painter.rect_filled(track, 0.0, background);
    let viewport = viewport.max(1.0);
    let content = content.max(viewport);
    let span =
        (viewport / content * track.width()).clamp(minimum_span.min(track.width()), track.width());
    let travel = (track.width() - span).max(0.0);
    let limit = crate::scroll::max_offset(content, viewport);
    let progress = if limit <= 0.0 {
        0.0
    } else {
        (offset / limit).clamp(0.0, 1.0)
    };
    let left = track.left() + progress * travel;
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(left, track.top() + 2.0),
            egui::pos2((left + span).min(track.right()), track.bottom() - 2.0),
        ),
        2.0,
        thumb,
    );
    let response = ui.interact(track, id, egui::Sense::click_and_drag());
    let grabbed = response.interact_pointer_pos()?;
    if travel <= 0.0 {
        return None;
    }
    let fraction = ((grabbed.x - track.left() - span / 2.0) / travel).clamp(0.0, 1.0);
    Some(fraction * limit)
}

/// What one side of a comparison holds, for the line under the path bar.
///
/// Every field is optional because the comparisons differ in what they know: a
/// byte comparison has no encoding, a table has no line ending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileInfo {
    /// Which side this is.
    pub side: String,
    /// Size in bytes.
    pub size: Option<u64>,
    /// Last modification time.
    pub modified: Option<std::time::SystemTime>,
    /// What the file was read as.
    pub format: Option<String>,
    /// The character encoding the text was decoded with.
    pub encoding: Option<String>,
    /// The line ending the file uses.
    pub line_ending: Option<String>,
    /// True when the pane holds edits that are not written.
    pub is_modified: bool,
}

impl FileInfo {
    /// A side with nothing known about it yet.
    #[must_use]
    pub fn new(side: impl Into<String>) -> Self {
        Self {
            side: side.into(),
            ..Self::default()
        }
    }

    /// The line this side reads as, in the machine's own time zone.
    #[must_use]
    pub fn summary(&self, offset_seconds: i32) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(size) = self.size {
            parts.push(format!("{} bytes", crate::format::format_bytes(size)));
        }
        if self.modified.is_some() {
            parts.push(crate::format::format_stamp(self.modified, offset_seconds));
        }
        for value in [&self.format, &self.encoding, &self.line_ending]
            .into_iter()
            .flatten()
        {
            if !value.is_empty() {
                parts.push(value.clone());
            }
        }
        if self.is_modified {
            parts.push("modified".to_owned());
        }
        if parts.is_empty() {
            return format!("{}: -", self.side);
        }
        format!("{}: {}", self.side, parts.join(", "))
    }
}

/// The line under a path bar naming what each side holds.
pub fn file_info_bar(ui: &mut egui::Ui, offset_seconds: i32, sides: &[FileInfo]) {
    ui.horizontal_wrapped(|ui| {
        for (index, side) in sides.iter().enumerate() {
            if index > 0 {
                ui.separator();
            }
            ui.label(side.summary(offset_seconds));
        }
    });
}

/// The character an elided middle is marked with.
const ELLIPSIS: char = '\u{2026}';

/// Shorten `text` by removing characters from the middle.
///
/// The head and the tail are what identify a path, so both are kept and the
/// part between them is dropped. A budget below three characters yields the
/// marker alone.
#[must_use]
pub fn elide_middle(text: &str, budget: usize) -> String {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() <= budget {
        return text.to_string();
    }
    if budget < 3 {
        return ELLIPSIS.to_string();
    }
    let kept = budget - 1;
    let tail = kept / 2;
    let head = kept - tail;
    let mut out: String = characters[..head].iter().collect();
    out.push(ELLIPSIS);
    out.extend(&characters[characters.len() - tail..]);
    out
}

/// Width one line of body text takes.
fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER)
            .size()
            .x
    })
}

/// Shorten `text` until one line of it fits `width`.
#[must_use]
pub fn elide_to_width(ui: &egui::Ui, text: &str, width: f32) -> String {
    if text_width(ui, text) <= width {
        return text.to_string();
    }
    let total = text.chars().count();
    let (mut low, mut high) = (0_usize, total);
    while low < high {
        let middle = usize::midpoint(low + 1, high);
        if text_width(ui, &elide_middle(text, middle)) <= width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    elide_middle(text, low)
}

/// A path drawn on one line that never runs past the space it was given.
///
/// The middle is dropped rather than the tail, so the file name stays
/// readable, and the whole path is attached as a tooltip.
pub fn path_line(ui: &mut egui::Ui, prefix: &str, path: &std::path::Path) -> egui::Response {
    path_text(ui, &format!("{prefix}{}", path.display()))
}

/// A line of text that may be a path, drawn so it never runs past the space it
/// was given.
pub fn path_text(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let width = (ui.available_width() - 1.0).max(16.0);
    let shown = elide_to_width(ui, text, width);
    let response = ui.add(egui::Label::new(shown).truncate());
    if text_width(ui, text) > width {
        return response.on_hover_text(text.to_string());
    }
    response
}

/// Text that may carry a path, drawn so no part of it runs past the space it
/// was given.
///
/// Words wrap on their own. A single word wider than the line cannot wrap, so
/// it is elided in the middle instead, and the untouched text is attached as a
/// tooltip.
pub fn wrapped_text(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let width = (ui.available_width() - 1.0).max(16.0);
    let mut shortened = String::with_capacity(text.len());
    let mut elided = false;
    let mut word = String::new();
    for character in text.chars().chain(std::iter::once(' ')) {
        if !character.is_whitespace() {
            word.push(character);
            continue;
        }
        if text_width(ui, &word) > width {
            shortened.push_str(&elide_to_width(ui, &word, width));
            elided = true;
        } else {
            shortened.push_str(&word);
        }
        word.clear();
        shortened.push(character);
    }
    shortened.pop();
    ui.set_max_width(width);
    let response = ui.add(egui::Label::new(shortened).wrap());
    if elided {
        return response.on_hover_text(text.to_string());
    }
    response
}

#[cfg(test)]
mod tests {
    use super::{elide_middle, FileInfo};

    #[test]
    fn a_side_with_nothing_known_reads_as_a_dash() {
        assert_eq!(FileInfo::new("Left").summary(0), "Left: -");
    }

    #[test]
    fn a_size_carries_a_thousands_separator_and_the_time_its_zone() {
        let info = FileInfo {
            size: Some(45_739),
            modified: Some(std::time::UNIX_EPOCH),
            ..FileInfo::new("Left")
        };
        assert_eq!(info.summary(0), "Left: 45,739 bytes, 1/1/1970 12:00:00 AM");
    }

    #[test]
    fn only_the_fields_a_comparison_knows_are_shown() {
        let info = FileInfo {
            encoding: Some("UTF-8".to_owned()),
            line_ending: Some("CRLF".to_owned()),
            is_modified: true,
            ..FileInfo::new("Right")
        };
        assert_eq!(info.summary(0), "Right: UTF-8, CRLF, modified");
    }

    #[test]
    fn an_empty_field_is_left_out() {
        let info = FileInfo {
            format: Some(String::new()),
            size: Some(7),
            ..FileInfo::new("Left")
        };
        assert_eq!(info.summary(0), "Left: 7 bytes");
    }

    #[test]
    fn a_short_path_is_left_alone() {
        assert_eq!(elide_middle("abcdef", 6), "abcdef");
        assert_eq!(elide_middle("abcdef", 99), "abcdef");
    }

    #[test]
    fn a_long_path_keeps_its_head_and_its_tail() {
        let elided = elide_middle("0123456789", 5);
        assert_eq!(elided.chars().count(), 5);
        assert!(elided.starts_with("01"));
        assert!(elided.ends_with("89"));
        assert!(elided.contains('\u{2026}'));
    }

    #[test]
    fn a_budget_below_three_yields_the_marker_alone() {
        assert_eq!(elide_middle("0123456789", 2), "\u{2026}");
    }
}
