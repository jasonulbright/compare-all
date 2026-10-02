//! Searching and replacing cell text.
//!
//! A search visits every shown cell of one side, which is a pass over the whole
//! comparison, so it runs on a worker and posts one outcome. The match test is
//! a plain function over one cell, which is what lets every case be tested
//! without a thread.

use crate::model::{DisplayFilter, Side, Source};
use ca_text::{SearchError, SearchOptions, Searcher, TextBuffer};
use ca_ui::find::FindSettings;
use ca_ui::worker::{Cancel, Job, Terminal};
use std::sync::Arc;

/// What a search walks over.
#[derive(Clone)]
pub struct Scope {
    /// The comparison the grid shows.
    pub source: Arc<dyn Source>,
    /// The rows the display keeps.
    pub filter: DisplayFilter,
    /// Whether minor rows count as matching for the filter.
    pub ignore_unimportant: bool,
    /// Comparison columns in display order.
    pub columns: Vec<usize>,
    /// The side whose cells are read.
    pub side: Side,
}

impl Scope {
    fn keeps(&self, row: usize) -> bool {
        self.filter
            .keeps(self.source.row_status(row), self.ignore_unimportant)
    }
}

/// A compiled cell matcher.
pub struct CellMatcher {
    searcher: Searcher,
}

impl CellMatcher {
    /// A matcher for the find strip's settings.
    ///
    /// # Errors
    ///
    /// Returns a sentence naming why the pattern cannot be used.
    pub fn new(settings: &FindSettings) -> Result<Self, String> {
        if settings.pattern.is_empty() {
            return Err("Enter something to search for.".to_owned());
        }
        let options = SearchOptions {
            regex: settings.regex,
            match_case: settings.match_case,
            whole_words: settings.whole_words,
            wrap: false,
            backwards: false,
            scope: None,
        };
        Searcher::new(&settings.pattern, options)
            .map(|searcher| Self { searcher })
            .map_err(|error| format!("The pattern is not usable: {error}"))
    }

    /// True when the cell holds a match.
    #[must_use]
    pub fn matches(&self, cell: &str) -> bool {
        if cell.is_empty() && !self.searcher.options().regex {
            return false;
        }
        let buffer = TextBuffer::from_text(cell);
        matches!(self.searcher.find_from(&buffer, 0), Ok(Some(_)))
    }

    /// The cell with its first match replaced, or `None` without a match.
    #[must_use]
    pub fn replace_first(&self, cell: &str, replacement: &str) -> Option<String> {
        let mut buffer = TextBuffer::from_text(cell);
        match self.searcher.replace_next(&mut buffer, 0, replacement) {
            Ok(Some(_)) => Some(buffer.text()),
            _ => None,
        }
    }

    /// The cell with every match replaced, or `None` without a match.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] when the engine gave up, because matches past
    /// that point are unknown.
    pub fn replace_every(
        &self,
        cell: &str,
        replacement: &str,
    ) -> Result<Option<String>, SearchError> {
        let mut buffer = TextBuffer::from_text(cell);
        let count = self.searcher.replace_all(&mut buffer, replacement)?;
        Ok((count > 0).then(|| buffer.text()))
    }
}

/// What a search posts back.
#[derive(Debug, PartialEq, Eq)]
pub enum SearchMessage {
    /// A match, as a comparison row and a display column.
    Found {
        /// Row of the comparison, not of the display.
        row: usize,
        /// Index into the shown columns.
        column: usize,
    },
    /// No shown cell matches.
    NotFound,
    /// The pattern cannot be used.
    BadPattern(String),
    /// The run stopped before reaching an answer.
    Cancelled,
}

impl Terminal for SearchMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::BadPattern(detail)
    }
}

