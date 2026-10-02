//! Stock formats, defined through the same public model a user-authored format
//! uses.
//!
//! Nothing here reaches into a private constructor, so a future grammar editor
//! can load one of these, change it and write it back out with no special case.
//! Every text grammar names at least the elements `Comment`, `String`,
//! `Number`, `Keyword` and `Identifier`, so an importance checklist looks the
//! same from one language to the next.

use crate::format::{FileFormat, FormatKind, FormatRegistry};
use crate::grammar::{Grammar, GrammarItem, ItemKind, MatchOptions};

/// The element name for comments in every stock grammar.
pub const COMMENT: &str = "Comment";
/// The element name for string and character literals.
pub const STRING: &str = "String";
/// The element name for numeric literals.
pub const NUMBER: &str = "Number";
/// The element name for reserved words.
pub const KEYWORD: &str = "Keyword";
/// The element name for user-chosen names.
pub const IDENTIFIER: &str = "Identifier";
/// The element name for preprocessor and build-time directives.
pub const DIRECTIVE: &str = "Preprocessor";

/// A comment running from `start` to the end of the line.
#[must_use]
pub fn line_comment(start: &str) -> GrammarItem {
    GrammarItem::new(
        COMMENT,
        ItemKind::Delimited {
            start: start.to_owned(),
            stop: String::new(),
            stop_at_end_of_line: true,
            escape: None,
            line_spanning: false,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// A comment between `start` and `stop` that may continue on later lines.
#[must_use]
pub fn block_comment(start: &str, stop: &str) -> GrammarItem {
    GrammarItem::new(
        COMMENT,
        ItemKind::Delimited {
            start: start.to_owned(),
            stop: stop.to_owned(),
            stop_at_end_of_line: false,
            escape: None,
            line_spanning: true,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// A literal delimited by `quote`, ending at the end of the line when unclosed.
#[must_use]
pub fn quoted(element: &str, quote: &str, escape: Option<char>) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::Delimited {
            start: quote.to_owned(),
            stop: quote.to_owned(),
            stop_at_end_of_line: false,
            escape,
            line_spanning: false,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// A quoted literal that continues only when its escape character precedes a
/// line ending, as in a Python backslash-continued string.
fn quoted_line_continued(element: &str, quote: &str, escape: char) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::Delimited {
            start: quote.to_owned(),
            stop: quote.to_owned(),
            stop_at_end_of_line: false,
            escape: Some(escape),
            line_spanning: false,
            continue_after_escaped_newline: true,
            options: MatchOptions::literal(),
        },
    )
}

/// A literal opened by `start` and closed by a different `stop`, ending at the
/// end of the line when unclosed.
#[must_use]
pub fn quoted_between(element: &str, start: &str, stop: &str, escape: Option<char>) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::Delimited {
            start: start.to_owned(),
            stop: stop.to_owned(),
            stop_at_end_of_line: false,
            escape,
            line_spanning: false,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// A literal opened by `start` and closed by a different `stop`, which may
/// continue on later lines.
#[must_use]
pub fn quoted_between_spanning(
    element: &str,
    start: &str,
    stop: &str,
    escape: Option<char>,
) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::Delimited {
            start: start.to_owned(),
            stop: stop.to_owned(),
            stop_at_end_of_line: false,
            escape,
            line_spanning: true,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// A literal delimited by `quote` that may continue on later lines.
#[must_use]
pub fn quoted_spanning(element: &str, quote: &str, escape: Option<char>) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::Delimited {
            start: quote.to_owned(),
            stop: quote.to_owned(),
            stop_at_end_of_line: false,
            escape,
            line_spanning: true,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// A list of literal tokens matched on word boundaries.
#[must_use]
pub fn words(element: &str, tokens: &[&str], match_case: bool) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::List {
            tokens: tokens.iter().map(|t| (*t).to_owned()).collect(),
            options: if match_case {
                MatchOptions::literal()
            } else {
                MatchOptions::literal_any_case()
            },
            whole_word: true,
        },
    )
}

/// A single pattern, matched on word boundaries.
#[must_use]
pub fn pattern(element: &str, regex: &str) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::Basic {
            text: regex.to_owned(),
            options: MatchOptions::regex(),
            whole_word: true,
        },
    )
}

/// A single pattern, matched wherever it occurs.
#[must_use]
pub fn pattern_anywhere(element: &str, regex: &str) -> GrammarItem {
    GrammarItem::new(
        element,
        ItemKind::Basic {
            text: regex.to_owned(),
            options: MatchOptions::regex(),
            whole_word: false,
        },
    )
}

/// The numeric literal item most of the stock grammars use.
#[must_use]
pub fn c_style_number() -> GrammarItem {
    pattern(
        NUMBER,
        r"0[xXbB][0-9a-fA-F_]+|\d[\d_]*\.?[\d_]*([eE][-+]?\d+)?",
    )
}

/// The identifier item most of the stock grammars use. It belongs at the end of
/// a grammar, because every earlier item that also matches a word has to win.
#[must_use]
pub fn c_style_identifier() -> GrammarItem {
    pattern(IDENTIFIER, r"[A-Za-z_][A-Za-z0-9_]*")
}

const C_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "auto",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "nullptr",
    "operator",
    "private",
    "protected",
    "public",
    "register",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
];

const CSHARP_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "record",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "var",
    "virtual",
    "void",
    "volatile",
    "while",
    "yield",
];

const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "record",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "var",
    "void",
    "volatile",
    "while",
    "yield",
];

const JS_KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "get",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "of",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const TS_KEYWORDS: &[&str] = &[
    "abstract",
    "any",
    "as",
    "asserts",
    "async",
    "await",
    "bigint",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "namespace",
    "never",
    "new",
    "null",
    "number",
    "object",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "static",
    "string",
    "super",
    "switch",
    "symbol",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "unique",
    "unknown",
    "var",
    "void",
    "while",
    "yield",
];

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "union",
    "unsafe", "use", "where", "while",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];

