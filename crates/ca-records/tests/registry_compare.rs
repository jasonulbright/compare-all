//! Registry tree comparison and planned edits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_records::compare::{AlignOptions, Status};
use ca_records::limits::Limits;
use ca_records::registry::plan::{preview_value, EditOp, EditPlan, Side};
use ca_records::registry::value::{ValueData, ValueName};
use ca_records::registry::{compare, RegFile, RegistryCompareOptions};
use ca_records::RecordTree;
use std::fmt::Write as _;

const HEADER: &str = "Windows Registry Editor Version 5.00\r\n";

fn file(body: &str) -> RegFile {
    RegFile::parse_text(&format!("{HEADER}{body}"), &Limits::default()).expect("parse")
}

fn tree(body: &str) -> RecordTree {
    file(body).to_tree("side")
}

fn options() -> RegistryCompareOptions {
    RegistryCompareOptions::default()
}

fn find<'a>(node: &'a ca_records::TreeDiff, name: &str) -> Option<&'a ca_records::TreeDiff> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.name.eq_ignore_ascii_case(name) {
            return Some(current);
        }
        stack.extend(current.children.iter());
    }
    None
}

#[test]
fn matching_trees_compare_as_the_same() {
    let body = "\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n";
    let merged = compare(&tree(body), &tree(body), &options());
    assert_eq!(merged.status, Status::Same);
    assert_eq!(merged.counts().same, 1);
    assert_eq!(merged.counts().different, 0);
}

#[test]
fn values_with_data_after_a_terminator_or_width_mismatch_compare_as_different() {
    let left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"Sz\"=hex(1):61,00,00,00,62,00,00,00\r\n\"Multi\"=hex(7):61,00,00,00,00,00,62,00,00,00,00,00\r\n\"Dw\"=hex(4):01,00,00,00,02,00,00,00\r\n");
    let right = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"Sz\"=hex(1):61,00,00,00,63,00,00,00\r\n\"Multi\"=hex(7):61,00,00,00,00,00,63,00,00,00,00,00\r\n\"Dw\"=hex(4):01,00,00,00,03,00,00,00\r\n");
    let merged = compare(&left.to_tree("left"), &right.to_tree("right"), &options());
    assert_eq!(merged.counts().different, 3);

    let saved = left.to_text();
    assert!(saved.contains("\"Sz\"=hex(1):61,00,00,00,62,00,00,00"));
    assert!(saved.contains("\"Multi\"=hex(7):61,00,00,00,00,00,62,00,00,00,00,00"));
    assert!(saved.contains("\"Dw\"=hex(4):01,00,00,00,02,00,00,00"));
    assert_eq!(
        RegFile::parse_text(&saved, &Limits::default()).expect("reparse saved values"),
        left
    );
}

#[test]
fn a_changed_value_rolls_a_different_status_up_the_keys() {
    let left = tree("\r\n[HKEY_CURRENT_USER\\A\\B]\r\n\"N\"=\"one\"\r\n");
    let right = tree("\r\n[HKEY_CURRENT_USER\\A\\B]\r\n\"N\"=\"two\"\r\n");
    let merged = compare(&left, &right, &options());
    assert_eq!(merged.status, Status::Different);
    assert_eq!(find(&merged, "A").expect("key A").status, Status::Different);
    assert_eq!(merged.counts().different, 1);
}

#[test]
fn a_key_on_one_side_only_is_an_orphan() {
    let left = tree("\r\n[HKEY_CURRENT_USER\\Only]\r\n\"N\"=\"v\"\r\n");
    let right = tree("\r\n[HKEY_CURRENT_USER\\Other]\r\n\"N\"=\"v\"\r\n");
    let merged = compare(&left, &right, &options());
    assert_eq!(find(&merged, "Only").expect("key").status, Status::LeftOnly);
    assert_eq!(
        find(&merged, "Other").expect("key").status,
        Status::RightOnly
    );
    let counts = merged.counts();
    assert_eq!(counts.left_only, 1);
    assert_eq!(counts.right_only, 1);
}

#[test]
fn key_names_align_without_regard_to_case() {
    let left = tree("\r\n[HKEY_CURRENT_USER\\Alpha]\r\n\"Name\"=\"v\"\r\n");
    let right = tree("\r\n[HKEY_CURRENT_USER\\ALPHA]\r\n\"NAME\"=\"v\"\r\n");
    let merged = compare(&left, &right, &options());
    assert_eq!(merged.status, Status::Same);
}

#[test]
fn a_type_change_alone_is_a_difference() {
    let left = tree("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=hex:41\r\n");
    let right = tree("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=hex(6):41\r\n");
    let merged = compare(&left, &right, &options());
    assert_eq!(merged.counts().different, 1);

    let loose = RegistryCompareOptions {
        align: AlignOptions {
            compare_types: false,
            ..AlignOptions::default()
        },
        ..RegistryCompareOptions::default()
    };
    let merged = compare(&left, &right, &loose);
    assert_eq!(
        merged.counts().different,
        0,
        "the payload is equal, so only the type differed"
    );
}

