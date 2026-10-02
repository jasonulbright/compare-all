//! Tokenizer behavior: one test per element kind, plus precedence, case
//! sensitivity and the multi-line carry.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use ca_grammar::grammar::{ColumnEnd, Grammar, GrammarItem, ItemKind, MatchOptions};
use ca_grammar::lexer::{LexToken, Lexer, LineState, StateCache};

/// The element names of a line's tokens, with `None` for unclaimed text.
fn names(lexer: &Lexer, tokens: &[LexToken]) -> Vec<Option<String>> {
    tokens
        .iter()
        .map(|t| t.item.and_then(|i| lexer.element_of(i)).map(str::to_owned))
        .collect()
}

/// Token element names paired with the text each token covers.
fn spans(lexer: &Lexer, line: &str, tokens: &[LexToken]) -> Vec<(String, String)> {
    tokens
        .iter()
        .map(|t| {
            (
                t.item
                    .and_then(|i| lexer.element_of(i))
                    .unwrap_or("-")
                    .to_owned(),
                line[t.range.clone()].to_owned(),
            )
        })
        .collect()
}

fn lex(lexer: &Lexer, line: &str) -> Vec<LexToken> {
    lexer
        .lex_line(line, 0, LineState::start())
        .expect("lexing succeeds")
        .tokens
}

fn assert_tiles(line: &str, tokens: &[LexToken]) {
    let mut cursor = 0;
    for token in tokens {
        assert_eq!(token.range.start, cursor, "gap or overlap in {line:?}");
        assert!(token.range.end > token.range.start, "empty token");
        assert!(line.is_char_boundary(token.range.start));
        assert!(line.is_char_boundary(token.range.end));
        cursor = token.range.end;
    }
    assert_eq!(cursor, line.len(), "tokens stop short of the line");
}

#[test]
fn a_basic_item_claims_its_string() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "Mark",
        ItemKind::Basic {
            text: "end".to_owned(),
            options: MatchOptions::literal(),
            whole_word: true,
        },
    )]))
    .unwrap();
    let line = "the end here";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(
        spans(&lexer, line, &tokens),
        vec![
            ("-".to_owned(), "the ".to_owned()),
            ("Mark".to_owned(), "end".to_owned()),
            ("-".to_owned(), " here".to_owned()),
        ]
    );
}

#[test]
fn a_basic_item_respects_word_boundaries() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "Mark",
        ItemKind::Basic {
            text: "end".to_owned(),
            options: MatchOptions::literal(),
            whole_word: true,
        },
    )]))
    .unwrap();
    let tokens = lex(&lexer, "bend ending");
    assert_eq!(names(&lexer, &tokens), vec![None]);
}

#[test]
fn a_list_item_claims_any_token_and_prefers_the_longest() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "Keyword",
        ItemKind::List {
            tokens: vec!["else".to_owned(), "elseif".to_owned(), "if".to_owned()],
            options: MatchOptions::literal(),
            whole_word: true,
        },
    )]))
    .unwrap();
    let line = "elseif if else";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(
        spans(&lexer, line, &tokens),
        vec![
            ("Keyword".to_owned(), "elseif".to_owned()),
            ("-".to_owned(), " ".to_owned()),
            ("Keyword".to_owned(), "if".to_owned()),
            ("-".to_owned(), " ".to_owned()),
            ("Keyword".to_owned(), "else".to_owned()),
        ]
    );
}

#[test]
fn a_delimited_item_honors_its_escape_character() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "String",
        ItemKind::Delimited {
            start: "'".to_owned(),
            stop: "'".to_owned(),
            stop_at_end_of_line: false,
            escape: Some('\\'),
            line_spanning: false,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )]))
    .unwrap();
    let line = r"s = 'I can\'t see straight' x";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(
        spans(&lexer, line, &tokens)[1],
        ("String".to_owned(), r"'I can\'t see straight'".to_owned())
    );
}

#[test]
fn a_delimited_item_can_stop_at_the_end_of_the_line() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "Comment",
        ItemKind::Delimited {
            start: "//".to_owned(),
            stop: String::new(),
            stop_at_end_of_line: true,
            escape: None,
            line_spanning: false,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )]))
    .unwrap();
    let line = "code // trailing note";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(
        spans(&lexer, line, &tokens)[1],
        ("Comment".to_owned(), "// trailing note".to_owned())
    );
}

