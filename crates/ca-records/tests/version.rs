//! Version resource reading and comparison.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_records::compare::Status;
use ca_records::limits::Limits;
use ca_records::version::{
    compare, read, LanguageChoice, VersionCompareOptions, VersionReadOptions,
};
use ca_records::RecordTree;

fn binary(entries: &[(&str, &str)], is_64_bit: bool, signed: bool) -> Vec<u8> {
    let resource = common::version_resource(entries, (1, 2, 3, 4));
    common::pe_with_version(&resource, is_64_bit, signed)
}

fn tree(entries: &[(&str, &str)], is_64_bit: bool, signed: bool) -> RecordTree {
    read(
        &binary(entries, is_64_bit, signed),
        &VersionReadOptions::default(),
    )
    .expect("read")
}

fn record<'a>(tree: &'a RecordTree, group: &str, name: &str) -> &'a ca_records::Record {
    let mut stack = vec![tree];
    while let Some(node) = stack.pop() {
        if node.name.eq_ignore_ascii_case(group) {
            if let Some(found) = node.record(name) {
                return found;
            }
        }
        stack.extend(node.children.iter());
    }
    panic!("no record {group}/{name}");
}

#[test]
fn the_string_table_entries_appear_with_their_byte_ranges() {
    let tree = tree(
        &[("FileDescription", "Fixture"), ("CompanyName", "Nobody")],
        false,
        false,
    );
    let entry = record(&tree, "040904B0", "FileDescription");
    assert_eq!(entry.display, "Fixture");
    let range = entry.source.expect("range");
    assert!(range.len > 0);
    assert!(range.end() <= u64::try_from(binary(&[], false, false).len() + 4096).unwrap());
}

#[test]
fn the_fixed_block_reports_the_dotted_versions() {
    let tree = tree(&[("FileVersion", "1.2.3.4")], false, false);
    assert_eq!(
        record(&tree, "FixedFileInfo", "File Version").display,
        "1.2.3.4"
    );
    assert_eq!(
        record(&tree, "FixedFileInfo", "File Type").display,
        "Application"
    );
    assert_eq!(
        record(&tree, "FixedFileInfo", "File Flags").display,
        "Debug"
    );
}

#[test]
fn the_translation_list_is_read() {
    let tree = tree(&[("FileVersion", "1.0")], false, false);
    assert_eq!(
        record(&tree, "VarFileInfo", "Translation").display,
        "040904B0"
    );
}

#[test]
fn the_header_facts_report_the_processor_and_word_size() {
    let thirty_two = tree(&[("FileVersion", "1.0")], false, false);
    assert_eq!(record(&thirty_two, "File", "Processor").display, "x86");
    assert_eq!(record(&thirty_two, "File", "Word Size").display, "32-bit");
    assert_eq!(
        record(&thirty_two, "File", "Subsystem").display,
        "Windows console"
    );
    assert_eq!(
        record(&thirty_two, "File", "Time Stamp").display,
        "0x12345678"
    );

    let sixty_four = tree(&[("FileVersion", "1.0")], true, false);
    assert_eq!(record(&sixty_four, "File", "Processor").display, "x64");
    assert_eq!(record(&sixty_four, "File", "Word Size").display, "64-bit");
}

#[test]
fn the_signature_block_is_reported_by_presence_only() {
    let unsigned = tree(&[("FileVersion", "1.0")], false, false);
    assert_eq!(
        record(&unsigned, "File", "Signature Block").display,
        "absent"
    );
    let signed = tree(&[("FileVersion", "1.0")], false, true);
    assert_eq!(
        record(&signed, "File", "Signature Block").display,
        "present"
    );
}

#[test]
fn the_dll_flags_are_named() {
    let tree = tree(&[("FileVersion", "1.0")], false, false);
    let flags = record(&tree, "File", "DLL Flags").display.clone();
    assert!(flags.contains("Relocatable"));
    assert!(flags.contains("Control flow guard"));
}

