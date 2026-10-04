//! One toolbar builder every view draws through.
//!
//! A view declares its items as data: a command item, a separator, or a slot a
//! custom control is drawn into. The builder resolves the order and the
//! visibility from the options document, falling back to the order the view
//! declared, and draws the result with the shared overflow behavior.
//!
//! Declaration and drawing are two passes on purpose. A view holds one mutable
//! borrow of itself while it draws, so the custom controls are drawn through a
//! single callback rather than through one closure per slot.

use crate::command::Command;
use crate::widgets;
use ca_session::options::CommandOptions;

/// Which toolbar a view carries.
///
/// The toolbar follows the comparison type rather than the menu bar, because
/// each comparison type carries a different set of controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolbarView {
    /// The text comparison toolbar.
    Text,
    /// The folder comparison toolbar.
    Folder,
    /// The hex comparison toolbar.
    Hex,
    /// The table comparison toolbar.
    Table,
    /// The picture comparison toolbar.
    Picture,
    /// The three way merge toolbar.
    Merge,
    /// The folder merge toolbar.
    FolderMerge,
    /// The registry, version and media comparison toolbar.
    Records,
}

impl ToolbarView {
    /// Every toolbar, in the order the options page lists them.
    pub const ALL: &'static [Self] = &[
        Self::Text,
        Self::Folder,
        Self::Hex,
        Self::Table,
        Self::Picture,
        Self::Merge,
        Self::FolderMerge,
        Self::Records,
    ];

    /// The stable name a stored document holds this toolbar under.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Folder => "folder",
            Self::Hex => "hex",
            Self::Table => "table",
            Self::Picture => "picture",
            Self::Merge => "merge",
            Self::FolderMerge => "folder-merge",
            Self::Records => "records",
        }
    }

    /// The name the options page shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "Text comparisons",
            Self::Folder => "Folder comparisons",
            Self::Hex => "Hex comparisons",
            Self::Table => "Table comparisons",
            Self::Picture => "Picture comparisons",
            Self::Merge => "Merges",
            Self::FolderMerge => "Folder merges",
            Self::Records => "Registry, version and media comparisons",
        }
    }

    /// The toolbar stored under `id`, where this build has one.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|view| view.id() == id)
    }

    /// The items the view declares, in the order it declares them.
    #[must_use]
    pub fn built_in(self) -> &'static [ItemName] {
        defaults(self)
    }
}

/// The name and the label of one declared item.
///
/// The options page needs both without a view being built, so the declaration
/// of each toolbar is stated here as well and a guard test holds the two
/// together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemName {
    /// Stable name the stored order holds the item under.
    pub name: &'static str,
    /// What the options page shows for it.
    pub label: &'static str,
}

const fn item(name: &'static str, label: &'static str) -> ItemName {
    ItemName { name, label }
}

/// Name every separator carries, numbered from the left.
macro_rules! separator_name {
    ($index:literal) => {
        concat!("separator-", $index)
    };
}

/// The label a separator row carries in the options page.
pub const SEPARATOR_LABEL: &str = "Separator";