const SQL_KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "and",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "case",
    "cast",
    "check",
    "column",
    "commit",
    "constraint",
    "create",
    "cross",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "foreign",
    "from",
    "full",
    "group",
    "having",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "procedure",
    "references",
    "right",
    "rollback",
    "select",
    "set",
    "table",
    "then",
    "transaction",
    "union",
    "unique",
    "update",
    "values",
    "view",
    "when",
    "where",
    "with",
];

const SHELL_KEYWORDS: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "readonly", "return", "select", "then", "until", "while",
];

const POWERSHELL_KEYWORDS: &[&str] = &[
    "begin",
    "break",
    "catch",
    "class",
    "continue",
    "data",
    "do",
    "dynamicparam",
    "else",
    "elseif",
    "end",
    "enum",
    "exit",
    "filter",
    "finally",
    "for",
    "foreach",
    "function",
    "hidden",
    "if",
    "in",
    "param",
    "process",
    "return",
    "static",
    "switch",
    "throw",
    "trap",
    "try",
    "until",
    "using",
    "while",
];

const PASCAL_KEYWORDS: &[&str] = &[
    "and",
    "array",
    "as",
    "begin",
    "case",
    "class",
    "const",
    "constructor",
    "destructor",
    "div",
    "do",
    "downto",
    "else",
    "end",
    "except",
    "file",
    "finally",
    "for",
    "function",
    "goto",
    "if",
    "implementation",
    "in",
    "inherited",
    "initialization",
    "interface",
    "is",
    "label",
    "mod",
    "nil",
    "not",
    "object",
    "of",
    "or",
    "packed",
    "procedure",
    "program",
    "property",
    "raise",
    "record",
    "repeat",
    "set",
    "shl",
    "shr",
    "then",
    "threadvar",
    "to",
    "try",
    "type",
    "unit",
    "until",
    "uses",
    "var",
    "while",
    "with",
    "xor",
];

const VB_KEYWORDS: &[&str] = &[
    "And",
    "As",
    "Boolean",
    "ByRef",
    "ByVal",
    "Call",
    "Case",
    "Class",
    "Const",
    "Dim",
    "Do",
    "Double",
    "Each",
    "Else",
    "ElseIf",
    "End",
    "Enum",
    "Erase",
    "Error",
    "Exit",
    "False",
    "For",
    "Function",
    "Get",
    "GoTo",
    "If",
    "Implements",
    "In",
    "Integer",
    "Is",
    "Let",
    "Long",
    "Loop",
    "Me",
    "Mod",
    "Module",
    "New",
    "Next",
    "Not",
    "Nothing",
    "Object",
    "On",
    "Option",
    "Optional",
    "Or",
    "Private",
    "Property",
    "Public",
    "ReDim",
    "Resume",
    "Return",
    "Select",
    "Set",
    "Single",
    "Static",
    "Step",
    "Stop",
    "String",
    "Sub",
    "Then",
    "To",
    "True",
    "Try",
    "Type",
    "Until",
    "Variant",
    "Wend",
    "While",
    "With",
    "Xor",
];

/// The grammar shared by the brace-and-slash language family.
fn c_family_grammar(keywords: &[&str], directives: bool) -> Grammar {
    let mut items = vec![line_comment("//"), block_comment("/*", "*/")];
    if directives {
        items.push(pattern_anywhere(DIRECTIVE, r"^[ \t]*#[ \t]*[a-z_]+"));
    }
    items.push(quoted(STRING, "\"", Some('\\')));
    items.push(quoted(STRING, "'", Some('\\')));
    items.push(words(KEYWORD, keywords, true));
    items.push(c_style_number());
    items.push(c_style_identifier());
    Grammar::from_items(items)
}

fn c_family_format(name: &str, mask: &str, description: &str, keywords: &[&str]) -> FileFormat {
    FileFormat::text(name, mask)
        .with_description(description)
        .with_grammar(c_family_grammar(keywords, false))
        .with_tab_stop(4)
}

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
    "true",
    "false",
    "nil",
];

const KOTLIN_KEYWORDS: &[&str] = &[
    "as",
    "break",
    "class",
    "continue",
    "do",
    "else",
    "false",
    "for",
    "fun",
    "if",
    "in",
    "interface",
    "is",
    "null",
    "object",
    "package",
    "return",
    "super",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "when",
    "while",
];

const SWIFT_KEYWORDS: &[&str] = &[
    "associatedtype",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "defer",
    "default",
    "deinit",
    "do",
    "else",
    "enum",
    "extension",
    "fallthrough",
    "false",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "inout",
    "internal",
    "let",
    "nil",
    "operator",
    "private",
    "protocol",
    "public",
    "repeat",
    "return",
    "self",
    "static",
    "struct",
    "subscript",
    "super",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "var",
    "where",
    "while",
];

const DART_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "assert",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "deferred",
    "do",
    "dynamic",
    "else",
    "enum",
    "export",
    "extends",
    "extension",
    "external",
    "factory",
    "false",
    "final",
    "finally",
    "for",
    "get",
    "hide",
    "if",
    "implements",
    "import",
    "in",
    "interface",
    "is",
    "late",
    "library",
    "mixin",
    "new",
    "null",
    "on",
    "operator",
    "part",
    "required",
    "rethrow",
    "return",
    "sealed",
    "set",
    "show",
    "static",
    "super",
    "switch",
    "sync",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "var",
    "void",
    "when",
    "while",
    "with",
    "yield",
];

