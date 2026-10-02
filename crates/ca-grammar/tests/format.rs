//! Format lookup, conversion command expansion, and forward-compatible
//! persistence.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use ca_grammar::builtin;
use ca_grammar::format::{
    ConversionPaths, ConversionSettings, ExternalSettings, FileFormat, FormatKind, FormatRegistry,
};
use ca_grammar::Extensible;

#[test]
fn lookup_stops_at_the_first_enabled_entry_whose_mask_matches() {
    let mut registry = FormatRegistry::new(FileFormat::text("Everything else", "*"));
    registry.push(FileFormat::text("Headers", "*.h"));
    registry.push(FileFormat::text("C/C++", "*.c;*.h"));
    assert_eq!(registry.lookup("a.h").name, "Headers");
    assert_eq!(registry.lookup("a.c").name, "C/C++");
    assert_eq!(registry.lookup("a.txt").name, "Everything else");
}

#[test]
fn reordering_changes_which_format_wins() {
    let mut registry = FormatRegistry::new(FileFormat::text("Everything else", "*"));
    registry.push(FileFormat::text("Headers", "*.h"));
    registry.push(FileFormat::text("C/C++", "*.c;*.h"));
    assert_eq!(registry.move_down(0), Some(1));
    assert_eq!(registry.lookup("a.h").name, "C/C++");
    assert_eq!(registry.move_up(0), None);
    assert_eq!(registry.move_down(1), None);
}

#[test]
fn a_disabled_entry_is_skipped() {
    let mut registry = FormatRegistry::new(FileFormat::text("Everything else", "*"));
    let mut headers = FileFormat::text("Headers", "*.h");
    headers.enabled = false;
    registry.push(headers);
    registry.push(FileFormat::text("C/C++", "*.c;*.h"));
    assert_eq!(registry.lookup("a.h").name, "C/C++");
}

#[test]
fn an_empty_mask_claims_nothing_and_is_reachable_only_by_name() {
    let mut registry = FormatRegistry::new(FileFormat::text("Everything else", "*"));
    registry.push(FileFormat::text("Manual only", ""));
    assert_eq!(registry.lookup("anything").name, "Everything else");
    assert!(registry.by_name("Manual only").is_some());
    assert!(!registry.is_shadowed(0));
}

#[test]
fn an_entry_every_filename_of_which_is_claimed_above_is_reported_as_shadowed() {
    let mut registry = FormatRegistry::new(FileFormat::text("Everything else", "*"));
    registry.push(FileFormat::text("Catch all", "*"));
    registry.push(FileFormat::text("C/C++", "*.c"));
    assert!(!registry.is_shadowed(0));
    assert!(registry.is_shadowed(1));
}

#[test]
fn the_stock_registry_routes_familiar_names() {
    let registry = builtin::registry();
    for (path, want) in [
        ("main.c", "C/C++"),
        ("main.cpp", "C/C++"),
        ("Program.cs", "C#"),
        ("Main.java", "Java"),
        ("app.mjs", "JavaScript"),
        ("app.tsx", "TypeScript"),
        ("setup.py", "Python"),
        ("lib.rs", "Rust"),
        ("main.go", "Go"),
        ("server.kt", "Kotlin"),
        ("App.swift", "Swift"),
        ("app.dart", "Dart"),
        ("Main.scala", "Scala"),
        ("Rakefile", "Ruby"),
        ("index.php", "PHP"),
        ("init.lua", "Lua"),
        ("script.pl", "Perl"),
        ("analysis.R", "R"),
        ("Main.hs", "Haskell"),
        ("CMakeLists.txt", "CMake"),
        ("Dockerfile", "Dockerfile"),
        ("Makefile", "Makefile"),
        ("index.html", "HTML"),
        ("pom.xml", "XML"),
        ("schema.xsd", "XML"),
        ("icon.svg", "XML"),
        ("site.scss", "CSS"),
        ("package.json", "JSON"),
        ("settings.jsonc", "JSON"),
        ("events.jsonl", "JSON"),
        ("schema.sql", "SQL"),
        ("desktop.ini", "INI"),
        ("app.properties", "INI"),
        ("Cargo.toml", "TOML"),
        ("ci.yaml", "YAML"),
        ("values.yml", "YAML"),
        ("build.sh", "Shell"),
        ("Deploy.ps1", "PowerShell"),
        ("unit1.pas", "Pascal"),
        ("Form1.frm", "Visual Basic"),
        ("README.md", "Markdown"),
        ("notes.rtfd", "Everything else"),
    ] {
        assert_eq!(registry.lookup(path).name, want, "for {path}");
    }
}