const TEXT_ITEMS: &[ItemName] = &[
    item("home", "Home"),
    item("sessions", "Sessions"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("all", "All"),
    item("diffs", "Diffs"),
    item("same", "Same"),
    item("context", "Context"),
    item("minor", "Minor"),
    item("rules", "Rules"),
    item("format", "Format"),
    item(separator_name!(2), SEPARATOR_LABEL),
    item("copy", "Copy"),
    item("next-section", "Next Section"),
    item("previous-section", "Prev Section"),
    item("swap", "Swap"),
    item("reload", "Reload"),
    item(separator_name!(3), SEPARATOR_LABEL),
    item("report", "Report"),
];

const FOLDER_ITEMS: &[ItemName] = &[
    item("home", "Home"),
    item("sessions", "Sessions"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("all", "All"),
    item("diffs", "Diffs"),
    item("same", "Same"),
    item("filter", "Display filter"),
    item("structure", "Structure"),
    item("minor", "Minor"),
    item("rules", "Rules"),
    item(separator_name!(2), SEPARATOR_LABEL),
    item("copy", "Copy"),
    item("expand", "Expand"),
    item("collapse", "Collapse"),
    item("select", "Select"),
    item("files", "Files"),
    item("method", "Content method"),
    item("compare-contents", "Compare Contents"),
    item("refresh", "Refresh"),
    item("swap", "Swap"),
    item("stop", "Stop"),
    item(separator_name!(3), SEPARATOR_LABEL),
    item("name-filter", "Filters field"),
    item("peek", "Peek"),
    item("report", "Report"),
];

const HEX_ITEMS: &[ItemName] = &[
    item("previous-section", "Prev Section"),
    item("previous-difference", "Prev Difference"),
    item("next-difference", "Next Difference"),
    item("next-section", "Next Section"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("filter", "Display filter"),
    item(separator_name!(2), SEPARATOR_LABEL),
    item("alignment", "Alignment"),
    item("encoding", "Encoding"),
    item(separator_name!(3), SEPARATOR_LABEL),
    item("width", "Row width"),
    item("hex-addresses", "Hex addresses"),
    item("addresses", "Addresses"),
    item("thumbnail", "Thumbnail"),
    item("file-info", "File info"),
    item(separator_name!(4), SEPARATOR_LABEL),
    item("copy-left", "Copy to Left"),
    item("copy-right", "Copy to Right"),
    item(separator_name!(5), SEPARATOR_LABEL),
    item("swap", "Swap"),
    item("reload", "Reload"),
    item("text-compare", "Text Compare"),
    item("parent-folders", "Parent Folders"),
    item("report", "Report"),
    item(separator_name!(6), SEPARATOR_LABEL),
    item("font", "Font size"),
];

const TABLE_ITEMS: &[ItemName] = &[
    item("filter", "Display filter"),
    item("minor", "Minor"),
    item("hide-same", "Hide same columns"),
    item("unhide-column", "Unhide Column"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("previous-difference", "Prev Difference"),
    item("next-difference", "Next Difference"),
    item(separator_name!(2), SEPARATOR_LABEL),
    item("settings", "Settings"),
    item("recompare", "Recompare"),
    item("swap", "Swap"),
    item("stop", "Stop"),
    item("report", "Report"),
    item(separator_name!(3), SEPARATOR_LABEL),
    item("row-numbers", "Row numbers"),
    item("strip", "Thumbnail"),
    item("details", "Details"),
];

const PICTURE_ITEMS: &[ItemName] = &[
    item("home", "Home"),
    item("mode", "Display mode"),
    item("tolerance", "Tolerance"),
    item("zoom-in", "Zoom in"),
    item("zoom-out", "Zoom out"),
    item("actual-size", "Actual size"),
    item("fit", "Fit"),
    item("panes", "Panes"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("report", "Report"),
    item("more", "More"),
];

const MERGE_ITEMS: &[ItemName] = &[
    item("previous-conflict", "Prev Conflict"),
    item("next-conflict", "Next Conflict"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("take-left", "Take Left"),
    item("take-center", "Take Center"),
    item("take-right", "Take Right"),
    item("take-both", "Take Both"),
    item("take-all", "Take All"),
    item("favor-left", "Favor Left"),
    item("favor-right", "Favor Right"),
    item("ignore-same", "Ignore Same"),
    item(separator_name!(2), SEPARATOR_LABEL),
    item("center-pane", "Center Pane"),
    item("save", "Save"),
    item("reload", "Reload"),
    item("report", "Report"),
    item(separator_name!(3), SEPARATOR_LABEL),
    item("filter", "Display filter"),
    item("minor", "Minor"),
];

const FOLDER_MERGE_ITEMS: &[ItemName] = &[
    item("filter", "Display filter"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("previous-conflict", "Prev Conflict"),
    item("next-conflict", "Next Conflict"),
    item(separator_name!(2), SEPARATOR_LABEL),
    item("take-left", "Take Left"),
    item("take-center", "Take Center"),
    item("take-right", "Take Right"),
    item(separator_name!(3), SEPARATOR_LABEL),
    item("merge", "Merge"),
    item("copy-to-output", "Copy to Output"),
    item("text-merge", "Text Merge"),
    item(separator_name!(4), SEPARATOR_LABEL),
    item("center-pane", "Center Pane"),
    item("ignore-same", "Ignore Same"),
    item("swap", "Swap"),
    item("reload", "Reload"),
    item("report", "Report"),
];

const RECORD_ITEMS: &[ItemName] = &[
    item("filter", "Display filter"),
    item("minor", "Minor"),
    item(separator_name!(1), SEPARATOR_LABEL),
    item("previous-difference", "Prev Difference"),
    item("next-difference", "Next Difference"),
    item(separator_name!(2), SEPARATOR_LABEL),
    item("expand", "Expand"),
    item("collapse", "Collapse"),
    item(separator_name!(3), SEPARATOR_LABEL),
    item("reload", "Reload"),
    item("recompare", "Recompare"),
    item("swap", "Swap"),
    item("stop", "Stop"),
    item("report", "Report"),
    item(separator_name!(4), SEPARATOR_LABEL),
    item("strip", "Thumbnail"),
    item("details", "Details"),
    item("hex", "Hex details"),
];

/// The items `view` declares, in the order it declares them.
#[must_use]
pub const fn defaults(view: ToolbarView) -> &'static [ItemName] {
    match view {
        ToolbarView::Text => TEXT_ITEMS,
        ToolbarView::Folder => FOLDER_ITEMS,
        ToolbarView::Hex => HEX_ITEMS,
        ToolbarView::Table => TABLE_ITEMS,
        ToolbarView::Picture => PICTURE_ITEMS,
        ToolbarView::Merge => MERGE_ITEMS,
        ToolbarView::FolderMerge => FOLDER_MERGE_ITEMS,
        ToolbarView::Records => RECORD_ITEMS,
    }
}

/// One item a view puts on its toolbar.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A button that runs a command.
    Command {
        /// Stable name the stored order holds the item under.
        name: &'static str,
        /// The command the button runs.
        command: Command,
        /// What the button shows.
        label: &'static str,
        /// True when the command can run now.
        enabled: bool,
        /// Why it cannot run, shown when it is disabled.
        reason: &'static str,
        /// Drawn as a toggle when the item has a state.
        checked: Option<bool>,
    },
    /// A rule between two groups.
    Separator {
        /// Stable name the stored order holds the rule under.
        name: &'static str,
    },
    /// Room for a control the view draws itself.
    Widget {
        /// Stable name the stored order holds the slot under.
        name: &'static str,
        /// Width the slot claims before the control is built.
        ///
        /// A control that sizes itself while it is built would otherwise run
        /// past the end of a wrapped line.
        width: f32,
    },
}

impl Item {
    /// The stable name of the item.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Command { name, .. } | Self::Separator { name } | Self::Widget { name, .. } => {
                name
            }
        }
    }

    /// A button that runs `command` under its own label.
    #[must_use]
    pub const fn command(
        name: &'static str,
        command: Command,
        label: &'static str,
        enabled: bool,
        reason: &'static str,
    ) -> Self {
        Self::Command {
            name,
            command,
            label,
            enabled,
            reason,
            checked: None,
        }
    }

    /// A toggle that runs `command` and shows whether it is on.
    #[must_use]
    pub const fn toggle(
        name: &'static str,
        command: Command,
        label: &'static str,
        enabled: bool,
        checked: bool,
    ) -> Self {
        Self::Command {
            name,
            command,
            label,
            enabled,
            reason: "",
            checked: Some(checked),
        }
    }

    /// A rule between two groups.
    #[must_use]
    pub const fn separator(name: &'static str) -> Self {
        Self::Separator { name }
    }

    /// Room for a control the view draws itself.
    #[must_use]
    pub const fn widget(name: &'static str, width: f32) -> Self {
        Self::Widget { name, width }
    }
}

/// The order and the visibility one toolbar is drawn under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    order: Vec<String>,
    hidden: Vec<String>,
}

impl Layout {
    /// The order the view declares, with nothing hidden.
    #[must_use]
    pub fn built_in() -> Self {
        Self::default()
    }

    /// What the options document states for `view`.
    #[must_use]
    pub fn from_options(options: &CommandOptions, view: ToolbarView) -> Self {
        Self {
            order: options.toolbar(view.id()).unwrap_or_default().to_vec(),
            hidden: options
                .view(view.id())
                .map(|entry| entry.toolbar_hidden.clone())
                .unwrap_or_default(),
        }
    }

    /// True when the item named is drawn.
    #[must_use]
    pub fn shows(&self, name: &str) -> bool {
        !self.hidden.iter().any(|held| held == name)
    }

    /// The declared items in the order this layout puts them in.
    ///
    /// An item the stored order does not name follows the ones it does, in the
    /// order the view declared it, so a build that adds an item still draws it
    /// under a stored order written before that item existed.
    #[must_use]
    pub fn arrange<'a>(&self, items: &'a [Item]) -> Vec<&'a Item> {
        let mut arranged: Vec<&Item> = Vec::with_capacity(items.len());
        for name in &self.order {
            if let Some(found) = items.iter().find(|held| held.name() == name) {
                arranged.push(found);
            }
        }
        for held in items {
            if !self.order.iter().any(|name| name == held.name()) {
                arranged.push(held);
            }
        }
        arranged.retain(|held| self.shows(held.name()));
        arranged
    }

    /// The declared names in the order this layout puts them in.
    #[must_use]
    pub fn arrange_names(&self, items: &[ItemName]) -> Vec<ItemName> {
        let mut arranged: Vec<ItemName> = Vec::with_capacity(items.len());
        for name in &self.order {
            if let Some(found) = items.iter().find(|held| held.name == name) {
                arranged.push(*found);
            }
        }
        for held in items {
            if !self.order.iter().any(|name| name == held.name) {
                arranged.push(*held);
            }
        }
        arranged
    }
}