const SCALA_KEYWORDS: &[&str] = &[
    "abstract",
    "case",
    "catch",
    "class",
    "def",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "final",
    "finally",
    "for",
    "given",
    "if",
    "implicit",
    "import",
    "lazy",
    "match",
    "new",
    "null",
    "object",
    "override",
    "package",
    "private",
    "protected",
    "return",
    "sealed",
    "super",
    "then",
    "this",
    "throw",
    "trait",
    "true",
    "try",
    "type",
    "val",
    "var",
    "while",
    "with",
    "yield",
];

const RUBY_KEYWORDS: &[&str] = &[
    "alias", "and", "begin", "BEGIN", "break", "case", "class", "def", "defined?", "do", "else",
    "elsif", "end", "END", "ensure", "false", "for", "if", "in", "module", "next", "nil", "not",
    "or", "redo", "rescue", "retry", "return", "self", "super", "then", "true", "undef", "unless",
    "until", "when", "while", "yield",
];

const PHP_KEYWORDS: &[&str] = &[
    "abstract",
    "array",
    "as",
    "break",
    "callable",
    "case",
    "catch",
    "class",
    "clone",
    "const",
    "continue",
    "declare",
    "default",
    "die",
    "do",
    "echo",
    "else",
    "elseif",
    "empty",
    "endfor",
    "endforeach",
    "endif",
    "endswitch",
    "endwhile",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "fn",
    "for",
    "foreach",
    "function",
    "global",
    "if",
    "implements",
    "include",
    "instanceof",
    "interface",
    "match",
    "namespace",
    "new",
    "null",
    "private",
    "protected",
    "public",
    "readonly",
    "require",
    "return",
    "static",
    "switch",
    "throw",
    "trait",
    "true",
    "try",
    "use",
    "var",
    "while",
];

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

const PERL_KEYWORDS: &[&str] = &[
    "and", "cmp", "continue", "do", "else", "elsif", "eq", "for", "foreach", "ge", "given", "gt",
    "if", "le", "local", "lt", "my", "ne", "next", "no", "not", "our", "package", "redo",
    "require", "return", "sub", "unless", "until", "use", "when", "while", "x", "xor",
];

const R_KEYWORDS: &[&str] = &[
    "break", "else", "FALSE", "for", "function", "if", "Inf", "in", "NA", "NaN", "next", "NULL",
    "repeat", "return", "TRUE", "while",
];

const HASKELL_KEYWORDS: &[&str] = &[
    "case", "class", "data", "default", "deriving", "do", "else", "foreign", "if", "import", "in",
    "infix", "instance", "let", "module", "newtype", "of", "then", "type", "where",
];

const CMAKE_KEYWORDS: &[&str] = &[
    "AND",
    "BOOL",
    "BREAK",
    "BUILD_INTERFACE",
    "CACHE",
    "COMMAND",
    "CONFIGURE_DEPENDS",
    "ELSE",
    "ELSEIF",
    "ENDFOREACH",
    "ENDIF",
    "ENDMACRO",
    "ENDWHILE",
    "FALSE",
    "FILE",
    "FOREACH",
    "FUNCTION",
    "GET",
    "IF",
    "IN_LIST",
    "INHERITED",
    "LIST",
    "MACRO",
    "NOT",
    "OR",
    "PARENT_SCOPE",
    "POLICY",
    "PROJECT",
    "RETURN",
    "SET",
    "STREQUAL",
    "TARGET",
    "TRUE",
    "UNSET",
    "WHILE",
];

const DOCKER_KEYWORDS: &[&str] = &[
    "ADD",
    "ARG",
    "CMD",
    "COPY",
    "ENTRYPOINT",
    "ENV",
    "EXPOSE",
    "FROM",
    "HEALTHCHECK",
    "LABEL",
    "MAINTAINER",
    "ONBUILD",
    "RUN",
    "SHELL",
    "STOPSIGNAL",
    "USER",
    "VOLUME",
    "WORKDIR",
];

const MAKE_KEYWORDS: &[&str] = &[
    "define", "else", "endef", "endif", "export", "ifdef", "ifndef", "ifeq", "ifneq", "include",
    "override", "private", "sinclude", "unexport", "vpath",
];

/// Go source files.
#[must_use]
pub fn go() -> FileFormat {
    c_family_format("Go", "*.go", "Go source files.", GO_KEYWORDS)
}

/// Kotlin source files and scripts.
#[must_use]
pub fn kotlin() -> FileFormat {
    FileFormat::text("Kotlin", "*.kt;*.kts")
        .with_description("Kotlin source files and scripts.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("//"),
            block_comment("/*", "*/"),
            quoted_spanning(STRING, "\"\"\"", None),
            quoted_line_continued(STRING, "\"", '\\'),
            quoted_line_continued(STRING, "'", '\\'),
            words(KEYWORD, KOTLIN_KEYWORDS, true),
            c_style_number(),
            c_style_identifier(),
        ]))
}

/// Swift source files.
#[must_use]
pub fn swift() -> FileFormat {
    FileFormat::text("Swift", "*.swift")
        .with_description("Swift source files.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("//"),
            block_comment("/*", "*/"),
            quoted_spanning(STRING, "\"\"\"", None),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, SWIFT_KEYWORDS, true),
            c_style_number(),
            c_style_identifier(),
        ]))
}

