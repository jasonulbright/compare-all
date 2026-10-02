//! Registry export file parsing, writing and round trip.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_records::limits::Limits;
use ca_records::registry::value::{ValueData, ValueKind, ValueName};
use ca_records::registry::{RegEntry, RegFile, RegFileVersion, RegKeyBlock};
use ca_records::{ByteRange, RecordError};
use std::fmt::Write as _;

const HEADER5: &str = "Windows Registry Editor Version 5.00\r\n";

fn parse(text: &str) -> RegFile {
    RegFile::parse_text(text, &Limits::default()).expect("parse")
}

fn one_value(text: &str) -> ValueData {
    let file = parse(text);
    file.keys
        .first()
        .and_then(|block| block.entries.first())
        .and_then(|entry| entry.data.clone())
        .expect("one value")
}

#[test]
fn the_version_five_header_is_recognised() {
    let file = parse(&format!("{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n"));
    assert_eq!(file.version, RegFileVersion::V5);
    assert_eq!(file.keys.len(), 1);
}

#[test]
fn the_version_four_header_is_recognised() {
    let file = parse("REGEDIT4\r\n\r\n[HKEY_CURRENT_USER\\A]\r\n");
    assert_eq!(file.version, RegFileVersion::V4);
}

#[test]
fn a_file_without_a_header_is_refused() {
    let error =
        RegFile::parse_text("[HKEY_CURRENT_USER\\A]\r\n", &Limits::default()).expect_err("refused");
    assert!(matches!(error, RecordError::Malformed { .. }));
}

#[test]
fn a_utf16_file_decodes_through_the_text_loader() {
    let bytes = common::utf16_file(&format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"V\"\r\n"
    ));
    let file = RegFile::parse(&bytes, &Limits::default()).expect("parse");
    assert_eq!(file.version, RegFileVersion::V5);
    assert_eq!(file.keys.len(), 1);
}

#[test]
fn every_value_type_parses_and_round_trips() {
    let cases: Vec<(&str, ValueData)> = vec![
        ("\"a\"", ValueData::Sz("a".to_owned())),
        ("\"\"", ValueData::Sz(String::new())),
        (
            "\"c:\\\\dir\\\"q\\\"\"",
            ValueData::Sz("c:\\dir\"q\"".to_owned()),
        ),
        ("dword:0000002a", ValueData::Dword(42)),
        ("dword:ffffffff", ValueData::Dword(u32::MAX)),
        ("hex:01,02,ff", ValueData::Binary(vec![1, 2, 255])),
        ("hex:", ValueData::Binary(Vec::new())),
        ("hex(0):01", ValueData::None(vec![1])),
        (
            "hex(2):25,00,50,00,00,00",
            ValueData::ExpandSz("%P".to_owned()),
        ),
        ("hex(5):00,00,00,2a", ValueData::DwordBigEndian(42)),
        ("hex(6):01,02", ValueData::Link(vec![1, 2])),
        (
            "hex(7):41,00,00,00,42,00,00,00,00,00",
            ValueData::MultiSz(vec!["A".to_owned(), "B".to_owned()]),
        ),
        ("hex(b):2a,00,00,00,00,00,00,00", ValueData::Qword(42)),
        (
            "hex(8):01",
            ValueData::Other {
                kind: 8,
                bytes: vec![1],
            },
        ),
        (
            "hex(1f):07",
            ValueData::Other {
                kind: 31,
                bytes: vec![7],
            },
        ),
    ];
    for (written, expected) in cases {
        let text = format!("{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"={written}\r\n");
        let data = one_value(&text);
        assert_eq!(data, expected, "parsing {written}");

        let file = parse(&text);
        let again = RegFile::parse_text(&file.to_text(), &Limits::default()).expect("reparse");
        assert_eq!(again, file, "round trip of {written}");
    }
}

#[test]
fn strings_with_line_breaks_write_as_hex_and_round_trip() {
    let text = format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"Two\"=hex(1):41,00,0d,00,0a,00,42,00,00,00\r\n\"Other\"=\"x\"\r\n"
    );
    let file = parse(&text);
    let bytes = file.to_bytes().expect("write valid string value");
    let again = RegFile::parse(&bytes, &Limits::default()).expect("saved export parses");
    assert_eq!(again, file);
    assert!(again
        .to_text()
        .contains("\"Two\"=hex(1):41,00,0d,00,0a,00,42,00,00,00"));
}

