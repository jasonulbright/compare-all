//! Embedded monochrome masks and their semantic lookup.

use crate::{toolbar::ToolbarView, Command};
use ca_fs::{criteria::ContentOutcome, merge::MergeStatus, NodeStatus};
use ca_session::SessionKind;
use egui::emath::GuiRounding;
use std::collections::VecDeque;

const CACHE_BYTES: usize = 8 * 1024 * 1024;
const CACHE_ITEMS: usize = 256;

#[derive(Clone, Default)]
struct IconCache {
    entries: VecDeque<((Icon, u32, u32), Option<egui::TextureHandle>)>,
    bytes: usize,
}

enum CachedIcon {
    Missing,
    Ready(Option<egui::TextureHandle>),
}

impl IconCache {
    fn get(&mut self, key: (Icon, u32, u32)) -> CachedIcon {
        let Some(index) = self.entries.iter().position(|entry| entry.0 == key) else {
            return CachedIcon::Missing;
        };
        let Some(entry) = self.entries.remove(index) else {
            return CachedIcon::Missing;
        };
        let result = entry.1.clone();
        self.entries.push_back(entry);
        CachedIcon::Ready(result)
    }

    fn insert(&mut self, key: (Icon, u32, u32), texture: Option<egui::TextureHandle>) {
        let bytes = texture.as_ref().map_or(0, egui::TextureHandle::byte_size);
        while self.entries.len() >= CACHE_ITEMS || self.bytes + bytes > CACHE_BYTES {
            let Some((_, texture)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= texture.as_ref().map_or(0, egui::TextureHandle::byte_size);
        }
        self.bytes += bytes;
        self.entries.push_back((key, texture));
    }
}

impl Icon {
    /// Choose the master that lands on the target pixel grid.
    #[must_use]
    pub fn master(self, px: u32) -> u32 {
        if self.has_24() && px.is_multiple_of(24) {
            24
        } else if self.has_16() && (px.is_multiple_of(16) || px < 24) {
            16
        } else if self.has_24() {
            24
        } else {
            16
        }
    }

    /// A white alpha mask cached independently of the tint.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn texture(self, ctx: &egui::Context, size_pt: f32) -> Option<egui::TextureHandle> {
        let target = size_pt * ctx.pixels_per_point();
        if !target.is_finite() || target <= 0.0 || target > 512.0 {
            return None;
        }
        let px = target.round().max(1.0) as u32;
        let master = self.master(px);
        let key = (self, master, px);
        let cache = egui::Id::new("icon-textures");
        if let CachedIcon::Ready(cached) =
            ctx.data_mut(|data| data.get_temp_mut_or_default::<IconCache>(cache).get(key))
        {
            return cached;
        }
        let texture = self.raster(master, px).map(|image| {
            ctx.load_texture(
                format!("icon/{}/{master}/{px}", self.id()),
                image,
                egui::TextureOptions::LINEAR,
            )
        });
        ctx.data_mut(|data| {
            data.get_temp_mut_or_default::<IconCache>(cache)
                .insert(key, texture.clone());
        });
        texture
    }

    fn raster(self, master: u32, px: u32) -> Option<egui::ColorImage> {
        let tree =
            resvg::usvg::Tree::from_data(self.bytes(master)?, &resvg::usvg::Options::default())
                .ok()?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
        #[allow(clippy::cast_precision_loss)]
        let scale = px as f32 / master as f32;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        for pixel in pixmap.data_mut().chunks_exact_mut(4) {
            pixel[0] = pixel[3];
            pixel[1] = pixel[3];
            pixel[2] = pixel[3];
        }
        Some(egui::ColorImage::from_rgba_premultiplied(
            [px as usize, px as usize],
            pixmap.data(),
        ))
    }

    /// Paint a tinted mask on whole pixel edges.
    pub fn paint(self, painter: &egui::Painter, rect: egui::Rect, tint: egui::Color32) {
        if let Some(texture) = self.texture(painter.ctx(), rect.height()) {
            painter.image(
                texture.id(),
                rect.round_to_pixels(painter.ctx().pixels_per_point()),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                tint,
            );
        }
    }