/// Draw `items` in the order `layout` states, with the shared overflow.
///
/// `widget` is called for each slot the view declared, with the slot's name.
/// The command a button reports is returned; at most one button is pressed in
/// one frame.
pub fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    items: &[Item],
    layout: &Layout,
    widget: impl FnMut(&mut egui::Ui, &'static str),
) -> Outcome {
    show_for(ToolbarView::Text, ui, id, items, layout, widget)
}

/// Draw one comparison type's toolbar with its own slot icons.
pub fn show_for(
    view: ToolbarView,
    ui: &mut egui::Ui,
    id: egui::Id,
    items: &[Item],
    layout: &Layout,
    mut widget: impl FnMut(&mut egui::Ui, &'static str),
) -> Outcome {
    let arranged = layout.arrange(items);
    let mut overflow = widgets::Overflow::new(ui, id.with("overflow"));
    let room = ui.available_width();
    let inline = overflow.is_collapsed();
    let mut pressed = None;
    let rect = overflow.show(ui, |ui| {
        ui.ctx()
            .data_mut(|data| data.insert_temp(egui::Id::new("toolbar-inline"), inline));
        for held in arranged {
            match held {
                Item::Separator { .. } => {
                    ui.separator();
                }
                Item::Widget { name, width } => {
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("toolbar-icon"),
                            crate::icons::toolbar_icon(view, name),
                        );
                    });
                    widgets::sized(ui, *width, |ui| widget(ui, name));
                    ui.ctx().data_mut(|data| {
                        data.remove::<Option<crate::icons::Icon>>(egui::Id::new("toolbar-icon"));
                    });
                }
                Item::Command {
                    command,
                    label,
                    enabled,
                    reason,
                    checked,
                    ..
                } => {
                    if let Some(state) = checked {
                        let response = widgets::disabled_reason(
                            ui.add_enabled(
                                *enabled,
                                widgets::IconButton::new(
                                    label,
                                    crate::icons::command_icon(*command),
                                )
                                .selected(*state),
                            ),
                            reason,
                        );
                        if response.clicked() {
                            pressed = Some(*command);
                        }
                    } else {
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("toolbar-icon"),
                                crate::icons::command_icon(*command),
                            );
                        });
                        if widgets::toolbar_button(ui, label, *enabled, reason) {
                            pressed = Some(*command);
                        }
                        ui.ctx().data_mut(|data| {
                            data.remove::<Option<crate::icons::Icon>>(egui::Id::new(
                                "toolbar-icon",
                            ));
                        });
                    }
                }
            }
        }
        ui.ctx()
            .data_mut(|data| data.remove::<bool>(egui::Id::new("toolbar-inline")));
    });
    Outcome {
        command: pressed,
        room,
        used: if overflow.is_collapsed() {
            rect.width().min(room)
        } else {
            rect.width()
        },
        rect,
        collapsed: overflow.is_collapsed(),
    }
}