#[test]
fn value_names_with_line_breaks_cannot_be_written_as_reg_files() {
    let file = RegFile {
        version: RegFileVersion::V5,
        keys: vec![RegKeyBlock {
            path: "HKEY_CURRENT_USER\\A".to_owned(),
            delete: false,
            entries: vec![RegEntry {
                name: ValueName::from_raw("bad\nname"),
                data: Some(ValueData::Sz("v".to_owned())),
                source: ByteRange::default(),
            }],
            source: ByteRange::default(),
        }],
    };
    let error = file
        .to_bytes()
        .expect_err("line breaks cannot be represented in a value name");
    assert!(matches!(error, RecordError::Unsupported(_)));
}

#[test]
fn truncated_registry_values_keep_their_raw_bytes() {
    let cases = [
        (ValueKind::Sz, vec![b'a', 0, 0, 0, b'b', 0, 0, 0]),
        (
            ValueKind::MultiSz,
            vec![b'a', 0, 0, 0, 0, 0, b'b', 0, 0, 0, 0, 0],
        ),
        (ValueKind::Dword, vec![1, 0, 0, 0, 2, 0, 0, 0]),
        (ValueKind::DwordBigEndian, vec![0, 0, 0, 1, 0, 0, 0, 2]),
        (ValueKind::Qword, vec![1, 0, 0, 0, 0, 0, 0, 0, 2]),
        (ValueKind::Sz, vec![b'a', 0, 0x21]),
    ];
    for (kind, bytes) in cases {
        let value = ValueData::from_raw(kind, &bytes, &Limits::default()).expect("decode");
        assert_eq!(value.to_raw(), bytes, "{kind:?}");
    }
}

#[test]
fn preserved_registry_values_show_bytes_hidden_by_the_decoded_text() {
    let left = ValueData::from_raw(ValueKind::Sz, &[b'a', 0, b'b', 0], &Limits::default())
        .expect("decode left");
    let right = ValueData::from_raw(ValueKind::Sz, &[b'a', 0, b'c', 0], &Limits::default())
        .expect("decode right");

    assert_ne!(left.to_display(), right.to_display());
    assert!(left.to_display().contains("61 00 62 00"));
    assert!(right.to_display().contains("61 00 63 00"));
}

#[test]
fn expanded_strings_keep_their_meaning_when_export_versions_change() {
    let legacy = parse(
        "REGEDIT4\r\n\r\n[HKEY_CURRENT_USER\\A]\r\n\
         \"P\"=hex(2):61,62\r\n",
    );
    let mut modern = legacy.clone();
    modern.version = RegFileVersion::V5;
    let modern_text = modern.to_text();
    assert!(modern_text.contains("\"P\"=hex(2):61,00,62,00"));
    let reparsed_modern = parse(&modern_text);
    let modern_value = reparsed_modern.keys[0].entries[0]
        .data
        .as_ref()
        .expect("expanded value");
    assert_eq!(
        modern_value.to_display(),
        "ab [raw REG_EXPAND_SZ: 61 00 62 00]"
    );

    let modern_source = parse(&format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"P\"=hex(2):61,00,62,00\r\n"
    ));
    let mut legacy_export = modern_source;
    legacy_export.version = RegFileVersion::V4;
    let legacy_text = legacy_export.to_text();
    assert!(legacy_text.contains("\"P\"=hex(2):61,62,00"));
    let reparsed_legacy = parse(&legacy_text);
    assert_eq!(
        reparsed_legacy.keys[0].entries[0]
            .data
            .as_ref()
            .map(ValueData::to_display),
        Some("ab".to_owned())
    );
}

