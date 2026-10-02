//! Every stock grammar lexes a realistic sample, with the element checked at
//! chosen offsets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use ca_diff::{classify_hunks, diff_line_slices, Importance, LineCompareOptions, RuleSet};
use ca_grammar::builtin;
use ca_grammar::classify::GrammarClassifier;
use ca_grammar::format::FileFormat;
use ca_grammar::highlight::{StyleMap, StyleSlot};
use ca_grammar::lexer::{Lexer, StateCache};

/// The element covering the byte at `offset` of line `line`, or `"-"`.
fn element_at(lexer: &Lexer, lines: &[&str], line: usize, offset: usize) -> String {
    let cache = StateCache::build(lexer, lines).unwrap();
    let out = lexer
        .lex_line(lines[line], line, cache.state_at(line))
        .unwrap();
    for token in &out.tokens {
        if token.range.contains(&offset) {
            return token
                .item
                .and_then(|i| lexer.element_of(i))
                .unwrap_or("-")
                .to_owned();
        }
    }
    "-".to_owned()
}

/// Assert the element at each `(line, offset)` of a sample, and that every line
/// is tiled completely.
fn check(format: &FileFormat, lines: &[&str], expectations: &[(usize, usize, &str)]) {
    let lexer = Lexer::new(&format.grammar)
        .unwrap_or_else(|e| panic!("{} grammar does not compile: {e}", format.name));
    let cache = StateCache::build(&lexer, lines).unwrap();
    for (index, line) in lines.iter().enumerate() {
        let out = lexer.lex_line(line, index, cache.state_at(index)).unwrap();
        let mut cursor = 0;
        for token in &out.tokens {
            assert_eq!(
                token.range.start, cursor,
                "{} line {index} is not tiled",
                format.name
            );
            cursor = token.range.end;
        }
        assert_eq!(cursor, line.len(), "{} line {index}", format.name);
    }
    for (line, offset, want) in expectations {
        assert_eq!(
            element_at(&lexer, lines, *line, *offset),
            (*want).to_owned(),
            "{} line {line} offset {offset}: {:?}",
            format.name,
            lines[*line]
        );
    }
}

/// The byte offset of the first occurrence of `needle` in `line`.
fn at(line: &str, needle: &str) -> usize {
    line.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is not in {line:?}"))
}

#[test]
fn c_and_cpp() {
    let lines = [
        "#include <stdio.h>",
        "/* a block",
        "   comment */",
        "int main(void) { /* inline */",
        "    const char *s = \"he said \\\"hi\\\"\";",
        "    return 0x1F; // done",
        "}",
    ];
    check(
        &builtin::c_cpp(),
        &lines,
        &[
            (0, 0, "Preprocessor"),
            (1, 3, "Comment"),
            (2, 5, "Comment"),
            (3, at(lines[3], "int"), "Keyword"),
            (3, at(lines[3], "main"), "Identifier"),
            (3, at(lines[3], "/* inline */") + 3, "Comment"),
            (4, at(lines[4], "\"he"), "String"),
            (4, at(lines[4], "hi"), "String"),
            (5, at(lines[5], "0x1F"), "Number"),
            (5, at(lines[5], "// done"), "Comment"),
        ],
    );
}

#[test]
fn csharp() {
    let lines = [
        "using System;",
        "public class Greeter {",
        "    public string Name { get; set; } = \"world\";",
        "    // greet",
        "}",
    ];
    check(
        &builtin::csharp(),
        &lines,
        &[
            (0, 0, "Keyword"),
            (0, at(lines[0], "System"), "Identifier"),
            (1, at(lines[1], "class"), "Keyword"),
            (2, at(lines[2], "\"world\""), "String"),
            (3, at(lines[3], "//"), "Comment"),
        ],
    );
}

#[test]
fn java() {
    let lines = [
        "package demo;",
        "public final class Main {",
        "    static final int LIMIT = 42;",
        "    /* note */",
        "}",
    ];
    check(
        &builtin::java(),
        &lines,
        &[
            (0, 0, "Keyword"),
            (1, at(lines[1], "final"), "Keyword"),
            (2, at(lines[2], "42"), "Number"),
            (2, at(lines[2], "LIMIT"), "Identifier"),
            (3, at(lines[3], "/*"), "Comment"),
        ],
    );
}

#[test]
fn javascript() {
    let lines = [
        "const greet = (name) => {",
        "  // say hello",
        "  return `hi ${name}`;",
        "};",
    ];
    check(
        &builtin::javascript(),
        &lines,
        &[
            (0, 0, "Keyword"),
            (0, at(lines[0], "greet"), "Identifier"),
            (1, at(lines[1], "//"), "Comment"),
            (2, at(lines[2], "`hi"), "String"),
        ],
    );
}

#[test]
fn typescript() {
    let lines = [
        "interface Point { x: number; y: number }",
        "export function norm(p: Point): number {",
        "  return Math.sqrt(p.x ** 2 + p.y ** 2); // hypot",
        "}",
    ];
    check(
        &builtin::typescript(),
        &lines,
        &[
            (0, 0, "Keyword"),
            (0, at(lines[0], "number"), "Keyword"),
            (1, at(lines[1], "export"), "Keyword"),
            (2, at(lines[2], "// hypot"), "Comment"),
            (2, at(lines[2], "Math"), "Identifier"),
        ],
    );
}

