//! The listing a container or a recorded capture is browsed through.
//!
//! A container names its entries in a flat list and may leave the directories
//! above them implied, so the listing is rebuilt here into a tree. Three
//! properties matter more than anything else in this module.
//!
//! No record is ever dropped, and no name is listed twice under one parent. A
//! container may hold both a file `a` and an entry `a/b`, or two records under
//! one name, which no real file system can represent. The directory wins the
//! name, or the first record does, and every other record is kept beside it
//! under a disambiguated name, with its own content locator and an error that
//! says what happened. Losing a record would silently omit a whole subtree
//! from a folder comparison, and a name listed twice would leave one of the
//! two unreachable.
//!
//! Nothing here recurses. A name can carry hundreds of components and a
//! listing can carry hundreds of thousands of names, so every walk over either
//! is a loop.
//!
//! Nothing here stores a path twice. Records live in one vector, a parent
//! names its children as a range of one shared index, and lookup is a binary
//! search over an ordering of the same vector. Every buffer is shrunk once the
//! listing is built, because a listing is read for the life of a comparison
//! and never added to, and growth slack would otherwise be half of it again.

use std::collections::{HashMap, HashSet};

use crate::entry::{EntryKind, TimeFidelity, VfsEntry};
use crate::path::{VfsPath, MAX_PATH_BYTES};

/// A locator value meaning the entry carries none.
const NO_LOCATOR: u64 = u64::MAX;

/// Most entries one tree holds, so a node is addressed by a `u32`.
///
/// Every expansion ceiling in this crate is reached long before a listing gets
/// near this, and entries past it are refused rather than truncated into a
/// wrapped index that would point at the wrong node.
const MAX_NODES: usize = (u32::MAX - 1) as usize;

/// Most listed entries accepted, leaving room for the directories they imply.
const MAX_LISTED: usize = MAX_NODES / 2;

/// The index a node about to be pushed will take.
fn node_id(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// One node of the finished listing.
#[derive(Debug)]
struct Node {
    entry: VfsEntry,
    /// Where this node's children start in [`Tree::child_index`].
    first_child: u32,
    child_count: u32,
    /// Backend-private position of the entry's content, or [`NO_LOCATOR`].
    locator: u64,
}

/// One entry on the way in, before the tree is built.
#[derive(Debug)]
pub(crate) struct RawEntry {
    /// The listing record.
    pub entry: VfsEntry,
    /// Where the backend can find the content again, when it can say.
    pub locator: Option<u64>,
}

impl From<VfsEntry> for RawEntry {
    fn from(entry: VfsEntry) -> Self {
        Self {
            entry,
            locator: None,
        }
    }
}

/// A node while the tree is still being assembled.
#[derive(Debug)]
struct BuildNode {
    entry: VfsEntry,
    children: Vec<u32>,
    locator: u64,
    /// The child that most recently took a name of its own.
    last_named: Option<u32>,
}

/// A listing keyed by path, with the directories entries imply filled in.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    nodes: Vec<Node>,
    /// Every node's children, laid end to end.
    child_index: Vec<u32>,
    /// Node indices ordered by path, for binary search.
    order: Vec<u32>,
    /// Where the entries directly under the root start in `child_index`.
    first_root: u32,
    root_count: u32,
}

/// The tree under construction.
#[derive(Debug, Default)]
struct Builder {
    nodes: Vec<BuildNode>,
    roots: Vec<u32>,
    /// The entry under the root that most recently took a name of its own.
    root_last_named: Option<u32>,
    /// Entries that lost their name to another, each with its parent.
    ///
    /// They are named only once every stored name is placed, because a name
    /// chosen earlier could be the stored name of a record that sorts later.
    unnamed: Vec<(Option<u32>, u32)>,
}

