//! Finding rows in a Registry, Version or Media comparison.
//!
//! The search runs away from the frame thread. It walks the rows the current
//! listing shows, matching names, types and values on either side.

use crate::model::{Listing, Node, Side};
use ca_text::{SearchError, TextBuffer};
use ca_ui::find::FindSettings;
use ca_ui::worker::{Cancel, Job, Terminal};
use std::sync::Arc;

/// What a find run posts back to its view.
#[derive(Debug, PartialEq, Eq)]
pub enum FindMessage {
    /// A visible listing row matched.
    Found(usize),
    /// The visible rows contain no match.
    NotFound,
    /// The pattern is empty.
    NoPattern,
    /// The pattern could not be compiled.
    BadPattern(String),
    /// Matching a value exceeded the engine's backtracking limit.
    LimitExceeded,
    /// The run stopped before reaching an answer.
    Cancelled,
}

impl Terminal for FindMessage {
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

/// Start a row search on a worker thread.
#[must_use]
pub fn spawn_find(
    listing: Listing,
    settings: FindSettings,
    start: usize,
    backwards: bool,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<FindMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            if let Some(message) = find(&listing, &settings, start, backwards, cancel) {
                emitter.send(message);
            }
        },
        notify,
    )
}

/// Find the next or previous matching visible row after `start`.
///
/// The current row is skipped. Wrapping follows the Find panel setting. The
/// returned row is a visible row index, ready for [`Listing::set_cursor`].
#[must_use]
pub fn find(
    listing: &Listing,
    settings: &FindSettings,
    start: usize,
    backwards: bool,
    cancel: &Cancel,
) -> Option<FindMessage> {
    if settings.pattern.is_empty() {
        return Some(FindMessage::NoPattern);
    }
    let searcher = match settings.searcher(backwards, None) {
        Ok(searcher) => searcher,
        Err(error) => return Some(FindMessage::BadPattern(error.to_string())),
    };
    let rows = listing.rows();
    if rows == 0 {
        return Some(FindMessage::NotFound);
    }
    let start = start.min(rows - 1);
    let steps = if settings.wrap {
        rows
    } else if backwards {
        start
    } else {
        rows - 1 - start
    };
    let mut row = start;
    for step in 0..steps {
        if step % 128 == 0 && cancel.is_cancelled() {
            return None;
        }
        row = if backwards {
            row.checked_sub(1).unwrap_or(rows - 1)
        } else if row + 1 == rows {
            0
        } else {
            row + 1
        };
        let Some(node) = listing.node_at(row) else {
            continue;
        };
        match row_matches(node, &searcher) {
            Ok(true) => return Some(FindMessage::Found(row)),
            Ok(false) => {}
            Err(SearchError::BacktrackLimit) => return Some(FindMessage::LimitExceeded),
            Err(error) => return Some(FindMessage::BadPattern(error.to_string())),
        }
    }
    if cancel.is_cancelled() {
        None
    } else {
        Some(FindMessage::NotFound)
    }
}

