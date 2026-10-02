//! What one cached listing costs in memory.
//!
//! A folder comparison holds a listing of every entry on each side for as long
//! as the comparison is open, so the cost per entry is a property callers
//! depend on rather than an implementation detail. It is measured here instead
//! of reasoned about, because storing a path in a second structure is an easy
//! change to make and doubles the cost without changing any behaviour.

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

use ca_vfs::{Cancel, FileSystem, VfsPath};
use support::{open_memory, tar_bytes};

/// What one cached entry may cost, in bytes.
///
/// The figure covers the record, its path, its name and the links that place
/// it under its parent, for names of the length a source tree has. It is a
/// ceiling with room in it rather than a target.
const BUDGET_PER_ENTRY: usize = 300;

/// A container whose names are shaped like a source tree.
fn source_tree_listing(count: usize) -> (usize, usize) {
    let names: Vec<String> = (0..count)
        .map(|index| format!("project/module{:03}/source_file_{index}.rs", index % 250))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (name.as_str(), b"" as &[u8]))
        .collect();
    let bytes = tar_bytes(&borrowed);
    drop(names);

    let fs = open_memory(bytes, "big.tar").unwrap();
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert!(!root.is_empty(), "the listing answers");
    (fs.entry_count(), fs.listing_bytes())
}

#[test]
fn a_cached_listing_costs_less_than_the_budget_for_each_entry() {
    let count = 100_000;
    let (entries, bytes) = source_tree_listing(count);
    assert!(entries >= count);
    let per_entry = bytes / entries;
    assert!(
        per_entry < BUDGET_PER_ENTRY,
        "{entries} entries hold {bytes} bytes, {per_entry} each, over the {BUDGET_PER_ENTRY} allowed"
    );
}

#[test]
fn the_cost_of_a_listing_grows_with_the_entries_and_not_faster() {
    let (small_entries, small_bytes) = source_tree_listing(10_000);
    let (large_entries, large_bytes) = source_tree_listing(40_000);
    let small = small_bytes as f64 / small_entries as f64;
    let large = large_bytes as f64 / large_entries as f64;
    assert!(
        large < small * 1.5,
        "{small:.0} bytes an entry at {small_entries}, {large:.0} at {large_entries}"
    );
}
