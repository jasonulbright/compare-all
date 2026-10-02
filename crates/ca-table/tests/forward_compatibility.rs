//! A settings document written by a later build survives a load and a save.
//!
//! Every settings struct carries an unknown field map and every settings enum
//! has an unknown arm, so nothing a later build writes is dropped here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_table::align::{RowAlignOptions, RowAlignmentMode};
use ca_table::compare::CompareOptions;
use ca_table::detect::DetectOptions;
use ca_table::parse::{FieldSyntax, FirstLineContains, ParseOptions};
use ca_table::regional::DateOrder;
use ca_table::schema::{ColumnAlignment, ColumnType, SchemaSettings};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

/// A document from a later build, with unknown fields at every level and
/// unknown values in every enum.
const DOCUMENT: &str = r##"{
  "parse": {
    "syntax": {
      "kind": "delimited",
      "delimiters": [";", "|"],
      "textQualifier": "'",
      "consecutiveDelimitersAsOne": true,
      "surroundingWhitespaceIsDelimiter": false,
      "escapeCharacter": "\\"
    },
    "firstLineContains": "column-names",
    "commentPrefix": "#"
  },
  "schema": {
    "alignment": "by-similarity",
    "defaultHandling": {
      "key": false,
      "useDefault": false,
      "columnType": "currency",
      "unimportant": true,
      "ignoreCase": true,
      "ignoreWhitespace": true,
      "numericTolerance": 0.25,
      "dateToleranceSeconds": 60.0,
      "currencySymbol": "kr"
    },
    "handling": {
      "0": { "key": true, "columnType": "text", "collation": "natural" }
    },
    "custom": [{ "left": 2, "right": 0, "note": "swapped" }],
    "leftRegional": {
      "useSystem": false,
      "decimalSeparator": ",",
      "thousandsSeparator": ".",
      "dateOrder": "ydm",
      "dateSeparator": "-",
      "calendar": "buddhist"
    },
    "rightRegional": { "useSystem": true },
    "sheetName": "Sheet2"
  },
  "align": {
    "mode": "histogram",
    "neverAlignDifferences": true,
    "skewTolerance": 250,
    "useClosenessMatching": false,
    "sortRowsBeforeAlignment": true,
    "closenessThreshold": 0.8
  },
  "compare": {
    "ignoreUnimportantDifferences": true,
    "reportBlankRuns": true
  },
  "detect": { "sampleLines": 25, "sampleBytes": 4096, "sampleSheets": 3 }
}"##;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &Value) -> T {
    let parsed: T = serde_json::from_value(value.clone()).expect("a loadable settings value");
    let written = serde_json::to_value(&parsed).expect("a writable settings value");
    let again: T = serde_json::from_value(written.clone()).expect("a reloadable settings value");
    assert_eq!(parsed, again, "a second load must yield the first result");
    assert_contains(value, &written, "");
    parsed
}

/// Every key the document carries must appear unchanged in the written form.
/// The written form may carry more, because a field the document leaves out is
/// written with its default.
fn assert_contains(original: &Value, written: &Value, path: &str) {
    match (original, written) {
        (Value::Object(left), Value::Object(right)) => {
            for (key, value) in left {
                let found = right
                    .get(key)
                    .unwrap_or_else(|| panic!("{path}/{key} was dropped"));
                assert_contains(value, found, &format!("{path}/{key}"));
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            assert_eq!(left.len(), right.len(), "{path} changed length");
            for (index, value) in left.iter().enumerate() {
                assert_contains(value, &right[index], &format!("{path}/{index}"));
            }
        }
        (left, right) => assert_eq!(left, right, "{path} changed"),
    }
}

#[test]
fn a_later_document_survives_a_load_and_a_save() {
    let document: Value = serde_json::from_str(DOCUMENT).expect("a loadable document");

    let parse_options: ParseOptions = round_trip(&document["parse"]);
    assert!(parse_options.unknown.contains_key("commentPrefix"));
    assert_eq!(
        parse_options.first_line_contains,
        FirstLineContains::ColumnNames
    );
    match &parse_options.syntax {
        FieldSyntax::Delimited {
            delimiters,
            text_qualifier,
            unknown,
            ..
        } => {
            assert_eq!(delimiters, &vec![';', '|']);
            assert_eq!(*text_qualifier, Some('\''));
            assert!(unknown.contains_key("escapeCharacter"));
        }
        other => unreachable!("expected a delimited syntax, got {other:?}"),
    }

    let schema: SchemaSettings = round_trip(&document["schema"]);
    assert!(matches!(schema.alignment, ColumnAlignment::Unknown(_)));
    assert!(matches!(
        schema.default_handling.column_type,
        ColumnType::Unknown(_)
    ));
    assert!(schema
        .default_handling
        .unknown
        .contains_key("currencySymbol"));
    assert!(matches!(
        schema.left_regional.date_order,
        DateOrder::Unknown(_)
    ));
    assert!(schema.left_regional.unknown.contains_key("calendar"));
    assert!(schema.unknown.contains_key("sheetName"));
    assert!(schema.custom[0].unknown.contains_key("note"));
    assert_eq!(schema.handling[&0].column_type, ColumnType::Text);
    assert!(schema.handling[&0].unknown.contains_key("collation"));

    let align: RowAlignOptions = round_trip(&document["align"]);
    assert!(matches!(align.mode, RowAlignmentMode::Unknown(_)));
    assert_eq!(align.skew_tolerance, Some(250));
    assert!(align.unknown.contains_key("closenessThreshold"));

    let compare: CompareOptions = round_trip(&document["compare"]);
    assert!(compare.ignore_unimportant_differences);
    assert!(compare.unknown.contains_key("reportBlankRuns"));

    let detect: DetectOptions = round_trip(&document["detect"]);
    assert_eq!(detect.sample_lines, 25);
    assert!(detect.unknown.contains_key("sampleSheets"));
}

#[test]
fn an_unknown_syntax_is_carried_through_whole() {
    let text = r#"{"kind":"columnar","boundaries":[0,8,16]}"#;
    let syntax: FieldSyntax = serde_json::from_str(text).expect("a loadable syntax");
    assert!(matches!(syntax, FieldSyntax::Unknown(_)));
    let back = serde_json::to_string(&syntax).expect("a writable syntax");
    let again: FieldSyntax = serde_json::from_str(&back).expect("a reloadable syntax");
    assert_eq!(again, syntax);
    assert!(back.contains("boundaries"));
}