    /// Allocate a decorative icon without introducing a control.
    pub fn show(self, ui: &mut egui::Ui, size: f32, tint: egui::Color32) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        self.paint(ui.painter(), rect, tint);
    }

    /// A row glyph limited to the row's available height.
    pub fn paint_in_row(
        self,
        painter: &egui::Painter,
        center: egui::Pos2,
        height: f32,
        tint: egui::Color32,
    ) {
        let size = (height - 2.0).clamp(1.0, 16.0);
        self.paint(
            painter,
            egui::Rect::from_center_size(center, egui::vec2(size, size)),
            tint,
        );
    }
}

macro_rules! master {
    ($id:literal, none) => {
        None
    };
    ($id:literal, $size:literal) => {
        Some(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/ui-icons/",
                $size,
                "/",
                $id,
                ".svg"
            ))
            .as_slice(),
        )
    };
}

macro_rules! icons {
    ($($variant:ident, $id:literal, $large:tt, $small:tt;)*) => {
        /// A tintable interface shape.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon {
            $(#[doc = $id] $variant,)*
        }
        impl Icon {
            /// Every embedded shape.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];
            /// Stable file stem.
            #[must_use]
            pub const fn id(self) -> &'static str {
                match self { $(Self::$variant => $id,)* }
            }
            /// Available SVG bytes at the requested master size.
            #[must_use]
            pub fn bytes(self, master: u32) -> Option<&'static [u8]> {
                match (self, master) {
                    $((Self::$variant, 24) => master!($id, $large),
                    (Self::$variant, 16) => master!($id, $small),)*
                    _ => None,
                }
            }
            /// Whether a 24 pixel master exists.
            #[must_use]
            pub fn has_24(self) -> bool { self.bytes(24).is_some() }
            /// Whether a 16 pixel master exists.
            #[must_use]
            pub fn has_16(self) -> bool { self.bytes(16).is_some() }
        }
    };
}

