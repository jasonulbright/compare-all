//! Escaping under hostile file names and hostile file content.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_report::escape::{csv_field, html, text_single_line, xml};
use ca_report::input::{EntryStatus, FolderRow, Importance, RowKind, SideFacts, TextCell, TextRow};
use ca_report::options::{
    FolderLayout, FolderReportOptions, OutputOptions, ReportMeta, TextLayout, TextReportOptions,
};
use ca_report::NeverCancel;

/// Values a file name or a line of a file can hold that a naive report would
/// let escape into markup, into a record separator, or into a formula.
const HOSTILE: &[&str] = &[
    "<script>alert(1)</script>",
    "</td></tr><tr><td>forged",
    "\"onmouseover=\"steal()",
    "a&amp;b",
    "]]><!--",
    "<![CDATA[x]]>",
    "=HYPERLINK(\"http://example.invalid\")",
    "+1+1",
    "-2-2",
    "@SUM(A1)",
    "quote\"inside",
    "comma,inside",
    "break\r\ninside",
    "tab\tinside",
    "null\u{0}inside",
    "escape\u{1b}[31m",
    "high\u{9f}control",
    "noncharacter\u{fffe}",
    "emoji \u{1f600}",
    "\u{202e}reversed",
];

#[test]
fn no_hostile_value_leaves_markup_open_in_html() {
    for value in HOSTILE {
        let escaped = html(value);
        assert!(!escaped.contains('<'), "{value:?} produced {escaped:?}");
        assert!(!escaped.contains('>'), "{value:?} produced {escaped:?}");
        assert!(!escaped.contains('"'), "{value:?} produced {escaped:?}");
        assert!(!escaped.contains('\''), "{value:?} produced {escaped:?}");
    }
}

#[test]
fn no_hostile_value_leaves_markup_open_in_xml() {
    for value in HOSTILE {
        let escaped = xml(value);
        assert!(!escaped.contains('<'), "{value:?} produced {escaped:?}");
        assert!(!escaped.contains("]]>"), "{value:?} produced {escaped:?}");
    }
}

#[test]
fn xml_carries_no_code_point_it_forbids() {
    for value in HOSTILE {
        for ch in xml(value).chars() {
            let allowed = matches!(ch, '\t' | '\n' | '\r')
                || ('\u{20}'..'\u{7f}').contains(&ch)
                || (ch > '\u{9f}' && ch != '\u{fffe}' && ch != '\u{ffff}');
            assert!(allowed, "{value:?} kept {ch:?}");
        }
    }
}

#[test]
fn every_hostile_value_is_one_csv_field() {
    for value in HOSTILE {
        let field = csv_field(value);
        assert!(field.starts_with('"') && field.ends_with('"'), "{field}");
        let body = &field[1..field.len() - 1];
        let mut quotes = 0usize;
        for ch in body.chars() {
            if ch == '"' {
                quotes += 1;
            }
        }
        assert_eq!(quotes % 2, 0, "{value:?} left an odd quote count");
    }
}

#[test]
fn a_formula_character_never_starts_a_csv_field_body() {
    for value in HOSTILE {
        let field = csv_field(value);
        let first = field.chars().nth(1).unwrap_or(' ');
        assert!(
            !matches!(first, '=' | '+' | '-' | '@' | '\t' | '\r'),
            "{value:?} produced {field}"
        );
    }
}

#[test]
fn plain_text_keeps_every_hostile_value_on_one_line() {
    for value in HOSTILE {
        let folded = text_single_line(value);
        assert!(!folded.contains('\n'), "{value:?}");
        assert!(!folded.contains('\r'), "{value:?}");
    }
}

