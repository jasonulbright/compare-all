//! Structural properties every engine must hold for arbitrary inputs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

use ca_diff::bytes::{diff_bytes, ByteAlignment, ByteHunk};
use ca_diff::lines::{
    diff_line_slices, diff_line_slices_anchored, AlignAnchor, AlignmentMode, AlignmentOptions,
    Hunk, HunkKind, LineCompareOptions,
};
use ca_diff::merge3::{merge3, MergeOptions};
use proptest::prelude::*;

/// Lines drawn from a small alphabet so matches and mismatches both occur.
fn lines() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(
        prop_oneof![
            Just("alpha\n".to_owned()),
            Just("alpha\r".to_owned()),
            Just("alpha\r\n".to_owned()),
            Just("alpha".to_owned()),
            Just("beta\n".to_owned()),
            Just("gamma\n".to_owned()),
            Just("delta\n".to_owned()),
            Just("\n".to_owned()),
            Just("\r\n".to_owned()),
            Just("  indented\n".to_owned()),
            Just("café\n".to_owned()),
            "[a-c]{1,4}".prop_map(|s| format!("{s}\n")),
        ],
        0..24,
    )
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn with_whitespace(line: &str) -> String {
    let (body, ending) = if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else if let Some(body) = line.strip_suffix('\r') {
        (body, "\r")
    } else {
        (line, "")
    };
    format!(" \t{body} \t{ending}")
}

fn assert_covers(hunks: &[Hunk], left_len: u32, right_len: u32) {
    let mut cl = 0;
    let mut cr = 0;
    for h in hunks {
        assert_eq!(h.left.start, cl, "left gap or overlap at {h:?}");
        assert_eq!(h.right.start, cr, "right gap or overlap at {h:?}");
        assert!(h.left.end >= h.left.start);
        assert!(h.right.end >= h.right.start);
        cl = h.left.end;
        cr = h.right.end;
    }
    assert_eq!(cl, left_len);
    assert_eq!(cr, right_len);
}

/// Rebuild each input using the ranges allowed by each hunk kind.
fn assert_rebuilds_both_sides(
    hunks: &[Hunk],
    left: &[&str],
    right: &[&str],
    options: &LineCompareOptions,
) {
    assert_covers(hunks, left.len() as u32, right.len() as u32);
    let mut rebuilt_left = Vec::new();
    let mut rebuilt_right = Vec::new();
    for hunk in hunks {
        let left_part = &left[hunk.left.start as usize..hunk.left.end as usize];
        let right_part = &right[hunk.right.start as usize..hunk.right.end as usize];
        match hunk.kind {
            HunkKind::Same => {
                assert_eq!(left_part.len(), right_part.len(), "{hunk:?}");
                for (left, right) in left_part.iter().zip(right_part) {
                    assert_eq!(
                        ca_diff::normalize_line(left, options),
                        ca_diff::normalize_line(right, options),
                        "{hunk:?}"
                    );
                }
            }
            HunkKind::Changed => {
                assert!(!left_part.is_empty() && !right_part.is_empty(), "{hunk:?}");
            }
            HunkKind::LeftOnly => assert!(right_part.is_empty(), "{hunk:?}"),
            HunkKind::RightOnly => assert!(left_part.is_empty(), "{hunk:?}"),
        }
        if hunk.kind != HunkKind::RightOnly {
            rebuilt_left.extend(left_part.iter().map(|line| (*line).to_owned()));
        }
        if hunk.kind != HunkKind::LeftOnly {
            rebuilt_right.extend(right_part.iter().map(|line| (*line).to_owned()));
        }
    }
    assert_eq!(rebuilt_left, left, "left side from {hunks:?}");
    assert_eq!(rebuilt_right, right, "right side from {hunks:?}");
}

fn assert_same_under_default_rules(left: &[String], right: &[String]) {
    assert_eq!(left.len(), right.len());
    let options = LineCompareOptions::default();
    for (left, right) in left.iter().zip(right) {
        assert_eq!(
            ca_diff::normalize_line(left, &options),
            ca_diff::normalize_line(right, &options)
        );
    }
}