icons! {
    NextDifference, "next-difference", "24", "16";
    PreviousDifference, "previous-difference", "24", "16";
    NextSection, "next-section", "24", "16";
    PreviousSection, "previous-section", "24", "16";
    NextConflict, "next-conflict", "24", "16";
    PreviousConflict, "previous-conflict", "24", "16";
    NextFile, "next-file", none, "16";
    PreviousFile, "previous-file", none, "16";
    GoTo, "go-to", none, "16";
    Bookmark, "bookmark", none, "16";
    Find, "find", none, "16";
    FindNext, "find-next", none, "16";
    FindPrevious, "find-previous", none, "16";
    Replace, "replace", none, "16";
    ShowAll, "show-all", "24", "16";
    ShowContext, "show-context", "24", "16";
    IgnoreUnimportant, "ignore-unimportant", "24", "16";
    IgnoreSameChanges, "ignore-same-changes", "24", "16";
    Rules, "rules", "24", none;
    FileFormat, "file-format", "24", none;
    HideSameColumns, "hide-same-columns", "24", "16";
    UnhideColumn, "unhide-column", "24", none;
    CenterPane, "center-pane", "24", "16";
    Thumbnail, "thumbnail", "24", "16";
    LineNumbers, "line-numbers", "24", "16";
    LineDetails, "line-details", "24", "16";
    HexDetails, "hex-details", "24", "16";
    FileInfo, "file-info", "24", "16";
    Peek, "peek", "24", none;
    FontIncrease, "font-increase", "24", "16";
    FontDecrease, "font-decrease", "24", "16";
    ZoomIn, "zoom-in", "24", none;
    ZoomOut, "zoom-out", "24", none;
    ZoomActualSize, "zoom-actual-size", "24", none;
    ZoomFit, "zoom-fit", "24", none;
    RotateClockwise, "rotate-clockwise", none, "16";
    RotateCounterclockwise, "rotate-counterclockwise", none, "16";
    FlipHorizontal, "flip-horizontal", none, "16";
    FlipVertical, "flip-vertical", none, "16";
    Undo, "undo", none, "16";
    Redo, "redo", none, "16";
    Cut, "cut", none, "16";
    Copy, "copy", none, "16";
    Paste, "paste", none, "16";
    Delete, "delete", none, "16";
    Rename, "rename", none, "16";
    Duplicate, "duplicate", none, "16";
    CopyToRight, "copy-to-right", "24", "16";
    CopyToLeft, "copy-to-left", "24", "16";
    CopyToOtherSide, "copy-to-other-side", "24", "16";
    MoveToOtherSide, "move-to-other-side", none, "16";
    CopyToFolder, "copy-to-folder", none, "16";
    MoveToFolder, "move-to-folder", none, "16";
    NewFolder, "new-folder", none, "16";
    ExpandAll, "expand-all", "24", "16";
    CollapseAll, "collapse-all", "24", "16";
    Select, "select", "24", none;
    CompareContents, "compare-contents", "24", "16";
    Synchronize, "synchronize", none, "16";
    SwapSides, "swap-sides", "24", "16";
    Reload, "reload", "24", "16";
    Recompare, "recompare", "24", "16";
    Stop, "stop", "24", "16";
    Report, "report", "24", "16";
    CompareParentFolders, "compare-parent-folders", "24", "16";
    DeleteLeft, "delete-left", none, "16";
    DeleteRight, "delete-right", none, "16";
    LeaveAlone, "leave-alone", none, "16";
    ApplyPatch, "apply-patch", none, "16";
    TakeLeft, "take-left", "24", "16";
    TakeCenter, "take-center", "24", "16";
    TakeRight, "take-right", "24", "16";
    TakeLeftThenRight, "take-left-then-right", "24", "16";
    TakeAllNonConflicting, "take-all-non-conflicting", "24", "16";
    FavorLeft, "favor-left", "24", "16";
    FavorRight, "favor-right", "24", "16";
    MergeFolders, "merge-folders", "24", "16";
    CopyToOutput, "copy-to-output", "24", "16";
    Home, "home", "24", "16";
    Sessions, "sessions", "24", none;
    NewSession, "new-session", none, "16";
    NewTab, "new-tab", none, "16";
    OpenSession, "open-session", none, "16";
    SaveSession, "save-session", none, "16";
    SessionSettings, "session-settings", "24", "16";
    Options, "options", none, "16";
    Close, "close", none, "16";
    OpenFile, "open-file", none, "16";
    Browse, "browse", none, "16";
    Save, "save", "24", "16";
    SaveAs, "save-as", none, "16";
    SaveAll, "save-all", none, "16";
    More, "more", "24", none;
    Lock, "lock", none, "16";
    Unlock, "unlock", none, "16";
    SessionTextCompare, "session-text-compare", "24", "16";
    SessionTextMerge, "session-text-merge", "24", "16";
    SessionTextEdit, "session-text-edit", "24", "16";
    SessionTextPatch, "session-text-patch", "24", "16";
    SessionFolderCompare, "session-folder-compare", "24", "16";
    SessionFolderMerge, "session-folder-merge", "24", "16";
    SessionFolderSync, "session-folder-sync", "24", "16";
    SessionTableCompare, "session-table-compare", "24", "16";
    SessionHexCompare, "session-hex-compare", "24", "16";
    SessionPictureCompare, "session-picture-compare", "24", "16";
    SessionRegistryCompare, "session-registry-compare", "24", "16";
    SessionMediaCompare, "session-media-compare", "24", "16";
    SessionVersionCompare, "session-version-compare", "24", "16";
    SessionUnknown, "session-unknown", none, "16";
    File, "file", none, "16";
    Folder, "folder", none, "16";
    FolderOpen, "folder-open", none, "16";
    ChevronRight, "chevron-right", none, "16";
    ChevronDown, "chevron-down", none, "16";
    SortAscending, "sort-ascending", none, "16";
    SortDescending, "sort-descending", none, "16";
    Key, "key", none, "16";
    LinkBadge, "link-badge", none, "16";
    SourceArchive, "source-archive", none, "16";
    SourceRemote, "source-remote", none, "16";
    SourceSnapshot, "source-snapshot", none, "16";
    StateSame, "state-same", "24", "16";
    StateDifferent, "state-different", "24", "16";
    StateUnimportant, "state-unimportant", none, "16";
    StateLeftNewer, "state-left-newer", none, "16";
    StateRightNewer, "state-right-newer", none, "16";
    StateOrphanLeft, "state-orphan-left", none, "16";
    StateOrphanRight, "state-orphan-right", none, "16";
    StateKindMismatch, "state-kind-mismatch", none, "16";
    StateNotCompared, "state-not-compared", none, "16";
    StateIncomplete, "state-incomplete", none, "16";
    StateCloudPlaceholder, "state-cloud-placeholder", none, "16";
    MergeLeftChange, "merge-left-change", none, "16";
    MergeRightChange, "merge-right-change", none, "16";
    MergeSameChange, "merge-same-change", none, "16";
    MergeMergeable, "merge-mergeable", none, "16";
    Conflict, "conflict", none, "16";
    Info, "info", "24", "16";
    Warning, "warning", "24", "16";
    Error, "error", "24", "16";
    Question, "question", "24", "16";
}

