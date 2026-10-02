//! Folder synchronisation: a method, a preview of every operation it would
//! carry out, and a per row override.

use ca_fs::{SyncAction, SyncPreset, SyncPreview};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The standard methods, in the order the selector lists them.
pub const PRESETS: [Method; 5] = [
    Method::UpdateLeft,
    Method::UpdateRight,
    Method::UpdateBoth,
    Method::MirrorToLeft,
    Method::MirrorToRight,
];

/// One standard synchronisation method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// Copy newer and orphan items from right to left.
    UpdateLeft,
    /// Copy newer and orphan items from left to right.
    #[default]
    UpdateRight,
    /// Copy newer and orphan items in both directions.
    UpdateBoth,
    /// Replace every differing item on the left and remove left orphans.
    MirrorToLeft,
    /// Replace every differing item on the right and remove right orphans.
    MirrorToRight,
}

impl Method {
    /// The label the selector shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UpdateLeft => "Update Left",
            Self::UpdateRight => "Update Right",
            Self::UpdateBoth => "Update Both",
            Self::MirrorToLeft => "Mirror to Left",
            Self::MirrorToRight => "Mirror to Right",
        }
    }

    /// The engine's form of this method.
    #[must_use]
    pub const fn preset(self) -> SyncPreset {
        match self {
            Self::UpdateLeft => SyncPreset::UpdateLeft,
            Self::UpdateRight => SyncPreset::UpdateRight,
            Self::UpdateBoth => SyncPreset::UpdateBoth,
            Self::MirrorToLeft => SyncPreset::MirrorToLeft,
            Self::MirrorToRight => SyncPreset::MirrorToRight,
        }
    }

    /// True when the method removes items.
    #[must_use]
    pub const fn removes_items(self) -> bool {
        matches!(self, Self::MirrorToLeft | Self::MirrorToRight)
    }
}

/// The actions a preview row can be overridden to, in menu order.
pub const ACTIONS: [SyncAction; 5] = [
    SyncAction::LeaveAlone,
    SyncAction::CopyLeftToRight,
    SyncAction::CopyRightToLeft,
    SyncAction::DeleteLeft,
    SyncAction::DeleteRight,
];

/// The label one preview action shows.
#[must_use]
pub const fn action_label(action: SyncAction) -> &'static str {
    match action {
        SyncAction::LeaveAlone => "Leave alone",
        SyncAction::CopyLeftToRight => "Copy left to right",
        SyncAction::CopyRightToLeft => "Copy right to left",
        SyncAction::DeleteLeft => "Delete left",
        SyncAction::DeleteRight => "Delete right",
    }
}

/// The mark drawn in a preview row's action column.
#[must_use]
pub const fn action_mark(action: SyncAction) -> &'static str {
    match action {
        SyncAction::LeaveAlone => "=",
        SyncAction::CopyLeftToRight => "\u{2192}",
        SyncAction::CopyRightToLeft => "\u{2190}",
        SyncAction::DeleteLeft => "\u{2717}L",
        SyncAction::DeleteRight => "\u{2717}R",
    }
}

/// Icon in a synchronization preview's action column.
#[must_use]
pub const fn action_icon(action: SyncAction) -> ca_ui::icons::Icon {
    match action {
        SyncAction::LeaveAlone => ca_ui::icons::Icon::LeaveAlone,
        SyncAction::CopyLeftToRight => ca_ui::icons::Icon::CopyToRight,
        SyncAction::CopyRightToLeft => ca_ui::icons::Icon::CopyToLeft,
        SyncAction::DeleteLeft => ca_ui::icons::Icon::DeleteLeft,
        SyncAction::DeleteRight => ca_ui::icons::Icon::DeleteRight,
    }
}

/// What the synchronisation half of the view holds.
#[derive(Debug, Default)]
pub struct SyncMode {
    /// The method in force.
    pub method: Method,
    /// The preview, once one has been built.
    pub preview: Option<SyncPreview>,
    /// Why the last attempt produced nothing.
    pub refusal: Option<String>,
    /// True once the parameters have changed and the preview is out of date.
    pub stale: bool,
    /// How many times a method was chosen since the view opened.
    pub choices: u64,
}

