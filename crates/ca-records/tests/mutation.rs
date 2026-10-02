//! Mutation properties: no parser panics, and each case returns inside a bound.
//!
//! Each property starts from a valid fixture, applies random byte mutations,
//! truncations and insertions, and asserts that the parser returns. The wall
//! clock assertion runs in the release profile only, because the debug profile
//! has no meaningful time budget.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_records::limits::Limits;
use ca_records::media::{read as read_media, MediaReadOptions};
use ca_records::registry::RegFile;
use ca_records::version::{read as read_version, VersionReadOptions};
use proptest::prelude::*;
use std::time::{Duration, Instant};

/// Time budget for one mutated case, applied in the release profile only.
const CASE_BUDGET: Duration = Duration::from_secs(2);

fn check_budget(start: Instant, what: &str) {
    if cfg!(debug_assertions) {
        return;
    }
    let taken = start.elapsed();
    assert!(
        taken <= CASE_BUDGET,
        "{what} took longer than the budget for one case"
    );
}

/// Apply the mutation operations to a copy of `seed`.
fn mutate(seed: &[u8], ops: &[(usize, u8, u8)]) -> Vec<u8> {
    let mut out = seed.to_vec();
    for (position, byte, action) in ops {
        if out.is_empty() {
            break;
        }
        let at = position % out.len();
        match action % 3 {
            0 => out[at] = *byte,
            1 => out.truncate(at),
            _ => out.insert(at, *byte),
        }
    }
    out
}

fn ops_strategy() -> impl Strategy<Value = Vec<(usize, u8, u8)>> {
    proptest::collection::vec((any::<usize>(), any::<u8>(), any::<u8>()), 1..12)
}

fn reg_seed() -> Vec<u8> {
    let text = "Windows Registry Editor Version 5.00\r\n\r\n\
         [HKEY_CURRENT_USER\\Software\\Example]\r\n\
         \"Text\"=\"value\"\r\n\
         \"Number\"=dword:0000002a\r\n\
         \"Blob\"=hex:01,02,03,\\\r\n  04,05\r\n\
         \"Multi\"=hex(7):41,00,00,00,42,00,00,00,00,00\r\n\
         @=\"default\"\r\n\r\n\
         [-HKEY_CURRENT_USER\\Software\\Gone]\r\n";
    text.as_bytes().to_vec()
}

fn version_seed() -> Vec<u8> {
    let resource = common::version_resource(
        &[("FileVersion", "1.0.0.0"), ("CompanyName", "Fixture")],
        (1, 0, 0, 0),
    );
    common::pe_with_version(&resource, false, true)
}

fn media_seeds() -> Vec<Vec<u8>> {
    let mut mp3 = common::id3v2(&[("TIT2", "Title"), ("TPE1", "Artist")]);
    mp3.extend_from_slice(&common::mpeg_frames(8));
    mp3.extend_from_slice(&common::id3v1("T", "A", "L", Some(3)));
    vec![
        mp3,
        common::flac(44100, 2, &[("TITLE", "Song")]),
        common::wav(44100, 2, 512),
        common::mp4(1500, &[("\u{a9}nam", "Song")]),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_mutated_registry_file_never_panics(ops in ops_strategy()) {
        let bytes = mutate(&reg_seed(), &ops);
        let start = Instant::now();
        let _ = RegFile::parse(&bytes, &Limits::default());
        check_budget(start, "the registry parser");
    }

    #[test]
    fn a_mutated_binary_never_panics(ops in ops_strategy()) {
        let bytes = mutate(&version_seed(), &ops);
        let start = Instant::now();
        let _ = read_version(&bytes, &VersionReadOptions::default());
        check_budget(start, "the version parser");
    }

    #[test]
    fn a_mutated_media_file_never_panics(index in 0usize..4, ops in ops_strategy()) {
        let seeds = media_seeds();
        let seed = seeds.get(index % seeds.len()).cloned().unwrap_or_default();
        let bytes = mutate(&seed, &ops);
        let start = Instant::now();
        let _ = read_media(&bytes, &MediaReadOptions::default());
        check_budget(start, "the media parser");
    }

    #[test]
    fn arbitrary_bytes_never_panic_in_any_parser(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let start = Instant::now();
        let _ = RegFile::parse(&bytes, &Limits::default());
        let _ = read_version(&bytes, &VersionReadOptions::default());
        let _ = read_media(&bytes, &MediaReadOptions::default());
        check_budget(start, "the parsers");
    }
}