#[test]
fn a_regular_expression_item_matches_only_at_a_position_it_starts_at() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "Number",
        ItemKind::Basic {
            text: r"\d+".to_owned(),
            options: MatchOptions::regex(),
            whole_word: true,
        },
    )]))
    .unwrap();
    let line = "a 12 b 345";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(
        spans(&lexer, line, &tokens)
            .into_iter()
            .filter(|(n, _)| n == "Number")
            .map(|(_, t)| t)
            .collect::<Vec<_>>(),
        vec!["12".to_owned(), "345".to_owned()]
    );
}

#[test]
fn a_column_item_claims_a_fixed_range() {
    let lexer = Lexer::new(&Grammar::from_items(vec![
        GrammarItem::new(
            "Sequence",
            ItemKind::Columns {
                start_column: 1,
                end: ColumnEnd::Column(6),
            },
        ),
        GrammarItem::new(
            "Body",
            ItemKind::Columns {
                start_column: 7,
                end: ColumnEnd::EndOfLine,
            },
        ),
    ]))
    .unwrap();
    let line = "000100 PROCEDURE DIVISION.";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(
        spans(&lexer, line, &tokens),
        vec![
            ("Sequence".to_owned(), "000100".to_owned()),
            ("Body".to_owned(), " PROCEDURE DIVISION.".to_owned()),
        ]
    );
}

#[test]
fn a_column_item_stops_at_the_end_of_a_short_line() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "Sequence",
        ItemKind::Columns {
            start_column: 1,
            end: ColumnEnd::Column(80),
        },
    )]))
    .unwrap();
    let line = "short";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(tokens.len(), 1);
}

#[test]
fn a_block_item_claims_a_run_of_whole_lines() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "Page heading",
        ItemKind::Lines {
            text: "\u{c}".to_owned(),
            or_line_1: true,
            line_count: 3,
            options: MatchOptions::literal(),
        },
    )]))
    .unwrap();
    let lines = [
        "title",
        "dated",
        "-----",
        "body a",
        "body b",
        "\u{c}title",
        "dated",
        "-----",
        "body c",
    ];
    let cache = StateCache::build(&lexer, &lines).unwrap();
    let claimed: Vec<bool> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            lexer
                .lex_line(line, i, cache.state_at(i))
                .unwrap()
                .tokens
                .iter()
                .all(|t| t.item.is_some())
        })
        .collect();
    assert_eq!(
        claimed,
        vec![true, true, true, false, false, true, true, true, false]
    );
}

#[test]
fn an_earlier_item_takes_precedence_over_a_later_one() {
    let comment_first = Grammar::from_items(vec![
        GrammarItem::new(
            "Comment",
            ItemKind::Delimited {
                start: "#".to_owned(),
                stop: String::new(),
                stop_at_end_of_line: true,
                escape: None,
                line_spanning: false,
                continue_after_escaped_newline: false,
                options: MatchOptions::literal(),
            },
        ),
        GrammarItem::new(
            "Directive",
            ItemKind::Basic {
                text: "#define".to_owned(),
                options: MatchOptions::literal(),
                whole_word: false,
            },
        ),
    ]);
    let mut directive_first = comment_first.clone();
    directive_first.items.swap(0, 1);

    let line = "#define X 1";
    let a = Lexer::new(&comment_first).unwrap();
    assert_eq!(
        spans(&a, line, &lex(&a, line))[0].0,
        "Comment",
        "the earlier item has to win"
    );
    let b = Lexer::new(&directive_first).unwrap();
    assert_eq!(spans(&b, line, &lex(&b, line))[0].0, "Directive");
}

#[test]
fn match_character_case_controls_whether_a_token_matches() {
    let sensitive = Grammar::from_items(vec![GrammarItem::new(
        "Keyword",
        ItemKind::List {
            tokens: vec!["SELECT".to_owned()],
            options: MatchOptions::literal(),
            whole_word: true,
        },
    )]);
    let mut insensitive = sensitive.clone();
    insensitive.items[0].kind = ca_grammar::Extensible::Known(ItemKind::List {
        tokens: vec!["SELECT".to_owned()],
        options: MatchOptions::literal_any_case(),
        whole_word: true,
    });

    let line = "select 1";
    let a = Lexer::new(&sensitive).unwrap();
    assert_eq!(names(&a, &lex(&a, line)), vec![None]);
    let b = Lexer::new(&insensitive).unwrap();
    assert_eq!(spans(&b, line, &lex(&b, line))[0].0, "Keyword");
}