impl SyncMode {
    /// A synchronisation that has not been previewed yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stale: true,
            ..Self::default()
        }
    }

    /// Put `method` in force, which leaves the preview out of date.
    pub fn choose(&mut self, method: Method) {
        self.method = method;
        self.choices = self.choices.wrapping_add(1);
        self.stale = true;
    }

    /// The overrides the next preview should carry forward.
    #[must_use]
    pub fn overrides(&self) -> BTreeMap<PathBuf, SyncAction> {
        self.preview
            .as_ref()
            .map(|preview| preview.overrides.clone())
            .unwrap_or_default()
    }

    /// Force one row to an action other than the one the method chose.
    pub fn set_override(&mut self, rel: PathBuf, action: SyncAction) {
        if let Some(preview) = self.preview.as_mut() {
            preview.set_override(rel, action);
        }
    }

    /// Take a row out of the pending synchronisation.
    pub fn exclude(&mut self, rel: PathBuf) {
        self.set_override(rel, SyncAction::LeaveAlone);
    }

    /// How many rows would do something.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.preview
            .as_ref()
            .map_or(0, |preview| preview.pending().len())
    }

    /// The action one row will actually receive.
    #[must_use]
    pub fn effective(&self, index: usize) -> Option<SyncAction> {
        let preview = self.preview.as_ref()?;
        let row = preview.rows.get(index)?;
        Some(preview.effective(&row.rel, row.action))
    }
}

#[cfg(test)]
mod tests {
    use super::{action_label, action_mark, Method, SyncMode, ACTIONS, PRESETS};
    use ca_fs::{NodeStatus, PreviewRow, SyncAction, SyncPreview};
    use std::path::PathBuf;

    fn preview() -> SyncPreview {
        SyncPreview {
            rows: vec![
                PreviewRow {
                    rel: PathBuf::from("a.txt"),
                    name: "a.txt".to_string(),
                    is_dir: false,
                    status: NodeStatus::LeftNewer,
                    action: SyncAction::CopyLeftToRight,
                },
                PreviewRow {
                    rel: PathBuf::from("b.txt"),
                    name: "b.txt".to_string(),
                    is_dir: false,
                    status: NodeStatus::Same,
                    action: SyncAction::LeaveAlone,
                },
            ],
            overrides: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn every_method_and_action_has_a_label() {
        for method in PRESETS {
            assert!(!method.label().is_empty());
        }
        for action in ACTIONS {
            assert!(!action_label(action).is_empty());
            assert!(!action_mark(action).is_empty());
        }
    }

    #[test]
    fn only_the_mirror_methods_remove_items() {
        assert!(Method::MirrorToLeft.removes_items());
        assert!(!Method::UpdateBoth.removes_items());
    }

    #[test]
    fn an_override_changes_one_row_and_leaves_the_rest() {
        let mut mode = SyncMode::new();
        mode.preview = Some(preview());
        assert_eq!(mode.pending(), 1);
        mode.exclude(PathBuf::from("a.txt"));
        assert_eq!(mode.pending(), 0);
        assert_eq!(mode.effective(0), Some(SyncAction::LeaveAlone));
        mode.set_override(PathBuf::from("b.txt"), SyncAction::DeleteRight);
        assert_eq!(mode.pending(), 1);
        assert_eq!(mode.effective(1), Some(SyncAction::DeleteRight));
    }

    #[test]
    fn overrides_survive_a_rebuilt_preview() {
        let mut mode = SyncMode::new();
        mode.preview = Some(preview());
        mode.exclude(PathBuf::from("a.txt"));
        let carried = mode.overrides();
        mode.preview = Some(preview());
        if let Some(view) = mode.preview.as_mut() {
            view.overrides = carried;
        }
        assert_eq!(mode.effective(0), Some(SyncAction::LeaveAlone));
    }
}