#[test]
fn python() {
    let lines = [
        "@decorator",
        "def f(x):",
        "    \"\"\"A docstring",
        "    spanning lines.\"\"\"",
        "    return x + 1  # add",
    ];
    check(
        &builtin::python(),
        &lines,
        &[
            (0, 0, "Preprocessor"),
            (1, 0, "Keyword"),
            (1, at(lines[1], "f("), "Identifier"),
            (2, at(lines[2], "A doc"), "String"),
            (3, 6, "String"),
            (4, at(lines[4], "# add"), "Comment"),
            (4, at(lines[4], "1"), "Number"),
        ],
    );
}

#[test]
fn rust() {
    let lines = [
        "#[derive(Debug)]",
        "pub struct Point { x: i32 }",
        "fn main() {",
        "    let s = r#\"raw \" string\"#;",
        "    let n = 0xFF_u8; // hex",
        "}",
    ];
    check(
        &builtin::rust(),
        &lines,
        &[
            (0, 0, "Preprocessor"),
            (1, 0, "Keyword"),
            (1, at(lines[1], "Point"), "Identifier"),
            (3, at(lines[3], "raw"), "String"),
            (4, at(lines[4], "0xFF"), "Number"),
            (4, at(lines[4], "// hex"), "Comment"),
        ],
    );
}

#[test]
fn html() {
    let lines = [
        "<!DOCTYPE html>",
        "<!-- a note",
        "     continued -->",
        "<a href=\"/x\" class='y'>text</a>",
    ];
    check(
        &builtin::html(),
        &lines,
        &[
            (0, 2, "Preprocessor"),
            (1, 4, "Comment"),
            (2, 6, "Comment"),
            (3, 1, "Tag"),
            (3, at(lines[3], "href"), "Attribute"),
            (3, at(lines[3], "\"/x\""), "String"),
            (3, at(lines[3], "'y'"), "String"),
        ],
    );
}

#[test]
fn xml() {
    let lines = [
        "<?xml version=\"1.0\"?>",
        "<root attr=\"v\">",
        "  <child>7</child>",
        "</root>",
    ];
    check(
        &builtin::xml(),
        &lines,
        &[
            (0, 2, "Preprocessor"),
            (1, 1, "Tag"),
            (1, at(lines[1], "attr"), "Attribute"),
            (2, at(lines[2], "7"), "Number"),
        ],
    );
}

#[test]
fn css() {
    let lines = [
        "@media screen {",
        "  .card { color: #ff8800; margin: 12px }",
        "  /* spacing",
        "     note */",
        "}",
    ];
    check(
        &builtin::css(),
        &lines,
        &[
            (0, 0, "Preprocessor"),
            (1, at(lines[1], "color:"), "Property"),
            (1, at(lines[1], "#ff8800"), "Color"),
            (1, at(lines[1], "12px"), "Number"),
            (2, at(lines[2], "/*"), "Comment"),
            (3, 6, "Comment"),
        ],
    );
}

#[test]
fn json() {
    let lines = [
        "{",
        "  \"name\": \"value\",",
        "  \"count\": -12.5e3,",
        "  \"ok\": true",
        "}",
    ];
    check(
        &builtin::json(),
        &lines,
        &[
            (1, 2, "String"),
            (1, at(lines[1], "\"value\""), "String"),
            (2, at(lines[2], "-12.5e3"), "Number"),
            (3, at(lines[3], "true"), "Keyword"),
        ],
    );
}

#[test]
fn sql() {
    let lines = [
        "-- pick rows",
        "SELECT name, 42 FROM t",
        "/* block",
        "   comment */",
        "WHERE name = 'o''brien';",
    ];
    check(
        &builtin::sql(),
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Keyword"),
            (1, at(lines[1], "42"), "Number"),
            (2, 0, "Comment"),
            (3, 5, "Comment"),
            (4, 0, "Keyword"),
            (4, at(lines[4], "'o'"), "String"),
        ],
    );
}

#[test]
fn ini() {
    let lines = [
        "; a comment",
        "[Section]",
        "Key=value",
        "Count=12",
        "# another comment",
    ];
    check(
        &builtin::ini(),
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Section"),
            (2, 0, "Key"),
            (3, at(lines[3], "12"), "Number"),
            (4, 0, "Comment"),
        ],
    );
}

#[test]
fn toml() {
    let lines = [
        "# a comment",
        "[package]",
        "name = \"demo\"",
        "edition = 2021",
        "text = \"\"\"multi",
        "line\"\"\"",
    ];
    check(
        &builtin::toml(),
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Section"),
            (2, 0, "Key"),
            (2, at(lines[2], "\"demo\""), "String"),
            (3, at(lines[3], "2021"), "Number"),
            (5, 0, "String"),
        ],
    );
}

