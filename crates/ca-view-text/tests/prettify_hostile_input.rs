//! Hostile and malformed YAML and TOML for the comparison formatter.
//!
//! Release builds abort on a panic, so every input here must format or be
//! refused without one. The tests run on a thread with the stack size of the
//! worker that formats in the application.

#![allow(clippy::expect_used)]

use ca_view_text::prettify::{format, StructuredFormat};
use std::fmt::Write as _;

const WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;

fn on_worker_stack(body: impl FnOnce() + Send + 'static) {
    let worker = std::thread::Builder::new()
        .stack_size(WORKER_STACK_BYTES)
        .spawn(body)
        .expect("start the worker thread");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

/// Formatted text formats to itself, including comments. YAML output has
/// LF line breaks only.
fn assert_formats_or_refuses(text: &str, kind: StructuredFormat) {
    let Ok(formatted) = format(text, kind) else {
        return;
    };
    let again = format(&formatted, kind);
    assert!(
        again.is_ok(),
        "output {formatted:?} refused for {text:?}: {again:?}"
    );
    assert_eq!(again.as_ref(), Ok(&formatted), "not stable for {text:?}");
    if kind == StructuredFormat::Yaml {
        assert!(!formatted.contains('\r'), "CR in output for {text:?}");
    }
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 >> 33).unwrap_or(0)
    }

    fn below(&mut self, bound: usize) -> usize {
        self.next() % bound.max(1)
    }
}

/// Applies one to four edits: inserted syntax, odd characters, deleted or
/// repeated ranges, and truncation.
fn mutate(seed: &str, tokens: &[&str], random: &mut Random) -> String {
    const ODD: [char; 14] = [
        '\0',
        '\u{feff}',
        '\r',
        '\t',
        '\u{7f}',
        '\u{85}',
        '\u{2028}',
        'é',
        '\u{1f600}',
        '\\',
        '%',
        '\u{b}',
        '\u{c}',
        ' ',
    ];
    let mut text: Vec<char> = seed.chars().collect();
    for _ in 0..=random.below(4) {
        let at = random.below(text.len() + 1);
        match random.below(6) {
            0 | 1 => {
                let token = tokens[random.below(tokens.len())];
                text.splice(at..at, token.chars());
            }
            2 => text.insert(at, ODD[random.below(ODD.len())]),
            3 => {
                let end = (at + 1 + random.below(8)).min(text.len());
                if at < end {
                    text.drain(at..end);
                }
            }
            4 => {
                let end = (at + 1 + random.below(16)).min(text.len());
                let repeated: Vec<char> = text.get(at..end).unwrap_or_default().to_vec();
                text.splice(at..at, repeated);
            }
            _ => text.truncate(at),
        }
    }
    text.into_iter().collect()
}

const YAML_SEEDS: [&str; 6] = [
    "%YAML 1.2\n%TAG !e! tag:example.com,2000:\n--- !e!root\nbase: &b {x: 1, y: [a, 'b', \"c\\n\"]}\nuse: *b\n? plain key\n: value # trailing\n...\n",
    "# head\n- &a !!str one\n- [*a, {k: v}, []]\n- |+\n  keep\n\n- >-\n  folded\n  text\n- \"multi\n  line\"\n- 'it''s'\n",
    "a:\n  b:\n    - c: 1\n      d: [x, y]\n    - - nested\n      - seq\n  e: {}\n# end\n",
    "{\"json\": [1, 2.5, true, null, {\"deep\": [[], {}]}], \"s\": \"\\u00e9\"}\n",
    "--- |\n  literal\n--- >\n  folded\n...\n--- [a, b]\n--- {a: b}\n",
    "key: value\n? [complex, key]\n: v\n!!set {a, b}: x\nlist:\n- a\n-   b\n",
];

const YAML_TOKENS: [&str; 30] = [
    "\n",
    " ",
    "  ",
    "- ",
    ": ",
    ":",
    "? ",
    "[",
    "]",
    "{",
    "}",
    ",",
    "#",
    " # c\n",
    "&a ",
    "*a",
    "!t ",
    "!!str ",
    "|",
    ">-",
    "|+2",
    "'",
    "\"",
    "---\n",
    "...\n",
    "%YAML 1.2\n",
    "\t",
    "\\",
    "\r\n",
    "\u{feff}",
];

const TOML_SEEDS: [&str; 5] = [
    "# head\ntitle = \"x\" # t\n[owner]\nname = 'n'\ndob = 1979-05-27T07:32:00-08:00\n[database]\nports = [ 8000, 8001, 8002 ]\ndata = [ [\"delta\", \"phi\"], [3.14] ]\ntemp = { cpu = 79.5, case = 72.0 }\n",
    "[[products]]\nname = \"Hammer\"\nsku = 738594937\n\n[[products]]\n\n[[products]]\nname = \"Nail\"\ncolor = \"gray\"\n",
    "a.b.c = 1\na.d = \"\"\"\nmulti\\\n  line\"\"\"\ne = '''raw\n'''\n[f.g]\nh = 0x_ff\ni = -inf\nj = [\n  1, # one\n  2,\n]\n",
    "k = { a = 1, b = { c = [1, 2, { d = 3 }] } }\n\"quoted key\" = true\n'lit' = 1_000\n",
    "t = {\n  a = 1, # c\n  b = 2,\n}\n[x.y.z]\nw = 1e3\n",
];

