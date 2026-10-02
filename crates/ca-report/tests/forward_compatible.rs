//! A settings document a later build writes round trips through this build.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_report::options::{
    FolderColumns, FolderDisplayFilter, FolderLayout, FolderReportOptions, HexLayout,
    HexReportOptions, HtmlScheme, OutputFormat, OutputOptions, PageOrientation, PairLayout,
    PatchFormat, PictureReportOptions, PrintOptions, PrintScheme, RecordReportOptions, ReportMeta,
    TableLayout, TableReportOptions, TextDisplayFilter, TextLayout, TextReportOptions, Unknown,
    Wrap,
};
use ca_report::palette::ReportPalette;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

/// Add a field a later build might write to every object in the tree.
///
/// The names are unique, so one assertion covers every depth at once.
fn inject(value: &mut Value, counter: &mut u32, injected: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for nested in map.values_mut() {
                inject(nested, counter, injected);
            }
            *counter += 1;
            let name = format!("futureField{counter}");
            map.insert(
                name.clone(),
                json!({ "kind": "future", "depth": *counter, "values": [1, 2, 3] }),
            );
            injected.push(name);
        }
        Value::Array(items) => {
            for item in items {
                inject(item, counter, injected);
            }
        }
        _ => {}
    }
}

fn depth(value: &Value) -> u32 {
    match value {
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        Value::Array(items) => items.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn survives<T: Serialize + DeserializeOwned>(populated: &T, least_depth: u32) {
    let original = serde_json::to_value(populated).expect("serialize");
    assert!(
        depth(&original) >= least_depth,
        "the populated value is shallower than the test expects: {original}"
    );
    let mut injected_value = original;
    let mut counter = 0u32;
    let mut names = Vec::new();
    inject(&mut injected_value, &mut counter, &mut names);
    assert!(!names.is_empty(), "nothing was injected");

    let loaded: T = serde_json::from_value(injected_value).expect("deserialize");
    let written = serde_json::to_string(&loaded).expect("serialize again");
    for name in &names {
        assert!(
            written.contains(name.as_str()),
            "{name} was dropped: {written}"
        );
    }
}

fn populated_output() -> OutputOptions {
    OutputOptions {
        format: OutputFormat::Html,
        html: HtmlScheme::Custom {
            stylesheet: "report.css".into(),
            unknown: Unknown::new(),
        },
        print: PrintOptions {
            scheme: PrintScheme::Color,
            orientation: PageOrientation::Landscape,
            unknown: Unknown::new(),
        },
        wrap: Wrap::Word,
        palette: ReportPalette::default(),
        unknown: Unknown::new(),
    }
}

#[test]
fn an_unknown_field_at_every_depth_of_the_output_options_survives() {
    survives(&populated_output(), 3);
}

#[test]
fn an_unknown_field_survives_the_text_options() {
    let options = TextReportOptions {
        layout: TextLayout::Interleaved,
        display: TextDisplayFilter::Context,
        context_lines: 5,
        ignore_unimportant: true,
        line_numbers: true,
        strikeout_left_diffs: true,
        strikeout_right_diffs: true,
        patch_format: PatchFormat::Unified,
        unknown: Unknown::new(),
    };
    survives(&options, 1);
}

#[test]
fn an_unknown_field_survives_the_folder_options() {
    let options = FolderReportOptions {
        layout: FolderLayout::Xml,
        display: FolderDisplayFilter::LeftNewerOrphans,
        columns: FolderColumns {
            size: true,
            timestamp: true,
            vcs: true,
            revision: true,
            version: true,
            crc: true,
            attributes: true,
            owner: true,
            group: true,
            unknown: Unknown::new(),
        },
        include_file_links: true,
        unknown: Unknown::new(),
    };
    survives(&options, 2);
}

#[test]
fn an_unknown_field_survives_the_other_options() {
    survives(
        &HexReportOptions {
            layout: HexLayout::Interleaved,
            line_numbers: true,
            bytes_per_row: 32,
            ..HexReportOptions::default()
        },
        1,
    );
    survives(
        &TableReportOptions {
            layout: TableLayout::Interleaved,
            ignore_unimportant: true,
            line_numbers: true,
            current_sheet_only: true,
            ..TableReportOptions::default()
        },
        1,
    );
    survives(
        &PictureReportOptions {
            layout: PairLayout::Summary,
            ignore_unimportant: true,
            unknown: Unknown::new(),
        },
        1,
    );
    survives(
        &RecordReportOptions {
            layout: PairLayout::SideBySide,
            ignore_unimportant: true,
            ..RecordReportOptions::default()
        },
        1,
    );
    survives(&ReportMeta::new("left", "right").with_title("Title"), 1);
    survives(&ReportPalette::default(), 1);
}

#[test]
fn an_unknown_enum_value_survives() {
    let text = r#"{"layout":"over-under","display":"display-future","patchFormat":"ed"}"#;
    let options: TextReportOptions = serde_json::from_str(text).expect("deserialize");
    assert!(matches!(options.layout, TextLayout::Unknown(_)));
    assert!(matches!(options.display, TextDisplayFilter::Unknown(_)));
    assert!(matches!(options.patch_format, PatchFormat::Unknown(_)));
    let written = serde_json::to_string(&options).expect("serialize");
    assert!(written.contains("over-under"), "{written}");
    assert!(written.contains("display-future"), "{written}");
    assert!(written.contains("\"ed\""), "{written}");
}

#[test]
fn a_known_document_keeps_its_names() {
    let written = serde_json::to_string(&populated_output()).expect("serialize");
    assert!(written.contains("\"wrap\":\"word\""), "{written}");
    assert!(
        written.contains("\"orientation\":\"landscape\""),
        "{written}"
    );
    assert!(
        written.contains("\"stylesheet\":\"report.css\""),
        "{written}"
    );
}