#[test]
fn yaml() {
    let lines = [
        "---",
        "name: demo   # a comment",
        "count: 12",
        "items:",
        "  - first",
        "  - \"second\"",
    ];
    check(
        &builtin::yaml(),
        &lines,
        &[
            (0, 0, "Preprocessor"),
            (1, 0, "Key"),
            (1, at(lines[1], "#"), "Comment"),
            (2, at(lines[2], "12"), "Number"),
            (5, at(lines[5], "\"second\""), "String"),
        ],
    );
}

#[test]
fn shell() {
    let lines = [
        "#!/bin/sh",
        "# a comment",
        "for f in \"$HOME\"/*; do",
        "  echo \"${f}\"",
        "done",
    ];
    check(
        &builtin::shell(),
        &lines,
        &[
            (0, 0, "Preprocessor"),
            (1, 0, "Comment"),
            (2, 0, "Keyword"),
            (2, at(lines[2], "\"$HOME\""), "String"),
            (4, 0, "Keyword"),
        ],
    );
}

#[test]
fn powershell() {
    let lines = [
        "<#",
        " .SYNOPSIS block help",
        "#>",
        "function Get-Thing {",
        "  param($Name)",
        "  Write-Output \"hi $Name\"  # note",
        "}",
    ];
    check(
        &builtin::powershell(),
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 3, "Comment"),
            (3, 0, "Keyword"),
            (4, at(lines[4], "$Name"), "Variable"),
            (5, at(lines[5], "\"hi"), "String"),
            (5, at(lines[5], "# note"), "Comment"),
        ],
    );
}

#[test]
fn pascal() {
    let lines = [
        "unit Demo;",
        "{ a brace",
        "  comment }",
        "const Answer = 42;",
        "var S: string = 'text'; // trailing",
        "(* old style *)",
    ];
    check(
        &builtin::pascal(),
        &lines,
        &[
            (0, 0, "Keyword"),
            (1, 2, "Comment"),
            (2, 4, "Comment"),
            (3, at(lines[3], "42"), "Number"),
            (4, at(lines[4], "'text'"), "String"),
            (4, at(lines[4], "//"), "Comment"),
            (5, 3, "Comment"),
        ],
    );
}

#[test]
fn visual_basic() {
    let lines = [
        "' a comment",
        "Public Sub Greet(ByVal Name As String)",
        "    Dim Count As Integer",
        "    Count = &H1F",
        "    MsgBox \"hi \" & Name",
        "End Sub",
    ];
    check(
        &builtin::visual_basic(),
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Keyword"),
            (2, at(lines[2], "Dim"), "Keyword"),
            (3, at(lines[3], "&H1F"), "Number"),
            (4, at(lines[4], "\"hi \""), "String"),
        ],
    );
}

#[test]
fn markdown() {
    let lines = [
        "# Title",
        "",
        "Some text with `code` and a [link](http://example.com).",
        "",
        "```rust",
        "fn main() {}",
        "```",
    ];
    check(
        &builtin::markdown(),
        &lines,
        &[
            (0, 0, "Heading"),
            (2, at(lines[2], "`code`"), "Code"),
            (2, at(lines[2], "[link]"), "Link"),
            (4, 0, "Code"),
            (5, 0, "Code"),
        ],
    );
}

/// Assert that no line of `lines` past `after` is claimed by `element`, which
/// is what a construct the flat model cannot express must never cause.
fn assert_not_swallowed(format: &FileFormat, lines: &[&str], after: usize, element: &str) {
    let lexer = Lexer::new(&format.grammar).unwrap();
    for line in (after + 1)..lines.len() {
        if lines[line].is_empty() {
            continue;
        }
        assert_ne!(
            element_at(&lexer, lines, line, 0),
            element.to_owned(),
            "{} swallowed line {line}: {:?}",
            format.name,
            lines[line]
        );
    }
}

#[test]
fn c_and_cpp_literal_forms() {
    let lines = [
        "// a line comment ending in a backslash \\",
        "int resumed = 1;",
        "/* block",
        "   still block */",
        "const double f = 3.5e-2;",
        "const int h = 0xDEAD;",
        "const char c = '\\n';",
        "const char *s = \"text\";",
        "const char *r = R\"tag(unquoted \" inside)tag\";",
        "int tail = 7;",
    ];
    check(
        &builtin::c_cpp(),
        &lines,
        &[
            (0, 0, "Comment"),
            // The continuation a compiler honors is not carried: the next line
            // is ordinary code, which is wrong but never swallows the file.
            (1, at(lines[1], "int"), "Keyword"),
            (3, 3, "Comment"),
            (4, at(lines[4], "3.5e-2"), "Number"),
            (5, at(lines[5], "0xDEAD"), "Number"),
            (6, at(lines[6], "'\\n'"), "String"),
            (7, at(lines[7], "\"text\""), "String"),
            (9, at(lines[9], "int"), "Keyword"),
        ],
    );
    // The raw string form is approximated, and the file recovers below it.
    assert_not_swallowed(&builtin::c_cpp(), &lines, 8, "String");
}