#[test]
fn element_case_sensitivity_is_recorded_separately_from_matching() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Keyword",
        ItemKind::List {
            tokens: vec!["select".to_owned()],
            options: MatchOptions::literal_any_case(),
            whole_word: true,
        },
    )
    .case_sensitive()]);
    assert!(grammar.is_case_sensitive("Keyword"));
    let lexer = Lexer::new(&grammar).unwrap();
    assert_eq!(
        spans(&lexer, "SELECT", &lex(&lexer, "SELECT"))[0].0,
        "Keyword"
    );
}

fn block_comment_grammar() -> Grammar {
    Grammar::from_items(vec![
        GrammarItem::new(
            "Comment",
            ItemKind::Delimited {
                start: "/*".to_owned(),
                stop: "*/".to_owned(),
                stop_at_end_of_line: false,
                escape: None,
                line_spanning: true,
                continue_after_escaped_newline: false,
                options: MatchOptions::literal(),
            },
        ),
        GrammarItem::new(
            "Identifier",
            ItemKind::Basic {
                text: r"[A-Za-z_]\w*".to_owned(),
                options: MatchOptions::regex(),
                whole_word: true,
            },
        ),
    ])
}

#[test]
fn an_unterminated_delimited_element_carries_into_the_next_line() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let lines = ["a /* start", "middle", "end */ b"];
    let cache = StateCache::build(&lexer, &lines).unwrap();
    assert_eq!(cache.state_at(0), LineState::start());
    assert!(cache.state_at(1).open_item.is_some());
    assert!(cache.state_at(2).open_item.is_some());
    assert_eq!(cache.state_at(3), LineState::start());

    let last = lexer.lex_line(lines[2], 2, cache.state_at(2)).unwrap();
    assert_eq!(
        spans(&lexer, lines[2], &last.tokens)[0],
        ("Comment".to_owned(), "end */".to_owned())
    );
}

#[test]
fn an_element_that_does_not_span_lines_ends_at_the_line_end() {
    let lexer = Lexer::new(&Grammar::from_items(vec![GrammarItem::new(
        "String",
        ItemKind::Delimited {
            start: "\"".to_owned(),
            stop: "\"".to_owned(),
            stop_at_end_of_line: false,
            escape: Some('\\'),
            line_spanning: false,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )]))
    .unwrap();
    let out = lexer
        .lex_line("x = \"unclosed", 0, LineState::start())
        .unwrap();
    assert_eq!(out.state, LineState::start());
    assert_eq!(out.tokens.last().unwrap().range.end, "x = \"unclosed".len());
}

#[test]
fn re_lexing_after_an_edit_stops_once_the_carried_state_converges() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let mut lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
    lines[10] = "/* open".to_owned();
    lines[12] = "close */".to_owned();
    let mut cache = StateCache::build(&lexer, &lines).unwrap();
    assert!(cache.state_at(11).open_item.is_some());

    // An edit well inside the closed comment cannot change anything past the
    // line after it, so the re-lex stops there.
    lines[11] = "still inside".to_owned();
    let stopped = cache.relex_from(&lexer, &lines, 11).unwrap();
    assert_eq!(stopped, 12);

    // Removing the closing delimiter leaves the element open, so the re-lex has
    // to run to the end of the file.
    lines[12] = "no longer closing".to_owned();
    let stopped = cache.relex_from(&lexer, &lines, 12).unwrap();
    assert_eq!(stopped, lines.len());
    assert!(cache.state_at(199).open_item.is_some());
}

#[test]
fn an_incremental_re_lex_agrees_with_a_full_one() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let mut lines: Vec<String> = vec![
        "a /* one".to_owned(),
        "two".to_owned(),
        "three */ b".to_owned(),
        "plain".to_owned(),
        "/* again".to_owned(),
        "still".to_owned(),
    ];
    let mut cache = StateCache::build(&lexer, &lines).unwrap();
    lines[2] = "three b".to_owned();
    cache.relex_from(&lexer, &lines, 2).unwrap();
    let fresh = StateCache::build(&lexer, &lines).unwrap();
    for i in 0..=lines.len() {
        assert_eq!(cache.state_at(i), fresh.state_at(i), "line {i}");
    }
}

