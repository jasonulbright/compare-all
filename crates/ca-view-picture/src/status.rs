//! The sentences the status bar states.
//!
//! Two of them carry a duty beyond reporting. A verdict of equal is stated
//! only for what the comparison actually saw: the comparison runs at eight bits
//! per channel, so a pair of files that stored more precision and came out
//! equal is reported as equal at eight bits, never as equal. The counters are
//! stated the same way, as shares of the pixels at least one image covers.

use ca_image::compare::Totals;
use ca_image::decode::Fidelity;

/// The one-line verdict.
#[must_use]
pub fn verdict(totals: Totals, ignore_unimportant: bool) -> String {
    if totals.compared_total() == 0 {
        return "Nothing to compare".to_owned();
    }
    if !totals.is_identical(ignore_unimportant) {
        return format!(
            "{} of {} pixels differ ({:.2}%)",
            totals.difference_count(ignore_unimportant),
            totals.compared_total(),
            totals.percent_different(ignore_unimportant)
        );
    }
    if totals.equal_at_eight_bits {
        return "Equal at 8 bits per channel".to_owned();
    }
    "The images are identical".to_owned()
}

/// The counters, each with its share of the compared pixels.
#[must_use]
pub fn counts(totals: Totals) -> String {
    let total = totals.compared_total();
    let share = |count: u64| -> String {
        if total == 0 {
            return "0".to_owned();
        }
        format!("{count} ({:.2}%)", percent(count, total))
    };
    let mut parts = vec![
        format!("Same {}", share(totals.same)),
        format!("Unimportant {}", share(totals.similar)),
        format!("Important {}", share(totals.different)),
    ];
    if totals.left_only > 0 {
        parts.push(format!("Left only {}", share(totals.left_only)));
    }
    if totals.right_only > 0 {
        parts.push(format!("Right only {}", share(totals.right_only)));
    }
    parts.join("   ")
}

/// The share of `total` that `part` makes up, as a percentage.
#[allow(clippy::cast_precision_loss)]
fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    part as f64 * 100.0 / total as f64
}

/// A sentence stating that the comparison saw less than the files hold, or
/// `None` when it saw everything.
///
/// It is written so that an equal verdict is never left standing on its own
/// when the comparison could not see every bit the files stored.
#[must_use]
pub fn fidelity_note(totals: Totals, left: Fidelity, right: Fidelity) -> Option<String> {
    if totals.equal_at_eight_bits {
        let precision = match (left.precision_reduced, right.precision_reduced) {
            (true, true) => "Both files store more than eight bits per channel.",
            (true, false) => "The left file stores more than eight bits per channel.",
            (false, true) => "The right file stores more than eight bits per channel.",
            (false, false) => "The comparison used eight bits per channel.",
        };
        return Some(
            format!(
                "{precision} The comparison ran at eight bits, so the files may still differ in the bits it did not see."
            ),
        );
    }
    let mut parts = Vec::new();
    if left.precision_reduced || right.precision_reduced {
        parts.push("more than eight bits per channel");
    }
    if left.cmyk || right.cmyk {
        parts.push("cyan, magenta, yellow and black samples");
    }
    if left.icc_profile || right.icc_profile {
        parts.push("an embedded color profile that is not applied");
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "The comparison ran on eight-bit red, green, blue and alpha. It did not see: {}.",
        parts.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::{counts, fidelity_note, verdict};
    use ca_image::compare::Totals;
    use ca_image::decode::Fidelity;

    fn deep() -> Fidelity {
        Fidelity {
            bits_per_channel: 16,
            precision_reduced: true,
            cmyk: false,
            icc_profile: false,
        }
    }

    #[test]
    fn an_identical_pair_is_reported_as_identical() {
        let totals = Totals {
            same: 100,
            ..Totals::default()
        };
        assert_eq!(verdict(totals, false), "The images are identical");
        assert_eq!(
            fidelity_note(totals, Fidelity::default(), Fidelity::default()),
            None
        );
    }

    #[test]
    fn an_equal_verdict_at_reduced_precision_says_so() {
        let totals = Totals {
            same: 100,
            equal_at_eight_bits: true,
            ..Totals::default()
        };
        assert_eq!(verdict(totals, false), "Equal at 8 bits per channel");
        let note = fidelity_note(totals, deep(), deep());
        assert!(note.is_some_and(|text| text.contains("may still differ")));
    }

    #[test]
    fn a_reduced_precision_notice_names_only_the_side_that_stores_it() {
        let totals = Totals {
            same: 100,
            equal_at_eight_bits: true,
            ..Totals::default()
        };
        let left_only = fidelity_note(totals, deep(), Fidelity::default()).unwrap_or_default();
        assert!(left_only.starts_with("The left file stores more than eight bits"));
        assert!(!left_only.starts_with("Both files"));

        let right_only = fidelity_note(totals, Fidelity::default(), deep()).unwrap_or_default();
        assert!(right_only.starts_with("The right file stores more than eight bits"));
        assert!(!right_only.starts_with("Both files"));
    }

    #[test]
    fn a_difference_is_counted_and_shared() {
        let totals = Totals {
            same: 75,
            different: 25,
            ..Totals::default()
        };
        assert_eq!(verdict(totals, false), "25 of 100 pixels differ (25.00%)");
        let text = counts(totals);
        assert!(text.contains("Same 75 (75.00%)"));
        assert!(text.contains("Important 25 (25.00%)"));
        assert!(!text.contains("Left only"));
    }

    #[test]
    fn ignoring_unimportant_differences_changes_the_verdict() {
        let totals = Totals {
            same: 90,
            similar: 10,
            ..Totals::default()
        };
        assert_eq!(verdict(totals, false), "10 of 100 pixels differ (10.00%)");
        assert_eq!(verdict(totals, true), "The images are identical");
    }

    #[test]
    fn orphan_pixels_are_listed_when_there_are_any() {
        let totals = Totals {
            same: 10,
            left_only: 5,
            right_only: 5,
            ..Totals::default()
        };
        let text = counts(totals);
        assert!(text.contains("Left only 5 (25.00%)"));
        assert!(text.contains("Right only 5 (25.00%)"));
    }

    #[test]
    fn a_dropped_color_model_is_reported_even_when_the_images_differ() {
        let totals = Totals {
            different: 10,
            ..Totals::default()
        };
        let cmyk = Fidelity {
            bits_per_channel: 8,
            precision_reduced: false,
            cmyk: true,
            icc_profile: true,
        };
        let note = fidelity_note(totals, cmyk, Fidelity::default());
        assert!(note.is_some_and(|text| text.contains("cyan") && text.contains("color profile")));
    }

    #[test]
    fn an_empty_comparison_says_there_is_nothing_to_compare() {
        assert_eq!(verdict(Totals::default(), false), "Nothing to compare");
        assert_eq!(
            counts(Totals::default()),
            "Same 0   Unimportant 0   Important 0"
        );
    }
}