#[test]
fn the_default_value_uses_the_at_sign() {
    let file = parse(&format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n@=\"root\"\r\n"
    ));
    let entry = file.keys[0].entries[0].clone();
    assert_eq!(entry.name, ValueName::Default);
    assert_eq!(entry.name.display(), "(Default)");
    assert!(file.to_text().contains("@=\"root\""));
}

#[test]
fn a_key_deletion_and_a_value_deletion_parse() {
    let file = parse(&format!(
        "{HEADER5}\r\n[-HKEY_CURRENT_USER\\Gone]\r\n\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=-\r\n"
    ));
    assert!(file.keys[0].delete);
    assert!(file.keys[1].entries[0].is_delete());
    let text = file.to_text();
    assert!(text.contains("[-HKEY_CURRENT_USER\\Gone]"));
    assert!(text.contains("\"N\"=-"));
}

#[test]
fn comments_and_blank_lines_are_skipped() {
    let file = parse(&format!(
        "{HEADER5}\r\n; a comment\r\n\r\n[HKEY_CURRENT_USER\\A]\r\n; another\r\n\"N\"=\"V\"\r\n"
    ));
    assert_eq!(file.keys.len(), 1);
    assert_eq!(file.keys[0].entries.len(), 1);
}

#[test]
fn a_continued_hex_value_joins_its_lines() {
    let file = parse(&format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=hex:01,02,\\\r\n  03,04\r\n"
    ));
    assert_eq!(
        file.keys[0].entries[0].data,
        Some(ValueData::Binary(vec![1, 2, 3, 4]))
    );
}

#[test]
fn a_long_line_parses_without_error() {
    let payload: Vec<String> = (0..40_000).map(|_| "ff".to_owned()).collect();
    let text = format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=hex:{}\r\n",
        payload.join(",")
    );
    assert!(text.lines().any(|line| line.len() > 65_536));
    let data = one_value(&text);
    assert_eq!(data, ValueData::Binary(vec![0xFF; 40_000]));
}

#[test]
fn a_written_hex_value_wraps_with_continuations() {
    let text = format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=hex:{}\r\n",
        vec!["aa"; 200].join(",")
    );
    let file = parse(&text);
    let written = file.to_text();
    assert!(written.contains("\\\r\n  "));
    let again = RegFile::parse_text(&written, &Limits::default()).expect("reparse");
    // The byte ranges follow the new wrapping, so the data is what must match.
    assert_eq!(again.to_text(), written);
    assert_eq!(again.keys[0].entries[0].data, file.keys[0].entries[0].data);
}

#[test]
fn the_record_tree_nests_by_key_path() {
    let file = parse(&format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\Software\\Example]\r\n\"N\"=dword:00000001\r\n"
    ));
    let tree = file.to_tree("left");
    let hive = tree.child("HKEY_CURRENT_USER").expect("hive");
    let software = hive.child("Software").expect("software");
    let example = software.child("Example").expect("example");
    let record = example.record("N").expect("value");
    assert_eq!(record.type_name, "REG_DWORD");
    assert_eq!(record.display, "0x00000001 (1)");
    assert!(record.source.is_some());
    assert_eq!(tree.record_count(), 1);
}

#[test]
fn a_value_over_the_limit_is_refused_before_allocation() {
    let limits = Limits {
        max_value_bytes: 8,
        ..Limits::default()
    };
    let text = format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=hex:{}\r\n",
        vec!["ff"; 64].join(",")
    );
    let error = RegFile::parse_text(&text, &limits).expect_err("refused");
    assert!(error.is_limit());
}

#[test]
fn a_record_count_over_the_limit_is_refused() {
    let limits = Limits {
        max_records: 2,
        ..Limits::default()
    };
    let mut text = format!("{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n");
    for index in 0..8 {
        let _ = write!(text, "\"N{index}\"=\"v\"\r\n");
    }
    assert!(RegFile::parse_text(&text, &limits)
        .expect_err("refused")
        .is_limit());
}