impl Tree {
    /// Build a tree from a flat listing.
    pub(crate) fn build<I>(entries: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<RawEntry>,
    {
        let mut raw: Vec<RawEntry> = entries
            .into_iter()
            .map(Into::into)
            .filter(|item| !item.entry.path.is_root())
            .take(MAX_LISTED)
            .collect();
        // Component order puts every record of one name directly ahead of
        // the records under it, so the node that holds a name is always the
        // last child of its parent that took a name, whatever sorts between
        // the name and its children as text. A usable record sorts ahead of a
        // refused one with the same path, so the usable one keeps the name.
        raw.sort_by(|left, right| {
            component_order(left.entry.path.as_str(), right.entry.path.as_str())
                .then(left.entry.refused.cmp(&right.entry.refused))
        });

        let mut builder = Builder::default();
        // Directories above the entry being placed, from the root down. The
        // listing is walked depth first, so the chain only grows at the end
        // or is truncated from it.
        let mut chain: Vec<u32> = Vec::new();

        for item in raw {
            let path = item.entry.path.clone();
            while let Some(top) = chain.last() {
                let above = &builder.nodes[*top as usize].entry.path;
                // A record of the same path is a sibling of the node on top,
                // not a child of it.
                if *above != path && path.starts_with(above) {
                    break;
                }
                chain.pop();
            }

            let components: Vec<&str> = path.components().collect();
            let mut prefix = match chain.last() {
                Some(top) => builder.nodes[*top as usize].entry.path.clone(),
                None => VfsPath::root(),
            };
            for depth in chain.len()..components.len().saturating_sub(1) {
                let Some(part) = components.get(depth) else {
                    break;
                };
                let Ok(child) = prefix.join(part) else {
                    break;
                };
                let id = builder.ensure_directory(chain.last().copied(), child.clone());
                chain.push(id);
                prefix = child;
            }

            let parent = chain.last().copied();
            let id = builder.place(parent, item);
            if builder.nodes[id as usize].entry.is_dir() {
                chain.push(id);
            }
        }

        builder.name_unnamed();
        builder.finish()
    }
}

impl Builder {
    /// Children of `parent`, or the entries at the root.
    fn child_list(&self, parent: Option<u32>) -> &[u32] {
        match parent {
            Some(id) => &self.nodes[id as usize].children,
            None => &self.roots,
        }
    }

    fn push_child(&mut self, parent: Option<u32>, id: u32) {
        match parent {
            Some(parent) => self.nodes[parent as usize].children.push(id),
            None => self.roots.push(id),
        }
    }

    fn push_node(&mut self, parent: Option<u32>, entry: VfsEntry, locator: u64) -> u32 {
        let id = node_id(self.nodes.len());
        self.nodes.push(BuildNode {
            entry,
            children: Vec::new(),
            locator,
            last_named: None,
        });
        self.push_child(parent, id);
        id
    }

    /// Add an entry that keeps the name it was stored under.
    fn push_named(&mut self, parent: Option<u32>, entry: VfsEntry, locator: u64) -> u32 {
        let id = self.push_node(parent, entry, locator);
        match parent {
            Some(parent) => self.nodes[parent as usize].last_named = Some(id),
            None => self.root_last_named = Some(id),
        }
        id
    }

    /// Add an entry whose name another entry holds.
    fn push_unnamed(&mut self, parent: Option<u32>, entry: VfsEntry, locator: u64) -> u32 {
        let id = self.push_node(parent, entry, locator);
        self.unnamed.push((parent, id));
        id
    }

    /// The node under `parent` that holds `path`, if one does.
    fn named(&self, parent: Option<u32>, path: &VfsPath) -> Option<u32> {
        let last = match parent {
            Some(id) => self.nodes[id as usize].last_named,
            None => self.root_last_named,
        }?;
        (self.nodes[last as usize].entry.path == *path).then_some(last)
    }

    /// The node holding the directory at `path`, creating or promoting it.
    fn ensure_directory(&mut self, parent: Option<u32>, path: VfsPath) -> u32 {
        let Some(existing) = self.named(parent, &path) else {
            return self.push_named(parent, VfsEntry::directory(path), NO_LOCATOR);
        };
        if !self.nodes[existing as usize].entry.is_dir() {
            // A file holds the name a directory needs. The directory takes
            // it, because the entries below it are only reachable through
            // it, and the file moves aside with its content locator.
            let node = &mut self.nodes[existing as usize];
            let displaced = std::mem::replace(&mut node.entry, VfsEntry::directory(path));
            let locator = std::mem::replace(&mut node.locator, NO_LOCATOR);
            self.push_unnamed(parent, displaced, locator);
        }
        existing
    }

