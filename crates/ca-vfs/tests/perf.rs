//! Timings for the container sizes this crate is expected to survive.
//!
//! These are measurements, not assertions: they print and are ignored by
//! default, because the numbers depend on the machine. Run them with
//! `cargo test -p ca-vfs --release -- --ignored --nocapture`.
//!
//! The bounded versions of the same cases, which do fail, live in
//! `regressions.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::default_trait_access,
    clippy::field_reassign_with_default
)]

mod support;

use std::io::Read;
use std::time::Instant;

use ca_vfs::{Cancel, FileSystem, VfsPath};
use support::{gzip, open_memory, sevenz_many, tar_bytes, zip_bytes};

/// Names shaped like a source tree, which is what a real listing looks like.
fn names(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("project/module{:03}/source_file_{index}.rs", index % 250))
        .collect()
}

fn bodies(names: &[String]) -> Vec<(&str, &[u8])> {
    names
        .iter()
        .map(|name| (name.as_str(), b"" as &[u8]))
        .collect()
}

#[test]
#[ignore = "timing harness"]
fn open_and_list_a_three_hundred_thousand_entry_zip() {
    let names = names(300_000);
    let built = Instant::now();
    let bytes = zip_bytes(&bodies(&names));
    println!("  built the container in {:?}", built.elapsed());

    let started = Instant::now();
    let fs = open_memory(bytes, "big.zip").unwrap();
    let opened = started.elapsed();
    let listed = Instant::now();
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    println!(
        "zip: open+list {} entries in {opened:?} (+{:?} to list {} at the root)",
        fs.entry_count(),
        listed.elapsed(),
        root.len()
    );
    println!(
        "     the listing holds {} bytes, {} an entry",
        fs.listing_bytes(),
        fs.listing_bytes() / fs.entry_count().max(1)
    );
}

#[test]
#[ignore = "timing harness"]
fn open_and_list_a_three_hundred_thousand_entry_compressed_tar() {
    let names = names(300_000);
    let bytes = gzip(&tar_bytes(&bodies(&names)));

    let started = Instant::now();
    let fs = open_memory(bytes, "big.tar.gz").unwrap();
    println!(
        "tar.gz: open+list {} entries in {:?}",
        fs.entry_count(),
        started.elapsed()
    );
}

#[test]
#[ignore = "timing harness"]
fn read_every_entry_of_a_twenty_thousand_entry_solid_container() {
    let count = 20_000;
    let fs = open_memory(sevenz_many(count), "big.7z").unwrap();

    let started = Instant::now();
    let mut bytes = 0u64;
    for index in 0..count {
        let path = VfsPath::parse(&format!("f{index}.txt")).unwrap();
        let mut open = fs
            .open(&path, &Cancel::new())
            .unwrap_or_else(|error| panic!("entry {index}: {error}"));
        let mut sink = Vec::new();
        open.read_to_end(&mut sink).unwrap();
        bytes += sink.len() as u64;
    }
    let elapsed = started.elapsed();
    println!(
        "7z: read {count} entries ({bytes} bytes) in {elapsed:?}, {:.0} entries a second",
        count as f64 / elapsed.as_secs_f64()
    );
}

#[test]
#[ignore = "timing harness"]
fn read_every_entry_of_a_fifty_thousand_entry_compressed_tar() {
    let count = 50_000;
    let names: Vec<String> = (0..count).map(|index| format!("f{index}.txt")).collect();
    let contents: Vec<Vec<u8>> = (0..count)
        .map(|index| format!("body {index}").into_bytes())
        .collect();
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .zip(&contents)
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    let fs = open_memory(gzip(&tar_bytes(&entries)), "big.tar.gz").unwrap();

    let started = Instant::now();
    let mut bytes = 0u64;
    for index in 0..count {
        let path = VfsPath::parse(&format!("f{index}.txt")).unwrap();
        let mut open = fs
            .open(&path, &Cancel::new())
            .unwrap_or_else(|error| panic!("entry {index}: {error}"));
        let mut sink = Vec::new();
        open.read_to_end(&mut sink).unwrap();
        bytes += sink.len() as u64;
    }
    let elapsed = started.elapsed();
    println!(
        "tar.gz: read {count} entries ({bytes} bytes) in {elapsed:?}, {:.0} entries a second",
        count as f64 / elapsed.as_secs_f64()
    );
}
