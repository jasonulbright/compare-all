//! Script text that is meant to break the parser.
//!
//! Each case has to come back as an error. None may panic, and none may make
//! the parser hold more than the limits allow.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_script::lex::{MAX_COMMANDS, MAX_CONTINUATION_LINES, MAX_LOGICAL_LINE_CHARS};
use ca_script::parse;

#[test]
fn a_nul_byte_is_an_error() {
    let error = parse("beep\nex\0pand all\n").expect_err("an error");
    assert_eq!(error.line, 2);
    assert!(error.message.contains("NUL"), "{}", error.message);
}

#[test]
fn a_very_long_line_is_an_error() {
    let source = format!("expand {}", "a".repeat(MAX_LOGICAL_LINE_CHARS + 10));
    let error = parse(&source).expect_err("an error");
    assert!(error.message.contains("too long"), "{}", error.message);
}

#[test]
fn a_long_continuation_chain_is_an_error() {
    let mut source = String::from("expand");
    for _ in 0..=MAX_CONTINUATION_LINES {
        source.push_str(" a &\n");
    }
    source.push_str(" b\n");
    let error = parse(&source).expect_err("an error");
    assert!(error.message.contains("continued"), "{}", error.message);
}

#[test]
fn too_many_commands_is_an_error() {
    let source = "beep\n".repeat(MAX_COMMANDS + 2);
    let error = parse(&source).expect_err("an error");
    assert!(error.message.contains("too many"), "{}", error.message);
}

#[test]
fn a_script_at_the_command_limit_still_parses() {
    let source = "beep\n".repeat(1_000);
    let script = parse(&source).expect("parses");
    assert_eq!(script.statements.len(), 1_000);
}

#[test]
fn deep_quoting_is_an_error_or_one_argument() {
    let source = format!("expand {}", "\"".repeat(1_001));
    let error = parse(&source).expect_err("an error");
    assert!(error.message.contains("not closed"), "{}", error.message);
    let balanced = format!("expand {}a{}", "\"".repeat(500), "\"".repeat(500));
    let script = parse(&balanced).expect("parses");
    assert_eq!(script.statements.len(), 1);
}

#[test]
fn an_unterminated_quote_at_the_end_of_the_text_is_an_error() {
    assert!(parse("expand \"open").is_err());
}

#[test]
fn a_line_of_only_ampersands_does_not_loop() {
    let source = "&\n".repeat(10);
    let script = parse(&source).expect("empty continuation lines form no command");
    assert!(script.statements.is_empty());
}

#[test]
fn a_command_word_of_control_characters_is_an_error() {
    assert!(parse("\u{7}\u{8}\u{1b}").is_err());
}

#[test]
fn deeply_nested_percent_signs_do_not_loop() {
    let source = format!("expand {}", "%".repeat(2_000));
    assert!(parse(&source).is_ok());
}