/// Find the next or the previous matching cell, after the one at
/// `(row, column)`.
#[must_use]
pub fn find(
    scope: &Scope,
    matcher: &CellMatcher,
    start: (usize, usize),
    backwards: bool,
    wrap: bool,
    cancel: &Cancel,
) -> Option<SearchMessage> {
    let width = scope.columns.len();
    let rows = scope.source.rows();
    let total = rows.saturating_mul(width);
    if total == 0 {
        return Some(SearchMessage::NotFound);
    }
    let here = start
        .0
        .saturating_mul(width)
        .saturating_add(start.1)
        .min(total - 1);
    let steps = if wrap {
        total
    } else if backwards {
        here
    } else {
        total - 1 - here
    };
    let mut position = here;
    let mut row_checked = usize::MAX;
    let mut row_kept = false;
    for step in 0..steps {
        position = if backwards {
            position.checked_sub(1).unwrap_or(total - 1)
        } else if position + 1 == total {
            0
        } else {
            position + 1
        };
        let row = position / width;
        if row != row_checked {
            if step % 256 == 0 && cancel.is_cancelled() {
                return None;
            }
            row_checked = row;
            row_kept = scope.keeps(row);
        }
        if !row_kept {
            continue;
        }
        let display = position % width;
        let Some(column) = scope.columns.get(display) else {
            continue;
        };
        if matcher.matches(&scope.source.cell_text(row, *column, scope.side)) {
            return Some(SearchMessage::Found {
                row,
                column: display,
            });
        }
    }
    Some(SearchMessage::NotFound)
}

/// Run [`find`] on a worker thread.
#[must_use]
pub fn spawn_find(
    scope: Scope,
    settings: FindSettings,
    start: (usize, usize),
    backwards: bool,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<SearchMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let matcher = match CellMatcher::new(&settings) {
                Ok(matcher) => matcher,
                Err(reason) => {
                    emitter.send(SearchMessage::BadPattern(reason));
                    return;
                }
            };
            if let Some(message) = find(&scope, &matcher, start, backwards, settings.wrap, cancel) {
                emitter.send(message);
            }
        },
        notify,
    )
}

/// One cell a replace all changes, in the file's own row and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellChange {
    /// Row of the side's file, counted from zero.
    pub row: usize,
    /// Column of the side's file, counted from zero.
    pub column: usize,
    /// The new value.
    pub value: String,
}

/// What a replace all posts back.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplaceMessage {
    /// Every changed cell.
    Changes(Vec<CellChange>),
    /// The pattern cannot be used, or the engine gave up.
    Failed(String),
    /// The run stopped before reaching an answer.
    Cancelled,
}

impl Terminal for ReplaceMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// The new value of every shown cell of the scope's side that holds a match.
///
/// `file_columns` names the side's file column of each comparison column.
///
/// # Errors
///
/// Returns a sentence when the engine gave up on a cell, because a partial
/// replace all would leave matches behind with no way to tell which.
pub fn replace_all(
    scope: &Scope,
    file_columns: &[Option<u32>],
    matcher: &CellMatcher,
    replacement: &str,
    cancel: &Cancel,
) -> Result<Option<Vec<CellChange>>, String> {
    let mut changes = Vec::new();
    for row in 0..scope.source.rows() {
        if row % 256 == 0 && cancel.is_cancelled() {
            return Ok(None);
        }
        if !scope.keeps(row) {
            continue;
        }
        let (left, right) = scope.source.row_numbers(row);
        let file_row = match scope.side {
            Side::Left => left,
            Side::Right => right,
        };
        let Some(file_row) = file_row.and_then(|line| line.checked_sub(1)) else {
            continue;
        };
        for column in &scope.columns {
            let Some(Some(file_column)) = file_columns.get(*column) else {
                continue;
            };
            let text = scope.source.cell_text(row, *column, scope.side);
            let replaced = matcher
                .replace_every(&text, replacement)
                .map_err(|_| "The pattern is too costly to match on this text.".to_owned())?;
            if let Some(value) = replaced {
                changes.push(CellChange {
                    row: file_row as usize,
                    column: *file_column as usize,
                    value,
                });
            }
        }
    }
    Ok(Some(changes))
}