#[test]
fn csharp_string_forms() {
    let lines = [
        "// note",
        "/* block */",
        "var path = @\"C:\\temp\\\";",
        "var n = $\"count {items.Count}\";",
        "var both = $@\"C:\\{dir}\\\";",
        "var other = @$\"C:\\{dir}\\\";",
        "var plain = \"escaped \\\" quote\";",
        "const int hex = 0xFF;",
        "const double d = 1.5e3;",
        "int tail = 3;",
    ];
    let format = builtin::csharp();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Comment"),
            (2, at(lines[2], "@\""), "String"),
            // A verbatim literal ends at its own closing quote, so the trailing
            // backslash does not carry the literal past it.
            (2, at(lines[2], ";"), "-"),
            (3, at(lines[3], "$\""), "String"),
            (4, at(lines[4], "$@\""), "String"),
            (5, at(lines[5], "@$\""), "String"),
            (6, at(lines[6], "\"escaped"), "String"),
            (7, at(lines[7], "0xFF"), "Number"),
            (8, at(lines[8], "1.5e3"), "Number"),
            (9, at(lines[9], "int"), "Keyword"),
        ],
    );
    assert_not_swallowed(&format, &lines, 2, "String");
}

#[test]
fn java_literal_forms() {
    let lines = [
        "// note",
        "/* block",
        "   more */",
        "class Sample {",
        "  static final int HEX = 0x2A;",
        "  static final double D = 6.02e23;",
        "  static final char C = 'x';",
        "  static final String S = \"a \\\" b\";",
        "}",
    ];
    check(
        &builtin::java(),
        &lines,
        &[
            (0, 0, "Comment"),
            (2, 3, "Comment"),
            (3, 0, "Keyword"),
            (4, at(lines[4], "0x2A"), "Number"),
            (5, at(lines[5], "6.02e23"), "Number"),
            (6, at(lines[6], "'x'"), "String"),
            (7, at(lines[7], "\"a"), "String"),
        ],
    );
}

#[test]
fn javascript_template_literals_and_slashes() {
    let lines = [
        "// note",
        "/* block */",
        "const re = /ab+c/g;",
        "const q = total / count;",
        "const t = `outer ${inner} end`;",
        "const n = 0xFF + 1.5e2;",
        "const s = 'single' + \"double\";",
        "let tail = 1;",
    ];
    let format = builtin::javascript();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Comment"),
            // A regular expression literal and a division are both left to the
            // ordinary items; neither is mistaken for a comment.
            (2, at(lines[2], "/ab"), "-"),
            (3, at(lines[3], "/ count"), "-"),
            (4, at(lines[4], "`outer"), "String"),
            (4, at(lines[4], "${inner}"), "String"),
            (5, at(lines[5], "0xFF"), "Number"),
            (5, at(lines[5], "1.5e2"), "Number"),
            (6, at(lines[6], "'single'"), "String"),
            (6, at(lines[6], "\"double\""), "String"),
            (7, 0, "Keyword"),
        ],
    );
    assert_not_swallowed(&format, &lines, 4, "String");
}

#[test]
fn typescript_literal_forms() {
    let lines = [
        "// note",
        "/* block */",
        "const t: string = `hi ${name}`;",
        "const h = 0x10;",
        "const f = 2.5e-1;",
        "const s = 'a' + \"b\";",
        "let tail: number = 1;",
    ];
    check(
        &builtin::typescript(),
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Comment"),
            (2, at(lines[2], "`hi"), "String"),
            (3, at(lines[3], "0x10"), "Number"),
            (4, at(lines[4], "2.5e-1"), "Number"),
            (5, at(lines[5], "'a'"), "String"),
            (6, at(lines[6], "number"), "Keyword"),
        ],
    );
}

#[test]
fn python_string_prefixes_and_triple_quotes() {
    let lines = [
        "# note",
        "raw = r\"C:\\temp\\x\"",
        "data = b\"bytes\"",
        "msg = f\"count {n}\"",
        "mix = rb\"both\"",
        "doc = \"\"\"first",
        "second\"\"\"",
        "alt = '''also",
        "more'''",
        "h = 0x1F",
        "fl = 1.5e3",
        "tail = 1",
    ];
    let format = builtin::python();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Comment"),
            // A prefix letter is an identifier and the quoted part is the
            // string; the two together cover the literal.
            (1, at(lines[1], "r\""), "Identifier"),
            (1, at(lines[1], "\"C:"), "String"),
            (2, at(lines[2], "\"bytes\""), "String"),
            (3, at(lines[3], "{n}"), "String"),
            (4, at(lines[4], "\"both\""), "String"),
            (6, 0, "String"),
            (8, 0, "String"),
            (9, at(lines[9], "0x1F"), "Number"),
            (10, at(lines[10], "1.5e3"), "Number"),
            (11, 0, "Identifier"),
        ],
    );
    assert_not_swallowed(&format, &lines, 8, "String");
}

