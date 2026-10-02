//! Properties that have to hold for every grammar and every input: tokens tile
//! the line exactly, and an incremental re-lex equals a full one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use ca_grammar::builtin;
use ca_grammar::grammar::Grammar;
use ca_grammar::lexer::{Lexer, LineState, StateCache};
use proptest::prelude::*;

/// The stock grammars, plus an empty one, as the corpus a property runs over.
fn corpus() -> Vec<(String, Grammar)> {
    let mut out = vec![("empty".to_owned(), Grammar::empty())];
    for format in builtin::formats() {
        out.push((format.name.clone(), format.grammar));
    }
    out
}

/// Text made of the characters grammars actually key on, so a random line has a
/// real chance of exercising delimiters and escapes rather than only plain
/// words.
fn line_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("/*".to_owned()),
            Just("*/".to_owned()),
            Just("//".to_owned()),
            Just("\"".to_owned()),
            Just("'".to_owned()),
            Just("`".to_owned()),
            Just("\\".to_owned()),
            Just("#".to_owned()),
            Just("<!--".to_owned()),
            Just("-->".to_owned()),
            Just("```".to_owned()),
            Just("\"\"\"".to_owned()),
            Just("<#".to_owned()),
            Just("#>".to_owned()),
            Just(" ".to_owned()),
            Just("\t".to_owned()),
            Just("ω".to_owned()),
            Just("naïve".to_owned()),
            "[A-Za-z_]{1,6}",
            "[0-9]{1,4}",
            "[\\[\\]{}()<>=:;,.@$&|+*%!?-]{1,3}",
        ],
        0..24,
    )
    .prop_map(|parts| parts.concat())
}

fn assert_tiles(line: &str, lexer: &Lexer, state: LineState, index: usize) -> LineState {
    let Ok(out) = lexer.lex_line(line, index, state) else {
        // A pattern that runs out of budget is reported, not tokenized; the
        // tiling property has nothing to say about that case.
        return LineState::start();
    };
    let mut cursor = 0;
    for token in &out.tokens {
        assert_eq!(token.range.start, cursor, "gap or overlap in {line:?}");
        assert!(token.range.end > token.range.start, "empty token");
        assert!(
            line.is_char_boundary(token.range.start) && line.is_char_boundary(token.range.end),
            "token edge off a character boundary in {line:?}"
        );
        cursor = token.range.end;
    }
    assert_eq!(cursor, line.len(), "tokens stop short of {line:?}");
    out.state
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn tokens_tile_every_line_of_every_stock_grammar(
        lines in proptest::collection::vec(line_strategy(), 1..12)
    ) {
        for (name, grammar) in corpus() {
            let lexer = Lexer::new(&grammar).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut state = LineState::start();
            for (index, line) in lines.iter().enumerate() {
                state = assert_tiles(line, &lexer, state, index);
            }
        }
    }

    #[test]
    fn an_incremental_re_lex_equals_a_full_one_after_a_random_edit(
        original in proptest::collection::vec(line_strategy(), 1..24),
        replacement in line_strategy(),
        edit_at in 0_usize..24,
        remove in 0_usize..3,
        insert in 0_usize..3,
    ) {
        for (name, grammar) in corpus() {
            let lexer = Lexer::new(&grammar).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut lines = original.clone();
            let Ok(mut cache) = StateCache::build(&lexer, &lines) else { continue };

            let at = edit_at % lines.len();
            lines[at] = replacement.clone();
            let removed = remove.min(lines.len() - at - 1);
            for _ in 0..removed {
                lines.remove(at + 1);
            }
            for i in 0..insert {
                lines.insert(at + 1, format!("inserted {i} {replacement}"));
            }
            if removed > 0 || insert > 0 {
                cache.invalidate_from(at);
            }
            if cache.relex_from(&lexer, &lines, at).is_err() {
                continue;
            }
            let Ok(fresh) = StateCache::build(&lexer, &lines) else { continue };
            for i in 0..=lines.len() {
                prop_assert_eq!(
                    cache.state_at(i),
                    fresh.state_at(i),
                    "{} disagrees at line {}", name, i
                );
            }
        }
    }

    #[test]
    fn re_lexing_from_a_cached_state_matches_lexing_the_file_from_the_top(
        lines in proptest::collection::vec(line_strategy(), 1..16),
        start in 0_usize..16,
    ) {
        for (name, grammar) in corpus() {
            let lexer = Lexer::new(&grammar).unwrap_or_else(|e| panic!("{name}: {e}"));
            let Ok(cache) = StateCache::build(&lexer, &lines) else { continue };
            let at = start % lines.len();
            let Ok(from_cache) = lexer.lex_line(&lines[at], at, cache.state_at(at)) else {
                continue;
            };

            let mut state = LineState::start();
            let mut from_top = None;
            for (index, line) in lines.iter().enumerate() {
                let Ok(out) = lexer.lex_line(line, index, state) else { break };
                state = out.state;
                if index == at {
                    from_top = Some(out);
                    break;
                }
            }
            if let Some(from_top) = from_top {
                prop_assert_eq!(from_cache.tokens, from_top.tokens, "{}", name);
            }
        }
    }
}
