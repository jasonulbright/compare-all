//! Display filter predicates over a node's comparison status.

use crate::compare::{Node, NodeStatus, StatusFlags};

/// The display filter choices of a folder session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayFilter {
    /// No filtering.
    #[default]
    ShowAll,
    /// Hide matching items.
    ShowDifferences,
    /// Hide everything except matching items.
    ShowSame,
    /// Hide orphans.
    ShowNoOrphans,
    /// Hide matching items and orphans.
    ShowDifferencesNoOrphans,
    /// Hide everything except orphans.
    ShowOrphans,
    /// Hide matches, orphans, and items newer on the right.
    ShowLeftNewer,
    /// Hide matches, orphans, and items newer on the left.
    ShowRightNewer,
    /// Hide matches, right newer items, and right orphans.
    ShowLeftNewerAndLeftOrphans,
    /// Hide matches, left newer items, and left orphans.
    ShowRightNewerAndRightOrphans,
    /// Hide everything except left orphans.
    ShowLeftOrphans,
    /// Hide everything except right orphans.
    ShowRightOrphans,
    /// Hide every file.
    ShowNone,
}

impl DisplayFilter {
    /// True when an item with this status is shown.
    ///
    /// An uncompared item is shown by every filter that shows differences,
    /// because its status may still turn out to be one.
    #[must_use]
    pub fn shows(self, status: NodeStatus) -> bool {
        use NodeStatus::{
            Different, Error, KindMismatch, LeftNewer, LeftOrphan, NotCompared, RightNewer,
            RightOrphan, Same,
        };
        match self {
            Self::ShowAll => true,
            Self::ShowNone => false,
            Self::ShowDifferences => status != Same,
            Self::ShowSame => status == Same,
            Self::ShowNoOrphans => !status.is_orphan(),
            Self::ShowDifferencesNoOrphans => status != Same && !status.is_orphan(),
            Self::ShowOrphans => status.is_orphan(),
            Self::ShowLeftNewer => {
                matches!(
                    status,
                    LeftNewer | Different | KindMismatch | NotCompared | Error
                )
            }
            Self::ShowRightNewer => {
                matches!(
                    status,
                    RightNewer | Different | KindMismatch | NotCompared | Error
                )
            }
            Self::ShowLeftNewerAndLeftOrphans => {
                matches!(
                    status,
                    LeftNewer | LeftOrphan | Different | KindMismatch | NotCompared | Error
                )
            }
            Self::ShowRightNewerAndRightOrphans => {
                matches!(
                    status,
                    RightNewer | RightOrphan | Different | KindMismatch | NotCompared | Error
                )
            }
            Self::ShowLeftOrphans => status == LeftOrphan,
            Self::ShowRightOrphans => status == RightOrphan,
        }
    }
}

/// When folders appear, independently of the main display filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FolderDisplayFilter {
    /// Every folder appears.
    AlwaysShowFolders,
    /// Folders respect the display filter through their own status.
    #[default]
    CompareFilesAndFolderStructure,
    /// Only folders holding a visible file appear.
    OnlyCompareFiles,
}

/// True when a folder is shown, given the display filter and whether any of
/// its descendants are visible.
#[must_use]
pub fn folder_visible(
    folder: FolderDisplayFilter,
    filter: DisplayFilter,
    status: NodeStatus,
    has_visible_child: bool,
) -> bool {
    match folder {
        FolderDisplayFilter::AlwaysShowFolders => true,
        FolderDisplayFilter::CompareFilesAndFolderStructure => {
            filter.shows(status) || has_visible_child
        }
        FolderDisplayFilter::OnlyCompareFiles => has_visible_child,
    }
}

/// True when the node is shown under the two filters.
///
/// Folders consult their descendants, so a folder holding a visible file stays
/// visible. Descendants are walked on an explicit stack, because tree depth
/// follows the file system.
#[must_use]
pub fn visible(node: &Node, filter: DisplayFilter, folder: FolderDisplayFilter) -> bool {
    struct Frame<'a> {
        node: &'a Node,
        children: std::slice::Iter<'a, Node>,
        any_visible: bool,
    }

    if !node.is_dir {
        return filter.shows(node.status);
    }
    let mut stack: Vec<Frame<'_>> = vec![Frame {
        node,
        children: node.children.iter(),
        any_visible: false,
    }];

    loop {
        let Some(frame) = stack.last_mut() else {
            return false;
        };
        match frame.children.next() {
            Some(child) if child.is_dir => stack.push(Frame {
                node: child,
                children: child.children.iter(),
                any_visible: false,
            }),
            Some(child) => frame.any_visible |= filter.shows(child.status),
            None => {
                let done = stack.pop().unwrap_or(Frame {
                    node,
                    children: [].iter(),
                    any_visible: false,
                });
                let shown = folder_visible(folder, filter, done.node.status, done.any_visible);
                match stack.last_mut() {
                    Some(parent) => parent.any_visible |= shown,
                    None => return shown,
                }
            }
        }
    }
}