#[test]
fn an_unimportant_field_is_filtered_by_the_option() {
    let left = tree("\r\n[HKEY_CURRENT_USER\\A]\r\n\"Noise\"=\"one\"\r\n");
    let right = tree("\r\n[HKEY_CURRENT_USER\\A]\r\n\"Noise\"=\"two\"\r\n");
    let mut align = AlignOptions {
        ignore_unimportant: true,
        ..AlignOptions::default()
    };
    align.unimportant_fields.insert("noise".to_owned());
    let merged = compare(
        &left,
        &right,
        &RegistryCompareOptions {
            align,
            ..RegistryCompareOptions::default()
        },
    );
    assert_eq!(merged.counts().unimportant, 1);
    assert_eq!(merged.counts().different, 0);
    assert_eq!(merged.status, Status::Same);
}

#[test]
fn the_flattened_rows_carry_both_display_forms() {
    let left = tree("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=dword:00000001\r\n");
    let right = tree("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=dword:00000002\r\n");
    let merged = compare(&left, &right, &options());
    let rows = merged.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "N");
    assert_eq!(rows[0].group.as_deref(), Some("HKEY_CURRENT_USER\\A"));
    assert_eq!(rows[0].left.as_deref(), Some("0x00000001 (1)"));
    assert_eq!(rows[0].right.as_deref(), Some("0x00000002 (2)"));
}

#[test]
fn a_deep_tree_compares_without_exhausting_the_stack() {
    let mut path = String::from("HKEY_CURRENT_USER");
    for index in 0..2000 {
        let _ = write!(path, "\\k{index}");
    }
    let body = format!("\r\n[{path}]\r\n\"N\"=\"v\"\r\n");
    let merged = compare(&tree(&body), &tree(&body), &options());
    assert_eq!(merged.counts().same, 1);
}

#[test]
fn a_plan_copies_a_value_into_the_other_file() {
    let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"left\"\r\n");
    let mut right = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"Other\"=\"right\"\r\n");
    let plan = EditPlan::new().with(EditOp::copy_value(
        Side::Left,
        "HKEY_CURRENT_USER\\A",
        ValueName::from_raw("N"),
    ));
    plan.apply_to_files(&mut left, &mut right).expect("apply");
    assert_eq!(
        preview_value(&right, "HKEY_CURRENT_USER\\A", &ValueName::from_raw("N")),
        Some(ValueData::Sz("left".to_owned()))
    );
    assert_eq!(right.keys[0].entries.len(), 2);
}

#[test]
fn a_plan_copies_a_key_and_everything_under_it() {
    let mut left = file(
        "\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n\r\n[HKEY_CURRENT_USER\\A\\Child]\r\n\"C\"=\"c\"\r\n",
    );
    let mut right = file("\r\n[HKEY_CURRENT_USER\\Z]\r\n\"N\"=\"v\"\r\n");
    let plan = EditPlan::new().with(EditOp::copy_key(Side::Left, "HKEY_CURRENT_USER\\A"));
    plan.apply_to_files(&mut left, &mut right).expect("apply");
    assert!(right.key("HKEY_CURRENT_USER\\A").is_some());
    assert!(right.key("HKEY_CURRENT_USER\\A\\Child").is_some());
}

#[test]
fn a_plan_writes_the_deletion_forms() {
    let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n");
    let mut right = file("\r\n[HKEY_CURRENT_USER\\B]\r\n\"N\"=\"v\"\r\n");
    let plan = EditPlan::new()
        .with(EditOp::delete_key(Side::Left, "HKEY_CURRENT_USER\\A"))
        .with(EditOp::delete_value(
            Side::Right,
            "HKEY_CURRENT_USER\\B",
            ValueName::from_raw("N"),
        ));
    plan.apply_to_files(&mut left, &mut right).expect("apply");
    assert!(left.to_text().contains("[-HKEY_CURRENT_USER\\A]"));
    assert!(right.to_text().contains("\"N\"=-"));
}

#[test]
fn a_plan_naming_a_missing_value_is_refused() {
    let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n");
    let mut right = file("\r\n[HKEY_CURRENT_USER\\A]\r\n");
    let plan = EditPlan::new().with(EditOp::copy_value(
        Side::Left,
        "HKEY_CURRENT_USER\\A",
        ValueName::from_raw("Missing"),
    ));
    assert!(plan.apply_to_files(&mut left, &mut right).is_err());
}

#[test]
fn applying_a_plan_to_the_live_registry_is_refused() {
    let plan = EditPlan::new().with(EditOp::delete_key(Side::Left, "HKEY_CURRENT_USER\\A"));
    let error = plan.apply_to_live().expect_err("refused");
    assert!(matches!(error, ca_records::RecordError::Unsupported(_)));
}