#[test]
fn rust_raw_strings_and_character_literals() {
    let lines = [
        "// note",
        "/* block */",
        "let a = r\"plain raw\";",
        "let b = r#\"one \" hash\"#;",
        "let c = r##\"two \"# hashes\"##;",
        "let d = r###\"three \"## hashes\"###;",
        "let e = br\"bytes\";",
        "let f = br#\"byte \" hash\"#;",
        "let g = 'x';",
        "let h = '\\n';",
        "let i = '\\u{1F600}';",
        "let j = b'z';",
        "fn k<'a>(s: &'a str) -> &'static str { s }",
        "let n = 42;",
        "let m = 3.5e2;",
        "let o = 0xFF_u8;",
        "let tail = 1;",
    ];
    let format = builtin::rust();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Comment"),
            (2, at(lines[2], "r\""), "String"),
            (3, at(lines[3], "r#\""), "String"),
            (4, at(lines[4], "r##\""), "String"),
            (5, at(lines[5], "r###\""), "String"),
            (6, at(lines[6], "br\""), "String"),
            (7, at(lines[7], "br#\""), "String"),
            (8, at(lines[8], "'x'"), "String"),
            (9, at(lines[9], "'\\n'"), "String"),
            (10, at(lines[10], "'\\u{"), "String"),
            (11, at(lines[11], "b'z'"), "String"),
            // A lifetime carries no closing apostrophe and stays a lifetime.
            (12, at(lines[12], "'a>"), "-"),
            (12, at(lines[12], "'static"), "-"),
            (13, at(lines[13], "42"), "Number"),
            (14, at(lines[14], "3.5e2"), "Number"),
            (15, at(lines[15], "0xFF_u8"), "Number"),
            (16, 0, "Keyword"),
        ],
    );
    assert_not_swallowed(&format, &lines, 7, "String");
}

#[test]
fn rust_multiline_string_content_is_not_a_line_comment() {
    let lines = [
        "let message = \"usage:\n",
        "  // run with --dry-run\n",
        "\";\n",
    ];
    let format = builtin::rust();
    check(&format, &lines, &[(1, 2, "String"), (2, 0, "String")]);
}

#[test]
fn python_backslash_continued_string_content_is_not_a_comment() {
    let lines = [
        "message = \"first line\\\n",
        "# keep this\"\n",
        "print(message)\n",
    ];
    let format = builtin::python();
    check(&format, &lines, &[(1, 0, "String"), (2, 0, "Identifier")]);

    let malformed = ["message = \"unterminated\n", "# this is a comment\n"];
    check(&format, &malformed, &[(1, 0, "Comment")]);
}

#[test]
fn html_script_style_and_attributes() {
    let lines = [
        "<!-- a note -->",
        "<style>",
        "  .a { color: red }",
        "</style>",
        "<script>",
        "  var n = 1; // inner",
        "</script>",
        "<p class=\"x\" id='y'>7</p>",
    ];
    let format = builtin::html();
    check(
        &format,
        &lines,
        &[
            (0, 4, "Comment"),
            (1, 1, "Tag"),
            (3, 1, "Tag"),
            (4, 1, "Tag"),
            (6, 1, "Tag"),
            (7, at(lines[7], "class"), "Attribute"),
            (7, at(lines[7], "\"x\""), "String"),
            (7, at(lines[7], "'y'"), "String"),
            (7, at(lines[7], "7"), "Number"),
        ],
    );
    // The embedded languages are colored as markup, never as one long element.
    assert_not_swallowed(&format, &lines, 0, "Comment");
}

#[test]
fn xml_character_data_sections() {
    let lines = [
        "<?xml version=\"1.0\"?>",
        "<!-- a note -->",
        "<root>",
        "  <![CDATA[ raw < > & \" text ]]>",
        "  <child n=\"2\">3</child>",
        "</root>",
    ];
    let format = builtin::xml();
    check(
        &format,
        &lines,
        &[
            (0, 2, "Preprocessor"),
            (1, 4, "Comment"),
            (2, 1, "Tag"),
            (4, at(lines[4], "n=\""), "Attribute"),
            (4, at(lines[4], "3<"), "Number"),
            (5, 1, "Tag"),
        ],
    );
    // A character data section has no item of its own; the file still recovers.
    assert_not_swallowed(&format, &lines, 3, "String");
}

#[test]
fn json_escape_sequences() {
    let lines = [
        "{",
        "  \"quote\": \"a \\\" b\",",
        "  \"backslash\": \"a \\\\ b\",",
        "  \"unicode\": \"a \\u00e9 b\",",
        "  \"int\": 12,",
        "  \"float\": -12.5e3,",
        "  \"ok\": null",
        "}",
    ];
    check(
        &builtin::json(),
        &lines,
        &[
            (1, at(lines[1], "\"a \\\""), "String"),
            (1, at(lines[1], ","), "-"),
            (2, at(lines[2], "\"a \\\\"), "String"),
            (3, at(lines[3], "\\u00e9"), "String"),
            (4, at(lines[4], "12"), "Number"),
            (5, at(lines[5], "-12.5e3"), "Number"),
            (6, at(lines[6], "null"), "Keyword"),
        ],
    );
}

