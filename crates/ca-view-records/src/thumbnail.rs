//! The strip beside the panes that shows where the differences are.
//!
//! The reduction itself is [`ca_ui::thumbnail`]. What is decided here is which
//! rows speak for a pixel. A key that is open lets its own rows speak, so a key
//! paints only when it is closed or present on one side only; otherwise one
//! difference would paint the whole run of keys above it.

use crate::model::Listing;
use ca_ui::theme::records::RecordClass;

/// The strip's colors, one per pixel of its height.
pub type Strip = ca_ui::thumbnail::Strip<RecordClass>;

/// Work the strip out for the rows on screen and a height in points.
#[must_use]
pub fn build(listing: &Listing, height: f32) -> Strip {
    Strip::from_rows(height, listing.rows(), |row| {
        if !listing.is_stop(row) {
            return None;
        }
        listing.class_at(row)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::build;
    use crate::model::{flatten, Listing};
    use ca_records::compare::compare_trees;
    use ca_records::{AlignOptions, Record, RecordTree, RecordValue};
    use ca_ui::theme::records::RecordClass;
    use std::sync::Arc;

    #[test]
    fn one_changed_value_among_many_still_shows() {
        let mut left = RecordTree::new("", "root");
        let mut right = RecordTree::new("", "root");
        for index in 0..10_000 {
            let name = format!("v{index:05}");
            let text = if index == 9_999 { "changed" } else { "same" };
            left.push(Record::new(
                "",
                &name,
                "REG_SZ",
                RecordValue::Text("same".into()),
            ));
            right.push(Record::new(
                "",
                &name,
                "REG_SZ",
                RecordValue::Text(text.into()),
            ));
        }
        let diff = compare_trees(&left, &right, &AlignOptions::default());
        let listing = Listing::new(Arc::new(flatten(diff)));
        let strip = build(&listing, 100.0);
        assert_eq!(strip.pixels(), 100);
        assert_eq!(strip.class_at(99), Some(RecordClass::Different));
        assert_eq!(strip.class_at(0), None);
    }
}