#[test]
fn inserting_a_line_after_invalidation_still_agrees_with_a_full_re_lex() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let mut lines: Vec<String> = vec![
        "plain".to_owned(),
        "/* open".to_owned(),
        "body".to_owned(),
        "*/ done".to_owned(),
        "tail".to_owned(),
    ];
    let mut cache = StateCache::build(&lexer, &lines).unwrap();
    lines.insert(2, "/* another".to_owned());
    cache.invalidate_from(2);
    cache.relex_from(&lexer, &lines, 2).unwrap();
    let fresh = StateCache::build(&lexer, &lines).unwrap();
    for i in 0..=lines.len() {
        assert_eq!(cache.state_at(i), fresh.state_at(i), "line {i}");
    }
}

#[test]
fn splicing_a_one_line_insert_keeps_the_cache_converging_just_under_the_edit() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let mut lines: Vec<String> = (0..1_000_000).map(|i| format!("line {i}")).collect();
    let mut cache = StateCache::build(&lexer, &lines).unwrap();
    lines.insert(10, "fresh".to_owned());
    cache.splice_lines(10, 0, 1);
    let stopped = cache.relex_from(&lexer, &lines, 10).unwrap();
    assert!(
        stopped <= 16,
        "a one line insert re-lexed to line {stopped} of {}",
        lines.len()
    );
    let fresh = StateCache::build(&lexer, &lines).unwrap();
    for i in [0, 9, 10, 11, 12, 500_000, lines.len()] {
        assert_eq!(cache.state_at(i), fresh.state_at(i), "line {i}");
    }
}

#[test]
fn splicing_a_delete_keeps_the_cache_converging_just_under_the_edit() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let mut lines: Vec<String> = (0..2_000).map(|i| format!("line {i}")).collect();
    let mut cache = StateCache::build(&lexer, &lines).unwrap();
    lines.drain(10..13);
    cache.splice_lines(10, 3, 0);
    let stopped = cache.relex_from(&lexer, &lines, 10).unwrap();
    assert!(stopped <= 16, "a delete re-lexed to line {stopped}");
    let fresh = StateCache::build(&lexer, &lines).unwrap();
    for i in 0..=lines.len() {
        assert_eq!(cache.state_at(i), fresh.state_at(i), "line {i}");
    }
}

#[test]
fn a_splice_that_changes_a_carried_state_still_agrees_with_a_full_re_lex() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let mut lines: Vec<String> = vec![
        "plain".to_owned(),
        "/* open".to_owned(),
        "body".to_owned(),
        "*/ done".to_owned(),
        "tail".to_owned(),
    ];
    let mut cache = StateCache::build(&lexer, &lines).unwrap();
    lines.insert(2, "/* another".to_owned());
    cache.splice_lines(2, 0, 1);
    cache.relex_from(&lexer, &lines, 2).unwrap();
    let fresh = StateCache::build(&lexer, &lines).unwrap();
    for i in 0..=lines.len() {
        assert_eq!(cache.state_at(i), fresh.state_at(i), "line {i}");
    }
}

#[test]
fn a_column_item_counts_display_columns_so_a_tab_does_not_shift_the_range() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Field",
        ItemKind::Columns {
            start_column: 9,
            end: ColumnEnd::Column(12),
        },
    )]);
    let lexer = Lexer::with_tab_stop(&grammar, 8).unwrap();
    // The tab fills columns 1 to 8, so the field starts right after it.
    let line = "\tabcdefgh";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(
        spans(&lexer, line, &tokens),
        vec![
            ("-".to_owned(), "\t".to_owned()),
            ("Field".to_owned(), "abcd".to_owned()),
            ("-".to_owned(), "efgh".to_owned()),
        ]
    );
}

#[test]
fn the_tab_stop_decides_where_a_column_item_starts() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Field",
        ItemKind::Columns {
            start_column: 5,
            end: ColumnEnd::EndOfLine,
        },
    )]);
    let line = "\tabcd";
    let lexer = Lexer::with_tab_stop(&grammar, 4).unwrap();
    assert_eq!(
        spans(&lexer, line, &lex(&lexer, line)),
        vec![
            ("-".to_owned(), "\t".to_owned()),
            ("Field".to_owned(), "abcd".to_owned()),
        ]
    );
    let lexer = Lexer::with_tab_stop(&grammar, 8).unwrap();
    assert_eq!(
        spans(&lexer, line, &lex(&lexer, line)),
        vec![("-".to_owned(), "\tabcd".to_owned())]
    );
}