/// Icon for a command that carries artwork.
#[must_use]
pub const fn command_icon(command: Command) -> Option<Icon> {
    match command {
        Command::ActualSize => Some(Icon::ZoomActualSize),
        Command::ApplyPatch => Some(Icon::ApplyPatch),
        Command::Cancel => Some(Icon::Stop),
        Command::CloseTab => Some(Icon::Close),
        Command::CollapseAll => Some(Icon::CollapseAll),
        Command::CompareContents => Some(Icon::CompareContents),
        Command::CompareInfo | Command::MergeInfo => Some(Icon::Info),
        Command::CompareParentFolders => Some(Icon::CompareParentFolders),
        Command::CompareReport => Some(Icon::Report),
        Command::ContextHelp | Command::HelpContents => Some(Icon::Question),
        Command::Copy => Some(Icon::Copy),
        Command::CopyToFolder => Some(Icon::CopyToFolder),
        Command::CopyToLeft => Some(Icon::CopyToLeft),
        Command::CopyToOtherSide => Some(Icon::CopyToOtherSide),
        Command::CopyToOutput => Some(Icon::CopyToOutput),
        Command::CopyToRight => Some(Icon::CopyToRight),
        Command::Cut => Some(Icon::Cut),
        Command::DecreaseFontSize => Some(Icon::FontDecrease),
        Command::Delete => Some(Icon::Delete),
        Command::EditTextFile => Some(Icon::SessionTextEdit),
        Command::ExpandAll => Some(Icon::ExpandAll),
        Command::FavorLeft => Some(Icon::FavorLeft),
        Command::FavorRight => Some(Icon::FavorRight),
        Command::FileInfo => Some(Icon::FileInfo),
        Command::Find => Some(Icon::Find),
        Command::FindNext => Some(Icon::FindNext),
        Command::FindPrevious => Some(Icon::FindPrevious),
        Command::FlipHorizontal => Some(Icon::FlipHorizontal),
        Command::FlipVertical => Some(Icon::FlipVertical),
        Command::GoTo => Some(Icon::GoTo),
        Command::HexDetails => Some(Icon::HexDetails),
        Command::HideSameColumns => Some(Icon::HideSameColumns),
        Command::IncreaseFontSize => Some(Icon::FontIncrease),
        Command::MergeFolders => Some(Icon::MergeFolders),
        Command::MoveToFolder => Some(Icon::MoveToFolder),
        Command::MoveToOtherSide => Some(Icon::MoveToOtherSide),
        Command::NewFolder => Some(Icon::NewFolder),
        Command::NewSession => Some(Icon::NewSession),
        Command::NewTab => Some(Icon::NewTab),
        Command::NextConflict => Some(Icon::NextConflict),
        Command::NextDifference => Some(Icon::NextDifference),
        Command::NextDifferenceFiles => Some(Icon::NextFile),
        Command::NextSection => Some(Icon::NextSection),
        Command::OpenFile => Some(Icon::OpenFile),
        Command::OpenSession => Some(Icon::OpenSession),
        Command::OpenTextMerge => Some(Icon::SessionTextMerge),
        Command::Options => Some(Icon::Options),
        Command::Paste => Some(Icon::Paste),
        Command::PreviousConflict => Some(Icon::PreviousConflict),
        Command::PreviousDifference => Some(Icon::PreviousDifference),
        Command::PreviousDifferenceFiles => Some(Icon::PreviousFile),
        Command::PreviousSection => Some(Icon::PreviousSection),
        Command::Recompare => Some(Icon::Recompare),
        Command::Redo => Some(Icon::Redo),
        Command::Reload => Some(Icon::Reload),
        Command::Rename => Some(Icon::Rename),
        Command::Replace => Some(Icon::Replace),
        Command::RotateClockwise => Some(Icon::RotateClockwise),
        Command::RotateCounterclockwise => Some(Icon::RotateCounterclockwise),
        Command::SaveBoth => Some(Icon::SaveAll),
        Command::SaveFile => Some(Icon::Save),
        Command::SaveFileAs => Some(Icon::SaveAs),
        Command::SaveSession => Some(Icon::SaveSession),
        Command::SessionSettings => Some(Icon::SessionSettings),
        Command::ShowAll => Some(Icon::ShowAll),
        Command::ShowContext => Some(Icon::ShowContext),
        Command::ShowDifferences => Some(Icon::StateDifferent),
        Command::ShowSame => Some(Icon::StateSame),
        Command::SwapSides => Some(Icon::SwapSides),
        Command::Synchronize => Some(Icon::Synchronize),
        Command::TakeAllNonConflicting => Some(Icon::TakeAllNonConflicting),
        Command::TakeCenter => Some(Icon::TakeCenter),
        Command::TakeLeft => Some(Icon::TakeLeft),
        Command::TakeLeftThenRight => Some(Icon::TakeLeftThenRight),
        Command::TakeRight => Some(Icon::TakeRight),
        Command::Thumbnail => Some(Icon::Thumbnail),
        Command::ToggleBookmark => Some(Icon::Bookmark),
        Command::ToggleCenterPane => Some(Icon::CenterPane),
        Command::ToggleIgnoreSameChanges => Some(Icon::IgnoreSameChanges),
        Command::ToggleIgnoreUnimportant => Some(Icon::IgnoreUnimportant),
        Command::ToggleLineDetails => Some(Icon::LineDetails),
        Command::ToggleLineNumbers => Some(Icon::LineNumbers),
        Command::ToggleLocked => Some(Icon::Lock),
        Command::Undo => Some(Icon::Undo),
        Command::ViewPatch => Some(Icon::SessionTextPatch),
        Command::ZoomIn => Some(Icon::ZoomIn),
        Command::ZoomOut => Some(Icon::ZoomOut),
        Command::ZoomToFit => Some(Icon::ZoomFit),
        _ => None,
    }
}