const TOML_TOKENS: [&str; 26] = [
    "\n",
    " ",
    "=",
    " = ",
    ".",
    "[",
    "]",
    "[[",
    "]]",
    "{",
    "}",
    ",",
    "#",
    " # c\n",
    "\"",
    "'",
    "\"\"\"",
    "'''",
    "\\",
    "1",
    "1.5",
    "true",
    "1979-05-27",
    "\r\n",
    "\t",
    "\u{feff}",
];

#[test]
fn mutated_yaml_and_toml_are_formatted_or_refused_without_a_panic() {
    on_worker_stack(|| {
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for round in 0..3_000 {
            let seed = YAML_SEEDS[round % YAML_SEEDS.len()];
            let text = mutate(seed, &YAML_TOKENS, &mut random);
            assert_formats_or_refuses(&text, StructuredFormat::Yaml);
            let seed = TOML_SEEDS[round % TOML_SEEDS.len()];
            let text = mutate(seed, &TOML_TOKENS, &mut random);
            assert_formats_or_refuses(&text, StructuredFormat::Toml);
        }
    });
}

#[test]
fn generated_deep_and_repeated_structures_are_formatted_or_refused() {
    on_worker_stack(|| {
        for depth in [1, 79, 80, 81, 254, 255, 256, 257, 300, 5_000] {
            for text in [
                format!("{}{}", "[".repeat(depth), "]".repeat(depth)),
                format!("{}x{}", "{a: ".repeat(depth), "}".repeat(depth)),
                format!("{}x\n", "- ".repeat(depth)),
                (0..depth.min(400)).fold(String::new(), |mut text, level| {
                    let _ = writeln!(text, "{}k:", " ".repeat(level));
                    text
                }),
                format!("{}x\n", "? ".repeat(depth)),
                format!("{}x\n", "&a ".repeat(depth)),
            ] {
                assert_formats_or_refuses(&text, StructuredFormat::Yaml);
            }
            for text in [
                format!("a = {}{}", "[".repeat(depth), "]".repeat(depth)),
                format!("a = {}{}", "{b = ".repeat(depth), "}".repeat(depth)),
                format!("{} = 1", vec!["k"; depth].join(".")),
                format!("[{}]\n", vec!["t"; depth].join(".")),
                format!("{} = {{}}", vec!["k"; depth].join(".")),
            ] {
                assert_formats_or_refuses(&text, StructuredFormat::Toml);
            }
        }
    });
}

#[test]
fn known_hard_yaml_is_formatted_or_refused() {
    on_worker_stack(|| {
        let laughs = {
            let mut text = String::from("a: &a [\"lol\", \"lol\", \"lol\", \"lol\"]\n");
            for (name, previous) in ["b", "c", "d", "e", "f", "g", "h", "i"]
                .iter()
                .zip(["a", "b", "c", "d", "e", "f", "g", "h"])
            {
                let _ = writeln!(
                    text,
                    "{name}: &{name} [*{previous}, *{previous}, *{previous}, *{previous}]"
                );
            }
            text
        };
        let formatted = format(&laughs, StructuredFormat::Yaml);
        assert!(
            formatted
                .as_ref()
                .is_ok_and(|text| text.len() < 2 * laughs.len()),
            "{formatted:?}"
        );

        let chain: String = std::iter::once("a0: &a0 x\n".to_owned())
            .chain((1..2_000).map(|i| format!("a{i}: &a{i} [*a{}]\n", i - 1)))
            .collect();
        assert!(format(&chain, StructuredFormat::Yaml).is_ok());

        let long_line = format!("k: {}\n", "word ".repeat(200_000));
        assert!(format(&long_line, StructuredFormat::Yaml).is_ok());

        for refused in [
            "a:\t1\n",
            "a: \"\\ud800\"\n",
            "a: 1\0\n",
            "a: \"x\0\"\n",
            "a: \u{1}\n",
            "a: 1\n\u{feff}b: 2\n",
            "\u{feff}\u{feff}a: 1\n",
            "\r\u{feff}{\u{feff}\"",
            "- a\nb: c\n",
            "a: 1\n a: 2\n",
            "%YAML 1.2\n%YAML 1.2\n---\na\n",
        ] {
            assert!(
                format(refused, StructuredFormat::Yaml).is_err(),
                "{refused:?}"
            );
        }

        for text in [
            "a:\n\t- b\n",
            "- \tx\n",
            "a:    \t   b\n",
            "a:\n     b:\n          c: 1\n     d: 2\n",
            "? a\n? b\n: c\n",
            "? - a\n  - b\n: c\n",
            "? |\n  k\n: v\n",
            "[? a : b, ? c]\n",
            "{? a : b}\n",
            "%TAG ! tag:example.com,2000:\n--- !x a\n",
            "%RESERVED param\n--- a\n",
            "a\n---\nb\n...\n...\nc\n",
            "a: 1\n--- \nb: 2\n",
            "---\n---\n---\n",
            "a: \"\u{85}\u{2028}\"\n",
            "a: \"\\0\\a\\e\\N\\_\\L\\P\\x41\\u0041\\U00000041\"\n",
            "a: \"\\ud83d\\ude00\"\n",
            "a: 'line\n\n  next'\n",
            "a: >\n\n  x\n\n\n  y\n\n",
            "a: |-\n   x\n  \n",
            "- |\n  a\n- >+\n  b\n\n- c\n",
            "a: !<tag:yaml.org,2002:str> x\nb: !!binary YQ==\n",
            "*a\n",
            "&a a: &b b\n*b : *a\n",
            "a: [b, c]: d\n",
            "{a: [b, {c: [d, {e: f}]}]}\n",
            "[a, b, c,]\n",
            "{a: 1,}\n",
            "- 'a\r\n  b'\r\n",
            "a: b # c\r# d\rc: e\r",
        ] {
            assert_formats_or_refuses(text, StructuredFormat::Yaml);
        }
    });
}