#[test]
fn yaml_hashes_inside_strings_and_block_scalars() {
    let lines = [
        "# a real comment",
        "double: \"a # not a comment\"",
        "single: 'b # also not'",
        "block: |",
        "  literal line one",
        "  literal line two",
        "folded: >",
        "  folded text",
        "count: 12",
        "ratio: -1.5e2",
        "flag: true",
    ];
    let format = builtin::yaml();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Comment"),
            (1, at(lines[1], "\"a #"), "String"),
            (2, at(lines[2], "'b #"), "String"),
            (3, 0, "Key"),
            (8, at(lines[8], "12"), "Number"),
            (9, at(lines[9], "-1.5e2"), "Number"),
            (10, at(lines[10], "true"), "Keyword"),
        ],
    );
    // A block scalar body has no item of its own and never opens an element
    // that outlives it.
    assert_not_swallowed(&format, &lines, 3, "String");
}

#[test]
fn sql_quoting_forms_and_comments() {
    let lines = [
        "-- a line comment",
        "/* a block",
        "   comment */",
        "SELECT [dbo].[Order Details].Quantity, \"quoted name\"",
        "FROM t WHERE name = 'o''brien' AND n = 12.5",
        "ORDER BY 1;",
    ];
    let format = builtin::sql();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Comment"),
            (2, 3, "Comment"),
            (3, 0, "Keyword"),
            (3, at(lines[3], "[dbo]"), "Quoted name"),
            (3, at(lines[3], "[Order Details]"), "Quoted name"),
            // The bracketed form closes on its own bracket, so the rest of the
            // line stays outside it.
            (3, at(lines[3], "Quantity"), "Identifier"),
            (3, at(lines[3], "\"quoted name\""), "Quoted name"),
            (4, at(lines[4], "'o'"), "String"),
            (4, at(lines[4], "12.5"), "Number"),
            (5, 0, "Keyword"),
        ],
    );
    assert_not_swallowed(&format, &lines, 3, "Quoted name");
}

#[test]
fn powershell_here_strings_and_backticks() {
    let lines = [
        "<# block",
        "   help #>",
        "# a line comment",
        "$text = @\"",
        "  here string body",
        "\"@",
        "$esc = \"a `\" b\"",
        "$lit = 'single'",
        "$h = 0x1F",
        "$f = 1.5e2",
        "function Get-Tail { }",
    ];
    let format = builtin::powershell();
    check(
        &format,
        &lines,
        &[
            (1, 3, "Comment"),
            (2, 0, "Comment"),
            (6, at(lines[6], "\"a"), "String"),
            (7, at(lines[7], "'single'"), "String"),
            (8, at(lines[8], "0x1F"), "Number"),
            (9, at(lines[9], "1.5e2"), "Number"),
            (10, 0, "Keyword"),
        ],
    );
    // A here-string has no item of its own; the closing sequence still ends the
    // element the opening quote started.
    assert_not_swallowed(&format, &lines, 5, "String");
}

#[test]
fn shell_heredocs_and_expansions() {
    let lines = [
        "#!/bin/sh",
        "# a comment",
        "cat <<EOF",
        "  it's a heredoc body",
        "  with ${braced} and $plain",
        "EOF",
        "echo \"${HOME}\" 'single'",
        "n=42",
        "done",
    ];
    let format = builtin::shell();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Preprocessor"),
            (1, 0, "Comment"),
            (4, at(lines[4], "${braced}"), "Variable"),
            (4, at(lines[4], "$plain"), "Variable"),
            (6, at(lines[6], "\"${HOME}\""), "String"),
            (6, at(lines[6], "'single'"), "String"),
            (7, at(lines[7], "42"), "Number"),
            (8, 0, "Keyword"),
        ],
    );
    // The apostrophe in the heredoc body opens nothing that outlives its line.
    assert_not_swallowed(&format, &lines, 3, "String");
}

#[test]
fn pascal_comment_forms() {
    let lines = [
        "unit Demo;",
        "{ a brace",
        "  comment }",
        "(* an old style",
        "   comment *)",
        "// a line comment",
        "const H = $1F;",
        "const F = 1.5e2;",
        "const S = 'it''s here';",
        "begin end.",
    ];
    check(
        &builtin::pascal(),
        &lines,
        &[
            (0, 0, "Keyword"),
            (2, 2, "Comment"),
            (4, 3, "Comment"),
            (5, 0, "Comment"),
            (6, at(lines[6], "$1F"), "Number"),
            (7, at(lines[7], "1.5e2"), "Number"),
            (8, at(lines[8], "'it'"), "String"),
            (9, 0, "Keyword"),
        ],
    );
}