/// Icon for each built-in toolbar slot or command item.
#[must_use]
pub fn toolbar_icon(view: ToolbarView, item: &str) -> Option<Icon> {
    let icon = match item {
        "home" => Icon::Home,
        "sessions" => Icon::Sessions,
        "all" => Icon::ShowAll,
        "diffs" => Icon::StateDifferent,
        "same" => Icon::StateSame,
        "context" => Icon::ShowContext,
        "minor" => Icon::IgnoreUnimportant,
        "rules" => Icon::Rules,
        "format" => Icon::FileFormat,
        "expand" => Icon::ExpandAll,
        "collapse" => Icon::CollapseAll,
        "select" => Icon::Select,
        "peek" => Icon::Peek,
        "addresses" | "row-numbers" => Icon::LineNumbers,
        "thumbnail" | "strip" => Icon::Thumbnail,
        "file-info" => Icon::FileInfo,
        "text-compare" => Icon::SessionTextCompare,
        "parent-folders" => Icon::CompareParentFolders,
        "hide-same" => Icon::HideSameColumns,
        "unhide-column" => Icon::UnhideColumn,
        "details" => Icon::LineDetails,
        "hex" => Icon::HexDetails,
        "zoom-in" => Icon::ZoomIn,
        "zoom-out" => Icon::ZoomOut,
        "actual-size" => Icon::ZoomActualSize,
        "fit" => Icon::ZoomFit,
        "more" => Icon::More,
        "copy" => Icon::CopyToOtherSide,
        "next-section" => Icon::NextSection,
        "previous-section" => Icon::PreviousSection,
        "next-difference" => Icon::NextDifference,
        "previous-difference" => Icon::PreviousDifference,
        "next-conflict" => Icon::NextConflict,
        "previous-conflict" => Icon::PreviousConflict,
        "copy-left" => Icon::CopyToLeft,
        "copy-right" => Icon::CopyToRight,
        "swap" => Icon::SwapSides,
        "reload" | "refresh" => Icon::Reload,
        "recompare" => Icon::Recompare,
        "report" => Icon::Report,
        "stop" => Icon::Stop,
        "settings" => Icon::SessionSettings,
        "compare-contents" => Icon::CompareContents,
        "take-left" => Icon::TakeLeft,
        "take-center" => Icon::TakeCenter,
        "take-right" => Icon::TakeRight,
        "take-both" => Icon::TakeLeftThenRight,
        "take-all" => Icon::TakeAllNonConflicting,
        "favor-left" => Icon::FavorLeft,
        "favor-right" => Icon::FavorRight,
        "ignore-same" => Icon::IgnoreSameChanges,
        "center-pane" => Icon::CenterPane,
        "save" => Icon::Save,
        "merge" => Icon::MergeFolders,
        "copy-to-output" => Icon::CopyToOutput,
        "text-merge" => Icon::SessionTextMerge,
        _ => {
            let label = view.built_in().iter().find(|held| held.name == item)?.label;
            let command = Command::ALL.iter().copied().find(|command| {
                command.label() == label || command.label_in(menu_view(view)) == label
            })?;
            return command_icon(command);
        }
    };
    Some(icon)
}