/// True when a folder's contained-status hints include something the filter
/// shows, without walking the children.
#[must_use]
pub fn flags_match(filter: DisplayFilter, flags: StatusFlags) -> bool {
    let statuses = [
        (flags.same, NodeStatus::Same),
        (flags.different, NodeStatus::Different),
        (flags.left_newer, NodeStatus::LeftNewer),
        (flags.right_newer, NodeStatus::RightNewer),
        (flags.left_orphan, NodeStatus::LeftOrphan),
        (flags.right_orphan, NodeStatus::RightOrphan),
        (flags.error, NodeStatus::Error),
        (flags.not_compared, NodeStatus::NotCompared),
    ];
    statuses
        .into_iter()
        .any(|(present, status)| present && filter.shows(status))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{flags_match, folder_visible, DisplayFilter, FolderDisplayFilter};
    use crate::compare::{NodeStatus, StatusFlags};

    const EVERY_STATUS: [NodeStatus; 9] = [
        NodeStatus::NotCompared,
        NodeStatus::Same,
        NodeStatus::Different,
        NodeStatus::LeftNewer,
        NodeStatus::RightNewer,
        NodeStatus::LeftOrphan,
        NodeStatus::RightOrphan,
        NodeStatus::KindMismatch,
        NodeStatus::Error,
    ];

    #[test]
    fn show_all_and_show_none_are_total() {
        for status in EVERY_STATUS {
            assert!(DisplayFilter::ShowAll.shows(status));
            assert!(!DisplayFilter::ShowNone.shows(status));
        }
    }

    #[test]
    fn same_and_difference_filters_partition_matches() {
        for status in EVERY_STATUS {
            assert_ne!(
                DisplayFilter::ShowSame.shows(status),
                DisplayFilter::ShowDifferences.shows(status)
            );
        }
    }

    #[test]
    fn orphan_filters() {
        assert!(DisplayFilter::ShowOrphans.shows(NodeStatus::LeftOrphan));
        assert!(DisplayFilter::ShowOrphans.shows(NodeStatus::RightOrphan));
        assert!(!DisplayFilter::ShowOrphans.shows(NodeStatus::Different));
        assert!(!DisplayFilter::ShowNoOrphans.shows(NodeStatus::LeftOrphan));
        assert!(DisplayFilter::ShowNoOrphans.shows(NodeStatus::Same));
        assert!(!DisplayFilter::ShowDifferencesNoOrphans.shows(NodeStatus::Same));
        assert!(!DisplayFilter::ShowDifferencesNoOrphans.shows(NodeStatus::RightOrphan));
        assert!(DisplayFilter::ShowDifferencesNoOrphans.shows(NodeStatus::Different));
        assert!(DisplayFilter::ShowLeftOrphans.shows(NodeStatus::LeftOrphan));
        assert!(!DisplayFilter::ShowLeftOrphans.shows(NodeStatus::RightOrphan));
        assert!(DisplayFilter::ShowRightOrphans.shows(NodeStatus::RightOrphan));
    }

    #[test]
    fn newer_filters_pick_one_direction() {
        assert!(DisplayFilter::ShowLeftNewer.shows(NodeStatus::LeftNewer));
        assert!(!DisplayFilter::ShowLeftNewer.shows(NodeStatus::RightNewer));
        assert!(!DisplayFilter::ShowLeftNewer.shows(NodeStatus::Same));
        assert!(!DisplayFilter::ShowLeftNewer.shows(NodeStatus::LeftOrphan));
        assert!(DisplayFilter::ShowRightNewer.shows(NodeStatus::RightNewer));
        assert!(DisplayFilter::ShowLeftNewerAndLeftOrphans.shows(NodeStatus::LeftOrphan));
        assert!(!DisplayFilter::ShowLeftNewerAndLeftOrphans.shows(NodeStatus::RightOrphan));
        assert!(DisplayFilter::ShowRightNewerAndRightOrphans.shows(NodeStatus::RightOrphan));
    }

    #[test]
    fn folder_filters_follow_their_contents() {
        assert!(folder_visible(
            FolderDisplayFilter::AlwaysShowFolders,
            DisplayFilter::ShowNone,
            NodeStatus::Same,
            false
        ));
        assert!(!folder_visible(
            FolderDisplayFilter::OnlyCompareFiles,
            DisplayFilter::ShowAll,
            NodeStatus::Same,
            false
        ));
        assert!(folder_visible(
            FolderDisplayFilter::OnlyCompareFiles,
            DisplayFilter::ShowAll,
            NodeStatus::Same,
            true
        ));
        assert!(folder_visible(
            FolderDisplayFilter::CompareFilesAndFolderStructure,
            DisplayFilter::ShowOrphans,
            NodeStatus::LeftOrphan,
            false
        ));
    }

    #[test]
    fn flags_answer_without_walking_children() {
        let flags = StatusFlags {
            left_orphan: true,
            same: true,
            ..StatusFlags::default()
        };
        assert!(flags_match(DisplayFilter::ShowOrphans, flags));
        assert!(!flags_match(
            DisplayFilter::ShowRightOrphans,
            StatusFlags {
                left_orphan: true,
                ..StatusFlags::default()
            }
        ));
    }
}