/// Dart source files.
#[must_use]
pub fn dart() -> FileFormat {
    c_family_format("Dart", "*.dart", "Dart source files.", DART_KEYWORDS)
}

/// Scala source files and scripts.
#[must_use]
pub fn scala() -> FileFormat {
    FileFormat::text("Scala", "*.scala;*.sc")
        .with_description("Scala source files and scripts.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("//"),
            block_comment("/*", "*/"),
            quoted_spanning(STRING, "\"\"\"", None),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, SCALA_KEYWORDS, true),
            c_style_number(),
            c_style_identifier(),
        ]))
}

/// Ruby source files and build scripts.
#[must_use]
pub fn ruby() -> FileFormat {
    FileFormat::text("Ruby", "*.rb;*.rake;*.gemspec;Gemfile;Rakefile")
        .with_description("Ruby source files and build scripts.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("#"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", None),
            words(KEYWORD, RUBY_KEYWORDS, true),
            pattern(
                NUMBER,
                r"0[xX][0-9a-fA-F_]+|\d[\d_]*(\.\d[\d_]*)?([eE][-+]?\d+)?",
            ),
            c_style_identifier(),
        ]))
}

/// PHP source files.
#[must_use]
pub fn php() -> FileFormat {
    FileFormat::text("PHP", "*.php;*.phtml;*.php3;*.php4;*.php5;*.phps")
        .with_description("PHP source files. Tagged HTML and PHP are highlighted as text.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("//"),
            line_comment("#"),
            block_comment("/*", "*/"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, PHP_KEYWORDS, false),
            c_style_number(),
            c_style_identifier(),
        ]))
}

/// Lua source files.
#[must_use]
pub fn lua() -> FileFormat {
    FileFormat::text("Lua", "*.lua")
        .with_description("Lua source files.")
        .with_grammar(Grammar::from_items(vec![
            block_comment("--[[", "]]"),
            line_comment("--"),
            quoted_between_spanning(STRING, "[[", "]]", None),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, LUA_KEYWORDS, true),
            pattern(NUMBER, r"0[xX][0-9a-fA-F]+|\d+\.?\d*([eE][-+]?\d+)?"),
            c_style_identifier(),
        ]))
}

/// Perl source files and tests.
#[must_use]
pub fn perl() -> FileFormat {
    FileFormat::text("Perl", "*.pl;*.pm;*.t;*.plx")
        .with_description("Perl source files and tests.")
        .with_grammar(Grammar::from_items(vec![
            pattern_anywhere(COMMENT, r"(?:^|[ \t])#.*"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, PERL_KEYWORDS, true),
            pattern(NUMBER, r"0[xX][0-9a-fA-F]+|\d+\.?\d*([eE][-+]?\d+)?"),
            pattern_anywhere(
                "Variable",
                r"\$[A-Za-z_][A-Za-z0-9_]*|[@%][A-Za-z_][A-Za-z0-9_]*",
            ),
            c_style_identifier(),
        ]))
}

/// R source files.
#[must_use]
pub fn r_language() -> FileFormat {
    FileFormat::text("R", "*.r;*.R;*.rmd")
        .with_description("R source files and R Markdown documents.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("#"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, R_KEYWORDS, true),
            pattern(NUMBER, r"\d+\.?\d*([eE][-+]?\d+)?"),
            c_style_identifier(),
        ]))
}

/// Haskell source files.
#[must_use]
pub fn haskell() -> FileFormat {
    FileFormat::text("Haskell", "*.hs;*.lhs")
        .with_description("Haskell source files.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("--"),
            block_comment("{-", "-}"),
            quoted(STRING, "\"", Some('\\')),
            pattern_anywhere(STRING, r"'(\\.|[^\\'])'"),
            words(KEYWORD, HASKELL_KEYWORDS, true),
            pattern(NUMBER, r"\d+\.?\d*([eE][-+]?\d+)?"),
            c_style_identifier(),
        ]))
}

/// `CMake` source and project files.
#[must_use]
pub fn cmake() -> FileFormat {
    FileFormat::text("CMake", "*.cmake;CMakeLists.txt")
        .with_description("CMake build scripts.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("#"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, CMAKE_KEYWORDS, false),
            pattern(NUMBER, r"\d+\.?\d*([eE][-+]?\d+)?"),
            pattern_anywhere("Variable", r"\$\{[^}]+\}|\$ENV\{[^}]+\}"),
            c_style_identifier(),
        ]))
}

/// Docker build files.
#[must_use]
pub fn dockerfile() -> FileFormat {
    FileFormat::text("Dockerfile", "Dockerfile;*.dockerfile")
        .with_description("Docker build instructions.")
        .with_grammar(Grammar::from_items(vec![
            pattern_anywhere(DIRECTIVE, r"(?i)^\s*#(syntax|escape|check)=.*"),
            pattern_anywhere(COMMENT, r"^[ \t]*#.*"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", None),
            words(KEYWORD, DOCKER_KEYWORDS, false),
            pattern(NUMBER, r"\d+\.?\d*([eE][-+]?\d+)?"),
            pattern_anywhere("Variable", r"\$\{[^}]+\}|\$[A-Za-z_][A-Za-z0-9_]*"),
            c_style_identifier(),
        ]))
}

/// Makefiles.
#[must_use]
pub fn makefile() -> FileFormat {
    FileFormat::text("Makefile", "Makefile;makefile;GNUmakefile;*.mk;*.mak")
        .with_description("GNU Make build files.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("#"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            pattern_anywhere(DIRECTIVE, r"^[A-Za-z_][A-Za-z0-9_]*[ \t]*[:+?]?="),
            words(KEYWORD, MAKE_KEYWORDS, true),
            pattern(NUMBER, r"\d+"),
            pattern_anywhere("Variable", r"\$\([^)]+\)|\$\{[^}]+\}|\$[@<^?*]"),
            c_style_identifier(),
        ]))
}