const fn menu_view(view: ToolbarView) -> crate::command::MenuView {
    use crate::command::MenuView;
    match view {
        ToolbarView::Text => MenuView::Text,
        ToolbarView::Folder => MenuView::Folder,
        ToolbarView::Table => MenuView::Table,
        ToolbarView::Merge | ToolbarView::FolderMerge => MenuView::Merge,
        _ => MenuView::Other,
    }
}

/// Kind artwork shared by tabs, launcher entries and saved sessions.
#[must_use]
pub fn session_icon(kind: &SessionKind) -> Icon {
    match kind {
        SessionKind::TextCompare => Icon::SessionTextCompare,
        SessionKind::TextMerge => Icon::SessionTextMerge,
        SessionKind::TextEdit => Icon::SessionTextEdit,
        SessionKind::TextPatch => Icon::SessionTextPatch,
        SessionKind::FolderCompare => Icon::SessionFolderCompare,
        SessionKind::FolderMerge => Icon::SessionFolderMerge,
        SessionKind::FolderSync => Icon::SessionFolderSync,
        SessionKind::TableCompare => Icon::SessionTableCompare,
        SessionKind::HexCompare => Icon::SessionHexCompare,
        SessionKind::PictureCompare => Icon::SessionPictureCompare,
        SessionKind::RegistryCompare => Icon::SessionRegistryCompare,
        SessionKind::MediaCompare => Icon::SessionMediaCompare,
        SessionKind::VersionCompare => Icon::SessionVersionCompare,
        _ => Icon::SessionUnknown,
    }
}

