//! The display filters of a merge.
//!
//! A merge filters on how each line stands in the merge rather than on whether
//! two sides differ, so its filters are its own. Which rows a filter keeps
//! follows from [`LineStatus`] alone; the shared [`Visible`] result then maps
//! screen positions to rows.

use crate::model::{LineStatus, MergeModel};
use ca_ui::command::Command;
use ca_ui::filter::{self, Visible};

/// Which lines the four panes show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeFilter {
    /// Every line.
    #[default]
    All,
    /// Every changed line, conflicts included.
    Changes,
    /// Only the lines that wait for review.
    Conflicts,
    /// The lines the left side changed.
    LeftChanges,
    /// The lines the right side changed.
    RightChanges,
    /// Changed lines that do not wait for review.
    Mergeable,
    /// Only the lines neither side changed.
    Unchanged,
    /// No line.
    None,
    /// Changed lines with this many unchanged lines around each run of them.
    Context(u32),
}

impl MergeFilter {
    /// Every filter the control lists, in menu order. The context count is a
    /// placeholder the view replaces with the configured one.
    pub const ALL: [Self; 9] = [
        Self::All,
        Self::Changes,
        Self::Conflicts,
        Self::LeftChanges,
        Self::RightChanges,
        Self::Mergeable,
        Self::Unchanged,
        Self::None,
        Self::Context(0),
    ];

    /// The command that selects this filter.
    #[must_use]
    pub const fn command(self) -> Command {
        match self {
            Self::All => Command::ShowAll,
            Self::Changes => Command::ShowDifferences,
            Self::Conflicts => Command::ShowConflicts,
            Self::LeftChanges => Command::ShowLeftChanges,
            Self::RightChanges => Command::ShowRightChanges,
            Self::Mergeable => Command::ShowMergeable,
            Self::Unchanged => Command::ShowSame,
            Self::None => Command::ShowNone,
            Self::Context(_) => Command::ShowContext,
        }
    }

    /// The filter a command selects, with `context` lines for Show Context.
    #[must_use]
    pub fn from_command(command: Command, context: u32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|filter| filter.command() == command)
            .map(|filter| match filter {
                Self::Context(_) => Self::Context(context),
                other => other,
            })
    }

    /// The name the control and the menu show.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "Show All",
            Self::Changes => "Show Changes",
            Self::Conflicts => "Show Conflicts",
            Self::LeftChanges => "Show Left Changes",
            Self::RightChanges => "Show Right Changes",
            Self::Mergeable => "Show Mergeable",
            Self::Unchanged => "Show Unchanged",
            Self::None => "Show None",
            Self::Context(_) => "Show Context",
        }
    }

    /// True when the two filters are the same choice, whatever context count
    /// each carries.
    #[must_use]
    pub const fn same_choice(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::All, Self::All)
                | (Self::Changes, Self::Changes)
                | (Self::Conflicts, Self::Conflicts)
                | (Self::LeftChanges, Self::LeftChanges)
                | (Self::RightChanges, Self::RightChanges)
                | (Self::Mergeable, Self::Mergeable)
                | (Self::Unchanged, Self::Unchanged)
                | (Self::None, Self::None)
                | (Self::Context(_), Self::Context(_))
        )
    }

    /// True when a line of this status passes the filter.
    ///
    /// Show Context decides by position as well, so this answers for the
    /// changed lines it always keeps.
    #[must_use]
    pub const fn keeps(self, status: LineStatus) -> bool {
        use LineStatus as S;
        match self {
            Self::All => true,
            Self::None => false,
            Self::Changes | Self::Context(_) => !matches!(status, S::Unchanged),
            Self::Conflicts => matches!(status, S::Conflict),
            Self::LeftChanges => matches!(
                status,
                S::SameChange | S::LeftChange | S::DifferentChange | S::Conflict
            ),
            Self::RightChanges => matches!(
                status,
                S::SameChange | S::RightChange | S::DifferentChange | S::Conflict
            ),
            Self::Mergeable => matches!(
                status,
                S::SameChange | S::LeftChange | S::RightChange | S::DifferentChange
            ),
            Self::Unchanged => matches!(status, S::Unchanged),
        }
    }
}