    /// Put one listed entry under `parent`, resolving a clash over the name.
    fn place(&mut self, parent: Option<u32>, item: RawEntry) -> u32 {
        let locator = item.locator.unwrap_or(NO_LOCATOR);
        let entry = item.entry;
        let Some(existing) = self.named(parent, &entry.path) else {
            return self.push_named(parent, entry, locator);
        };
        match (entry.is_dir(), self.nodes[existing as usize].entry.is_dir()) {
            (true, true) => {
                merge_directory(&mut self.nodes[existing as usize].entry, entry);
                existing
            }
            (true, false) => {
                let node = &mut self.nodes[existing as usize];
                let displaced = std::mem::replace(&mut node.entry, entry);
                let displaced_locator = std::mem::replace(&mut node.locator, locator);
                self.push_unnamed(parent, displaced, displaced_locator);
                existing
            }
            // Two records for one name, neither of which can be dropped
            // without hiding content the container holds.
            (false, _) => self.push_unnamed(parent, entry, locator),
        }
    }

    /// Give every entry that lost its name one no sibling holds.
    fn name_unnamed(&mut self) {
        let unnamed = std::mem::take(&mut self.unnamed);
        let mut taken: HashMap<Option<u32>, HashSet<String>> = HashMap::new();
        let mut next: HashMap<(Option<u32>, String), u64> = HashMap::new();
        for (parent, id) in unnamed {
            let names = taken.entry(parent).or_insert_with(|| {
                self.child_list(parent)
                    .iter()
                    .map(|child| self.nodes[*child as usize].entry.name.clone())
                    .collect()
            });
            let original = self.nodes[id as usize].entry.path.clone();
            let base = original.name().unwrap_or_default().to_owned();
            let dir = original.parent().unwrap_or_default();
            let counter = next.entry((parent, base.clone())).or_insert(1);
            let moved = loop {
                let spelling = format!("{base}~{counter}");
                *counter += 1;
                if names.contains(&spelling) {
                    continue;
                }
                match dir.join(&spelling) {
                    Ok(candidate) if candidate.as_str().len() <= MAX_PATH_BYTES => {
                        names.insert(spelling);
                        break Some(candidate);
                    }
                    _ => break None,
                }
            };
            // A parent path near the length ceiling leaves no room for a
            // suffix, so the entry is listed under the root instead.
            let moved = if let Some(path) = moved {
                path
            } else {
                let path = self.root_name(&mut taken, &mut next);
                self.move_to_root(parent, id);
                path
            };
            let entry = &mut self.nodes[id as usize].entry;
            note_shadowed(entry, &original, &moved);
            retarget(entry, moved);
        }
    }

    /// A short name under the root that no entry there holds.
    fn root_name(
        &self,
        taken: &mut HashMap<Option<u32>, HashSet<String>>,
        next: &mut HashMap<(Option<u32>, String), u64>,
    ) -> VfsPath {
        let names = taken.entry(None).or_insert_with(|| {
            self.roots
                .iter()
                .map(|child| self.nodes[*child as usize].entry.name.clone())
                .collect()
        });
        let counter = next.entry((None, String::new())).or_insert(1);
        loop {
            let spelling = format!("~{counter}");
            *counter += 1;
            if names.contains(&spelling) {
                continue;
            }
            if let Ok(path) = VfsPath::parse(&spelling) {
                names.insert(spelling);
                return path;
            }
        }
    }

    fn move_to_root(&mut self, parent: Option<u32>, id: u32) {
        if let Some(parent) = parent {
            self.nodes[parent as usize]
                .children
                .retain(|child| *child != id);
            self.roots.push(id);
        }
    }