#[test]
fn known_hard_toml_is_formatted_or_refused() {
    on_worker_stack(|| {
        for refused in [
            "a = 99999999999999999999999\n",
            "a = 0x1_0000_0000_0000_0000\n",
            "[a]\nb = 1\n[[a]]\n",
            "[[a]]\n[a]\n",
            "a.b = 1\n[a]\nc = 2\n",
            "a = { b = 1 }\n[a]\n",
            "a = { b = 1 }\na.c = 2\n",
            "a = 1\na = 2\n",
            "a = 1979-13-27\n",
            "a = \"\\ud800\"\n",
            "a = \"x\0\"\n",
            "a = 1\n\u{feff}b = 2\n",
            "a = \"\"\"\r x\"\"\"\n",
            &format!("a = {}1{}\n", "[".repeat(81), "]".repeat(81)),
            &format!("a = {}{}\n", "{b = ".repeat(81), "}".repeat(81)),
        ] {
            assert!(
                format(refused, StructuredFormat::Toml).is_err(),
                "{refused:?}"
            );
        }
        for text in [
            "\u{feff}a = 1\n",
            "a = \"\"\"\r\nx\r\ny\"\"\"\r\n",
            "a = '''\r\nx\r\n'''\r\n",
            "a = 1979-05-27T07:32:00.999999999999-07:00\nb = 07:32:00\nc = 1979-05-27\nd = 1979-05-27 07:32:00Z\n",
            "a = 9223372036854775807\nb = -9223372036854775808\nc = 0o777\nd = 0b1\ne = 1e308\nf = nan\n",
            "[a.b]\nc = 1\n[a]\nd = 2\n",
            "[a]\n[a.b]\n[a.b.c]\n",
            "[[a.b]]\nc = 1\n[a]\nd = 2\n",
            "[[a]]\n[a.b]\nc = 1\n[[a]]\n[a.b]\nc = 2\n",
            "a = [{ b = 1 }, { c = [{ d = 2 }] }]\n",
            "a = [[{}]]\n",
            "a.\"b.c\".'d' = 1\n",
            "\"\" = 1\n'' = 2\n",
            "a = \"\\\\ \\\" \\b \\t \\n \\f \\r \\u00e9 \\U0001F600\"\n",
            "a = \"\"\"\\\n   \\\n   x\"\"\"\n",
            "a = [\n  # only a comment\n]\n",
            "a = { }\nb = []\n",
            "# c1\n# c2\n\n[t] # h\n# c3\n",
            &format!("a = {}1{}\n", "[".repeat(79), "]".repeat(79)),
            &format!("{} = 1\n", vec!["k"; 79].join(".")),
        ] {
            assert_formats_or_refuses(text, StructuredFormat::Toml);
        }
    });
}

#[test]
fn nesting_at_the_limit_formats_on_the_worker_stack() {
    on_worker_stack(|| {
        let flow = format!("{}{}", "[".repeat(255), "]".repeat(255));
        assert!(format(&flow, StructuredFormat::Yaml).is_ok());
        let block = format!("{}x\n", "- ".repeat(255));
        assert!(format(&block, StructuredFormat::Yaml).is_ok());
        let key = vec!["k"; 79].join(".");
        // A 79-segment key over 79 nested arrays nests 158 levels.
        let deep = format!("{key} = {}1{}\n", "[".repeat(79), "]".repeat(79));
        let formatted = format(&deep, StructuredFormat::Toml);
        assert!(formatted.is_ok(), "{formatted:?}");
        // Three inline tables under 79-segment keys nest 240 tables; the
        // projected headers exceed the parser's key limit, so the writer
        // reports the nesting limit before reparsing.
        let deeper = format!("{key} = {{ {key} = {{ {key} = {{ z = [[[1]]] }} }} }}\n");
        assert!(format(&deeper, StructuredFormat::Toml)
            .is_err_and(|error| error.contains("80 segments")));
    });
}
