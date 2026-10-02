//! The plain listing parser: it never fails on a line, and it reads back what
//! a Unix server writes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::default_trait_access,
    clippy::assigning_clones,
    clippy::format_push_string,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::needless_pass_by_value,
    clippy::ref_option,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    missing_docs
)]

use ca_vfs::remote::ftp::listing::{parse_machine_line, parse_plain_line, ListedKind, ListedTime};
use ca_vfs::remote::timestamp::{civil_to_unix, month_name, unix_to_civil};
use proptest::prelude::*;

/// The reference the year heuristic works from.
const NOW: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

/// A name a Unix listing can carry, including spaces.
fn name_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("[A-Za-z0-9._][A-Za-z0-9._ -]{0,30}")
        .unwrap()
        .prop_filter("a listing name never ends in a space", |name| {
            !name.ends_with(' ') && !name.trim().is_empty() && name != "." && name != ".."
        })
}

proptest! {
    /// The parser never fails on any line, however the bytes fall.
    #[test]
    fn no_line_can_make_the_parser_fail(line in ".{0,300}") {
        let _ = parse_plain_line(&line, NOW);
        let _ = parse_machine_line(&line);
    }

    /// Lines built from the pieces a real listing uses never fail either.
    #[test]
    fn no_listing_shaped_line_can_make_the_parser_fail(
        permissions in "[-dlbcps][rwxsSt-]{9}",
        links in 0u32..999,
        owner in "[a-z]{1,12}",
        group in "[a-z]{1,12}",
        size in 0u64..u64::MAX,
        month in 1u32..=12,
        day in 1u32..=31,
        hour in 0u32..=23,
        minute in 0u32..=59,
        name in name_strategy(),
    ) {
        let line = format!(
            "{permissions} {links} {owner} {group} {size} {} {day} {hour:02}:{minute:02} {name}",
            month_name(month)
        );
        let _ = parse_plain_line(&line, NOW);
    }

    /// A line this crate can read is read back with the same facts.
    #[test]
    fn a_generated_unix_line_round_trips(
        directory in any::<bool>(),
        size in 0u64..1_000_000_000,
        month in 1u32..=12,
        day in 1u32..=28,
        hour in 0u32..=23,
        minute in 0u32..=59,
        name in name_strategy(),
    ) {
        let permissions = if directory { "drwxr-xr-x" } else { "-rw-r--r--" };
        let year = 2025;
        let line = format!(
            "{permissions}   1 owner group {size:>12} {} {day:>2} {hour:02}:{minute:02} {name}",
            month_name(month)
        );
        let parsed = parse_plain_line(&line, NOW).expect("a well formed line parses");
        prop_assert_eq!(&parsed.name, &name);
        prop_assert_eq!(
            parsed.kind,
            if directory { ListedKind::Directory } else { ListedKind::File }
        );
        if directory {
            prop_assert_eq!(parsed.size, None);
        } else {
            prop_assert_eq!(parsed.size, Some(size));
        }
        let Some(ListedTime::LocalMinute(seconds)) = parsed.modified else {
            return Err(TestCaseError::fail("a clock column is a minute precision time"));
        };
        // The year is not in the line, so the parser chooses the one that puts
        // the date inside the window around the reference.
        let (_, read_month, read_day, read_hour, read_minute, read_second) =
            unix_to_civil(seconds);
        prop_assert_eq!((read_month, read_day, read_hour, read_minute, read_second),
            (month, day, hour, minute, 0));
        prop_assert!(seconds <= civil_to_unix(year + 1, 1, 2, 0, 0, 0));
    }

    /// A machine-readable line is read back with the same facts.
    #[test]
    fn a_generated_machine_line_round_trips(
        directory in any::<bool>(),
        size in 0u64..1_000_000_000,
        year in 1971i64..2099,
        month in 1u32..=12,
        day in 1u32..=28,
        hour in 0u32..=23,
        minute in 0u32..=59,
        second in 0u32..=59,
        name in name_strategy(),
    ) {
        let kind = if directory { "dir" } else { "file" };
        let line = format!(
            "type={kind};size={size};modify={year:04}{month:02}{day:02}{hour:02}{minute:02}\
             {second:02}; {name}"
        );
        let parsed = parse_machine_line(&line).expect("a well formed line parses");
        prop_assert_eq!(&parsed.name, &name);
        prop_assert_eq!(parsed.size, Some(size));
        prop_assert_eq!(
            parsed.modified,
            Some(ListedTime::Utc(civil_to_unix(year, month, day, hour, minute, second)))
        );
    }
}

#[test]
fn the_server_families_a_listing_can_come_from_all_parse() {
    let cases: &[(&str, &str, ListedKind)] = &[
        (
            "-rw-r--r--   1 owner group     1234 Jan  2 03:04 report.txt",
            "report.txt",
            ListedKind::File,
        ),
        (
            "drwxr-xr-x   2 owner group     4096 Feb 10  2019 archive",
            "archive",
            ListedKind::Directory,
        ),
        (
            "lrwxrwxrwx   1 owner group        7 Mar  3 12:00 link -> elsewhere",
            "link",
            ListedKind::Link,
        ),
        (
            "01-02-24  03:04PM              1234 windows.txt",
            "windows.txt",
            ListedKind::File,
        ),
        (
            "01-02-2024  09:00AM       <DIR>          windows folder",
            "windows folder",
            ListedKind::Directory,
        ),
        (
            "-rw-r--r--   1 owner group     1234 Apr  5 06:07:08 2021 seconds.txt",
            "seconds.txt",
            ListedKind::File,
        ),
    ];
    for (line, name, kind) in cases {
        let parsed = parse_plain_line(line, NOW).unwrap_or_else(|| panic!("{line} did not parse"));
        assert_eq!(&parsed.name, name, "{line}");
        assert_eq!(parsed.kind, *kind, "{line}");
    }
}

#[test]
fn a_line_that_is_not_a_listing_is_skipped_rather_than_failing() {
    for line in [
        "total 48",
        "/var/www:",
        "",
        "   ",
        "garbage",
        "-rw-r--r--",
        "type=;name=",
        "\u{0}\u{1}\u{2}",
    ] {
        assert!(parse_plain_line(line, NOW).is_none(), "{line:?}");
    }
}