#[test]
fn markdown_fenced_code_and_inline_forms() {
    let lines = [
        "# Heading one",
        "",
        "Text with `inline code` and 12 and 3.5.",
        "",
        "<!-- a note -->",
        "",
        "```rust",
        "fn main() { let n = 1; }",
        "```",
        "",
        "After the fence: TODO check.",
    ];
    let format = builtin::markdown();
    check(
        &format,
        &lines,
        &[
            (0, 0, "Heading"),
            (2, at(lines[2], "`inline code`"), "Code"),
            (2, at(lines[2], "12"), "Number"),
            (2, at(lines[2], "3.5"), "Number"),
            (4, 4, "Comment"),
            (6, 0, "Code"),
            (7, 0, "Code"),
            (10, at(lines[10], "TODO"), "Keyword"),
        ],
    );
    assert_not_swallowed(&format, &lines, 8, "Code");
}

#[test]
fn css_literal_forms() {
    let lines = [
        "/* block",
        "   comment */",
        "// a line comment",
        "@media screen {",
        "  .card { color: #ff8800; margin: 12px; width: 1.5em }",
        "  .b { content: \"a\" ; font: 'x' }",
        "}",
    ];
    check(
        &builtin::css(),
        &lines,
        &[
            (1, 3, "Comment"),
            (2, 0, "Comment"),
            (3, 0, "Preprocessor"),
            (4, at(lines[4], "#ff8800"), "Color"),
            (4, at(lines[4], "12px"), "Number"),
            (4, at(lines[4], "1.5em"), "Number"),
            (5, at(lines[5], "\"a\""), "String"),
            (5, at(lines[5], "'x'"), "String"),
        ],
    );
}

#[test]
fn ini_and_toml_literal_forms() {
    let ini_lines = [
        "; a comment",
        "# another comment",
        "[Section]",
        "Key=value",
        "Count=12",
        "Ratio=1.5",
        "Text=\"quoted\"",
        "Flag=yes",
    ];
    check(
        &builtin::ini(),
        &ini_lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Comment"),
            (2, 0, "Section"),
            (4, at(ini_lines[4], "12"), "Number"),
            (5, at(ini_lines[5], "1.5"), "Number"),
            (6, at(ini_lines[6], "\"quoted\""), "String"),
        ],
    );

    let toml_lines = [
        "# a comment",
        "[package]",
        "name = \"demo\"",
        "lit = 'raw \\ text'",
        "multi = \"\"\"first",
        "second\"\"\"",
        "hex = 0xDEAD",
        "float = 1.5e2",
        "flag = true",
    ];
    let toml = builtin::toml();
    check(
        &toml,
        &toml_lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Section"),
            (2, at(toml_lines[2], "\"demo\""), "String"),
            (3, at(toml_lines[3], "'raw"), "String"),
            (5, 0, "String"),
            (6, at(toml_lines[6], "0xDEAD"), "Number"),
            (7, at(toml_lines[7], "1.5e2"), "Number"),
            (8, at(toml_lines[8], "true"), "Keyword"),
        ],
    );
    assert_not_swallowed(&toml, &toml_lines, 5, "String");
}

#[test]
fn visual_basic_literal_forms() {
    let lines = [
        "' a comment",
        "REM another comment",
        "#Region \"Body\"",
        "Public Sub Greet(ByVal Name As String)",
        "    Dim H As Integer = &H1F",
        "    Dim F As Double = 1.5e2",
        "    MsgBox \"hi\" & Name",
        "End Sub",
        "#End Region",
    ];
    check(
        &builtin::visual_basic(),
        &lines,
        &[
            (0, 0, "Comment"),
            (1, 0, "Comment"),
            (2, 0, "Preprocessor"),
            (3, 0, "Keyword"),
            (4, at(lines[4], "&H1F"), "Number"),
            (5, at(lines[5], "1.5e2"), "Number"),
            (6, at(lines[6], "\"hi\""), "String"),
            (7, 0, "Keyword"),
        ],
    );
}

#[test]
fn no_stock_grammar_lets_one_element_reach_the_end_of_a_file() {
    // Each sample opens something and never closes it. Whatever the first line
    // is colored, the last line must not still be inside it.
    let openers = [
        "\"", "'", "`", "/*", "(*", "{", "<#", "```", "<!--", "@\"", "r#\"",
    ];
    for format in builtin::formats() {
        let lexer = Lexer::new(&format.grammar).unwrap();
        for opener in openers {
            let first = format!("code {opener} unterminated");
            let lines = [first.as_str(), "plain body", "plain body", "int last = 1;"];
            let cache = StateCache::build(&lexer, &lines).unwrap();
            let end = cache.state_at(lines.len());
            assert!(
                end.open_item.is_none()
                    || opener_may_span(opener)
                    || (format.name == "Rust" && opener == "\""),
                "{} left {opener:?} open at the end of the file",
                format.name
            );
        }
    }
}

/// Openers whose stock items deliberately run across lines, which a file that
/// really does open one and never close it cannot avoid.
fn opener_may_span(opener: &str) -> bool {
    matches!(
        opener,
        "/*" | "(*" | "{" | "<#" | "```" | "<!--" | "`" | "@\"" | "r#\""
    )
}