fn hostile_folder_report(layout: FolderLayout, output: &OutputOptions) -> String {
    let rows: Vec<FolderRow> = HOSTILE
        .iter()
        .enumerate()
        .map(|(index, name)| FolderRow {
            depth: 0,
            name: (*name).to_owned(),
            relative_path: (*name).to_owned(),
            is_dir: false,
            status: if index % 2 == 0 {
                EntryStatus::Different
            } else {
                EntryStatus::LeftOrphan
            },
            left: SideFacts {
                present: true,
                size: Some(1),
                timestamp: Some((*name).to_owned()),
                ..SideFacts::default()
            },
            right: SideFacts::absent(),
            link: Some((*name).to_owned()),
        })
        .collect();
    let links = matches!(layout, FolderLayout::SideBySide) && output.is_html();
    let options = FolderReportOptions {
        layout,
        include_file_links: links,
        ..FolderReportOptions::default()
    };
    let mut out = Vec::new();
    ca_report::write_folder_report(
        &mut out,
        &ReportMeta::new("<left>", "\"right\""),
        &options,
        output,
        rows,
        &NeverCancel,
    )
    .expect("render");
    String::from_utf8(out).expect("utf-8")
}

#[test]
fn a_hostile_file_name_forges_no_row_in_an_html_folder_report() {
    let html = hostile_folder_report(FolderLayout::SideBySide, &OutputOptions::html_color());
    assert!(!html.contains("<script"), "{html}");
    assert!(
        html.contains("&lt;/td&gt;&lt;/tr&gt;"),
        "the forged markup stayed text: {html}"
    );
    assert!(
        !html.contains("onmouseover=\""),
        "a quote escaped its attribute: {html}"
    );
    let rows = html.matches("<tr class=").count();
    assert_eq!(rows, HOSTILE.len(), "one row per entry and no more");
}

#[test]
fn a_hostile_file_name_forges_no_element_in_an_xml_folder_report() {
    let text = hostile_folder_report(FolderLayout::Xml, &OutputOptions::plain_text());
    assert_eq!(text.matches("<entry ").count(), HOSTILE.len());
    assert_eq!(text.matches("</entry>").count(), HOSTILE.len());
    assert!(!text.contains("]]>"), "{text}");
}

#[test]
fn a_hostile_file_name_forges_no_row_in_a_plain_text_folder_report() {
    let text = hostile_folder_report(FolderLayout::SideBySide, &OutputOptions::plain_text());
    let body: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("---"))
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    assert_eq!(body.len(), HOSTILE.len(), "{text}");
}

#[test]
fn hostile_file_content_forges_no_row_in_a_text_report() {
    let rows: Vec<TextRow> = HOSTILE
        .iter()
        .enumerate()
        .map(|(index, line)| TextRow {
            kind: RowKind::Changed,
            importance: Some(Importance::Important),
            left: Some(TextCell::new(index as u64 + 1, *line)),
            right: Some(TextCell::new(index as u64 + 1, "clean")),
        })
        .collect();
    let mut out = Vec::new();
    ca_report::write_text_report(
        &mut out,
        &common::meta(),
        &TextReportOptions::default(),
        &OutputOptions::html_color(),
        rows.clone(),
        &NeverCancel,
    )
    .expect("render");
    let html = String::from_utf8(out).expect("utf-8");
    assert_eq!(html.matches("<tr class=").count(), HOSTILE.len());
    assert!(!html.contains("<script"), "{html}");

    let mut out = Vec::new();
    ca_report::write_text_report(
        &mut out,
        &common::meta(),
        &TextReportOptions {
            layout: TextLayout::Xml,
            ..TextReportOptions::default()
        },
        &OutputOptions::plain_text(),
        rows,
        &NeverCancel,
    )
    .expect("render");
    let xml = String::from_utf8(out).expect("utf-8");
    assert_eq!(xml.matches("<line ").count(), HOSTILE.len());
    assert_eq!(xml.matches("</line>").count(), HOSTILE.len());
}

#[test]
fn a_hostile_title_and_side_label_are_escaped() {
    let mut meta = ReportMeta::new("<a>", "\"b\"");
    meta.title = Some("</title><script>x()</script>".into());
    let mut out = Vec::new();
    ca_report::write_text_report(
        &mut out,
        &meta,
        &TextReportOptions::default(),
        &OutputOptions::html_color(),
        common::text_rows(),
        &NeverCancel,
    )
    .expect("render");
    let html = String::from_utf8(out).expect("utf-8");
    assert!(!html.contains("<script"), "{html}");
    assert_eq!(html.matches("</title>").count(), 1);
}
