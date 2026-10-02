//! Time-bounded runs at the sizes a real comparison has to survive.
//!
//! The bounds are deliberately loose. They are here to catch a change that
//! turns the scan quadratic or lets a pattern backtrack without limit, not to
//! measure throughput. A bound describes an optimized build, so every case
//! that asserts one runs there alone; the correctness they also carry is kept
//! in small always-on cases beside them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use ca_grammar::builtin;
use ca_grammar::grammar::{Grammar, GrammarItem, ItemKind, MatchOptions};
use ca_grammar::lexer::{LexScratch, Lexer, LineState, StateCache};
use std::time::{Duration, Instant};

/// The bound an optimized build has to stay inside. The margin covers a
/// loaded machine.
const BUDGET: Duration = Duration::from_secs(60);

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_five_megabyte_single_line_lexes_within_the_budget() {
    let lexer = Lexer::new(&builtin::c_cpp().grammar).unwrap();
    let unit = "int value_0 = 0x2A; /* note */ \"text\\\" more\" ";
    let line = unit.repeat(5 * 1024 * 1024 / unit.len() + 1);
    assert!(line.len() >= 5 * 1024 * 1024);

    let started = Instant::now();
    let out = lexer.lex_line(&line, 0, LineState::start()).unwrap();
    let elapsed = started.elapsed();

    let mut cursor = 0;
    for token in &out.tokens {
        assert_eq!(token.range.start, cursor);
        cursor = token.range.end;
    }
    assert_eq!(cursor, line.len());
    assert!(
        elapsed < BUDGET,
        "one 5 MB line took {elapsed:?}, over the {BUDGET:?} budget"
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_five_megabyte_line_of_one_unclaimed_run_lexes_within_the_budget() {
    // The worst shape for the position scan: no item ever matches, so every
    // position is tried and rejected.
    let lexer = Lexer::new(&builtin::c_cpp().grammar).unwrap();
    let line = "+".repeat(5 * 1024 * 1024);
    let started = Instant::now();
    let out = lexer.lex_line(&line, 0, LineState::start()).unwrap();
    assert_eq!(out.tokens.len(), 1);
    let elapsed = started.elapsed();
    assert!(elapsed < BUDGET, "one unclaimed 5 MB line took {elapsed:?}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_million_line_file_lexes_within_the_budget() {
    let lexer = Lexer::new(&builtin::c_cpp().grammar).unwrap();
    let pattern = ["int a = 1;", "// note", "x();", "/* open", "*/ done"];
    let lines: Vec<&str> = (0..1_000_000).map(|i| pattern[i % pattern.len()]).collect();

    let started = Instant::now();
    let mut scratch = LexScratch::new();
    let mut tokens = Vec::new();
    let mut state = LineState::start();
    let mut counted = 0_u64;
    for (index, line) in lines.iter().enumerate() {
        state = lexer
            .lex_line_into(&mut scratch, line, index, state, &mut tokens)
            .unwrap();
        counted += tokens.len() as u64;
    }
    let elapsed = started.elapsed();
    assert!(counted > 1_000_000);
    assert!(
        elapsed < BUDGET,
        "one million lines took {elapsed:?}, over the {BUDGET:?} budget"
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn an_edit_late_in_a_large_file_re_lexes_only_a_short_span() {
    let lexer = Lexer::new(&builtin::c_cpp().grammar).unwrap();
    let mut lines: Vec<String> = (0..200_000).map(|i| format!("int v{i} = {i};")).collect();
    let mut cache = StateCache::build(&lexer, &lines).unwrap();

    lines[100_000] = "int changed = 7;".to_owned();
    let started = Instant::now();
    let stopped = cache.relex_from(&lexer, &lines, 100_000).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(stopped, 100_001, "convergence has to be immediate here");
    assert!(elapsed < Duration::from_secs(1), "re-lex took {elapsed:?}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_pathological_user_pattern_cannot_run_unbounded() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Trap",
        ItemKind::Basic {
            text: r"(x+x+)+y\1".to_owned(),
            options: MatchOptions::regex(),
            whole_word: false,
        },
    )]);
    let lexer = Lexer::new(&grammar).unwrap();
    let line = "x".repeat(4096);
    let started = Instant::now();
    let _ = lexer.lex_line(&line, 0, LineState::start());
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(30),
        "a bounded pattern took {elapsed:?}"
    );
}

/// Correctness half of the single line cases, sized for a debug build: the
/// tokens of a long line cover it end to end and leave no gap.
#[test]
fn a_long_single_line_lexes_into_contiguous_tokens() {
    let lexer = Lexer::new(&builtin::c_cpp().grammar).unwrap();
    let unit = "int value_0 = 0x2A; /* note */ \"text\\\" more\" ";
    let line = unit.repeat(64 * 1024 / unit.len() + 1);
    let out = lexer.lex_line(&line, 0, LineState::start()).unwrap();
    let mut cursor = 0;
    for token in &out.tokens {
        assert_eq!(token.range.start, cursor);
        cursor = token.range.end;
    }
    assert_eq!(cursor, line.len());
}

/// Correctness half of the unclaimed run case: a run no item matches is one
/// token, whatever its length.
#[test]
fn a_line_of_one_unclaimed_run_lexes_into_one_token() {
    let lexer = Lexer::new(&builtin::c_cpp().grammar).unwrap();
    let line = "+".repeat(64 * 1024);
    let out = lexer.lex_line(&line, 0, LineState::start()).unwrap();
    assert_eq!(out.tokens.len(), 1);
}

/// Correctness half of the re-lex case: an edit that changes no carried state
/// stops at the line after the edit.
#[test]
fn an_edit_re_lexes_only_the_line_it_touches() {
    let lexer = Lexer::new(&builtin::c_cpp().grammar).unwrap();
    let mut lines: Vec<String> = (0..2_000).map(|i| format!("int v{i} = {i};")).collect();
    let mut cache = StateCache::build(&lexer, &lines).unwrap();
    lines[1_000] = "int changed = 7;".to_owned();
    let stopped = cache.relex_from(&lexer, &lines, 1_000).unwrap();
    assert_eq!(stopped, 1_001, "convergence has to be immediate here");
}
