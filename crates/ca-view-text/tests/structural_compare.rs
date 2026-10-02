#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_diff::NeverCancel;
use ca_ui::{command::Command, report::Payload, view::SessionView};
use ca_view_text::{
    sidecopy::Side,
    structure::{self, Format},
    TextView,
};
use std::time::{Duration, Instant};

fn same(left: &str, right: &str, format: Format) {
    let compared = structure::compare(left, right, format, &NeverCancel).unwrap();
    assert_eq!(compared.summary.added, 0);
    assert_eq!(compared.summary.removed, 0);
    assert_eq!(compared.summary.changed, 0);
    assert_eq!(compared.data.model.counts().differences, 0);
}

#[test]
fn both_input_limits_are_checked_before_either_parser_runs() {
    let huge = " ".repeat(4 * 1024 * 1024 + 1);
    for format in [Format::Json, Format::Xml] {
        let error = structure::compare("invalid", &huge, format, &NeverCancel)
            .err()
            .unwrap();
        assert!(
            error.starts_with("right side: input exceeds the 4 MiB"),
            "{error}"
        );
    }
}

#[test]
fn namespace_scopes_restore_shadowed_bindings_and_count_declaration_storage() {
    same(
        "<r xmlns:p='a'><p:x/><s xmlns:p='b'><p:x/></s><p:x/></r>",
        "<r xmlns:q='a'><q:x/><s xmlns:q='b'><q:x/></s><q:x/></r>",
        Format::Xml,
    );
    same(
        "<r xmlns='a'><s xmlns=''><x/></s><x/></r>",
        "<p:r xmlns:p='a'><s><x/></s><p:x/></p:r>",
        Format::Xml,
    );
    for text in [
        "<r x='1' x='2'/>",
        "<r xmlns:p='a' xmlns:p='b'/>",
        "<r xmlns:xml='other'/>",
        "<r xmlns:p='http://www.w3.org/XML/1998/namespace'/>",
        "<r xmlns:xmlns='urn:x'/>",
    ] {
        assert!(
            structure::compare(text, "<r/>", Format::Xml, &NeverCancel).is_err(),
            "{text}"
        );
    }
    let declarations = (0..10_000)
        .map(|index| format!("xmlns:p{index}='{}'", "v".repeat(390)))
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!("<r {declarations}/>");
    assert!(text.len() < 4 * 1024 * 1024);
    assert!(structure::compare(&text, "<r/>", Format::Xml, &NeverCancel)
        .err()
        .unwrap()
        .contains("storage"));
}