#[test]
fn a_language_choice_that_no_table_matches_falls_back_to_the_first() {
    let options = VersionReadOptions {
        language: LanguageChoice::Specific {
            language: 0x0C0C,
            code_page: 0x04B0,
        },
        ..VersionReadOptions::default()
    };
    let bytes = binary(&[("FileVersion", "1.0")], false, false);
    let tree = read(&bytes, &options).expect("read");
    assert_eq!(record(&tree, "040904B0", "FileVersion").display, "1.0");
}

#[test]
fn two_binaries_compare_field_by_field() {
    let left = tree(
        &[("FileVersion", "1.0"), ("CompanyName", "A")],
        false,
        false,
    );
    let right = tree(
        &[("FileVersion", "2.0"), ("CompanyName", "A")],
        false,
        false,
    );
    let merged = compare(&left, &right, &VersionCompareOptions::default());
    assert_eq!(merged.status, Status::Different);
    let counts = merged.counts();
    assert_eq!(counts.different, 1);
    let rows = merged.rows();
    let changed = rows
        .iter()
        .find(|row| row.name == "FileVersion")
        .expect("row");
    assert_eq!(changed.left.as_deref(), Some("1.0"));
    assert_eq!(changed.right.as_deref(), Some("2.0"));
}

#[test]
fn a_field_on_one_side_only_is_an_orphan_row() {
    let left = tree(&[("FileVersion", "1.0"), ("Extra", "x")], false, false);
    let right = tree(&[("FileVersion", "1.0")], false, false);
    let merged = compare(&left, &right, &VersionCompareOptions::default());
    assert_eq!(merged.counts().left_only, 1);
}

#[test]
fn a_file_that_is_not_a_windows_binary_is_refused() {
    assert!(read(b"not a binary", &VersionReadOptions::default()).is_err());
    assert!(read(&[], &VersionReadOptions::default()).is_err());
    assert!(read(b"MZ", &VersionReadOptions::default()).is_err());
}

#[test]
fn a_truncated_binary_never_panics() {
    let bytes = binary(&[("FileVersion", "1.0")], false, false);
    for end in 0..bytes.len() {
        let _ = read(&bytes[..end], &VersionReadOptions::default());
    }
}

#[test]
fn a_hostile_section_count_is_refused_before_allocation() {
    let mut bytes = binary(&[("FileVersion", "1.0")], false, false);
    let coff = 0x80 + 4;
    bytes[coff + 2..coff + 4].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(read(&bytes, &VersionReadOptions::default()).is_err());
}

#[test]
fn a_resource_larger_than_the_limit_is_refused() {
    let options = VersionReadOptions {
        limits: Limits {
            max_value_bytes: 8,
            ..Limits::default()
        },
        ..VersionReadOptions::default()
    };
    let bytes = binary(&[("FileVersion", "1.0")], false, false);
    assert!(read(&bytes, &options).expect_err("refused").is_limit());
}

#[test]
fn a_resource_directory_named_by_more_than_one_entry_is_refused() {
    let resource = common::version_resource(&[("FileVersion", "1.0")], (1, 2, 3, 4));
    for count in [64u16, u16::MAX] {
        let section = common::shared_resource_directories(count, &resource);
        let bytes = common::pe_with_resource_section(&section, false, false);
        let started = std::time::Instant::now();
        let result = read(&bytes, &VersionReadOptions::default());
        assert!(result.is_err(), "{count} entries a level were read");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "{count} entries a level took {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn every_resource_entry_the_walk_visits_counts_against_one_budget() {
    let resource = common::version_resource(&[("FileVersion", "1.0")], (1, 2, 3, 4));
    let section = common::distinct_resource_directories(50, 50, &resource);
    let bytes = common::pe_with_resource_section(&section, false, false);
    assert!(read(&bytes, &VersionReadOptions::default()).is_ok());
    let options = VersionReadOptions {
        limits: Limits {
            max_records: 1_000,
            ..Limits::default()
        },
        ..VersionReadOptions::default()
    };
    assert!(
        read(&bytes, &options).expect_err("refused").is_limit(),
        "2500 leaves under directories of 50 entries each pass a budget of 1000"
    );
}