    /// Lay the children out end to end and shrink everything that is left.
    fn finish(self) -> Tree {
        let total: usize =
            self.roots.len() + self.nodes.iter().map(|n| n.children.len()).sum::<usize>();
        let mut child_index = Vec::with_capacity(total);
        let first_root = node_id(child_index.len());
        child_index.extend_from_slice(&self.roots);
        let root_count = node_id(self.roots.len());

        let mut nodes = Vec::with_capacity(self.nodes.len());
        for node in self.nodes {
            let first_child = node_id(child_index.len());
            child_index.extend_from_slice(&node.children);
            nodes.push(Node {
                entry: node.entry,
                first_child,
                child_count: node_id(node.children.len()),
                locator: node.locator,
            });
        }

        let mut order: Vec<u32> = (0..node_id(nodes.len())).collect();
        order.sort_by(|left, right| {
            nodes[*left as usize]
                .entry
                .path
                .cmp(&nodes[*right as usize].entry.path)
        });

        nodes.shrink_to_fit();
        child_index.shrink_to_fit();
        order.shrink_to_fit();

        Tree {
            nodes,
            child_index,
            order,
            first_root,
            root_count,
        }
    }
}

/// Order two paths component by component, so a path sorts directly ahead of
/// everything under it and siblings sort by name.
///
/// Mapping the separator below every other byte gives that order over the
/// text itself, because a normalized path holds no empty component.
fn component_order(left: &str, right: &str) -> std::cmp::Ordering {
    let key = |byte: u8| {
        if byte == b'/' {
            0
        } else {
            u16::from(byte) + 1
        }
    };
    left.bytes().map(key).cmp(right.bytes().map(key))
}

/// Fold a second record of one directory into the node that holds it.
///
/// The later record's metadata wins, as it does when a tar is extracted. A
/// refused record never replaces a usable one; its reason is kept on the
/// directory instead, so the record is not lost without a trace.
fn merge_directory(held: &mut VfsEntry, record: VfsEntry) {
    if record.refused && !held.refused {
        if let Some(reason) = record.error {
            held.error = Some(match held.error.take() {
                Some(existing) => format!("{existing}; {reason}"),
                None => reason,
            });
        }
        return;
    }
    *held = record;
}

impl Tree {
    /// Move every stamp that is a DOS wall clock onto the instant that wall
    /// clock names in a zone `zone_offset_seconds` ahead of UTC.
    ///
    /// A stamp the move would carry outside the range a system time holds
    /// keeps its value.
    pub(crate) fn place_local_stamps(&mut self, zone_offset_seconds: i32) {
        if zone_offset_seconds == 0 {
            return;
        }
        let shift = std::time::Duration::from_secs(u64::from(zone_offset_seconds.unsigned_abs()));
        for node in &mut self.nodes {
            if node.entry.time_fidelity != TimeFidelity::LocalTwoSecond {
                continue;
            }
            node.entry.modified = node.entry.modified.map(|wall_clock| {
                let placed = if zone_offset_seconds > 0 {
                    wall_clock.checked_sub(shift)
                } else {
                    wall_clock.checked_add(shift)
                };
                placed.unwrap_or(wall_clock)
            });
        }
    }

    fn children_of(&self, id: Option<u32>) -> &[u32] {
        let (start, count) = match id {
            Some(id) => {
                let node = &self.nodes[id as usize];
                (node.first_child, node.child_count)
            }
            None => (self.first_root, self.root_count),
        };
        let start = start as usize;
        self.child_index
            .get(start..start + count as usize)
            .unwrap_or_default()
    }

    fn find(&self, path: &VfsPath) -> Option<u32> {
        let position = self
            .order
            .binary_search_by(|id| self.nodes[*id as usize].entry.path.cmp(path))
            .ok()?;
        self.order.get(position).copied()
    }

    pub(crate) fn get(&self, path: &VfsPath) -> Option<&VfsEntry> {
        self.find(path).map(|id| &self.nodes[id as usize].entry)
    }

    /// The backend-private locator recorded for `path`.
    pub(crate) fn locator(&self, path: &VfsPath) -> Option<u64> {
        let id = self.find(path)?;
        match self.nodes[id as usize].locator {
            NO_LOCATOR => None,
            value => Some(value),
        }
    }