fn row_matches(node: &Node, searcher: &ca_text::Searcher) -> Result<bool, SearchError> {
    if matches_text(&node.name, searcher)? {
        return Ok(true);
    }
    if node.is_group {
        return Ok(false);
    }
    for side in [Side::Left, Side::Right] {
        let Some(record) = node.record(side) else {
            continue;
        };
        if matches_text(&record.type_name, searcher)? || matches_text(&record.display, searcher)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn matches_text(text: &str, searcher: &ca_text::Searcher) -> Result<bool, SearchError> {
    if text.is_empty() {
        return Ok(false);
    }
    let buffer = TextBuffer::from_text(text);
    let from = if searcher.options().backwards {
        buffer.len_chars()
    } else {
        0
    };
    Ok(searcher.find_from(&buffer, from)?.is_some())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{find, FindMessage};
    use crate::model::{flatten, DisplayFilter, Listing};
    use ca_records::compare::compare_trees;
    use ca_records::{AlignOptions, Record, RecordTree, RecordValue};
    use ca_ui::find::FindSettings;
    use ca_ui::worker::Cancel;
    use std::sync::Arc;

    fn settings(pattern: &str) -> FindSettings {
        FindSettings {
            pattern: pattern.to_owned(),
            ..FindSettings::default()
        }
    }

    fn listing() -> Listing {
        let mut left = RecordTree::new("", "root");
        let mut right = RecordTree::new("", "root");
        let mut group_left = RecordTree::new("Software\\Example", "Example");
        let mut group_right = RecordTree::new("Software\\Example", "Example");
        group_left.push(Record::new(
            "Software\\Example",
            "Company",
            "REG_SZ",
            RecordValue::Text("Acme North".to_owned()),
        ));
        group_right.push(Record::new(
            "Software\\Example",
            "Company",
            "REG_SZ",
            RecordValue::Text("Acme South".to_owned()),
        ));
        group_left.push(Record::new(
            "Software\\Example",
            "BuildNumber",
            "REG_DWORD",
            RecordValue::Integer(41),
        ));
        group_right.push(Record::new(
            "Software\\Example",
            "BuildNumber",
            "REG_DWORD",
            RecordValue::Integer(41),
        ));
        left.push_child(group_left);
        right.push_child(group_right);
        let diff = compare_trees(&left, &right, &AlignOptions::default());
        Listing::new(Arc::new(flatten(diff)))
    }

    #[test]
    fn find_searches_visible_names_and_values_forward_and_backward() {
        let listing = listing();
        let cancel = Cancel::new();
        assert_eq!(
            find(&listing, &settings("South"), 0, false, &cancel),
            Some(FindMessage::Found(2))
        );
        assert_eq!(
            find(&listing, &settings("REG_DWORD"), 2, true, &cancel),
            Some(FindMessage::Found(1))
        );
    }

    #[test]
    fn find_matches_the_name_shown_on_each_row_not_its_parent_path() {
        let listing = listing();
        let cancel = Cancel::new();
        let group = (0..listing.rows())
            .find(|row| {
                listing
                    .node_at(*row)
                    .is_some_and(|node| node.name == "Example")
            })
            .unwrap();
        let company = (0..listing.rows())
            .find(|row| {
                listing
                    .node_at(*row)
                    .is_some_and(|node| node.name == "Company")
            })
            .unwrap();

        let mut group_search = settings("Example");
        group_search.wrap = true;
        assert_eq!(
            find(&listing, &group_search, listing.rows() - 1, false, &cancel),
            Some(FindMessage::Found(group))
        );
        let mut regex = settings("^Company$");
        regex.regex = true;
        regex.match_case = true;
        assert_eq!(
            find(&listing, &regex, 0, false, &cancel),
            Some(FindMessage::Found(company))
        );
    }

    #[test]
    fn find_respects_case_regex_filter_and_wrap() {
        let mut listing = listing();
        let cancel = Cancel::new();
        let mut parent_path = settings("Software\\Example\\BuildNumber");
        parent_path.wrap = true;
        assert_eq!(
            find(&listing, &parent_path, 0, false, &cancel),
            Some(FindMessage::NotFound)
        );

        let mut regex = settings("buildnumber$");
        regex.regex = true;
        regex.match_case = true;
        assert_eq!(
            find(&listing, &regex, 0, false, &cancel),
            Some(FindMessage::NotFound)
        );
        regex.match_case = false;
        let build_number = find(&listing, &regex, 0, false, &cancel);
        assert!(matches!(build_number, Some(FindMessage::Found(row))
            if listing.node_at(row).is_some_and(|node| node.name == "BuildNumber")));

        listing.set_filter(DisplayFilter::Differences);
        assert_eq!(
            find(&listing, &settings("41"), 1, false, &cancel),
            Some(FindMessage::NotFound)
        );
        let group = (0..listing.rows())
            .find(|row| {
                listing
                    .node_at(*row)
                    .is_some_and(|node| node.name == "Example")
            })
            .unwrap();
        let company = (0..listing.rows())
            .find(|row| {
                listing
                    .node_at(*row)
                    .is_some_and(|node| node.name == "Company")
            })
            .unwrap();
        assert_eq!(
            find(&listing, &settings("South"), group, false, &cancel),
            Some(FindMessage::Found(company))
        );
        let mut wrap = settings("example");
        wrap.wrap = true;
        assert_eq!(
            find(&listing, &wrap, company, false, &cancel),
            Some(FindMessage::Found(group))
        );
        assert_eq!(listing.node_at(group).unwrap().name, "Example");
        wrap.wrap = false;
        assert_eq!(
            find(&listing, &wrap, company, false, &cancel),
            Some(FindMessage::NotFound)
        );
    }

    #[test]
    fn find_reports_empty_and_invalid_patterns() {
        let listing = listing();
        let cancel = Cancel::new();
        assert_eq!(
            find(&listing, &settings(""), 0, false, &cancel),
            Some(FindMessage::NoPattern)
        );
        let mut invalid = settings("[");
        invalid.regex = true;
        assert!(matches!(
            find(&listing, &invalid, 0, false, &cancel),
            Some(FindMessage::BadPattern(_))
        ));
    }
}
