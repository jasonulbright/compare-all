//! The classifier the diff engine consumes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use ca_diff::importance::{
    classify_hunks_indexed, ClassifierSide, Importance, IndexedLineClassifier, LineClassifier,
    RuleSet, TokenCategory, WhitespaceClassifier,
};
use ca_diff::lines::{diff_line_slices, LineCompareOptions};
use ca_grammar::builtin;
use ca_grammar::classify::{GrammarClassifier, IndexedGrammarClassifier};
use ca_grammar::grammar::{Grammar, GrammarItem, ItemKind, MatchOptions};
use ca_grammar::lexer::LineState;

fn categories(classifier: &GrammarClassifier, line: &str) -> Vec<(String, TokenCategory)> {
    classifier
        .classify_line(line)
        .into_iter()
        .map(|t| {
            (
                line[t.range.start as usize..t.range.end as usize].to_owned(),
                t.category,
            )
        })
        .collect()
}

fn assert_tiles(classifier: &GrammarClassifier, line: &str) {
    let mut cursor = 0_u32;
    for token in classifier.classify_line(line) {
        assert_eq!(token.range.start, cursor, "gap or overlap in {line:?}");
        assert!(token.range.end > token.range.start);
        cursor = token.range.end;
    }
    assert_eq!(cursor as usize, line.len());
}

#[test]
fn whitespace_is_reported_by_where_it_sits_on_the_line() {
    let classifier = GrammarClassifier::new(&Grammar::empty()).unwrap();
    let line = "   a  b   ";
    assert_tiles(&classifier, line);
    assert_eq!(
        categories(&classifier, line),
        vec![
            ("   ".to_owned(), TokenCategory::LeadingWhitespace),
            ("a".to_owned(), TokenCategory::EverythingElse),
            ("  ".to_owned(), TokenCategory::EmbeddedWhitespace),
            ("b".to_owned(), TokenCategory::EverythingElse),
            ("   ".to_owned(), TokenCategory::TrailingWhitespace),
        ]
    );
}

#[test]
fn a_line_of_only_whitespace_is_trailing_whitespace() {
    let classifier = GrammarClassifier::new(&Grammar::empty()).unwrap();
    assert_eq!(
        categories(&classifier, "   "),
        vec![("   ".to_owned(), TokenCategory::TrailingWhitespace)]
    );
}

#[test]
fn a_claimed_run_reports_its_element_name() {
    let classifier = GrammarClassifier::new(&builtin::c_cpp().grammar).unwrap();
    let line = "  int x = 1; // note";
    assert_tiles(&classifier, line);
    let found = categories(&classifier, line);
    assert!(found.contains(&(
        "int".to_owned(),
        TokenCategory::Element("Keyword".to_owned())
    )));
    assert!(found.contains(&(
        "// note".to_owned(),
        TokenCategory::Element("Comment".to_owned())
    )));
    assert_eq!(found[0].1, TokenCategory::LeadingWhitespace);
}

#[test]
fn whitespace_inside_a_claimed_element_stays_part_of_that_element() {
    let classifier = GrammarClassifier::new(&builtin::c_cpp().grammar).unwrap();
    let line = "x = \"a  b\";";
    let found = categories(&classifier, line);
    assert!(found.contains(&(
        "\"a  b\"".to_owned(),
        TokenCategory::Element("String".to_owned())
    )));
}

#[test]
fn the_checklist_holds_the_grammar_element_names_in_order() {
    let classifier = GrammarClassifier::new(&builtin::c_cpp().grammar).unwrap();
    assert_eq!(
        classifier.element_names(),
        [
            "Comment",
            "Preprocessor",
            "String",
            "Keyword",
            "Number",
            "Identifier"
        ]
    );
}

#[test]
fn unchecking_an_element_makes_its_differences_unimportant() {
    let classifier = GrammarClassifier::new(&builtin::c_cpp().grammar).unwrap();
    let mut rules = RuleSet::all_important();
    rules.leading_whitespace_important = false;
    for name in classifier.element_names() {
        if name != "Comment" {
            rules.important_elements.insert(name.clone());
        }
    }
    rules.everything_else_important = true;

    let tokens = classifier.classify_line("int x = 1; // note");
    let comment = tokens
        .iter()
        .find(|t| t.category == TokenCategory::Element("Comment".to_owned()))
        .unwrap();
    assert!(!rules.important_elements.contains("Comment"));
    assert!(rules.important_elements.contains("Keyword"));
    assert_eq!(
        comment.category,
        TokenCategory::Element("Comment".to_owned())
    );
}

#[test]
fn classification_can_be_driven_line_by_line_with_a_carried_state() {
    let classifier = GrammarClassifier::new(&builtin::c_cpp().grammar).unwrap();
    let (first, state) = classifier
        .classify_line_from("a /* open", 0, LineState::start())
        .unwrap();
    assert!(state.open_item.is_some());
    assert!(first
        .iter()
        .any(|t| t.category == TokenCategory::Element("Comment".to_owned())));

    let (second, state) = classifier
        .classify_line_from("still inside */ b", 1, state)
        .unwrap();
    assert_eq!(state, LineState::start());
    assert_eq!(
        second[0].category,
        TokenCategory::Element("Comment".to_owned())
    );
}

