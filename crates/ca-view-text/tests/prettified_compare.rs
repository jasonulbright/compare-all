//! End-to-end checks for formatted, read-only structured text comparisons.

#![allow(clippy::expect_used)]

use ca_ui::command::Command;
use ca_ui::view::SessionView;
use ca_view_text::TextView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn wait_until_settled(view: &mut TextView) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !view.is_settled() && Instant::now() < deadline {
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(view.is_settled(), "comparison did not settle");
}

fn compare_pair(extension: &str, left_text: &str, right_text: &str) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let left = directory.path().join(format!("left.{extension}"));
    let right = directory.path().join(format!("right.{extension}"));
    std::fs::write(&left, left_text).expect("write left fixture");
    std::fs::write(&right, right_text).expect("write right fixture");
    let before_left = std::fs::read(&left).expect("read left fixture");
    let before_right = std::fs::read(&right).expect("read right fixture");

    let mut view = TextView::new(
        PathBuf::from(&left),
        PathBuf::from(&right),
        &ca_ui::testing::context(),
        1,
    );
    wait_until_settled(&mut view);
    assert!(view
        .commands()
        .iter()
        .any(|state| state.command == Command::PrettifyForComparison && state.enabled));
    assert!(view.status_fields()[0].starts_with("1 difference section"));

    view.run(Command::PrettifyForComparison);
    wait_until_settled(&mut view);
    assert_eq!(view.status_fields()[0], "0 difference section(s)");
    assert!(view.pane(ca_view_text::sidecopy::Side::Left).is_read_only());
    assert!(view
        .pane(ca_view_text::sidecopy::Side::Right)
        .is_read_only());
    assert!(!view.accepts(Command::SaveFile));
    assert_eq!(
        std::fs::read(&left).expect("left stays unchanged"),
        before_left
    );
    assert_eq!(
        std::fs::read(&right).expect("right stays unchanged"),
        before_right
    );

    view.run(Command::PrettifyForComparison);
    wait_until_settled(&mut view);
    assert!(view.status_fields()[0].starts_with("1 difference section"));
    assert!(!view.pane(ca_view_text::sidecopy::Side::Left).is_read_only());
    assert_eq!(
        std::fs::read(&left).expect("left stays unchanged"),
        before_left
    );
    assert_eq!(
        std::fs::read(&right).expect("right stays unchanged"),
        before_right
    );
}

#[test]
fn minified_json_can_be_compared_against_indented_json_without_writing_either_file() {
    compare_pair(
        "json",
        r#"{"service":"demo","ports":[80,443],"active":true}"#,
        "{\n  \"service\": \"demo\",\n  \"ports\": [80, 443],\n  \"active\": true\n}",
    );
}

#[test]
fn compressed_xml_can_be_compared_against_indented_xml_without_writing_either_file() {
    compare_pair(
        "xml",
        "<settings><service><name>demo</name></service><port>80</port></settings>",
        "<settings>\n  <service>\n    <name>demo</name>\n  </service>\n  <port>80</port>\n</settings>",
    );
}

#[test]
fn flow_yaml_can_be_compared_against_block_yaml_without_writing_either_file() {
    compare_pair(
        "yaml",
        "# service\n{service: demo, ports: [80, 443], tls: {enabled: true}} # end\n",
        "# service\nservice: demo\nports:\n    - 80\n    - 443\ntls:\n    enabled: true # end\n",
    );
}

#[test]
fn inline_toml_tables_can_be_compared_against_standard_tables_without_writing_either_file() {
    compare_pair(
        "toml",
        "name = \"demo\"\nserver = { host = \"h\", ports = [80, 443] } # main\n",
        "name = \"demo\"\n\n[server] # main\nhost = \"h\"\nports = [\n  80,\n  443,\n]\n",
    );
}

#[test]
fn malformed_input_on_one_side_leaves_the_original_text_editable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let left = directory.path().join("left.yml");
    let right = directory.path().join("right.yml");
    let left_text = "a: [1, 2\n";
    std::fs::write(&left, left_text).expect("write left fixture");
    std::fs::write(&right, "a: [1, 2]\n").expect("write right fixture");

    let mut view = TextView::new(
        PathBuf::from(&left),
        PathBuf::from(&right),
        &ca_ui::testing::context(),
        1,
    );
    wait_until_settled(&mut view);
    view.run(Command::PrettifyForComparison);
    let deadline = Instant::now() + Duration::from_secs(10);
    while view
        .message()
        .is_none_or(|message| !message.starts_with("Could not format"))
        && Instant::now() < deadline
    {
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    wait_until_settled(&mut view);

    assert!(view
        .message()
        .is_some_and(|message| message.contains("left side: invalid YAML")));
    let pane = view.pane(ca_view_text::sidecopy::Side::Left);
    assert_eq!(pane.buffer().text(), left_text);
    assert!(!pane.is_read_only());
    assert!(view
        .commands()
        .iter()
        .any(|state| state.command == Command::PrettifyForComparison && state.enabled));
}

#[test]
fn yaml_and_toml_extensions_offer_the_formatter_only_for_matching_pairs() {
    let pair = |left: &str, right: &str| {
        ca_view_text::prettify::pair_format(Path::new(left), Path::new(right))
    };
    assert_eq!(
        pair("a.yml", "b.YAML"),
        Some(ca_view_text::prettify::StructuredFormat::Yaml)
    );
    assert_eq!(
        pair("a.toml", "b.toml"),
        Some(ca_view_text::prettify::StructuredFormat::Toml)
    );
    assert_eq!(pair("a.yaml", "b.toml"), None);
    assert_eq!(pair("a.ini", "b.ini"), None);
}

#[test]
fn json_comments_and_json_lines_do_not_offer_the_strict_json_formatter() {
    assert!(
        ca_view_text::prettify::pair_format(Path::new("left.jsonc"), Path::new("right.jsonc"))
            .is_none()
    );
    assert!(
        ca_view_text::prettify::pair_format(Path::new("left.jsonl"), Path::new("right.jsonl"))
            .is_none()
    );
}