#[test]
fn structural_strings_paths_and_names_show_bidi_controls_as_escapes() {
    let controls = [
        '\u{61c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
        '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
    ];
    for control in controls {
        for (text, format) in [
            (
                format!("{{\"key{control}\":\"value{control}\"}}"),
                Format::Json,
            ),
            (
                format!("<r a='x{control}'><!--c{control}--><?p v{control}?><x>v{control}</x></r>"),
                Format::Xml,
            ),
        ] {
            let result = structure::compare(&text, &text, format, &NeverCancel).unwrap();
            assert_eq!(result.summary.changed, 0);
            assert!(
                !result.left_text.contains(control),
                "{control:?}: {}",
                result.left_text
            );
            assert!(result
                .left_text
                .contains(&format!("\\u{:04x}", u32::from(control))));
        }
    }
    let result = structure::compare(
        "{\"a\\u202e\":1}",
        "{\"a\\\\u202e\":1}",
        Format::Json,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!((result.summary.added, result.summary.removed), (1, 1));
}

#[test]
fn maximum_depth_parses_and_projects_on_a_worker_sized_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut json = "0".to_owned();
            for level in 0..128 {
                json = if level % 2 == 0 {
                    format!("[{json}]")
                } else {
                    format!("{{\"{}\":{json}}}", "k".repeat(100))
                };
            }
            same(&json, &json, Format::Json);
            let xml = format!("{}{}", "<x a='v'>".repeat(128), "</x>".repeat(128));
            same(&xml, &xml, Format::Xml);
            let error =
                structure::compare(&format!("[{json}]"), "null", Format::Json, &NeverCancel)
                    .err()
                    .unwrap();
            assert!(error.contains("parser limit of 128"), "{error}");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn qname_attribute_values_are_strings_without_schema_type_information() {
    same(
        "<r xmlns:p='a' type='p:T'/>",
        "<r xmlns:p='b' type='p:T'/>",
        Format::Xml,
    );
    assert_eq!(
        structure::compare(
            "<r xmlns:p='a' type='p:T'/>",
            "<r xmlns:q='a' type='q:T'/>",
            Format::Xml,
            &NeverCancel
        )
        .unwrap()
        .summary
        .changed,
        1
    );
}

#[test]
fn object_order_and_decoded_keys_do_not_change_paths() {
    same(
        r#"{"b":true,"a":{"\u0078":"\u0061"}}"#,
        r#"{"a":{"x":"a"},"b":true}"#,
        Format::Json,
    );
}

#[test]
fn exact_decimal_values_do_not_round_large_integers() {
    for (a, b) in [
        ("1", "1.000e0"),
        ("100", "1e2"),
        ("-0.000", "0"),
        ("0.001", "1e-3"),
        (
            "123456789012345678901234567890",
            "12345678901234567890123456789e1",
        ),
    ] {
        same(a, b, Format::Json);
    }
    let result = structure::compare(
        "9007199254740992",
        "9007199254740993",
        Format::Json,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(result.summary.changed, 1);
}

#[test]
fn additions_removals_and_changes_align_only_the_same_path() {
    let result = structure::compare(
        r#"{"gone":1,"value":"left","same":null}"#,
        r#"{"new":1,"value":"right","same":null}"#,
        Format::Json,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(
        (
            result.summary.added,
            result.summary.removed,
            result.summary.changed
        ),
        (1, 1, 1)
    );
    assert!(result.left_text.contains(r#"Removed  $["gone"] = 1"#));
    assert!(result.right_text.contains(r#"Added  $["new"] = 1"#));
    assert!(result.left_text.contains(r#"Changed  $["value"] = "left""#));
    assert_eq!(result.data.model.counts().differences, 3);
    for row in result.data.model.rows() {
        if let (Some(a), Some(b)) = (row.left, row.right) {
            let a = result.data.left.lines[a as usize]
                .split(" = ")
                .next()
                .unwrap();
            let b = result.data.right.lines[b as usize]
                .split(" = ")
                .next()
                .unwrap();
            assert_eq!(a, b);
        }
    }
}

#[test]
fn arrays_match_by_position_and_empty_containers_retain_their_type() {
    let result = structure::compare("[1,2]", "[2,1,3]", Format::Json, &NeverCancel).unwrap();
    assert_eq!((result.summary.added, result.summary.changed), (1, 2));
    for (a, b) in [
        ("{}", "[]"),
        ("null", "{}"),
        ("1", r#""1""#),
        ("false", "0"),
    ] {
        assert_eq!(
            structure::compare(a, b, Format::Json, &NeverCancel)
                .unwrap()
                .summary
                .changed,
            1
        );
    }
}

#[test]
fn escaped_path_components_cannot_collide_or_create_extra_lines() {
    let result = structure::compare(
        r#"{"a\"]\n":1,"a":{"x":2},"":null}"#,
        r#"{"a\"]\n":2,"a":{"x":2},"":null}"#,
        Format::Json,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(result.summary.changed, 1);
    assert_eq!(result.left_text.lines().count(), 5);
}

#[test]
fn duplicate_keys_and_invalid_json_are_refused_without_loss() {
    for text in [
        r#"{"a":1,"\u0061":2}"#,
        r#"{"nested":{"x":1,"x":2}}"#,
        "{",
        "[1,]",
        "true false",
        "01",
        r#""\uD800""#,
        "1e9999999999999999999999999",
    ] {
        assert!(
            structure::compare(text, "{}", Format::Json, &NeverCancel).is_err(),
            "{text}"
        );
    }
}

#[test]
fn xml_attributes_prefixes_entities_cdata_and_layout_compare_by_value() {
    same(
        r#"<p:root xmlns:p="urn:test" b="2" a="1"><p:value>A&amp;B</p:value></p:root>"#,
        r#"<root xmlns="urn:test" a="1" b="2">
 <value><![CDATA[A&B]]></value>
</root>"#,
        Format::Xml,
    );
    same("<r><x/></r>", "<r><x></x></r>", Format::Xml);
    same("<r>&#65;&#x42;</r>", "<r>AB</r>", Format::Xml);
    same("<r a='x\ny'/>", "<r a='x y'/>", Format::Xml);
}

#[test]
fn namespace_uris_and_qualified_attributes_remain_distinct() {
    let result = structure::compare(
        r#"<r xmlns="a" x="1"/>"#,
        r#"<r xmlns="b" x="1"/>"#,
        Format::Xml,
        &NeverCancel,
    )
    .unwrap();
    assert!(result.summary.added > 0 && result.summary.removed > 0);
    let result = structure::compare(
        r#"<r xmlns:p="a" p:x="1"/>"#,
        r#"<r xmlns:p="a" x="1"/>"#,
        Format::Xml,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!((result.summary.added, result.summary.removed), (1, 1));
}

#[test]
fn repeated_xml_elements_match_by_occurrence_and_order_is_retained() {
    let result = structure::compare(
        "<r><x>one</x><x>two</x></r>",
        "<r><x>one</x><x>changed</x></r>",
        Format::Xml,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(result.summary.changed, 1);
    assert!(result.left_text.contains("Changed  /r[1]/x[2]/text()[1]"));
    assert!(
        structure::compare(
            "<r><a/><b/></r>",
            "<r><b/><a/></r>",
            Format::Xml,
            &NeverCancel
        )
        .unwrap()
        .summary
        .changed
            > 0
    );
}

#[test]
fn mixed_content_preserves_text_order_and_white_space() {
    for (a, b) in [
        ("<r>Hello <b>world</b> !</r>", "<r>Hello<b>world</b> !</r>"),
        ("<r> <a/> text</r>", "<r><a/> text</r>"),
        ("<r> </r>", "<r/>"),
        ("<r> <!--c--> </r>", "<r><!--c--></r>"),
    ] {
        assert!(
            structure::compare(a, b, Format::Xml, &NeverCancel)
                .unwrap()
                .summary
                .changed
                > 0
                || structure::compare(a, b, Format::Xml, &NeverCancel)
                    .unwrap()
                    .summary
                    .removed
                    > 0
        );
    }
}

#[test]
fn xml_space_is_inherited_and_default_resets_preservation() {
    let a = r#"<r xml:space="preserve"> <x><a/> </x></r>"#;
    let b = r#"<r xml:space="preserve"><x><a/></x></r>"#;
    assert!(
        structure::compare(a, b, Format::Xml, &NeverCancel)
            .unwrap()
            .summary
            .removed
            > 0
    );
    same(
        r#"<r xml:space="preserve"><x xml:space="default"> <a/> </x></r>"#,
        r#"<r xml:space="preserve"><x xml:space="default"><a/></x></r>"#,
        Format::Xml,
    );
}

#[test]
fn xml_comments_and_processing_instructions_are_content() {
    let result = structure::compare(
        "<r><!--old--><?work one?></r>",
        "<r><!--new--><?work two?></r>",
        Format::Xml,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(result.summary.changed, 2);
    assert_eq!(
        structure::compare("<!--c--><r/>", "<r/><!--c-->", Format::Xml, &NeverCancel)
            .unwrap()
            .summary
            .changed,
        1
    );
}

#[test]
fn declarations_are_validated_but_encoding_spelling_is_not_content() {
    same(
        "<?xml version='1.0' encoding='UTF-8'?><r/>",
        "<r/>",
        Format::Xml,
    );
    for text in [
        "<?xml version='1.1'?><r/>",
        " <?xml version='1.0'?><r/>",
        "<?xml version='1.0' unknown='x'?><r/>",
        "<?xml version='1.0' standalone='perhaps'?><r/>",
        "<?xml version='1.0' encoding='1x'?><r/>",
    ] {
        assert!(
            structure::compare(text, "<r/>", Format::Xml, &NeverCancel).is_err(),
            "{text}"
        );
    }
}

#[test]
fn malformed_xml_and_external_entities_refuse_the_projection() {
    for text in [
        "<r>",
        "<r/><x/>",
        "outside<r/>",
        "<r a='1' a='2'/>",
        "<r a='<'/>",
        "<r a='&#0;'/>",
        "<r>&#0;</r>",
        "<r>&missing;</r>",
        "<1bad/>",
        "<r a:b:c='x'/>",
        "<p:r/>",
        "<r><!--a--b--></r>",
        "<!DOCTYPE r SYSTEM 'file:///private'><r/>",
        "<r xml:space='bad'/>",
        "<r xmlns:p='a' xmlns:q='a' p:x='1' q:x='2'/>",
        "<r xmlns:xml='wrong'/>",
        "<r>bad]]>text</r>",
        "<r xmlns:p='http://www.w3.org/2000/xmlns/'/>",
        "<r a='1'junk='2'/>",
    ] {
        assert!(
            structure::compare(text, "<r/>", Format::Xml, &NeverCancel).is_err(),
            "{text}"
        );
    }
}

#[test]
fn reserved_namespace_bindings_and_qualified_pi_targets_refuse_the_projection() {
    for text in [
        "<xmlns:a/>",
        "<r><xmlns:a xmlns:a='urn:a'/></r>",
        "<r xmlns:p=''/>",
        "<r xmlns='http://www.w3.org/XML/1998/namespace'/>",
        "<r xmlns='http://www.w3.org/2000/xmlns/'/>",
        "<?a:b data?><r/>",
        "<r><?a:b?></r>",
    ] {
        assert!(
            structure::compare(text, "<r/>", Format::Xml, &NeverCancel).is_err(),
            "{text}"
        );
    }
    same(
        "<r xmlns:xml='http://www.w3.org/XML/1998/namespace' xmlns=''/>",
        "<r/>",
        Format::Xml,
    );
}

#[test]
fn a_zero_width_no_break_space_before_decoded_content_is_refused() {
    for (text, other, format) in [
        ("\u{feff}{\"a\":1}", "{\"a\":1}", Format::Json),
        ("\u{feff}<r/>", "<r/>", Format::Xml),
        ("\u{feff}\u{feff}<r/>", "<r/>", Format::Xml),
    ] {
        assert!(
            structure::compare(text, other, format, &NeverCancel).is_err(),
            "{text:?}"
        );
    }
}

#[test]
fn processing_instruction_data_excludes_the_separator_after_the_target() {
    same("<?p  a?><r/>", "<?p a?><r/>", Format::Xml);
    same("<r><?p ?></r>", "<r><?p?></r>", Format::Xml);
    same("<r><?p\r\n a\r\nb?></r>", "<r><?p a\nb?></r>", Format::Xml);
    assert_eq!(
        structure::compare(
            "<r><?p a ?></r>",
            "<r><?p a?></r>",
            Format::Xml,
            &NeverCancel
        )
        .unwrap()
        .summary
        .changed,
        1
    );
    assert_eq!(
        structure::compare("<r><?p?></r>", "<r><?q?></r>", Format::Xml, &NeverCancel)
            .unwrap()
            .summary
            .changed,
        1
    );
}

#[test]
fn structural_limits_and_cancellation_refuse_without_partial_results() {
    let huge = format!("\"{}\"", "a".repeat(4 * 1024 * 1024));
    assert!(structure::compare(&huge, "null", Format::Json, &NeverCancel).is_err());
    let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    assert!(structure::compare(&deep, "null", Format::Json, &NeverCancel).is_err());
    let deep = format!("{}{}", "<x>".repeat(129), "</x>".repeat(129));
    assert!(structure::compare(&deep, "<r/>", Format::Xml, &NeverCancel).is_err());
    let wide = format!("[{}]", vec!["0"; 100_001].join(","));
    assert!(structure::compare(&wide, "[]", Format::Json, &NeverCancel).is_err());
    assert!(structure::compare("{}", "{}", Format::Json, &Cancelled)
        .err()
        .unwrap()
        .contains("cancelled"));
}

struct Cancelled;
impl ca_diff::Cancel for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

fn settle(view: &mut TextView) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !view.is_settled() && Instant::now() < deadline {
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(view.is_settled());
}

fn view(extension: &str, left: &str, right: &str) -> (tempfile::TempDir, TextView) {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join(format!("left.{extension}"));
    let b = directory.path().join(format!("right.{extension}"));
    std::fs::write(&a, left).unwrap();
    std::fs::write(&b, right).unwrap();
    let mut view = TextView::new(a, b, &ca_ui::testing::context(), 1);
    settle(&mut view);
    (directory, view)
}

#[test]
fn structural_projection_is_read_only_and_restores_source_and_editability() {
    let (directory, mut view) = view("json", r#"{"b":2,"a":1}"#, r#"{"a":1,"b":2}"#);
    let original = view.pane(Side::Left).buffer().text();
    assert!(view.accepts(Command::CompareStructure));
    view.run(Command::CompareStructure);
    settle(&mut view);
    assert_eq!(view.structural_summary().unwrap().changed, 0);
    assert_eq!(view.status_fields()[0], "0 difference section(s)");
    for command in [
        Command::SaveFile,
        Command::SaveFileAs,
        Command::SaveBoth,
        Command::Paste,
        Command::CopyToLeft,
        Command::CopyToRight,
    ] {
        assert!(!view.accepts(command));
    }
    assert!(view.pane(Side::Left).is_read_only());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("left.json")).unwrap(),
        original
    );
    view.run(Command::CompareStructure);
    settle(&mut view);
    assert!(view.structural_summary().is_none());
    assert_eq!(view.pane(Side::Left).buffer().text(), original);
    assert!(!view.pane(Side::Left).is_read_only());
}

#[test]
fn report_preserves_paths_values_classes_and_structural_heading() {
    let (_directory, mut view) = view(
        "json",
        r#"{"gone":1,"value":"left"}"#,
        r#"{"new":2,"value":"right"}"#,
    );
    view.run(Command::CompareStructure);
    settle(&mut view);
    let (meta, payload) = view.report_payload();
    assert!(meta
        .title
        .as_ref()
        .is_some_and(|title| title.contains("JSON Structure Compare")));
    let Payload::Text(payload) = payload else {
        panic!("text report expected")
    };
    let rows: Vec<_> = payload.iter().collect();
    assert!(rows
        .iter()
        .any(|row| row.kind == ca_ui::report::RowKind::LeftOnly
            && row.left.as_ref().unwrap().text.contains("Removed")));
    assert!(rows
        .iter()
        .any(|row| row.kind == ca_ui::report::RowKind::RightOnly
            && row.right.as_ref().unwrap().text.contains("Added")));
    assert!(rows
        .iter()
        .any(|row| row.kind == ca_ui::report::RowKind::Changed
            && row.left.as_ref().unwrap().text.contains(r#"$["value"]"#)));
}

#[test]
fn malformed_input_falls_back_to_the_existing_line_comparison() {
    for (extension, left, right) in [("json", "{broken", "{}"), ("xml", "<broken>", "<r/>")] {
        let (_directory, mut view) = view(extension, left, right);
        let before = view.status_fields()[0].clone();
        view.run(Command::CompareStructure);
        settle(&mut view);
        assert!(view.structural_summary().is_none());
        assert_eq!(view.status_fields()[0], before);
        assert_eq!(view.pane(Side::Left).buffer().text(), left);
        assert!(view.message().unwrap().contains("line comparison"));
        assert!(!view.pane(Side::Left).is_read_only());
    }
}

#[test]
fn text_ignore_rules_and_settings_cannot_hide_structural_value_changes() {
    let (_directory, mut view) = view("json", r#"{"a":"Case"}"#, r#"{"a":"case"}"#);
    view.run(Command::CompareStructure);
    settle(&mut view);
    assert_eq!(view.structural_summary().unwrap().changed, 1);
    let mut rules = view.rules();
    rules.elements.strings = false;
    rules.everything_else_important = false;
    view.set_rules(rules);
    settle(&mut view);
    view.run(Command::ToggleIgnoreUnimportant);
    assert_eq!(view.status_fields()[0], "1 difference section(s)");
    assert_eq!(view.structural_summary().unwrap().changed, 1);
}

#[test]
fn cancel_and_reload_never_install_a_stale_projection() {
    let (directory, mut view) = view("json", r#"{"a":1}"#, r#"{"a":2}"#);
    view.run(Command::CompareStructure);
    view.run(Command::Cancel);
    settle(&mut view);
    std::thread::sleep(Duration::from_millis(20));
    view.tick();
    assert!(view.structural_summary().is_none());
    assert_eq!(view.pane(Side::Left).buffer().text(), r#"{"a":1}"#);
    view.run(Command::CompareStructure);
    view.run(Command::Reload);
    settle(&mut view);
    assert!(view.structural_summary().is_none());
    view.run(Command::CompareStructure);
    settle(&mut view);
    std::fs::write(directory.path().join("left.json"), r#"{"a":3}"#).unwrap();
    view.run(Command::Reload);
    settle(&mut view);
    assert!(view.structural_summary().is_none());
    assert_eq!(view.pane(Side::Left).buffer().text(), r#"{"a":3}"#);
}

#[test]
fn unrelated_formats_keep_only_the_original_comparison_modes() {
    for extension in ["txt", "yaml", "toml"] {
        let (_directory, view) = view(extension, "one", "two");
        assert!(!view.accepts(Command::CompareStructure));
    }
}

#[test]
fn formatting_and_structural_projection_cannot_stack() {
    let (_directory, mut view) = view("json", r#"{"a":1}"#, r#"{"a":2}"#);
    view.run(Command::PrettifyForComparison);
    settle(&mut view);
    assert!(!view.accepts(Command::CompareStructure));
    view.run(Command::PrettifyForComparison);
    settle(&mut view);
    assert!(view.accepts(Command::CompareStructure));
    view.run(Command::CompareStructure);
    settle(&mut view);
    assert_eq!(view.structural_summary().unwrap().changed, 1);
}

#[test]
fn xml_literal_whitespace_and_referenced_whitespace_keep_their_meaning() {
    same("<r>a\r\nb</r>", "<r>a\nb</r>", Format::Xml);
    assert_eq!(
        structure::compare("<r a='&#10;'/>", "<r a='\n'/>", Format::Xml, &NeverCancel)
            .unwrap()
            .summary
            .changed,
        1
    );
    assert_eq!(
        structure::compare("<r>&#13;</r>", "<r>\r</r>", Format::Xml, &NeverCancel)
            .unwrap()
            .summary
            .changed,
        1
    );
}

#[test]
fn a_byte_order_mark_is_removed_by_decoding_before_structural_comparison() {
    let (_directory, mut view) = view("xml", "\u{feff}<r a='1'/>", "<r a='1'/>");
    view.run(Command::CompareStructure);
    settle(&mut view);
    assert_eq!(view.structural_summary().unwrap().changed, 0);
}

#[test]
fn decoding_errors_disable_structural_comparison() {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("left.json");
    let b = directory.path().join("right.json");
    std::fs::write(&a, b"\xef\xbb\xbf{\"a\":\"\xff\"}").unwrap();
    std::fs::write(&b, b"\xef\xbb\xbf{\"a\":\"\xff\"}").unwrap();
    let mut view = TextView::new(a, b, &ca_ui::testing::context(), 1);
    settle(&mut view);
    assert!(!view.accepts(Command::CompareStructure));
}

#[test]
fn cancelled_work_cannot_return_a_partial_path_projection() {
    struct StopAfter(std::sync::atomic::AtomicUsize);
    impl ca_diff::Cancel for StopAfter {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 100
        }
    }
    let text = format!("[{}]", vec!["0"; 1000].join(","));
    assert!(structure::compare(
        &text,
        &text,
        Format::Json,
        &StopAfter(std::sync::atomic::AtomicUsize::new(0))
    )
    .err()
    .unwrap()
    .contains("cancelled"));
}

#[test]
fn long_paths_and_repeated_prefix_expansion_are_bounded() {
    let text = format!(r#"{{"{}":0}}"#, "a".repeat(16 * 1024));
    assert!(structure::compare(&text, "{}", Format::Json, &NeverCancel)
        .err()
        .unwrap()
        .contains("path"));
    let text = format!(
        r#"{{"{}":[{}]}}"#,
        "a".repeat(4096),
        vec!["0"; 4000].join(",")
    );
    assert!(structure::compare(&text, "{}", Format::Json, &NeverCancel).is_err());
}

#[test]
fn malformed_mutations_are_bounded_on_a_worker_sized_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            for (seed, format) in [
                (r#"{"a":[true,null,"x"],"b":1.25e2}"#, Format::Json),
                (
                    r#"<r xmlns:p="urn:p"><p:x a="v">text&amp;value</p:x></r>"#,
                    Format::Xml,
                ),
            ] {
                for index in 0..seed.len() {
                    for replacement in [b'<', b'>', b'\'', b'"', b'&', b'[', b'}', 0] {
                        let mut bytes = seed.as_bytes().to_vec();
                        bytes[index] = replacement;
                        let text = String::from_utf8(bytes).unwrap();
                        let _ = structure::compare(&text, seed, format, &NeverCancel);
                    }
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn json_syntax_refusals_name_the_source_line_column_and_byte() {
    let text = "{\"é\": }";
    let error = structure::compare("{}", text, Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert!(error.contains("line 1, column 7, byte 7"), "{error}");

    for ending in ["\n", "\r\n", "\r"] {
        let text = format!("{{{ending} \"a\":1,{ending} //bad{ending}}}");
        let pretty =
            ca_view_text::prettify::format(&text, ca_view_text::prettify::StructuredFormat::Json)
                .unwrap_err();
        assert!(pretty.contains("line 3, column 2, byte"), "{pretty}");
        let error = structure::compare("{}", &text, Format::Json, &NeverCancel)
            .err()
            .unwrap();
        assert!(
            error.starts_with("right side: line 3, column 2, byte"),
            "{error}"
        );
    }
}

#[test]
fn duplicate_key_refusals_identify_a_bounded_escaped_key_and_its_location() {
    let cancelled_key = r#"{"cancelled":1,"cancelled":2}"#;
    let error = structure::compare("{}", cancelled_key, Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert!(error.contains("line 1, column 16, byte 15"), "{error}");

    let text = "{\n \"name\":1,\n \"name\":2\n}";
    let error = structure::compare("{}", text, Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert!(error.contains("line 3, column 2, byte"), "{error}");
    assert!(error.contains("duplicate JSON key \"name\""), "{error}");
    assert!(
        ca_view_text::prettify::format(text, ca_view_text::prettify::StructuredFormat::Json)
            .is_ok()
    );
    let key = format!("a\u{202e}{}", "x".repeat(2_000));
    let text = format!("{{\"{key}\":1,\"{key}\":2}}");
    let error = structure::compare("{}", &text, Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert!(!error.contains('\u{202e}'));
    assert!(error.contains("\\u202e"), "{error}");
    assert!(error.contains("truncated"), "{error}");
    assert!(error.len() < 800, "{} diagnostic bytes", error.len());
}

#[test]
fn xml_syntax_refusals_name_the_source_line_and_byte() {
    let text = "<r>\n<x>\n</r>";
    let error = structure::compare("<r/>", text, Format::Xml, &NeverCancel)
        .err()
        .unwrap();
    assert!(
        error.starts_with("right side: line 3, column 1, byte"),
        "{error}"
    );
    let error = ca_view_text::prettify::format(text, ca_view_text::prettify::StructuredFormat::Xml)
        .unwrap_err();
    assert!(error.contains("line 3, column 1, byte"), "{error}");
}

#[test]
fn duplicate_key_previews_escape_every_control_and_line_separator() {
    let key = "a\u{7f}\u{85}\u{9b}31m\u{2028}\u{2029}b";
    let text = format!("{{\"{key}\":1,\"{key}\":2}}");
    let error = structure::compare("{}", &text, Format::Json, &NeverCancel)
        .err()
        .unwrap();
    assert!(
        !error
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}')),
        "{error:?}"
    );
    assert!(
        error.contains(&format!(
            "duplicate JSON key \"a{0}u007f{0}u0085{0}u009b31m{0}u2028{0}u2029b\"",
            '\\'
        )),
        "{error}"
    );
}