/// Every stock format, in the order they take part in lookup.
///
/// The order puts narrow masks ahead of wide ones, so a format whose mask
/// overlaps another's still gets its files.
#[must_use]
pub fn formats() -> Vec<FileFormat> {
    vec![
        c_cpp(),
        csharp(),
        java(),
        javascript(),
        typescript(),
        python(),
        rust(),
        go(),
        kotlin(),
        swift(),
        dart(),
        scala(),
        ruby(),
        php(),
        lua(),
        perl(),
        r_language(),
        haskell(),
        cmake(),
        dockerfile(),
        makefile(),
        html(),
        xml(),
        css(),
        json(),
        sql(),
        ini(),
        toml(),
        yaml(),
        shell(),
        powershell(),
        pascal(),
        visual_basic(),
        markdown(),
    ]
}

/// A registry holding every stock format plus the plain text fallback.
#[must_use]
pub fn registry() -> FormatRegistry {
    let mut registry = FormatRegistry::new(plain_text());
    for format in formats() {
        registry.push(format);
    }
    registry
}

/// The format used for a filename no other format claims.
#[must_use]
pub fn plain_text() -> FileFormat {
    FileFormat::text("Everything else", "*")
        .with_description("Files no other format claims, compared as plain text.")
        .with_grammar(Grammar::from_items(vec![
            c_style_number(),
            c_style_identifier(),
        ]))
}

/// C and C++ sources and headers.
#[must_use]
pub fn c_cpp() -> FileFormat {
    FileFormat::text(
        "C/C++",
        "*.c;*.cpp;*.cxx;*.cc;*.c++;*.h;*.hpp;*.hxx;*.hh;*.inl",
    )
    .with_description("C and C++ source and header files.")
    .with_grammar(c_family_grammar(C_KEYWORDS, true))
    .with_tab_stop(4)
}