#[test]
fn a_line_terminator_is_left_out_of_the_body_the_way_the_engine_leaves_it_out() {
    let classifier = GrammarClassifier::new(&Grammar::empty()).unwrap();
    for line in ["alpha\r\n", "alpha\n", "alpha\r"] {
        let ours = classifier.classify_line(line);
        let theirs = WhitespaceClassifier.classify_line(line);
        assert_eq!(ours, theirs, "{line:?}");
        assert!(
            !ours
                .iter()
                .any(|t| t.category == TokenCategory::TrailingWhitespace),
            "{line:?}"
        );
    }
}

#[test]
fn a_terminator_after_real_trailing_whitespace_still_agrees_with_the_engine() {
    let classifier = GrammarClassifier::new(&Grammar::empty()).unwrap();
    for line in ["alpha   \r\n", "  alpha  \r\n", "a\tb \n"] {
        assert_eq!(
            classifier.classify_line(line),
            WhitespaceClassifier.classify_line(line),
            "{line:?}"
        );
    }
}

#[test]
fn a_line_inside_a_block_comment_is_classified_under_comment() {
    let left = [
        "int a = 1;\n",
        "/* explanation\n",
        "   first note\n",
        "*/\n",
        "int b = 2;\n",
    ];
    let right = [
        "int a = 1;\n",
        "/* explanation\n",
        "   second note\n",
        "*/\n",
        "int c = 2;\n",
    ];
    let classifier =
        IndexedGrammarClassifier::new(&builtin::c_cpp().grammar, &left, &right).unwrap();

    let inside = classifier.classify_line_at(ClassifierSide::Left, 2, left[2]);
    assert!(
        inside
            .iter()
            .any(|t| t.category == TokenCategory::Element("Comment".to_owned())),
        "{inside:?}"
    );
    // The same line read without its carried state is not a comment at all.
    let alone = classifier.classifier().classify_line(left[2]);
    assert!(!alone
        .iter()
        .any(|t| t.category == TokenCategory::Element("Comment".to_owned())));

    // A change after the comment closes is ordinary code.
    let after = classifier.classify_line_at(ClassifierSide::Left, 4, left[4]);
    assert!(after
        .iter()
        .any(|t| t.category == TokenCategory::Element("Keyword".to_owned())));
    assert!(!after
        .iter()
        .any(|t| t.category == TokenCategory::Element("Comment".to_owned())));
}

#[test]
fn comment_body_changes_go_unimportant_while_code_changes_stay_important() {
    let left = [
        "int a = 1;\n",
        "/* explanation\n",
        "   first note\n",
        "*/\n",
        "int b = 2;\n",
    ];
    let right = [
        "int a = 1;\n",
        "/* explanation\n",
        "   second note\n",
        "*/\n",
        "int c = 2;\n",
    ];
    let classifier =
        IndexedGrammarClassifier::new(&builtin::c_cpp().grammar, &left, &right).unwrap();
    let mut rules = RuleSet::all_important();
    rules.orphan_lines_always_important = false;
    for name in classifier.element_names() {
        if name != "Comment" {
            rules.important_elements.insert(name.clone());
        }
    }

    let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
    let outcome = classify_hunks_indexed(&left, &right, &hunks, &rules, &classifier).unwrap();
    let verdict = |line: u32| {
        outcome
            .iter()
            .find(|h| h.hunk.left.start <= line && line < h.hunk.left.end && h.importance.is_some())
            .and_then(|h| h.importance)
    };
    assert_eq!(verdict(2), Some(Importance::Unimportant));
    assert_eq!(verdict(4), Some(Importance::Important));
}

#[test]
fn a_line_index_past_the_end_falls_back_to_the_state_a_file_starts_in() {
    let left = ["/* open\n"];
    let right = ["/* open\n"];
    let classifier =
        IndexedGrammarClassifier::new(&builtin::c_cpp().grammar, &left, &right).unwrap();
    let tokens = classifier.classify_line_at(ClassifierSide::Left, 99, "plain text");
    assert!(!tokens
        .iter()
        .any(|t| t.category == TokenCategory::Element("Comment".to_owned())));
}

#[test]
fn a_failing_pattern_still_yields_a_complete_tiling() {
    let grammar = Grammar::from_items(vec![GrammarItem::new(
        "Trap",
        ItemKind::Basic {
            text: r"(a+)+$\1".to_owned(),
            options: MatchOptions::regex(),
            whole_word: false,
        },
    )]);
    let classifier = GrammarClassifier::new(&grammar).unwrap();
    let line = "a".repeat(200);
    assert_tiles(&classifier, &line);
}

#[test]
fn every_stock_grammar_tiles_its_own_element_names() {
    for format in builtin::formats() {
        let classifier = GrammarClassifier::new(&format.grammar).unwrap();
        for line in ["  mixed 123 \"text\" // tail  ", "", "\t", "ω identifier ω"] {
            assert_tiles(&classifier, line);
        }
    }
}