#[test]
fn every_stock_grammar_compiles_and_names_the_shared_elements() {
    for format in builtin::formats() {
        let lexer = Lexer::new(&format.grammar)
            .unwrap_or_else(|e| panic!("{} does not compile: {e}", format.name));
        let names = lexer.element_names();
        for required in [
            builtin::COMMENT,
            builtin::STRING,
            builtin::NUMBER,
            builtin::KEYWORD,
            builtin::IDENTIFIER,
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "{} has no {required} element",
                format.name
            );
        }
    }
}

#[test]
fn added_language_formats_route_and_color_their_keywords() {
    let registry = builtin::registry();
    for (path, source, keyword, language) in [
        ("main.go", "func main() {}", "func", "Go"),
        ("main.kt", "fun main() {}", "fun", "Kotlin"),
        ("main.swift", "func main() {}", "func", "Swift"),
        ("main.dart", "class App {}", "class", "Dart"),
        ("main.scala", "object Main {}", "object", "Scala"),
        ("Rakefile", "def build; end", "def", "Ruby"),
        ("main.php", "function run() {}", "function", "PHP"),
        ("main.lua", "function run() end", "function", "Lua"),
        ("main.pl", "sub run {}", "sub", "Perl"),
        ("analysis.R", "function(x) x", "function", "R"),
        ("main.hs", "case value of", "case", "Haskell"),
        ("CMakeLists.txt", "if (TRUE)", "if", "CMake"),
        ("Dockerfile", "FROM alpine", "FROM", "Dockerfile"),
        ("Makefile", "include common.mk", "include", "Makefile"),
    ] {
        let format = registry.lookup(path);
        assert_eq!(format.name, language, "format selected for {path}");
        let lexer = Lexer::new(&format.grammar)
            .unwrap_or_else(|error| panic!("{language} grammar does not compile: {error}"));
        assert_eq!(
            element_at(&lexer, &[source], 0, at(source, keyword)),
            "Keyword",
            "keyword coloring for {language}"
        );
    }
}

#[test]
fn perl_hash_inside_code_does_not_start_a_comment() {
    let source = "for my $i (0 .. $#items) { $total += $items[$i]; }";
    let lexer = Lexer::new(&builtin::perl().grammar).unwrap();

    assert_eq!(
        element_at(&lexer, &[source], 0, at(source, "$total")),
        "Variable"
    );
}

#[test]
fn dockerfile_hash_in_a_run_argument_does_not_start_a_comment() {
    let source = "RUN curl https://example.test/get#frag -o /tmp/x && chmod 700 /tmp/x";
    let lexer = Lexer::new(&builtin::dockerfile().grammar).unwrap();

    assert_eq!(
        element_at(&lexer, &[source], 0, at(source, "chmod")),
        "Identifier"
    );
}

#[test]
fn lua_long_strings_close_at_the_second_bracket_across_lines() {
    let lines = ["local text = [[first", "still string", "last]] return"];
    let lexer = Lexer::new(&builtin::lua().grammar).unwrap();

    assert_eq!(
        element_at(&lexer, &lines, 1, at(lines[1], "still")),
        "String"
    );
    assert_eq!(
        element_at(&lexer, &lines, 2, at(lines[2], "return")),
        "Keyword"
    );
}

#[test]
fn haskell_prime_identifiers_do_not_open_character_literals() {
    let source = "foldl' x' = x' where";
    let lexer = Lexer::new(&builtin::haskell().grammar).unwrap();

    assert_eq!(
        element_at(&lexer, &[source], 0, at(source, "where")),
        "Keyword"
    );
}

#[test]
fn commenting_code_out_remains_important_when_comments_are_unimportant() {
    let grammar = builtin::c_cpp().grammar;
    let classifier = GrammarClassifier::new(&grammar).unwrap();
    let mut rules = RuleSet::all_important();
    rules.important_elements.insert("Identifier".to_owned());
    rules.orphan_lines_always_important = false;
    for (left, right, left_important, right_important) in [
        ("launch();\n", "// launch();\n", true, false),
        ("// launch();\n", "launch();\n", false, true),
        ("launch();\n", "/* launch(); */\n", true, false),
    ] {
        let left = [left];
        let right = [right];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let outcomes = classify_hunks(&left, &right, &hunks, &rules, &classifier).unwrap();

        assert_eq!(outcomes[0].importance, Some(Importance::Important));
        assert_eq!(
            outcomes[0].left_lines[0] == Importance::Important,
            left_important
        );
        assert_eq!(
            outcomes[0].right_lines[0] == Importance::Important,
            right_important,
            "{left:?} -> {right:?}: {outcomes:#?}"
        );
    }

    let left = ["// launch();\n"];
    let right = ["// stop();\n"];
    let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
    let outcomes = classify_hunks(&left, &right, &hunks, &rules, &classifier).unwrap();
    assert_eq!(outcomes[0].importance, Some(Importance::Unimportant));
}

#[test]
fn every_stock_element_name_maps_to_a_specific_color_role() {
    let styles = StyleMap::new();
    for format in builtin::formats() {
        for name in format.grammar.element_names() {
            assert_ne!(
                styles.slot_for(&name),
                StyleSlot::Other,
                "{} element {name} has no role",
                format.name
            );
        }
    }
}