/// Run [`replace_all`] on a worker thread.
#[must_use]
pub fn spawn_replace_all(
    scope: Scope,
    file_columns: Vec<Option<u32>>,
    settings: FindSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<ReplaceMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let matcher = match CellMatcher::new(&settings) {
                Ok(matcher) => matcher,
                Err(reason) => {
                    emitter.send(ReplaceMessage::Failed(reason));
                    return;
                }
            };
            match replace_all(
                &scope,
                &file_columns,
                &matcher,
                &settings.replacement,
                cancel,
            ) {
                Ok(Some(changes)) => {
                    emitter.send(ReplaceMessage::Changes(changes));
                }
                Ok(None) => {}
                Err(reason) => {
                    emitter.send(ReplaceMessage::Failed(reason));
                }
            }
        },
        notify,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{find, replace_all, CellChange, CellMatcher, Scope, SearchMessage};
    use crate::model::{DisplayFilter, Side};
    use crate::testing::FakeSource;
    use ca_table::compare::RowStatus;
    use ca_ui::find::FindSettings;
    use ca_ui::worker::Cancel;
    use std::sync::Arc;

    fn settings(pattern: &str) -> FindSettings {
        FindSettings {
            pattern: pattern.to_owned(),
            ..FindSettings::default()
        }
    }

    fn scope(source: FakeSource, filter: DisplayFilter) -> Scope {
        let columns = (0..crate::model::Source::columns(&source).len()).collect();
        Scope {
            source: Arc::new(source),
            filter,
            ignore_unimportant: false,
            columns,
            side: Side::Left,
        }
    }

    #[test]
    fn a_cell_matcher_follows_the_find_settings() {
        let matcher = CellMatcher::new(&settings("an")).unwrap();
        assert!(matcher.matches("Ann"));
        let exact = CellMatcher::new(&FindSettings {
            match_case: true,
            ..settings("an")
        })
        .unwrap();
        assert!(!exact.matches("Ann"));
        assert_eq!(matcher.replace_first("an an", "X"), Some("X an".to_owned()));
        assert_eq!(
            matcher.replace_every("an an", "X").unwrap(),
            Some("X X".to_owned())
        );
        assert_eq!(matcher.replace_every("bob", "X").unwrap(), None);
        assert!(CellMatcher::new(&settings("")).is_err());
    }

    #[test]
    fn a_search_walks_rows_then_wraps() {
        let source = FakeSource::repeating(4, 2, &[RowStatus::Same]);
        let scope = scope(source, DisplayFilter::All);
        let matcher = CellMatcher::new(&settings("t2-1")).unwrap();
        let cancel = Cancel::default();
        assert_eq!(
            find(&scope, &matcher, (0, 0), false, false, &cancel),
            Some(SearchMessage::Found { row: 2, column: 1 })
        );
        assert_eq!(
            find(&scope, &matcher, (3, 0), false, false, &cancel),
            Some(SearchMessage::NotFound)
        );
        assert_eq!(
            find(&scope, &matcher, (3, 0), false, true, &cancel),
            Some(SearchMessage::Found { row: 2, column: 1 })
        );
        assert_eq!(
            find(&scope, &matcher, (3, 1), true, false, &cancel),
            Some(SearchMessage::Found { row: 2, column: 1 })
        );
    }

    #[test]
    fn a_search_skips_rows_the_filter_hides() {
        let source = FakeSource::repeating(3, 1, &[RowStatus::Same]);
        let scope = scope(source, DisplayFilter::Differences);
        let matcher = CellMatcher::new(&settings("t1-0")).unwrap();
        assert_eq!(
            find(&scope, &matcher, (0, 0), false, true, &Cancel::default()),
            Some(SearchMessage::NotFound)
        );
    }

    #[test]
    fn replace_all_reports_file_rows_and_columns() {
        let source = FakeSource::repeating(3, 2, &[RowStatus::Same]);
        let scope = scope(source, DisplayFilter::All);
        let matcher = CellMatcher::new(&settings("-1")).unwrap();
        let changes = replace_all(
            &scope,
            &[Some(0), Some(5)],
            &matcher,
            "Z",
            &Cancel::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            changes,
            vec![
                CellChange {
                    row: 0,
                    column: 5,
                    value: "Left1Z".to_owned()
                },
                CellChange {
                    row: 1,
                    column: 5,
                    value: "Left2Z".to_owned()
                },
            ]
        );
    }
}