/// Row state with structural and incomplete states taking precedence.
#[must_use]
pub const fn node_status_icon(
    status: NodeStatus,
    content: Option<ContentOutcome>,
    incomplete: bool,
) -> Icon {
    if matches!(status, NodeStatus::KindMismatch) {
        return Icon::StateKindMismatch;
    }
    if incomplete {
        return Icon::StateIncomplete;
    }
    if let Some(content) = content {
        return match content {
            ContentOutcome::BinarySame | ContentOutcome::RulesSame => Icon::StateSame,
            ContentOutcome::BinaryDifferences | ContentOutcome::ImportantDifferences => {
                Icon::StateDifferent
            }
            ContentOutcome::UnimportantDifferences => Icon::StateUnimportant,
            ContentOutcome::NotComparedCloudPlaceholder => Icon::StateCloudPlaceholder,
        };
    }
    match status {
        NodeStatus::Same => Icon::StateSame,
        NodeStatus::Different => Icon::StateDifferent,
        NodeStatus::LeftNewer => Icon::StateLeftNewer,
        NodeStatus::RightNewer => Icon::StateRightNewer,
        NodeStatus::LeftOrphan => Icon::StateOrphanLeft,
        NodeStatus::RightOrphan => Icon::StateOrphanRight,
        NodeStatus::NotCompared => Icon::StateNotCompared,
        NodeStatus::Error => Icon::Error,
        NodeStatus::KindMismatch => Icon::StateKindMismatch,
    }
}

/// Folder merge state artwork.
#[must_use]
pub const fn merge_status_icon(status: MergeStatus) -> Icon {
    match status {
        MergeStatus::Unchanged => Icon::StateSame,
        MergeStatus::LeftChange(_) => Icon::MergeLeftChange,
        MergeStatus::RightChange(_) => Icon::MergeRightChange,
        MergeStatus::SameChange(_) => Icon::MergeSameChange,
        MergeStatus::Mergeable => Icon::MergeMergeable,
        MergeStatus::Conflict => Icon::Conflict,
        MergeStatus::Unknown => Icon::StateIncomplete,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    fn display_size_changes_keep_the_texture_cache_within_both_limits() {
        let context = egui::Context::default();
        let first = super::Icon::StateSame.texture(&context, 1.0).unwrap();
        for size in 2..=300 {
            let _ = super::Icon::StateSame
                .texture(&context, f32::from(u16::try_from(size).unwrap()))
                .unwrap();
        }
        let cache = context
            .data(|data| data.get_temp::<super::IconCache>(egui::Id::new("icon-textures")))
            .unwrap();
        assert!(cache.entries.len() <= super::CACHE_ITEMS);
        assert!(cache.bytes <= super::CACHE_BYTES);
        assert!(cache.entries.iter().all(|entry| entry.0 .2 != 1));
        assert_ne!(
            first.id(),
            super::Icon::StateSame.texture(&context, 1.0).unwrap().id()
        );
        let current = super::Icon::StateSame.texture(&context, 300.0).unwrap();
        assert_eq!(
            current.id(),
            super::Icon::StateSame
                .texture(&context, 300.0)
                .unwrap()
                .id()
        );
    }

    #[test]
    fn every_master_renders_a_nonempty_white_alpha_mask() {
        for icon in super::Icon::ALL {
            for master in [16, 24] {
                if icon.bytes(master).is_none() {
                    continue;
                }
                let image = icon.raster(master, master).unwrap();
                assert!(
                    image.pixels.iter().any(|pixel| pixel.a() > 0),
                    "{}",
                    icon.id()
                );
                for pixel in image.pixels {
                    assert_eq!(pixel.r(), pixel.a());
                    assert_eq!(pixel.g(), pixel.a());
                    assert_eq!(pixel.b(), pixel.a());
                }
            }
        }
    }
}