/// A verbatim string: backslash is an ordinary character and only the closing
/// quote ends the literal.
///
/// The doubled quote that escapes a quote inside the literal closes one literal
/// and opens the next, which colors the same span either way.
fn verbatim_string(start: &str) -> GrammarItem {
    GrammarItem::new(
        STRING,
        ItemKind::Delimited {
            start: start.to_owned(),
            stop: "\"".to_owned(),
            stop_at_end_of_line: false,
            escape: None,
            line_spanning: true,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// C# sources.
#[must_use]
pub fn csharp() -> FileFormat {
    FileFormat::text("C#", "*.cs;*.csx")
        .with_description("C# source files, including the verbatim and interpolated string forms.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("//"),
            block_comment("/*", "*/"),
            pattern_anywhere(DIRECTIVE, r"^[ \t]*#[ \t]*[a-z_]+"),
            // The verbatim forms precede the ordinary one, and the longer
            // openers precede the shorter ones they start with: a backslash
            // inside a verbatim literal is an ordinary character, so treating
            // one as an escape would carry the literal past its closing quote.
            verbatim_string("$@\""),
            verbatim_string("@$\""),
            verbatim_string("@\""),
            quoted_between(STRING, "$\"", "\"", Some('\\')),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            words(KEYWORD, CSHARP_KEYWORDS, true),
            c_style_number(),
            c_style_identifier(),
        ]))
        .with_tab_stop(4)
}

/// Java sources.
#[must_use]
pub fn java() -> FileFormat {
    FileFormat::text("Java", "*.java")
        .with_description("Java source files.")
        .with_grammar(c_family_grammar(JAVA_KEYWORDS, false))
        .with_tab_stop(4)
}

/// JavaScript sources.
#[must_use]
pub fn javascript() -> FileFormat {
    let mut grammar = c_family_grammar(JS_KEYWORDS, false);
    grammar
        .items
        .insert(2, quoted_spanning(STRING, "`", Some('\\')));
    FileFormat::text("JavaScript", "*.js;*.mjs;*.cjs;*.jsx")
        .with_description("JavaScript and JSX source files.")
        .with_grammar(grammar)
        .with_tab_stop(2)
}

/// TypeScript sources.
#[must_use]
pub fn typescript() -> FileFormat {
    let mut grammar = c_family_grammar(TS_KEYWORDS, false);
    grammar
        .items
        .insert(2, quoted_spanning(STRING, "`", Some('\\')));
    FileFormat::text("TypeScript", "*.ts;*.tsx;*.mts;*.cts")
        .with_description("TypeScript and TSX source files.")
        .with_grammar(grammar)
        .with_tab_stop(2)
}

/// Python sources.
#[must_use]
pub fn python() -> FileFormat {
    FileFormat::text("Python", "*.py;*.pyw;*.pyi")
        .with_description("Python source files.")
        .with_grammar(Grammar::from_items(vec![
            // The triple-quoted forms come first: their opening delimiter
            // starts with the single-quoted forms' delimiter, so a later item
            // would claim the first quote and leave the rest adrift.
            quoted_spanning(STRING, "\"\"\"", None),
            quoted_spanning(STRING, "'''", None),
            line_comment("#"),
            quoted_line_continued(STRING, "\"", '\\'),
            quoted_line_continued(STRING, "'", '\\'),
            words(KEYWORD, PYTHON_KEYWORDS, true),
            pattern(
                NUMBER,
                r"0[xXbBoO][0-9a-fA-F_]+|\d[\d_]*\.?[\d_]*([eE][-+]?\d+)?[jJ]?",
            ),
            pattern(DIRECTIVE, r"@[A-Za-z_][A-Za-z0-9_.]*"),
            c_style_identifier(),
        ]))
        .with_tab_stop(4)
}

/// A raw string literal: no escape character, closed by its own delimiter.
fn raw_string(start: &str, stop: &str) -> GrammarItem {
    GrammarItem::new(
        STRING,
        ItemKind::Delimited {
            start: start.to_owned(),
            stop: stop.to_owned(),
            stop_at_end_of_line: false,
            escape: None,
            line_spanning: true,
            continue_after_escaped_newline: false,
            options: MatchOptions::literal(),
        },
    )
}

/// The raw string forms the Rust grammar recognizes, longest opener first.
///
/// The flat item model has no counter, so each hash depth is its own item and
/// the set of depths is fixed. Three hashes is the deepest form spelled out
/// here; a deeper one falls through to the ordinary string item, which ends at
/// the first quote rather than swallowing the rest of the file.
fn rust_raw_strings() -> Vec<GrammarItem> {
    let mut items = Vec::new();
    for prefix in ["br", "r"] {
        for hashes in [3_usize, 2, 1, 0] {
            let marks = "#".repeat(hashes);
            items.push(raw_string(
                &format!("{prefix}{marks}\""),
                &format!("\"{marks}"),
            ));
        }
    }
    items
}

/// Rust sources.
#[must_use]
pub fn rust() -> FileFormat {
    let mut items = vec![line_comment("//"), block_comment("/*", "*/")];
    items.extend(rust_raw_strings());
    items.extend([
        // A character literal always carries a closing apostrophe, which is
        // what keeps a lifetime from being read as one.
        pattern_anywhere(
            STRING,
            r"b?'(\\u\{[0-9a-fA-F]{1,6}\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'",
        ),
        // Rust strings may contain literal line breaks, so this carries state
        // into the following line until its closing quote is reached.
        quoted_spanning(STRING, "\"", Some('\\')),
        pattern_anywhere(DIRECTIVE, r"#!?\[[^\]]*\]"),
        pattern(
            NUMBER,
            r"(0[xXbBoO][0-9a-fA-F_]+|\d[\d_]*\.?[\d_]*([eE][-+]?\d+)?)(u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64)?",
        ),
        words(KEYWORD, RUST_KEYWORDS, true),
        pattern(IDENTIFIER, r"[A-Za-z_][A-Za-z0-9_]*"),
    ]);
    FileFormat::text("Rust", "*.rs")
        .with_description("Rust source files.")
        .with_grammar(Grammar::from_items(items))
        .with_tab_stop(4)
}

fn markup_grammar(directive: &str) -> Grammar {
    Grammar::from_items(vec![
        block_comment("<!--", "-->"),
        pattern_anywhere(DIRECTIVE, directive),
        pattern_anywhere("Tag", r"</?[A-Za-z_][A-Za-z0-9_.:-]*|/?>"),
        quoted(STRING, "\"", None),
        quoted(STRING, "'", None),
        pattern_anywhere("Attribute", r"[A-Za-z_][A-Za-z0-9_.:-]*[ \t]*="),
        c_style_number(),
        c_style_identifier(),
        words(KEYWORD, &["true", "false"], false),
    ])
}

/// HTML documents.
#[must_use]
pub fn html() -> FileFormat {
    FileFormat::text("HTML", "*.html;*.htm;*.xhtml;*.shtml")
        .with_description(
            "HTML documents. Tagged languages are matched item by item rather than parsed, \
             so nesting is approximated.",
        )
        .with_grammar(markup_grammar(r"<!(?i:doctype)[^>]*>"))
        .with_tab_stop(2)
}

/// XML documents.
#[must_use]
pub fn xml() -> FileFormat {
    FileFormat::text(
        "XML",
        "*.xml;*.xsd;*.xsl;*.xslt;*.svg;*.rss;*.config;*.csproj;*.plist",
    )
    .with_description("XML documents.")
    .with_grammar(markup_grammar(r"<\?[^?]*\?>"))
    .with_tab_stop(2)
}

/// Style sheets.
#[must_use]
pub fn css() -> FileFormat {
    FileFormat::text("CSS", "*.css;*.scss;*.sass;*.less")
        .with_description("Cascading style sheets.")
        .with_grammar(Grammar::from_items(vec![
            block_comment("/*", "*/"),
            line_comment("//"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", Some('\\')),
            pattern_anywhere(DIRECTIVE, r"@[A-Za-z-]+"),
            pattern_anywhere("Color", r"#[0-9a-fA-F]{3,8}"),
            pattern_anywhere("Property", r"[-A-Za-z][-A-Za-z0-9]*[ \t]*:"),
            pattern(
                NUMBER,
                r"\d*\.?\d+(px|em|rem|ex|ch|vh|vw|pt|pc|cm|mm|in|s|ms|deg|%)?",
            ),
            words(
                KEYWORD,
                &["inherit", "initial", "important", "none", "auto", "unset"],
                false,
            ),
            pattern(IDENTIFIER, r"[-A-Za-z_][-A-Za-z0-9_]*"),
        ]))
        .with_tab_stop(2)
}

/// JSON documents.
#[must_use]
pub fn json() -> FileFormat {
    FileFormat::text("JSON", "*.json;*.jsonc;*.jsonl")
        .with_description("JSON documents. Comment items serve the commented dialects.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("//"),
            block_comment("/*", "*/"),
            quoted(STRING, "\"", Some('\\')),
            words(KEYWORD, &["true", "false", "null"], true),
            pattern(NUMBER, r"-?\d+\.?\d*([eE][-+]?\d+)?"),
            c_style_identifier(),
        ]))
        .with_tab_stop(2)
}

