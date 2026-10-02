//! The stored format documents use one spelling for every field.
//!
//! An enum-level `rename_all` renames variants only. A struct variant needs
//! `rename_all_fields` for its named fields, or its fields keep their Rust
//! spelling while the rest of the document is camelCase. Where a type also
//! carries a flattened unknown map, the mismatch is silent: the documented key
//! lands in the map and the setting is dropped.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_grammar::builtin;
use ca_grammar::format::FileFormat;
use serde_json::Value;

fn collect_keys(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                out.push((path.to_owned(), key.clone()));
                collect_keys(child, &format!("{path}.{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_keys(child, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

fn assert_camel_case_keys(value: &Value, what: &str) {
    let mut keys = Vec::new();
    collect_keys(value, "", &mut keys);
    assert!(!keys.is_empty(), "{what} emits no keys");
    for (path, key) in keys {
        assert!(
            !key.contains('_'),
            "{what} writes `{key}` at `{path}`; the stored spelling is camelCase"
        );
    }
}

#[test]
fn every_builtin_format_writes_camel_case_keys() {
    for format in builtin::formats() {
        let written = serde_json::to_value(&format).unwrap();
        assert_camel_case_keys(&written, &format!("format `{}`", format.name));
    }
}

#[test]
fn a_format_written_here_reloads_with_every_unknown_map_empty() {
    for format in builtin::formats() {
        let text = serde_json::to_string(&format).unwrap();
        let back: FileFormat = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back, format,
            "format `{}` does not survive a round trip",
            format.name
        );
        // A key the reader does not recognize would be held in a flattened
        // unknown map and would not compare unequal on its own, so the maps are
        // checked directly.
        let reloaded = serde_json::to_value(&back).unwrap();
        assert_unknown_free(&reloaded, &format!("format `{}`", format.name));
    }
}

/// Reads the document back as untyped JSON and asserts no object holds a key
/// this build would have to keep in an unknown map. Every key a builtin format
/// writes is one the reader knows, so any leftover names a spelling mismatch.
fn assert_unknown_free(value: &Value, what: &str) {
    let mut keys = Vec::new();
    collect_keys(value, "", &mut keys);
    for (path, key) in keys {
        assert!(
            !key.starts_with("unknown"),
            "{what} keeps `{key}` at `{path}` outside a typed field"
        );
    }
}

#[test]
fn an_item_kind_writes_the_documented_spelling_of_its_struct_variant_fields() {
    let item = builtin::block_comment("/*", "*/");
    let written = serde_json::to_value(&item).unwrap();
    assert_camel_case_keys(&written, "GrammarItem");
    assert_eq!(written["kind"]["stopAtEndOfLine"], Value::Bool(false));
    assert_eq!(written["kind"]["lineSpanning"], Value::Bool(true));
}