#[test]
fn every_stock_grammar_compiles_for_its_lexer() {
    for format in builtin::formats() {
        ca_grammar::Lexer::new(&format.grammar)
            .unwrap_or_else(|error| panic!("{} grammar did not compile: {error}", format.name));
    }
}

#[test]
fn a_path_resolves_by_its_trailing_component() {
    let registry = builtin::registry();
    assert_eq!(registry.lookup(r"C:\src\deep\main.c").name, "C/C++");
    assert_eq!(registry.lookup("/home/x/main.c").name, "C/C++");
}

/// The program and arguments of a built command, as plain strings.
fn built(template: &str, paths: ConversionPaths<'_>) -> (String, Vec<String>) {
    let command = ConversionSettings::build_command(template, paths).expect("a program");
    (
        command.program.to_string_lossy().into_owned(),
        command
            .arguments
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
    )
}

fn simple_paths() -> ConversionPaths<'static> {
    ConversionPaths {
        source: "in.doc",
        target: "out.txt",
        original: "orig.doc",
    }
}

#[test]
fn conversion_variables_expand_to_their_paths() {
    let (program, arguments) = built("tool %s %t %o 100%%", simple_paths());
    assert_eq!(program, "tool");
    assert_eq!(arguments, ["in.doc", "out.txt", "orig.doc", "100%"]);
    let (program, arguments) = built("tool %q %", simple_paths());
    assert_eq!(program, "tool");
    assert_eq!(arguments, ["%q", "%"]);
}

#[test]
fn an_empty_template_names_no_program() {
    assert!(ConversionSettings::build_command("   ", simple_paths()).is_none());
    assert!(ConversionSettings::build_command("", simple_paths()).is_none());
}

#[test]
fn a_path_holding_spaces_stays_one_argument() {
    let paths = ConversionPaths {
        source: "C:\\my files\\a b.txt",
        target: "out dir/t.txt",
        original: "o.doc",
    };
    let (program, arguments) = built("tool %s %t", paths);
    assert_eq!(program, "tool");
    assert_eq!(arguments, ["C:\\my files\\a b.txt", "out dir/t.txt"]);
}

#[test]
fn a_path_holding_quotes_and_shell_operators_stays_one_argument() {
    let paths = ConversionPaths {
        source: "a.txt\" & calc.exe & echo \"",
        target: "b|c;d`e$f.txt",
        original: "o.doc",
    };
    let (program, arguments) = built("tool %s %t", paths);
    assert_eq!(program, "tool");
    assert_eq!(arguments, ["a.txt\" & calc.exe & echo \"", "b|c;d`e$f.txt"]);
}

#[test]
fn a_path_holding_percent_signs_and_carets_is_not_re_expanded() {
    let paths = ConversionPaths {
        source: "%t%%^x.txt",
        target: "T.txt",
        original: "o.doc",
    };
    let (program, arguments) = built("tool %s %t", paths);
    assert_eq!(program, "tool");
    assert_eq!(arguments, ["%t%%^x.txt", "T.txt"]);
}

#[test]
fn quotes_in_the_template_group_an_argument_and_are_removed() {
    let paths = ConversionPaths {
        source: "a b.txt",
        target: "t.txt",
        original: "o.doc",
    };
    let (program, arguments) = built("\"C:\\Program Files\\tool.exe\" \"%s\" -q", paths);
    assert_eq!(program, "C:\\Program Files\\tool.exe");
    assert_eq!(arguments, ["a b.txt", "-q"]);
}

#[test]
fn a_variable_inside_a_larger_argument_fills_only_its_slot() {
    let paths = ConversionPaths {
        source: "s.txt",
        target: "out dir/t.txt",
        original: "o.doc",
    };
    let (_, arguments) = built("tool --out=%t --in=%s", paths);
    assert_eq!(arguments, ["--out=out dir/t.txt", "--in=s.txt"]);
}

#[test]
fn a_path_beginning_with_a_dash_stays_exactly_one_argument() {
    let paths = ConversionPaths {
        source: "-o evil",
        target: "t.txt",
        original: "o.doc",
    };
    let (_, arguments) = built("tool %s %t", paths);
    assert_eq!(arguments, ["-o evil", "t.txt"]);
    // A template keeps such a path out of option position by embedding it.
    let (_, arguments) = built("tool --input=%s", paths);
    assert_eq!(arguments, ["--input=-o evil"]);
}

