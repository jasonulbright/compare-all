//! File masks, written in the same syntax file filters use.
//!
//! A mask list is a semicolon separated list of wildcard patterns. A pattern
//! holding no separator is matched against the filename alone; one that holds a
//! separator is matched against the whole path. Matching ignores character
//! case, because a format that claims `*.C` has to claim `main.c` too.
//!
//! An empty list matches nothing, which is how a format is kept out of
//! automatic lookup and reachable only by an explicit choice.

use crate::GrammarError;
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A compiled semicolon separated list of filename wildcards.
#[derive(Debug, Clone)]
pub struct MaskList {
    raw: String,
    names: GlobSet,
    paths: GlobSet,
    name_count: usize,
    path_count: usize,
}

impl MaskList {
    /// Compile a semicolon separated mask list.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError::Mask`] when one of the patterns is malformed.
    pub fn new(raw: &str) -> Result<Self, GrammarError> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        let mut name_count = 0;
        let mut path_count = 0;
        for piece in raw.split(';') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let glob = compile_glob(piece)?;
            if piece.contains('/') || piece.contains('\\') {
                paths.add(glob);
                path_count += 1;
            } else {
                names.add(glob);
                name_count += 1;
            }
        }
        let build = |b: GlobSetBuilder| {
            b.build().map_err(|e| GrammarError::Mask {
                mask: raw.to_owned(),
                message: e.to_string(),
            })
        };
        Ok(Self {
            raw: raw.to_owned(),
            names: build(names)?,
            paths: build(paths)?,
            name_count,
            path_count,
        })
    }

    /// An empty list, matching no filename at all.
    pub fn empty() -> Self {
        Self {
            raw: String::new(),
            names: GlobSet::empty(),
            paths: GlobSet::empty(),
            name_count: 0,
            path_count: 0,
        }
    }

    /// The list as the user typed it.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Whether the list holds no pattern, and so matches nothing.
    pub fn is_empty(&self) -> bool {
        self.name_count == 0 && self.path_count == 0
    }

    /// The individual patterns, in the order they were written.
    pub fn patterns(&self) -> impl Iterator<Item = &str> {
        self.raw.split(';').map(str::trim).filter(|p| !p.is_empty())
    }

    /// Whether `path` is claimed by this list.
    pub fn matches(&self, path: &str) -> bool {
        if self.path_count > 0 {
            let normalized = path.replace('\\', "/");
            if self.paths.is_match(&normalized) {
                return true;
            }
        }
        if self.name_count == 0 {
            return false;
        }
        self.names.is_match(file_name_of(path))
    }
}

/// The trailing path component of `path`, treating both separators alike.
fn file_name_of(path: &str) -> &str {
    let cut = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    &path[cut..]
}

fn compile_glob(pattern: &str) -> Result<Glob, GrammarError> {
    GlobBuilder::new(pattern)
        .case_insensitive(true)
        // A mask's `*` spans separators: the patterns are filename shaped, and
        // path-shaped ones are matched whole rather than segment by segment.
        .literal_separator(false)
        .build()
        .map_err(|e| GrammarError::Mask {
            mask: pattern.to_owned(),
            message: e.to_string(),
        })
}

impl Default for MaskList {
    fn default() -> Self {
        Self::empty()
    }
}

impl PartialEq for MaskList {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl Eq for MaskList {}

impl Serialize for MaskList {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for MaskList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        // A mask that no longer compiles must not make the whole settings file
        // unreadable; it degrades to a list that claims nothing.
        Ok(Self::new(&raw).unwrap_or_else(|_| Self {
            raw,
            names: GlobSet::empty(),
            paths: GlobSet::empty(),
            name_count: 0,
            path_count: 0,
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_semicolon_list_claims_each_of_its_patterns() {
        let mask = MaskList::new("*.c;*.cpp; *.h ").unwrap();
        assert!(mask.matches("main.c"));
        assert!(mask.matches("main.cpp"));
        assert!(mask.matches("main.h"));
        assert!(!mask.matches("main.rs"));
        assert_eq!(mask.patterns().count(), 3);
    }

    #[test]
    fn matching_ignores_character_case() {
        let mask = MaskList::new("*.C").unwrap();
        assert!(mask.matches("MAIN.c"));
    }

    #[test]
    fn a_bare_pattern_matches_the_filename_inside_a_path() {
        let mask = MaskList::new("*.rs").unwrap();
        assert!(mask.matches("src/deep/mod.rs"));
        assert!(mask.matches(r"C:\src\deep\mod.rs"));
    }

    #[test]
    fn a_pattern_holding_a_separator_matches_the_whole_path() {
        let mask = MaskList::new("src/*.rs").unwrap();
        assert!(mask.matches("src/mod.rs"));
        assert!(!mask.matches("other/mod.rs"));
    }

    #[test]
    fn an_empty_list_matches_nothing() {
        let mask = MaskList::new("").unwrap();
        assert!(mask.is_empty());
        assert!(!mask.matches("anything.txt"));
    }

    #[test]
    fn a_list_round_trips_as_its_own_text() {
        let mask = MaskList::new("*.a;*.b").unwrap();
        let text = serde_json::to_string(&mask).unwrap();
        assert_eq!(text, "\"*.a;*.b\"");
        let back: MaskList = serde_json::from_str(&text).unwrap();
        assert_eq!(back, mask);
        assert!(back.matches("x.b"));
    }

    #[test]
    fn a_malformed_stored_mask_loads_as_one_claiming_nothing() {
        let back: MaskList = serde_json::from_str("\"[\"").unwrap();
        assert!(!back.matches("x"));
        assert_eq!(back.as_str(), "[");
    }
}
