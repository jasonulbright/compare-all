//! Local names a path of this platform does not reach as themselves: on
//! Windows, a name that ends with a dot or a space and a DOS device name. A
//! path built from such a name reaches another item, a device or nothing, so
//! the scan lists the item as a refused row and no step reads or writes it.

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ca_fs::{
    align_trees, compare_contents_parallel, compare_quick, execute, plan_copy, plan_delete,
    resolve_selection, scan_with, AlignmentOptions, Bases, Cancel, CompareOptions, ContentMethod,
    ExecutionContext, Journaling, Node, NodeStatus, OperationOptions, RealFs, ScanOptions, Side,
    Sides,
};

/// `name` inside `dir`, spelled so the file system receives it unchanged.
fn exact(dir: &Path, name: &str) -> PathBuf {
    std::fs::canonicalize(dir).unwrap().join(name)
}

fn read_exact(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(exact(dir, name)).unwrap()
}

struct Sides2 {
    _dir: tempfile::TempDir,
    left: PathBuf,
    right: PathBuf,
}

impl Sides2 {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        Self {
            _dir: dir,
            left,
            right,
        }
    }

    fn write(&self, side: Side, name: &str, body: &[u8]) {
        let dir = match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        };
        std::fs::write(exact(dir, name), body).unwrap();
    }

    fn tree(&self, options: &CompareOptions) -> Node {
        let cancel = Cancel::new();
        let left = scan_with(&self.left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
        let right = scan_with(&self.right, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
        let mut tree = align_trees(&left, &right, &AlignmentOptions::default(), &cancel);
        compare_quick(&mut tree, options);
        tree
    }
}

impl Drop for Sides2 {
    fn drop(&mut self) {
        for dir in [&self.left, &self.right] {
            for name in ["foo.", "nul", "dir.", "keep. ", "con .txt."] {
                let path = exact(dir, name);
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

fn binary_content() -> CompareOptions {
    let mut options = CompareOptions::default();
    options.content.enabled = true;
    options.content.method = ContentMethod::Binary;
    options.content.skip_if_quick_same = false;
    options
}

/// `foo` is the same on both sides; `foo.` and `nul` hold different bytes. A
/// plain path to `foo.` reaches `foo`, and a plain path to `nul` reaches the
/// null device, so neither pair may read as the same.
#[test]
fn a_name_the_windows_path_layer_rewrites_is_never_compared_as_another_file() {
    let sides = Sides2::new();
    for side in [Side::Left, Side::Right] {
        sides.write(side, "foo", b"0123456789");
    }
    sides.write(Side::Left, "foo.", b"ABCDEFGHIJ");
    sides.write(Side::Right, "foo.", b"KLMNOPQRST");
    sides.write(Side::Left, "nul", b"left-nul--");
    sides.write(Side::Right, "nul", b"right-nul-");

    let options = binary_content();
    let mut tree = sides.tree(&options);
    let finished = compare_contents_parallel(
        &mut tree,
        &sides.left,
        &sides.right,
        &options,
        None,
        &Cancel::new(),
        &|_| {},
    );
    assert!(finished);

    let rows: BTreeSet<String> = tree
        .children
        .iter()
        .map(|node| node.rel.display().to_string())
        .collect();
    assert_eq!(
        rows,
        BTreeSet::from(["foo".into(), "foo_".into(), "nul_".into()])
    );
    for node in &tree.children {
        if node.rel == Path::new("foo") {
            assert_eq!(node.status, NodeStatus::Same);
            continue;
        }
        assert_eq!(node.status, NodeStatus::Error, "{}", node.rel.display());
        assert!(
            node.content.is_none(),
            "{}: {:?}",
            node.rel.display(),
            node.content
        );
        for entry in [node.left.as_ref(), node.right.as_ref()]
            .into_iter()
            .flatten()
        {
            assert!(entry.refused, "{entry:?}");
        }
    }
    assert_ne!(tree.status, NodeStatus::Same);
}

/// A copy and a delete of the refused rows never reach `foo`, and leave the
/// items they stand for as they were.
#[test]
fn a_copy_or_a_delete_of_a_rewritten_name_never_reaches_another_file() {
    let sides = Sides2::new();
    sides.write(Side::Left, "foo", b"LEFT-PLAIN");
    sides.write(Side::Left, "foo.", b"LEFT-DOT--");
    sides.write(Side::Right, "foo", b"RIGHTPLAIN");
    sides.write(Side::Right, "foo.", b"RIGHT-DOT-");

    let tree = sides.tree(&CompareOptions::default());
    let chosen: BTreeSet<PathBuf> = tree
        .children
        .iter()
        .filter(|node| node.rel != Path::new("foo"))
        .map(|node| node.rel.clone())
        .collect();
    assert!(!chosen.is_empty());
    let selection = resolve_selection(&tree, &chosen);
    let bases = Bases {
        left: &sides.left,
        right: &sides.right,
    };
    let options = OperationOptions {
        use_recycle_bin: false,
        ..OperationOptions::default()
    };
    let cancel = Cancel::new();
    let context = ExecutionContext::new(&RealFs, &cancel, Journaling::Disabled);

    let copy = plan_copy(&selection, Side::Left, bases, &options);
    let _ = execute(&copy, &context);
    let delete = plan_delete(&selection, Sides::Left, bases, &options);
    let _ = execute(&delete, &context);

    assert_eq!(read_exact(&sides.right, "foo"), b"RIGHTPLAIN");
    assert_eq!(read_exact(&sides.right, "foo."), b"RIGHT-DOT-");
    assert_eq!(read_exact(&sides.left, "foo"), b"LEFT-PLAIN");
    assert_eq!(read_exact(&sides.left, "foo."), b"LEFT-DOT--");
}

/// A refused directory is not descended into: a plain path to `dir.` reads
/// `dir`, so nothing under it can be listed as its content.
#[test]
fn a_refused_directory_lists_nothing_under_it() {
    let sides = Sides2::new();
    std::fs::create_dir(exact(&sides.left, "dir.")).unwrap();
    std::fs::write(exact(&sides.left, "dir.").join("inner.txt"), b"inner").unwrap();
    std::fs::create_dir(sides.left.join("dir")).unwrap();
    std::fs::write(sides.left.join("dir").join("other.txt"), b"other").unwrap();

    let cancel = Cancel::new();
    let left = scan_with(&sides.left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let names: BTreeSet<String> = left
        .entries
        .keys()
        .map(|rel| rel.display().to_string())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "dir".into(),
            Path::new("dir").join("other.txt").display().to_string(),
            "dir_".into()
        ]),
        "{:?}",
        left.entries.keys().collect::<Vec<_>>()
    );
    let refused = &left.entries[Path::new("dir_")];
    assert!(refused.refused && refused.is_dir && refused.listing_incomplete);
    assert!(refused
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("\"dir.\""));
}

/// A refused row never takes the name of another item of its folder.
#[test]
fn a_refused_row_takes_a_name_no_other_item_holds() {
    let sides = Sides2::new();
    std::fs::write(sides.left.join("KEEP__"), b"plain").unwrap();
    sides.write(Side::Left, "keep. ", b"dotted");

    let cancel = Cancel::new();
    let left = scan_with(&sides.left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let names: BTreeSet<String> = left
        .entries
        .keys()
        .map(|rel| rel.display().to_string())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["KEEP__".into(), "keep__~1".into()]),
        "{names:?}"
    );
    assert!(left.entries[Path::new("keep__~1")].refused);
    assert!(!left.entries[Path::new("KEEP__")].refused);
}

/// The name a refused row lists under never spells a device itself.
#[test]
fn a_refused_row_name_never_spells_a_device() {
    let sides = Sides2::new();
    sides.write(Side::Left, "con .txt.", b"dotted");

    let cancel = Cancel::new();
    let left = scan_with(&sides.left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let names: Vec<String> = left
        .entries
        .keys()
        .map(|rel| rel.display().to_string())
        .collect();
    assert_eq!(names, vec!["con _.txt_".to_owned()]);
}

/// A plan leaves a refused row alone and says why, so a batch never asks
/// about a path that names nothing.
#[test]
fn a_plan_leaves_a_refused_row_alone_and_says_why() {
    let sides = Sides2::new();
    sides.write(Side::Left, "foo.", b"LEFT-DOT--");
    sides.write(Side::Right, "foo.", b"RIGHT-DOT-");
    std::fs::write(sides.left.join("plain.txt"), b"plain").unwrap();

    let tree = sides.tree(&CompareOptions::default());
    let chosen: BTreeSet<PathBuf> = tree.children.iter().map(|node| node.rel.clone()).collect();
    let selection = resolve_selection(&tree, &chosen);
    let bases = Bases {
        left: &sides.left,
        right: &sides.right,
    };
    let options = OperationOptions {
        use_recycle_bin: false,
        ..OperationOptions::default()
    };
    let refused = Path::new("foo_");
    for plan in [
        plan_copy(&selection, Side::Left, bases, &options),
        plan_delete(&selection, Sides::Both, bases, &options),
    ] {
        assert!(
            plan.steps.iter().all(|step| step.rel != refused),
            "{:?}",
            plan.steps
        );
        assert!(plan
            .steps
            .iter()
            .any(|step| step.rel == Path::new("plain.txt")));
        let skip = plan
            .skipped
            .iter()
            .find(|skip| skip.path == refused)
            .expect("the refused row is reported");
        assert!(skip.reason.contains("\"foo.\""), "{}", skip.reason);
    }
}