/// What one frame of a toolbar reported.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outcome {
    /// The command a button ran, where one was pressed.
    pub command: Option<Command>,
    /// Room the bar was given.
    pub room: f32,
    /// Room its controls took.
    pub used: f32,
    /// Where the bar was drawn.
    pub rect: egui::Rect,
    /// True when the controls went into the menu rather than onto the bar.
    pub collapsed: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{defaults, Item, Layout, ToolbarView};
    use ca_session::options::CommandOptions;

    fn sample() -> Vec<Item> {
        vec![
            Item::command("a", crate::Command::Reload, "A", true, ""),
            Item::separator("separator-1"),
            Item::widget("b", 40.0),
            Item::command("c", crate::Command::SwapSides, "C", true, ""),
        ]
    }

    #[test]
    fn the_built_in_layout_keeps_the_declared_order() {
        let items = sample();
        let names: Vec<&str> = Layout::built_in()
            .arrange(&items)
            .iter()
            .map(|held| held.name())
            .collect();
        assert_eq!(names, vec!["a", "separator-1", "b", "c"]);
    }

    #[test]
    fn a_stored_order_moves_an_item_and_an_unnamed_item_follows() {
        let mut options = CommandOptions::default();
        options.set_toolbar("hex", vec!["c".to_owned(), "a".to_owned()]);
        let layout = Layout::from_options(&options, ToolbarView::Hex);
        let items = sample();
        let names: Vec<&str> = layout
            .arrange(&items)
            .iter()
            .map(|held| held.name())
            .collect();
        assert_eq!(names, vec!["c", "a", "separator-1", "b"]);
    }

    #[test]
    fn a_hidden_item_is_not_drawn() {
        let mut options = CommandOptions::default();
        options.set_hidden_from_toolbar("hex", "a", true);
        let layout = Layout::from_options(&options, ToolbarView::Hex);
        let items = sample();
        assert!(!layout.shows("a"));
        assert!(layout.arrange(&items).iter().all(|held| held.name() != "a"));
    }

    #[test]
    fn every_toolbar_names_its_items_once() {
        for view in ToolbarView::ALL {
            let mut names: Vec<&str> = defaults(*view).iter().map(|held| held.name).collect();
            let before = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), before, "{} names an item twice", view.id());
        }
    }

    #[test]
    fn every_toolbar_offers_a_report_item() {
        for view in ToolbarView::ALL {
            assert!(
                defaults(*view).iter().any(|held| held.name == "report"),
                "{} has no report item",
                view.id()
            );
        }
    }

    #[test]
    fn a_toolbar_name_round_trips() {
        for view in ToolbarView::ALL {
            assert_eq!(ToolbarView::from_id(view.id()), Some(*view));
        }
        assert_eq!(ToolbarView::from_id("nothing"), None);
    }
}