/// SQL scripts.
#[must_use]
pub fn sql() -> FileFormat {
    FileFormat::text("SQL", "*.sql;*.ddl;*.dml")
        .with_description("SQL scripts. Keywords match in any character case.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("--"),
            block_comment("/*", "*/"),
            quoted(STRING, "'", None),
            quoted("Quoted name", "\"", None),
            quoted_between("Quoted name", "[", "]", None),
            words(KEYWORD, SQL_KEYWORDS, false),
            pattern(NUMBER, r"\d+\.?\d*([eE][-+]?\d+)?"),
            pattern_anywhere("Variable", r"[@:][A-Za-z_][A-Za-z0-9_]*"),
            c_style_identifier(),
        ]))
        .with_tab_stop(4)
}

/// Initialization files.
#[must_use]
pub fn ini() -> FileFormat {
    FileFormat::text("INI", "*.ini;*.cfg;*.inf;*.reg;*.properties;*.desktop")
        .with_description("Key and value initialization files.")
        .with_grammar(Grammar::from_items(vec![
            line_comment(";"),
            line_comment("#"),
            pattern_anywhere("Section", r"\[[^\]]*\]"),
            quoted(STRING, "\"", Some('\\')),
            pattern_anywhere("Key", r"^[ \t]*[^=\r\n\[;#]+="),
            pattern(NUMBER, r"\d+\.?\d*"),
            c_style_identifier(),
            words(KEYWORD, &["true", "false", "yes", "no"], false),
        ]))
}

/// TOML documents.
#[must_use]
pub fn toml() -> FileFormat {
    FileFormat::text("TOML", "*.toml")
        .with_description("TOML configuration files.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("#"),
            quoted_spanning(STRING, "\"\"\"", None),
            quoted_spanning(STRING, "'''", None),
            pattern_anywhere("Section", r"^[ \t]*\[\[?[^\]]*\]\]?"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", None),
            pattern_anywhere("Key", r"^[ \t]*[A-Za-z0-9_.-]+[ \t]*="),
            words(KEYWORD, &["true", "false"], true),
            pattern(
                NUMBER,
                r"0[xXbBoO][0-9a-fA-F_]+|[-+]?\d[\d_]*\.?[\d_]*([eE][-+]?\d+)?",
            ),
            c_style_identifier(),
        ]))
        .with_tab_stop(4)
}

/// YAML documents.
#[must_use]
pub fn yaml() -> FileFormat {
    FileFormat::text("YAML", "*.yml;*.yaml")
        .with_description("YAML documents.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("#"),
            quoted(STRING, "\"", Some('\\')),
            quoted(STRING, "'", None),
            pattern_anywhere("Key", r"^[ \t-]*[A-Za-z_][A-Za-z0-9_. -]*:"),
            pattern_anywhere(DIRECTIVE, r"^---|^\.\.\.|^%[A-Z]+"),
            words(
                KEYWORD,
                &["true", "false", "null", "yes", "no", "on", "off", "~"],
                false,
            ),
            pattern(NUMBER, r"[-+]?\d+\.?\d*([eE][-+]?\d+)?"),
            c_style_identifier(),
        ]))
        .with_tab_stop(2)
}

/// Shell scripts.
#[must_use]
pub fn shell() -> FileFormat {
    FileFormat::text(
        "Shell",
        "*.sh;*.bash;*.zsh;*.ksh;*.command;.bashrc;.bash_profile;.profile;.zshrc",
    )
    .with_description("Bourne-family shell scripts.")
    .with_grammar(Grammar::from_items(vec![
        pattern_anywhere(DIRECTIVE, r"^#!.*"),
        line_comment("#"),
        // A bare apostrophe is ordinary in shell text, so a string item that
        // spanned lines would color the rest of a file from the first stray
        // one. A multi-line quotation is colored per line instead.
        quoted(STRING, "\"", Some('\\')),
        quoted(STRING, "'", None),
        pattern_anywhere(
            "Variable",
            r"\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\$[0-9*@#?$!-]",
        ),
        words(KEYWORD, SHELL_KEYWORDS, true),
        pattern(NUMBER, r"\d+"),
        c_style_identifier(),
    ]))
}

/// `PowerShell` scripts.
#[must_use]
pub fn powershell() -> FileFormat {
    FileFormat::text("PowerShell", "*.ps1;*.psm1;*.psd1;*.ps1xml")
        .with_description("PowerShell scripts and modules.")
        .with_grammar(Grammar::from_items(vec![
            block_comment("<#", "#>"),
            line_comment("#"),
            // A here-string body is colored a line at a time rather than as one
            // element, so a stray quote cannot color the rest of a file.
            quoted(STRING, "\"", Some('`')),
            quoted(STRING, "'", None),
            pattern_anywhere("Variable", r"\$\{[^}]*\}|\$[A-Za-z_:][A-Za-z0-9_:]*"),
            pattern_anywhere(DIRECTIVE, r"\[[A-Za-z_][A-Za-z0-9_.\[\]]*\]"),
            words(KEYWORD, POWERSHELL_KEYWORDS, false),
            pattern_anywhere(
                "Operator",
                r"-(eq|ne|lt|le|gt|ge|like|match|not|and|or|in|is)\b",
            ),
            pattern(
                NUMBER,
                r"0[xX][0-9a-fA-F]+|\d+\.?\d*([eE][-+]?\d+)?[kKmMgG]?[bB]?",
            ),
            c_style_identifier(),
        ]))
        .with_tab_stop(4)
}