    /// Number of entries, including the directories the listing implied.
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Bytes the listing holds for as long as it is open.
    ///
    /// The figure counts the records, the strings they own and the indices
    /// that link them, which is everything that scales with the number of
    /// entries. It excludes allocator overhead, so it measures what this
    /// module chose to store rather than resident memory.
    pub(crate) fn heap_bytes(&self) -> usize {
        let mut total = self.nodes.capacity() * std::mem::size_of::<Node>();
        total += (self.child_index.capacity() + self.order.capacity()) * std::mem::size_of::<u32>();
        for node in &self.nodes {
            total += node.entry.path.as_str().len();
            total += node.entry.name.capacity();
            total += node
                .entry
                .version_info
                .as_ref()
                .map_or(0, std::string::String::capacity);
            total += node
                .entry
                .error
                .as_ref()
                .map_or(0, std::string::String::capacity);
        }
        total
    }

    pub(crate) fn list(&self, dir: &VfsPath) -> Option<Vec<VfsEntry>> {
        let id = if dir.is_root() {
            None
        } else {
            let id = self.find(dir)?;
            if self.nodes[id as usize].entry.kind != EntryKind::Directory {
                return None;
            }
            Some(id)
        };
        Some(
            self.children_of(id)
                .iter()
                .map(|child| self.nodes[*child as usize].entry.clone())
                .collect(),
        )
    }
}

/// Record on the entry that it could not keep the name the container gave it.
fn note_shadowed(entry: &mut VfsEntry, original: &VfsPath, moved_to: &VfsPath) {
    let text = format!(
        "container holds more than one entry named {original}; this one is listed as {moved_to}"
    );
    entry.error = Some(match entry.error.take() {
        Some(existing) => format!("{existing}; {text}"),
        None => text,
    });
}

/// Move an entry onto a different path, keeping its name field in step.
fn retarget(entry: &mut VfsEntry, path: VfsPath) {
    path.name().unwrap_or_default().clone_into(&mut entry.name);
    entry.path = path;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn file(path: &str) -> VfsEntry {
        VfsEntry::file(VfsPath::parse(path).unwrap(), 3)
    }

    #[test]
    fn fills_in_implied_directories() {
        let tree = Tree::build(vec![file("a/b/c.txt")]);
        let root = tree.list(&VfsPath::root()).unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].name, "a");
        assert!(root[0].is_dir());
        let inner = tree.list(&VfsPath::parse("a/b").unwrap()).unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].name, "c.txt");
    }

    #[test]
    fn deep_paths_do_not_recurse() {
        let deep = "d/".repeat(400) + "f.txt";
        let tree = Tree::build(vec![file(&deep)]);
        assert_eq!(tree.len(), 401);
        assert!(tree.get(&VfsPath::parse(&deep).unwrap()).is_some());
    }

    #[test]
    fn a_file_and_a_directory_of_the_same_name_both_survive() {
        for order in [vec!["a", "a/b"], vec!["a/b", "a"]] {
            let tree = Tree::build(order.iter().map(|path| file(path)).collect::<Vec<_>>());
            let root = tree.list(&VfsPath::root()).unwrap();
            assert_eq!(root.len(), 2, "both the file and the directory are listed");
            let dir = tree.get(&VfsPath::parse("a").unwrap()).unwrap();
            assert!(dir.is_dir());
            let under = tree.list(&VfsPath::parse("a").unwrap()).unwrap();
            assert_eq!(under.len(), 1);
            assert_eq!(under[0].name, "b");
            let shadowed = root.iter().find(|entry| entry.name == "a~1").unwrap();
            assert!(shadowed.error.is_some());
            assert!(!shadowed.is_dir());
        }
    }

    #[test]
    fn duplicate_names_are_both_kept() {
        let tree = Tree::build(vec![file("dup.txt"), file("dup.txt")]);
        let root = tree.list(&VfsPath::root()).unwrap();
        assert_eq!(root.len(), 2);
        assert!(root.iter().any(|entry| entry.name == "dup.txt~1"));
        assert!(root.iter().any(|entry| entry
            .error
            .as_deref()
            .is_some_and(|text| text.contains("more than one"))));
    }

    #[test]
    fn a_real_directory_record_keeps_the_entries_under_it() {
        let mut directory = VfsEntry::directory(VfsPath::parse("d").unwrap());
        directory.size = 0;
        let tree = Tree::build(vec![file("d/one.txt"), directory, file("d/two.txt")]);
        let under = tree.list(&VfsPath::parse("d").unwrap()).unwrap();
        assert_eq!(under.len(), 2);
    }

    #[test]
    fn a_locator_survives_the_build() {
        let tree = Tree::build(vec![RawEntry {
            entry: file("a/b.txt"),
            locator: Some(4096),
        }]);
        assert_eq!(
            tree.locator(&VfsPath::parse("a/b.txt").unwrap()),
            Some(4096)
        );
        assert_eq!(tree.locator(&VfsPath::parse("a").unwrap()), None);
    }

    fn path(text: &str) -> VfsPath {
        VfsPath::parse(text).unwrap()
    }

    fn dated_directory(text: &str) -> VfsEntry {
        let mut entry = VfsEntry::directory(path(text));
        entry.modified =
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000));
        entry
    }

    /// Every order of `items`, so a test does not depend on the order a
    /// container happens to list its records in.
    fn orders<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        let mut out = vec![items.to_vec()];
        let mut current = items.to_vec();
        let mut counters = vec![0usize; items.len()];
        let mut index = 1;
        while index < items.len() {
            if counters[index] < index {
                let other = if index % 2 == 0 { 0 } else { counters[index] };
                current.swap(other, index);
                out.push(current.clone());
                counters[index] += 1;
                index = 1;
            } else {
                counters[index] = 0;
                index += 1;
            }
        }
        out
    }

    /// Every directory the tree lists, the root first, with its listing.
    fn listings(tree: &Tree) -> Vec<(VfsPath, Vec<VfsEntry>)> {
        let mut out = Vec::new();
        let mut pending = vec![VfsPath::root()];
        while let Some(dir) = pending.pop() {
            assert!(out.len() < 10_000, "the walk of the tree does not end");
            let listed = tree.list(&dir).unwrap();
            for entry in &listed {
                if entry.is_dir() {
                    pending.push(entry.path.clone());
                }
            }
            out.push((dir, listed));
        }
        out
    }

    /// No directory lists a name twice, every listed path finds the node it
    /// was listed as, and the walk reaches every node.
    fn assert_names_unique(tree: &Tree) {
        let mut reached = 0usize;
        for (dir, listed) in listings(tree) {
            let mut names: Vec<&str> = listed.iter().map(|entry| entry.name.as_str()).collect();
            names.sort_unstable();
            let count = names.len();
            names.dedup();
            assert_eq!(names.len(), count, "{dir:?} lists a name twice: {listed:?}");
            for entry in &listed {
                let parent = entry.path.parent().unwrap();
                assert_eq!(parent, dir, "{:?} is listed under {dir:?}", entry.path);
                assert_eq!(
                    tree.get(&entry.path),
                    Some(entry),
                    "{:?} resolves to another node",
                    entry.path
                );
            }
            reached += listed.len();
        }
        assert_eq!(reached, tree.len(), "a walk reaches every node once");
    }

    fn root_names(tree: &Tree) -> Vec<String> {
        let mut names: Vec<String> = tree
            .list(&VfsPath::root())
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn a_sibling_that_sorts_between_a_file_and_its_namesake_directory_splits_nothing() {
        for order in orders(&[file("src"), file("src-x"), file("src/main.rs")]) {
            let tree = Tree::build(order);
            assert_names_unique(&tree);
            assert_eq!(root_names(&tree), ["src", "src-x", "src~1"]);
            assert!(tree.get(&path("src")).unwrap().is_dir());
            let moved = tree.get(&path("src~1")).unwrap();
            assert!(!moved.is_dir());
            assert!(moved.error.is_some(), "the moved file says why");
            let under = tree.list(&path("src")).unwrap();
            assert_eq!(under.len(), 1);
            assert_eq!(under[0].name, "main.rs");
        }
    }

    #[test]
    fn an_explicit_directory_record_keeps_its_node_when_a_sibling_sorts_between() {
        let records = [
            dated_directory("src"),
            file("src-old/x.txt"),
            file("src/a.rs"),
            file("src/main.rs"),
        ];
        for order in orders(&records) {
            let tree = Tree::build(order);
            assert_names_unique(&tree);
            assert_eq!(root_names(&tree), ["src", "src-old"]);
            let src = tree.get(&path("src")).unwrap();
            assert!(
                src.modified.is_some(),
                "the stamp of the record is reachable"
            );
            assert_eq!(tree.list(&path("src")).unwrap().len(), 2);
            assert_eq!(tree.len(), 5);
        }
    }

    #[test]
    fn root_files_beside_a_directory_and_its_namesake_sibling_change_nothing() {
        for extra in 0..12usize {
            let mut records = vec![
                dated_directory("src"),
                file("src-old/x.txt"),
                file("src/a.rs"),
                file("src/main.rs"),
            ];
            records.extend((0..extra).map(|index| file(&format!("z{index}.txt"))));
            let tree = Tree::build(records);
            assert_names_unique(&tree);
            assert_eq!(tree.len(), 5 + extra, "with {extra} extra files");
            assert!(tree.get(&path("src")).unwrap().modified.is_some());
            assert_eq!(tree.list(&path("src")).unwrap().len(), 2);
        }
    }

    #[test]
    fn two_directories_with_namesake_siblings_each_stay_one_node() {
        let tree = Tree::build(vec![
            dated_directory("src"),
            file("src-old/x.txt"),
            file("src/main.rs"),
            dated_directory("build"),
            file("build.gradle"),
            file("build/out.bin"),
        ]);
        assert_names_unique(&tree);
        assert_eq!(tree.len(), 7);
        assert_eq!(
            root_names(&tree),
            ["build", "build.gradle", "src", "src-old"]
        );
    }

    #[test]
    fn a_repeated_directory_record_is_one_directory() {
        for order in orders(&[dated_directory("d"), dated_directory("d"), file("d/x.txt")]) {
            let tree = Tree::build(order);
            assert_names_unique(&tree);
            assert_eq!(tree.len(), 2);
            assert_eq!(root_names(&tree), ["d"]);
            assert_eq!(tree.list(&path("d")).unwrap().len(), 1);
        }
    }

    #[test]
    fn a_file_stored_after_a_directory_of_its_name_lands_beside_it() {
        for order in orders(&[dated_directory("d"), file("d")]) {
            let tree = Tree::build(order);
            assert_names_unique(&tree);
            assert_eq!(root_names(&tree), ["d", "d~1"]);
            assert!(tree.get(&path("d")).unwrap().is_dir());
            assert!(tree.list(&path("d")).unwrap().is_empty());
        }
    }

    #[test]
    fn a_file_moved_aside_keeps_its_content_locator() {
        let records = [
            RawEntry {
                entry: file("a"),
                locator: Some(7),
            },
            RawEntry {
                entry: file("a/b"),
                locator: Some(9),
            },
        ];
        let tree = Tree::build(records);
        assert_eq!(tree.locator(&path("a~1")), Some(7));
        assert_eq!(tree.locator(&path("a/b")), Some(9));
        assert_eq!(tree.locator(&path("a")), None);
    }

    #[test]
    fn a_moved_entry_with_no_room_for_a_suffix_is_listed_under_the_root() {
        let parent = "d".repeat(MAX_PATH_BYTES - 2);
        let name = format!("{parent}/x");
        let tree = Tree::build(vec![file(&name), file(&name)]);
        assert_names_unique(&tree);
        assert_eq!(tree.len(), 3);
        assert_eq!(root_names(&tree), [parent.as_str(), "~1"]);
        let moved = tree.get(&path("~1")).unwrap();
        assert!(moved.error.is_some(), "the moved file says where it went");
    }

    #[test]
    fn a_name_given_to_a_moved_entry_never_takes_a_stored_name() {
        for order in orders(&[file("a"), file("a"), file("a-b"), file("a~1")]) {
            let tree = Tree::build(order);
            assert_names_unique(&tree);
            assert_eq!(tree.len(), 4);
        }
    }
}