#[test]
fn a_path_holding_characters_outside_ascii_survives_intact() {
    let paths = ConversionPaths {
        source: "Ω 文件 café.txt",
        target: "t.txt",
        original: "o.doc",
    };
    let (_, arguments) = built("tool %s", paths);
    assert_eq!(arguments, ["Ω 文件 café.txt"]);
}

#[test]
fn runs_of_separators_in_the_template_do_not_make_empty_arguments() {
    let (program, arguments) = built("  tool \t  %s  ", simple_paths());
    assert_eq!(program, "tool");
    assert_eq!(arguments, ["in.doc"]);
}

#[test]
fn a_conversion_needs_both_a_clean_exit_and_output() {
    assert!(ConversionSettings::conversion_succeeded(0, 12));
    assert!(!ConversionSettings::conversion_succeeded(0, 0));
    assert!(!ConversionSettings::conversion_succeeded(1, 12));
}

#[test]
fn a_quick_command_reports_a_verdict_only_for_the_two_documented_codes() {
    assert_eq!(ExternalSettings::verdict_of(0), Some(true));
    assert_eq!(ExternalSettings::verdict_of(1), Some(false));
    assert_eq!(ExternalSettings::verdict_of(2), None);
}

#[test]
fn the_stock_registry_round_trips_through_json() {
    let registry = builtin::registry();
    let text = serde_json::to_string(&registry).unwrap();
    let back: FormatRegistry = serde_json::from_str(&text).unwrap();
    assert_eq!(back, registry);
}

#[test]
fn unknown_keys_and_unknown_variants_survive_a_load_and_save() {
    let source = serde_json::json!({
        "formats": [{
            "name": "From a newer build",
            "masks": "*.zz",
            "kind": "hologram",
            "enabled": true,
            "futureGroup": {"depth": 3},
            "conversion": {"method": "quantum", "futureFlag": true},
            "misc": {"tabStop": 3, "futureMiscKey": "x"},
            "grammar": {"items": [
                {"element": "Comment", "kind": {"category": "basic", "text": "#"}},
                {"element": "Future", "kind": {"category": "tomorrow", "n": 1}}
            ]}
        }],
        "fallback": {"name": "Everything else", "masks": "*"},
        "topLevelFutureKey": [1, 2]
    });
    let registry: FormatRegistry = serde_json::from_value(source.clone()).unwrap();

    let format = &registry.formats[0];
    assert!(!format.kind.is_known());
    assert!(!format.conversion.method.is_known());
    assert_eq!(format.misc.tab_stop, 3);
    assert_eq!(format.grammar.items.len(), 2);
    assert!(format.grammar.items[0].kind.is_known());
    assert!(!format.grammar.items[1].kind.is_known());

    let written = serde_json::to_value(&registry).unwrap();
    assert_eq!(written["topLevelFutureKey"], serde_json::json!([1, 2]));
    let out = &written["formats"][0];
    assert_eq!(out["kind"], "hologram");
    assert_eq!(out["futureGroup"]["depth"], 3);
    assert_eq!(out["conversion"]["method"], "quantum");
    assert_eq!(out["conversion"]["futureFlag"], true);
    assert_eq!(out["misc"]["futureMiscKey"], "x");
    assert_eq!(out["grammar"]["items"][1]["kind"]["category"], "tomorrow");

    // A second round trip has to be a fixed point, so repeated saves cannot
    // erode what a newer build wrote.
    let again: FormatRegistry = serde_json::from_value(written.clone()).unwrap();
    assert_eq!(serde_json::to_value(&again).unwrap(), written);
}

#[test]
fn an_unknown_variant_falls_back_without_losing_the_stored_spelling() {
    let kind: Extensible<FormatKind> = serde_json::from_str("\"hologram\"").unwrap();
    assert_eq!(kind.or(FormatKind::Text), FormatKind::Text);
    assert_eq!(serde_json::to_string(&kind).unwrap(), "\"hologram\"");
}

#[test]
fn the_table_example_carries_its_field_settings() {
    let format = builtin::comma_separated_values();
    assert_eq!(format.kind.or(FormatKind::Text), FormatKind::Table);
    let table = format.table.as_ref().unwrap();
    assert_eq!(table.delimiters, ",");
    assert_eq!(table.text_qualifier, Some('"'));
    let text = serde_json::to_string(&format).unwrap();
    let back: FileFormat = serde_json::from_str(&text).unwrap();
    assert_eq!(back, format);
}