/// Pascal and Delphi sources.
#[must_use]
pub fn pascal() -> FileFormat {
    FileFormat::text("Pascal", "*.pas;*.dpr;*.dpk;*.inc;*.pp;*.lpr")
        .with_description("Pascal and Delphi source files. Keywords match in any character case.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("//"),
            block_comment("{", "}"),
            block_comment("(*", "*)"),
            // The doubled apostrophe that escapes an apostrophe inside a
            // literal closes one literal and opens the next, which colors the
            // same span either way.
            quoted(STRING, "'", None),
            words(KEYWORD, PASCAL_KEYWORDS, false),
            pattern(NUMBER, r"\$[0-9a-fA-F]+|\d+\.?\d*([eE][-+]?\d+)?"),
            c_style_identifier(),
        ]))
        .with_tab_stop(2)
}

/// Visual Basic sources.
#[must_use]
pub fn visual_basic() -> FileFormat {
    FileFormat::text("Visual Basic", "*.vb;*.vbs;*.bas;*.cls;*.frm;*.ctl")
        .with_description("Visual Basic source files. Keywords match in any character case.")
        .with_grammar(Grammar::from_items(vec![
            line_comment("'"),
            GrammarItem::new(
                COMMENT,
                ItemKind::Delimited {
                    start: "REM ".to_owned(),
                    stop: String::new(),
                    stop_at_end_of_line: true,
                    escape: None,
                    line_spanning: false,
                    continue_after_escaped_newline: false,
                    options: MatchOptions::literal_any_case(),
                },
            ),
            quoted(STRING, "\"", None),
            pattern_anywhere(
                DIRECTIVE,
                r"^[ \t]*#(If|Else|End If|Region|End Region|Const)",
            ),
            words(KEYWORD, VB_KEYWORDS, false),
            pattern(NUMBER, r"&[hH][0-9a-fA-F]+|\d+\.?\d*([eE][-+]?\d+)?"),
            c_style_identifier(),
        ]))
        .with_tab_stop(4)
}

/// Markdown documents.
#[must_use]
pub fn markdown() -> FileFormat {
    FileFormat::text("Markdown", "*.md;*.markdown;*.mdown;*.mkd")
        .with_description("Markdown documents.")
        .with_grammar(Grammar::from_items(vec![
            GrammarItem::new(
                "Code",
                ItemKind::Delimited {
                    start: "```".to_owned(),
                    stop: "```".to_owned(),
                    stop_at_end_of_line: false,
                    escape: None,
                    line_spanning: true,
                    continue_after_escaped_newline: false,
                    options: MatchOptions::literal(),
                },
            ),
            block_comment("<!--", "-->"),
            pattern_anywhere("Heading", r"^#{1,6}[ \t].*"),
            pattern_anywhere("Heading", r"^[ \t]*(=+|-{2,})[ \t]*$"),
            quoted("Code", "`", None),
            pattern_anywhere("Link", r"\[[^\]]*\]\([^)]*\)|<https?://[^>]*>|https?://\S+"),
            pattern_anywhere(DIRECTIVE, r"^[ \t]*([-*+]|\d+\.)[ \t]|^[ \t]*>"),
            quoted(STRING, "\"", None),
            pattern(NUMBER, r"\d+\.?\d*"),
            words(KEYWORD, &["TODO", "NOTE", "FIXME", "WARNING"], true),
            c_style_identifier(),
        ]))
        .with_tab_stop(4)
}

/// A table format for delimiter separated values.
///
/// It is not part of [`formats`], because a text comparison is the usual
/// treatment for these files; it exists so the table settings have a worked
/// example a user can copy.
#[must_use]
pub fn comma_separated_values() -> FileFormat {
    use crate::format::{TableLayout, TableSettings};
    let mut format = FileFormat::text("Comma Separated Values", "*.csv")
        .with_description("Delimiter separated values, compared as a table.")
        .with_kind(FormatKind::Table);
    format.table = Some(TableSettings {
        layout: TableLayout::Delimited.into(),
        delimiters: ",".to_owned(),
        text_qualifier: Some('"'),
        ..TableSettings::default()
    });
    format
}

/// A table format for tab separated values.
#[must_use]
pub fn tab_separated_values() -> FileFormat {
    use crate::format::{TableLayout, TableSettings};
    let mut format = FileFormat::text("Tab Separated Values", "*.tsv;*.tab")
        .with_description("Tab separated values, compared as a table.")
        .with_kind(FormatKind::Table);
    format.table = Some(TableSettings {
        layout: TableLayout::Delimited.into(),
        delimiters: "\t".to_owned(),
        text_qualifier: Some('"'),
        ..TableSettings::default()
    });
    format
}

/// A picture format claiming the raster filenames the picture engine decodes.
///
/// It is not part of [`formats`], which holds text formats only.
#[must_use]
pub fn pictures() -> FileFormat {
    FileFormat::text(
        "Pictures",
        "*.png;*.jpg;*.jpeg;*.gif;*.bmp;*.tif;*.tiff;*.ico;*.tga;*.webp;*.pnm;*.pbm;*.pgm;*.ppm",
    )
    .with_description("Raster pictures, compared pixel by pixel.")
    .with_kind(FormatKind::Picture)
}

/// A registry holding every stock format, the table formats and the picture
/// format, so a lookup by filename names the comparison type.
#[must_use]
pub fn registry_with_media() -> FormatRegistry {
    let mut registry = registry();
    registry.push(comma_separated_values());
    registry.push(tab_separated_values());
    registry.push(pictures());
    registry
}