#[test]
fn a_list_of_patterns_takes_the_longest_alternative() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Token",
        ItemKind::List {
            tokens: vec![r"[a-z]".to_owned(), r"[a-z]+".to_owned()],
            options: MatchOptions::regex(),
            whole_word: false,
        },
    )]);
    let lexer = Lexer::new(&grammar).unwrap();
    assert_eq!(
        spans(&lexer, "alpha", &lex(&lexer, "alpha")),
        vec![("Token".to_owned(), "alpha".to_owned())]
    );
}

#[test]
fn a_list_mixing_patterns_and_literals_still_takes_the_longest_alternative() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Token",
        ItemKind::List {
            tokens: vec![r"[a-z]+".to_owned(), r"[a-z]+ [a-z]+".to_owned()],
            options: MatchOptions::regex(),
            whole_word: false,
        },
    )]);
    let lexer = Lexer::new(&grammar).unwrap();
    assert_eq!(
        spans(&lexer, "one two", &lex(&lexer, "one two"))[0],
        ("Token".to_owned(), "one two".to_owned())
    );
}

#[test]
fn a_case_insensitive_literal_matches_a_character_that_folds_onto_its_first() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Keyword",
        ItemKind::List {
            tokens: vec!["key".to_owned(), "set".to_owned()],
            options: MatchOptions::literal_any_case(),
            whole_word: false,
        },
    )]);
    let lexer = Lexer::new(&grammar).unwrap();
    // U+212A KELVIN SIGN lower-cases onto ASCII, so the first-byte filter has
    // to admit its lead byte.
    let line = "\u{212a}ey";
    assert_eq!(spans(&lexer, line, &lex(&lexer, line))[0].0, "Keyword");
    // A character whose own lowercase is itself is not a fold of the literal,
    // and neither path accepts it.
    let line = "\u{17f}et";
    assert_eq!(spans(&lexer, line, &lex(&lexer, line))[0].0, "-");
}

#[test]
fn the_first_byte_filter_agrees_with_the_matcher_whatever_else_the_list_holds() {
    // A second alternative changes nothing about which positions the first one
    // can match at.
    let one = Grammar::from_items(vec![GrammarItem::new(
        "Keyword",
        ItemKind::List {
            tokens: vec!["key".to_owned()],
            options: MatchOptions::literal_any_case(),
            whole_word: false,
        },
    )]);
    let two = Grammar::from_items(vec![GrammarItem::new(
        "Keyword",
        ItemKind::List {
            tokens: vec!["key".to_owned(), "\u{e9}tude".to_owned()],
            options: MatchOptions::literal_any_case(),
            whole_word: false,
        },
    )]);
    let line = "\u{212a}ey";
    let first = Lexer::new(&one).unwrap();
    let second = Lexer::new(&two).unwrap();
    assert_eq!(
        spans(&first, line, &lex(&first, line))[0].0,
        spans(&second, line, &lex(&second, line))[0].0
    );
}

#[test]
fn multi_byte_characters_keep_token_edges_on_boundaries() {
    let lexer = Lexer::new(&block_comment_grammar()).unwrap();
    let line = "naïve /* héllo */ ω";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
}

#[test]
fn an_item_whose_kind_is_unknown_claims_nothing_and_does_not_fail_the_grammar() {
    let grammar: Grammar =
        serde_json::from_str(r#"{"items":[{"element":"Future","kind":{"category":"tomorrow"}}]}"#)
            .unwrap();
    let lexer = Lexer::new(&grammar).unwrap();
    let line = "text";
    let tokens = lex(&lexer, line);
    assert_tiles(line, &tokens);
    assert_eq!(names(&lexer, &tokens), vec![None]);
}

#[test]
fn a_contradictory_item_is_reported_rather_than_ignored() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Broken",
        ItemKind::Delimited {
            start: "<".to_owned(),
            stop: String::new(),
            stop_at_end_of_line: false,
            escape: None,
            line_spanning: true,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )]);
    assert!(Lexer::new(&grammar).is_err());
}

#[test]
fn a_matching_failure_is_surfaced_rather_than_read_as_no_match() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Trap",
        ItemKind::Basic {
            text: r"(a+)+$\1".to_owned(),
            options: MatchOptions::regex(),
            whole_word: false,
        },
    )]);
    let lexer = Lexer::new(&grammar).unwrap();
    let line = "a".repeat(200);
    match lexer.lex_line(&line, 0, LineState::start()) {
        Err(ca_grammar::GrammarError::Backtrack { .. }) => {}
        Ok(out) => assert_tiles(&line, &out.tokens),
        Err(other) => panic!("unexpected error: {other}"),
    }
}