/// The rows of `model` that `filter` shows.
#[must_use]
pub fn visible(model: &MergeModel, filter: MergeFilter) -> Visible {
    let rows = model.rows().len();
    let status_of = |row: usize| {
        model
            .section_of_row(row)
            .and_then(|section| model.sections().get(section))
            .map_or(LineStatus::Unchanged, crate::model::Section::status)
    };
    match filter {
        MergeFilter::All => Visible::All(rows),
        MergeFilter::Context(reach) => {
            let changed: Vec<_> = model
                .sections()
                .iter()
                .enumerate()
                .filter(|(_, section)| !matches!(section.status(), LineStatus::Unchanged))
                .filter_map(|(index, _)| model.section_row_range(index))
                .collect();
            filter::context(&changed, reach as usize, rows)
        }
        other => filter::select(rows, |row| other.keeps(status_of(row))),
    }
}

#[cfg(test)]
mod tests {
    use super::MergeFilter;
    use crate::model::LineStatus;
    use ca_ui::command::Command;

    /// The status lists each filter keeps, as the documented filter set states
    /// them.
    #[test]
    fn each_filter_keeps_the_documented_statuses() {
        use LineStatus as S;
        let every = [
            S::Unchanged,
            S::SameChange,
            S::LeftChange,
            S::RightChange,
            S::DifferentChange,
            S::Conflict,
        ];
        let table: [(MergeFilter, &[S]); 8] = [
            (MergeFilter::All, &every),
            (
                MergeFilter::Changes,
                &[
                    S::SameChange,
                    S::LeftChange,
                    S::RightChange,
                    S::DifferentChange,
                    S::Conflict,
                ],
            ),
            (MergeFilter::Conflicts, &[S::Conflict]),
            (
                MergeFilter::LeftChanges,
                &[
                    S::SameChange,
                    S::LeftChange,
                    S::DifferentChange,
                    S::Conflict,
                ],
            ),
            (
                MergeFilter::RightChanges,
                &[
                    S::SameChange,
                    S::RightChange,
                    S::DifferentChange,
                    S::Conflict,
                ],
            ),
            (
                MergeFilter::Mergeable,
                &[
                    S::SameChange,
                    S::LeftChange,
                    S::RightChange,
                    S::DifferentChange,
                ],
            ),
            (MergeFilter::Unchanged, &[S::Unchanged]),
            (MergeFilter::None, &[]),
        ];
        for (filter, kept) in table {
            for status in every {
                assert_eq!(
                    filter.keeps(status),
                    kept.contains(&status),
                    "{filter:?} and {status:?}"
                );
            }
        }
    }

    fn merged() -> crate::model::MergeModel {
        use crate::model::{split, Inputs, MergeModel};
        MergeModel::build(
            Inputs {
                left: split("L\np\nq\nr\ns\nt\nX\n"),
                center: split("a\np\nq\nr\ns\nt\nd\n"),
                right: split("a\np\nq\nr\ns\nt\nY\n"),
                two_way: false,
            },
            &ca_diff::merge3::MergeOptions::default(),
            &ca_ui::worker::Cancel::new(),
        )
        .unwrap_or_default()
    }

    #[test]
    fn the_rows_a_filter_shows_follow_the_line_status() {
        let model = merged();
        let rows_of = |filter| {
            let visible = super::visible(&model, filter);
            (0..visible.len())
                .filter_map(|position| visible.row_at(position))
                .collect::<Vec<_>>()
        };
        assert_eq!(rows_of(MergeFilter::All).len(), 7);
        assert_eq!(rows_of(MergeFilter::Conflicts), vec![6]);
        assert_eq!(rows_of(MergeFilter::LeftChanges), vec![0, 6]);
        assert_eq!(rows_of(MergeFilter::Mergeable), vec![0]);
        assert_eq!(rows_of(MergeFilter::Unchanged), vec![1, 2, 3, 4, 5]);
        assert_eq!(rows_of(MergeFilter::Context(1)), vec![0, 1, 5, 6]);
        assert!(rows_of(MergeFilter::None).is_empty());
    }

    #[test]
    fn every_filter_has_its_own_command_and_reads_back_from_it() {
        for filter in MergeFilter::ALL {
            let back = MergeFilter::from_command(filter.command(), 0).unwrap_or_default();
            assert!(back.same_choice(filter), "{filter:?}");
            assert!(!filter.label().is_empty());
        }
        assert_eq!(
            MergeFilter::from_command(Command::ShowContext, 4),
            Some(MergeFilter::Context(4))
        );
        assert_eq!(MergeFilter::from_command(Command::Copy, 4), None);
    }
}
