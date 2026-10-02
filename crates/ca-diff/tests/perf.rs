//! Hand-run timing harness for the workloads the engines are sized for.
//!
//! Every case is `#[ignore]` because the inputs are large enough that a debug
//! build takes minutes. Run them with
//! `cargo test -p ca-diff --release -- --ignored --nocapture`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_diff::bytes::{diff_bytes, ByteAlignment};
use ca_diff::importance::{
    classify_hunks, ReplacementOptions, ReplacementRule, RuleSet, WhitespaceClassifier,
};
use ca_diff::inline::diff_chars;
use ca_diff::lines::{
    diff_line_slices, AlignmentMode, AlignmentOptions, HunkKind, LineCompareOptions,
};
use std::time::Instant;

fn report<T>(name: &str, work: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = work();
    println!("{name}: {:?}", start.elapsed());
    value
}

fn numbered(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|i| format!("line {i} of a fairly ordinary source file\n"))
        .collect()
}

fn change_every(lines: &[String], every: usize) -> Vec<String> {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i % every == 0 {
                format!("changed line {i}\n")
            } else {
                l.clone()
            }
        })
        .collect()
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[test]
#[ignore = "timing harness"]
fn million_line_text() {
    let left = numbered(1_000_000);
    let right = change_every(&left, 100);
    let (lr, rr) = (refs(&left), refs(&right));
    for mode in [
        AlignmentMode::Standard,
        AlignmentMode::Myers,
        AlignmentMode::Patience,
    ] {
        let opts = LineCompareOptions {
            alignment: AlignmentOptions {
                mode,
                ..AlignmentOptions::default()
            },
            ..LineCompareOptions::default()
        };
        let hunks = report(&format!("1M lines, 1% changed, {mode:?}"), || {
            diff_line_slices(&lr, &rr, &opts)
        });
        println!("  hunks: {}", hunks.len());
    }
}

/// Patience shares the histogram engine, so it must also leave that engine
/// above the size limit. Without the fallback this case runs an order of
/// magnitude longer than the same input under `Standard`.
#[test]
#[ignore = "timing harness"]
fn patience_takes_the_size_limit_like_standard() {
    let left = numbered(1_000_000);
    let right = change_every(&left, 100);
    let (lr, rr) = (refs(&left), refs(&right));

    let timed = |mode: AlignmentMode| {
        let opts = LineCompareOptions {
            alignment: AlignmentOptions {
                mode,
                ..AlignmentOptions::default()
            },
            ..LineCompareOptions::default()
        };
        let start = Instant::now();
        let hunks = diff_line_slices(&lr, &rr, &opts);
        let elapsed = start.elapsed();
        println!(
            "1M lines, 1% changed, {mode:?}: {elapsed:?} ({} hunks)",
            hunks.len()
        );
        (elapsed, hunks)
    };

    let (standard_time, standard_hunks) = timed(AlignmentMode::Standard);
    let (patience_time, patience_hunks) = timed(AlignmentMode::Patience);

    assert_eq!(
        patience_hunks, standard_hunks,
        "above the limit both modes run the same engine and report the same hunks"
    );
    assert!(
        patience_time < standard_time * 4,
        "patience took {patience_time:?} against {standard_time:?} for standard"
    );
}

#[test]
#[ignore = "timing harness"]
fn many_wide_changed_regions() {
    // 100 changed regions of 256 lines per side, each line a kilobyte: the
    // shape that makes pairwise closeness matching expensive.
    let line = |tag: char| -> String {
        let mut s: String = std::iter::repeat_n(tag, 1023).collect();
        s.push('\n');
        s
    };
    let mut left = Vec::new();
    let mut right = Vec::new();
    for region in 0..100u32 {
        for i in 0..256u32 {
            let l = char::from(b'a' + u8::try_from((region + i) % 13).unwrap());
            let r = char::from(b'n' + u8::try_from((region + i) % 13).unwrap());
            left.push(line(l));
            right.push(line(r));
        }
        left.push(format!("anchor {region}\n"));
        right.push(format!("anchor {region}\n"));
    }
    let (lr, rr) = (refs(&left), refs(&right));
    let hunks = report("100 regions of 256x256 kilobyte lines", || {
        diff_line_slices(&lr, &rr, &LineCompareOptions::default())
    });
    println!("  hunks: {}", hunks.len());
}

#[test]
#[ignore = "timing harness"]
fn one_huge_line() {
    // A minified file is one line of several megabytes.
    let left: String = "abcdefghij".repeat(500_000);
    let mut right = left.clone();
    right.replace_range(2_000_000..2_000_010, "ZZZZZZZZZZ");
    report("5 MB single line, character inline diff", || {
        diff_chars(&left, &right)
    });
    let dissimilar_left: String = "a".repeat(5_000_000);
    let dissimilar_right: String = "b".repeat(5_000_000);
    report("5 MB single line, nothing in common", || {
        diff_chars(&dissimilar_left, &dissimilar_right)
    });
}

#[test]
#[ignore = "timing harness"]
fn shifted_binary_insertions() {
    let total = 20 << 20;
    let mut a = Vec::with_capacity(total);
    let mut seed = 0x9E37_79B9u32;
    for _ in 0..total {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        a.push(u8::try_from(seed >> 24).unwrap());
    }
    let mut b = Vec::with_capacity(total + 20_000 * 100);
    for (i, byte) in a.iter().enumerate() {
        if i % 1024 == 0 {
            b.extend(std::iter::repeat_n(0xEEu8, 100));
        }
        b.push(*byte);
    }
    for mode in [ByteAlignment::Complete, ByteAlignment::Fast] {
        let hunks = report(&format!("20 MB, 20k shifted insertions, {mode:?}"), || {
            diff_bytes(&a, &b, mode)
        });
        println!("  hunks: {}", hunks.len());
    }
}

#[test]
#[ignore = "timing harness"]
fn importance_over_a_large_comparison() {
    let left = numbered(200_000);
    let right = change_every(&left, 10);
    let (lr, rr) = (refs(&left), refs(&right));
    let hunks = report("200k lines, 10% changed, alignment", || {
        diff_line_slices(&lr, &rr, &LineCompareOptions::default())
    });
    let mut rules = RuleSet::all_important();
    rules.trailing_whitespace_important = false;
    rules
        .replacements
        .push(ReplacementRule::new("line", "row", ReplacementOptions::default()).unwrap());
    let classified = report("200k lines, importance classification", || {
        classify_hunks(&lr, &rr, &hunks, &rules, &WhitespaceClassifier).unwrap()
    });
    let changed = classified
        .iter()
        .filter(|h| h.hunk.kind != HunkKind::Same)
        .count();
    println!("  changed hunks: {changed}");
}