#[test]
fn hostile_and_truncated_input_never_panics() {
    let cases = [
        "REGEDIT4\r\n[",
        "REGEDIT4\r\n[]\r\n",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"unterminated",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\"=",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\"=hex",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\"=hex(:01",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\"=hex(zz):01",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\"=dword:zzzz",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\"=hex:gg",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\"=hex:01,\\",
        "REGEDIT4\r\n\"N\"=\"orphan\"",
        "REGEDIT4\r\n[HKCU\\A]\r\n\"N\\\"=\"x\"",
    ];
    for case in cases {
        // The result may be an error; the requirement is that it returns.
        let _ = RegFile::parse_text(case, &Limits::default());
    }
}

#[test]
fn a_truncated_prefix_of_a_good_file_never_panics() {
    let text = format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=hex(7):41,00,00,00,00,00\r\n@=\"d\"\r\n"
    );
    for end in 0..text.len() {
        let _ = RegFile::parse_text(&text[..end], &Limits::default());
    }
}

#[test]
fn the_written_bytes_carry_the_forms_encoding() {
    let five = parse(&format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n"
    ));
    let bytes = five.to_bytes().expect("bytes");
    assert_eq!(bytes.get(..2), Some([0xFF, 0xFE].as_slice()));

    let four = parse("REGEDIT4\r\n\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n");
    let bytes = four.to_bytes().expect("bytes");
    assert!(bytes.starts_with(b"REGEDIT4"));
}

#[test]
fn regedit4_expand_and_multi_strings_use_windows_1252_bytes() {
    let v4 = parse(
        "REGEDIT4\r\n\r\n[HKEY_CURRENT_USER\\A]\r\n\
         \"Path\"=hex(2):25,53,79,73,74,65,6d,52,6f,6f,74,25,00\r\n\
         \"Names\"=hex(7):61,dc,00,61,df,00,00\r\n",
    );
    let v5 = parse(&format!(
        "{HEADER5}\r\n[HKEY_CURRENT_USER\\A]\r\n\
         \"Path\"=hex(2):25,00,53,00,79,00,73,00,74,00,65,00,6d,00,52,00,6f,00,6f,00,74,00,25,00,00,00\r\n"
    ));
    assert_eq!(
        v4.keys[0].entries[0]
            .data
            .as_ref()
            .map(ValueData::to_display),
        Some("%SystemRoot%".to_owned())
    );
    assert_eq!(
        v5.keys[0].entries[0]
            .data
            .as_ref()
            .map(ValueData::to_display),
        Some("%SystemRoot%".to_owned())
    );
    assert_eq!(
        v4.keys[0].entries[1]
            .data
            .as_ref()
            .map(ValueData::to_display),
        Some("aÜ | aß".to_owned())
    );
    let saved = v4.to_text();
    assert!(saved.contains("hex(2):25,53,79,73,74,65,6d,52,6f,6f,74,25,00"));
    assert!(saved.contains("hex(7):61,dc,00,61,df,00,00"));
    let reparsed = parse(&saved);
    assert_eq!(reparsed.keys[0].entries[0].data, v4.keys[0].entries[0].data);
    assert_eq!(reparsed.keys[0].entries[1].data, v4.keys[0].entries[1].data);
}

#[test]
fn regedit4_empty_multi_string_keeps_its_double_terminator() {
    let file = RegFile {
        version: RegFileVersion::V4,
        keys: vec![RegKeyBlock {
            path: "HKEY_CURRENT_USER\\A".to_owned(),
            delete: false,
            entries: vec![RegEntry {
                name: ValueName::from_raw("Empty"),
                data: Some(ValueData::MultiSz(Vec::new())),
                source: ByteRange::default(),
            }],
            source: ByteRange::default(),
        }],
    };

    let text = file.to_text();
    assert!(text.contains("\"Empty\"=hex(7):00,00"));
    let written = RegFile::parse_text(&text, &Limits::default()).expect("parse written file");
    assert_eq!(
        written.keys[0].entries[0].data,
        Some(ValueData::MultiSz(Vec::new()))
    );
}