fn all_option_sets() -> Vec<LineCompareOptions> {
    let modes = [
        AlignmentMode::Standard,
        AlignmentMode::Myers,
        AlignmentMode::MyersMinimal,
        AlignmentMode::Patience,
        AlignmentMode::Unaligned,
    ];
    let mut out = Vec::new();
    for mode in modes {
        for closeness in [false, true] {
            for never_align in [false, true] {
                for skew in [None, Some(3)] {
                    out.push(LineCompareOptions {
                        alignment: AlignmentOptions {
                            mode,
                            skew_tolerance: skew,
                            never_align_differences: never_align,
                            use_closeness_matching: closeness,
                        },
                        ..LineCompareOptions::default()
                    });
                }
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn line_hunks_cover_both_inputs(a in lines(), b in lines()) {
        let (la, lb) = (refs(&a), refs(&b));
        for opts in all_option_sets() {
            let hunks = diff_line_slices(&la, &lb, &opts);
            assert_covers(&hunks, la.len() as u32, lb.len() as u32);
        }
    }

    #[test]
    fn walking_hunks_reproduces_the_right_side(a in lines(), b in lines()) {
        let (la, lb) = (refs(&a), refs(&b));
        for opts in all_option_sets() {
            let hunks = diff_line_slices(&la, &lb, &opts);
            assert_rebuilds_both_sides(&hunks, &la, &lb, &opts);
        }
    }

    #[test]
    fn same_hunks_really_are_the_same_text(a in lines(), b in lines()) {
        let (la, lb) = (refs(&a), refs(&b));
        let hunks = diff_line_slices(&la, &lb, &LineCompareOptions::default());
        for h in hunks.iter().filter(|h| h.kind == HunkKind::Same) {
            prop_assert_eq!(h.left.end - h.left.start, h.right.end - h.right.start);
            for k in 0..(h.left.end - h.left.start) {
                prop_assert_eq!(
                    ca_diff::normalize_line(la[(h.left.start + k) as usize], &LineCompareOptions::default()),
                    ca_diff::normalize_line(lb[(h.right.start + k) as usize], &LineCompareOptions::default())
                );
            }
        }
    }

    #[test]
    fn identical_inputs_never_differ(a in lines()) {
        let la = refs(&a);
        for opts in all_option_sets() {
            let hunks = diff_line_slices(&la, &la, &opts);
            prop_assert!(hunks.iter().all(|h| h.kind == HunkKind::Same));
        }
    }

    #[test]
    fn ignore_case_never_reports_a_case_only_change(a in lines()) {
        let upper: Vec<String> = a.iter().map(|s| s.to_uppercase()).collect();
        let (la, lu) = (refs(&a), refs(&upper));
        let opts = LineCompareOptions { ignore_case: true, ..LineCompareOptions::default() };
        let hunks = diff_line_slices(&la, &lu, &opts);
        prop_assert!(hunks.iter().all(|h| h.kind == HunkKind::Same));
    }

    #[test]
    fn ignore_all_whitespace_never_reports_a_whitespace_only_change(a in lines()) {
        let padded: Vec<String> = a
            .iter()
            .map(|line| with_whitespace(line))
            .collect();
        let (la, lp) = (refs(&a), refs(&padded));
        let opts = LineCompareOptions {
            ignore_all_whitespace: true,
            ..LineCompareOptions::default()
        };
        let hunks = diff_line_slices(&la, &lp, &opts);
        prop_assert!(hunks.iter().all(|h| h.kind == HunkKind::Same));
    }

    #[test]
    fn anchors_are_honoured_and_coverage_holds(a in lines(), b in lines(), i in 0usize..24, j in 0usize..24) {
        let (la, lb) = (refs(&a), refs(&b));
        prop_assume!(!la.is_empty() && !lb.is_empty());
        let li = (i % la.len()) as u32;
        let rj = (j % lb.len()) as u32;
        let anchors = [AlignAnchor { left: li..li + 1, right: rj..rj + 1 }];
        let hunks = diff_line_slices_anchored(&la, &lb, &LineCompareOptions::default(), &anchors).unwrap();
        assert_covers(&hunks, la.len() as u32, lb.len() as u32);
        let hit = hunks
            .iter()
            .find(|h| h.left.start <= li && li < h.left.end)
            .expect("anchored line is covered");
        prop_assert!(hit.right.start <= rj && rj < hit.right.end);
    }

    #[test]
    fn merge_with_left_equal_to_base_yields_right(base in lines(), right in lines()) {
        let (lb, lr) = (refs(&base), refs(&right));
        let result = merge3(&lb, &lb, &lr, &MergeOptions::default());
        prop_assert_eq!(result.conflicts, 0);
        assert_same_under_default_rules(&result.output, &right);
    }

    #[test]
    fn merge_with_right_equal_to_base_yields_left(base in lines(), left in lines()) {
        let (lb, ll) = (refs(&base), refs(&left));
        let result = merge3(&ll, &lb, &lb, &MergeOptions::default());
        prop_assert_eq!(result.conflicts, 0);
        assert_same_under_default_rules(&result.output, &left);
    }

    #[test]
    fn merge_with_equal_sides_yields_that_side(base in lines(), side in lines()) {
        let (lb, ls) = (refs(&base), refs(&side));
        let result = merge3(&ls, &lb, &ls, &MergeOptions::default());
        prop_assert_eq!(result.conflicts, 0);
        assert_same_under_default_rules(&result.output, &side);
    }

    #[test]
    fn merge_regions_cover_the_ancestor_and_the_output(
        base in lines(), left in lines(), right in lines()
    ) {
        let result = merge3(&refs(&left), &refs(&base), &refs(&right), &MergeOptions::default());
        let mut base_cursor = 0u32;
        let mut out_cursor = 0u32;
        for region in &result.regions {
            prop_assert_eq!(region.base.start, base_cursor);
            prop_assert_eq!(region.out.start, out_cursor);
            base_cursor = region.base.end;
            out_cursor = region.out.end;
        }
        prop_assert_eq!(base_cursor as usize, base.len());
        prop_assert_eq!(out_cursor as usize, result.output.len());
    }
}

fn assert_byte_covers(hunks: &[ByteHunk], left_len: u64, right_len: u64) {
    let mut cl = 0;
    let mut cr = 0;
    for h in hunks {
        assert_eq!(h.left.start, cl, "left gap at {h:?}");
        assert_eq!(h.right.start, cr, "right gap at {h:?}");
        cl = h.left.end;
        cr = h.right.end;
    }
    assert_eq!(cl, left_len);
    assert_eq!(cr, right_len);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn byte_hunks_cover_both_inputs(
        a in prop::collection::vec(0u8..6, 0..3000),
        b in prop::collection::vec(0u8..6, 0..3000),
    ) {
        for mode in [ByteAlignment::None, ByteAlignment::Fast, ByteAlignment::Complete] {
            let hunks = diff_bytes(&a, &b, mode);
            assert_byte_covers(&hunks, a.len() as u64, b.len() as u64);
        }
    }

    #[test]
    fn walking_byte_hunks_reproduces_both_sides(
        a in prop::collection::vec(0u8..6, 0..3000),
        b in prop::collection::vec(0u8..6, 0..3000),
    ) {
        for mode in [ByteAlignment::None, ByteAlignment::Fast, ByteAlignment::Complete] {
            let hunks = diff_bytes(&a, &b, mode);
            let mut left = Vec::new();
            let mut right = Vec::new();
            for h in &hunks {
                left.extend_from_slice(&a[h.left.start as usize..h.left.end as usize]);
                right.extend_from_slice(&b[h.right.start as usize..h.right.end as usize]);
            }
            prop_assert_eq!(left, a.clone());
            prop_assert_eq!(right, b.clone());
        }
    }

    #[test]
    fn byte_same_runs_hold_equal_bytes(
        a in prop::collection::vec(0u8..6, 0..3000),
        b in prop::collection::vec(0u8..6, 0..3000),
    ) {
        for mode in [ByteAlignment::None, ByteAlignment::Fast, ByteAlignment::Complete] {
            for h in diff_bytes(&a, &b, mode).iter().filter(|h| h.kind == HunkKind::Same) {
                prop_assert_eq!(
                    &a[h.left.start as usize..h.left.end as usize],
                    &b[h.right.start as usize..h.right.end as usize]
                );
            }
        }
    }
}
