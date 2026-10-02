//! Normalized paths used inside every file system implementation.
//!
//! A [`VfsPath`] is relative to the root of one file system, uses forward
//! slashes, and cannot name anything outside that root. Container formats
//! store entry names as free-form bytes, so every name that enters the crate
//! is validated here before it can reach a real file system call.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Reason a string was rejected as a path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The path starts at a file system root rather than the container root.
    #[error("absolute path is not allowed inside a virtual file system: {0:?}")]
    Absolute(String),
    /// The path carries a drive letter or other volume prefix.
    #[error("volume prefix is not allowed inside a virtual file system: {0:?}")]
    VolumePrefix(String),
    /// A component walks above the root.
    #[error("parent component is not allowed inside a virtual file system: {0:?}")]
    ParentComponent(String),
    /// The path contains a NUL byte.
    #[error("NUL is not allowed in a path: {0:?}")]
    Nul(String),
    /// The path contains a component no file system can store.
    #[error("component is not a usable file name: {0:?}")]
    InvalidComponent(String),
    /// The path holds more components than [`MAX_COMPONENTS`].
    #[error("path has {count} components, more than the {MAX_COMPONENTS} allowed")]
    TooManyComponents {
        /// Components counted before the walk stopped.
        count: usize,
    },
    /// The path is longer than [`MAX_PATH_BYTES`].
    #[error("path is {len} bytes, more than the {MAX_PATH_BYTES} allowed")]
    TooLong {
        /// Length of the rejected path.
        len: usize,
    },
}

/// Most components one path may carry.
///
/// A container entry name is attacker controlled, and each component costs a
/// tree node, so the count is bounded before any per-component work runs.
pub const MAX_COMPONENTS: usize = 512;

/// Most bytes one path may occupy.
pub const MAX_PATH_BYTES: usize = 32 * 1024;

/// A validated, normalized, root-relative path.
///
/// The empty path is the root of the file system. Components are separated by
/// a single forward slash and no component is empty, `.` or `..`, so joining a
/// `VfsPath` onto an extraction root cannot escape it.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VfsPath(String);

impl VfsPath {
    /// The root of the file system.
    #[must_use]
    pub fn root() -> Self {
        Self(String::new())
    }

    /// Validate and normalize `raw`.
    ///
    /// Both slash kinds separate components, repeated separators collapse, and
    /// `.` components drop. A trailing separator, which container formats use
    /// to mark a directory, is not part of the path.
    ///
    /// # Errors
    /// Returns [`PathError`] when the path is absolute, carries a volume
    /// prefix, walks above the root, or holds a byte no file system accepts.
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        if raw.len() > MAX_PATH_BYTES {
            return Err(PathError::TooLong { len: raw.len() });
        }
        if raw.contains('\0') {
            return Err(PathError::Nul(raw.to_owned()));
        }
        if raw.starts_with('/') || raw.starts_with('\\') {
            return Err(PathError::Absolute(raw.to_owned()));
        }
        if has_volume_prefix(raw) {
            return Err(PathError::VolumePrefix(raw.to_owned()));
        }

        let mut out = String::with_capacity(raw.len());
        let mut count = 0usize;
        for part in raw.split(['/', '\\']) {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                return Err(PathError::ParentComponent(raw.to_owned()));
            }
            count += 1;
            if count > MAX_COMPONENTS {
                return Err(PathError::TooManyComponents { count });
            }
            validate_component(part, raw)?;
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(part);
        }
        Ok(Self(out))
    }

    /// The path as a forward-slash string; empty for the root.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True for the root of the file system.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Final component, or `None` at the root.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        if self.0.is_empty() {
            return None;
        }
        Some(self.0.rsplit('/').next().unwrap_or(&self.0))
    }

    /// Containing directory, or `None` at the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            return None;
        }
        match self.0.rfind('/') {
            Some(cut) => Some(Self(self.0[..cut].to_owned())),
            None => Some(Self::root()),
        }
    }

    /// Append a relative path.
    ///
    /// The joined path is held to the same ceilings as a parsed one, so a
    /// path built one component at a time cannot pass [`MAX_PATH_BYTES`] or
    /// [`MAX_COMPONENTS`].
    ///
    /// # Errors
    /// Returns [`PathError`] when `component` is not a usable relative path,
    /// and [`PathError::TooLong`] or [`PathError::TooManyComponents`] when the
    /// joined path passes a ceiling.
    pub fn join(&self, component: &str) -> Result<Self, PathError> {
        let suffix = VfsPath::parse(component)?;
        if suffix.is_root() {
            return Ok(self.clone());
        }
        if self.0.is_empty() {
            return Ok(suffix);
        }
        let len = self.0.len() + 1 + suffix.0.len();
        if len > MAX_PATH_BYTES {
            return Err(PathError::TooLong { len });
        }
        let count = self.depth() + suffix.depth();
        if count > MAX_COMPONENTS {
            return Err(PathError::TooManyComponents { count });
        }
        Ok(Self(format!("{}/{}", self.0, suffix.0)))
    }

    /// Components from the root down, empty at the root.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|part| !part.is_empty())
    }

    /// Number of components; zero at the root.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.components().count()
    }

    /// True when `self` is `prefix` or lies below it.
    #[must_use]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        if prefix.is_root() {
            return true;
        }
        if !self.0.starts_with(&prefix.0) {
            return false;
        }
        matches!(self.0.as_bytes().get(prefix.0.len()), None | Some(b'/'))
    }

    /// The part of `self` below `prefix`, or `None` when `self` is not below it.
    #[must_use]
    pub fn strip_prefix(&self, prefix: &Self) -> Option<Self> {
        if !self.starts_with(prefix) {
            return None;
        }
        if prefix.is_root() {
            return Some(self.clone());
        }
        let rest = self.0.get(prefix.0.len() + 1..).unwrap_or_default();
        Some(Self(rest.to_owned()))
    }

    /// Resolve against a real directory.
    ///
    /// Components are appended one at a time, so the result is always inside
    /// `root` even when the name came from an untrusted container.
    #[must_use]
    pub fn to_native(&self, root: &std::path::Path) -> std::path::PathBuf {
        let mut out = root.to_path_buf();
        for part in self.components() {
            out.push(part);
        }
        out
    }
}

impl fmt::Display for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for VfsPath {
    type Error = PathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<VfsPath> for String {
    fn from(value: VfsPath) -> Self {
        value.0
    }
}

/// True when `raw` names a volume: a drive letter, a UNC share or a device path.
fn has_volume_prefix(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return true;
    }
    // A colon anywhere else still names an alternate data stream on Windows.
    raw.contains(':')
}

/// Reject component spellings that resolve to something other than a child.
fn validate_component(part: &str, raw: &str) -> Result<(), PathError> {
    if part.chars().any(|ch| ch == '\0' || (ch as u32) < 0x20) {
        return Err(PathError::InvalidComponent(raw.to_owned()));
    }
    // Trailing dots and spaces are silently trimmed by the Windows file system,
    // so two distinct container entries could land on one file.
    if part.ends_with('.') || part.ends_with(' ') {
        return Err(PathError::InvalidComponent(raw.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn normalizes_separators_and_dot_components() {
        let path = VfsPath::parse("a\\b/./c/").unwrap();
        assert_eq!(path.as_str(), "a/b/c");
        assert_eq!(path.name(), Some("c"));
        assert_eq!(
            path.parent().map(|p| p.as_str().to_owned()),
            Some("a/b".into())
        );
        assert_eq!(path.depth(), 3);
    }

    #[test]
    fn rejects_traversal_and_absolute_names() {
        assert!(matches!(
            VfsPath::parse("../etc/passwd"),
            Err(PathError::ParentComponent(_))
        ));
        assert!(matches!(
            VfsPath::parse("a/../../b"),
            Err(PathError::ParentComponent(_))
        ));
        assert!(matches!(
            VfsPath::parse("/etc"),
            Err(PathError::Absolute(_))
        ));
        assert!(matches!(
            VfsPath::parse("\\\\server\\share"),
            Err(PathError::Absolute(_))
        ));
        assert!(matches!(
            VfsPath::parse("C:/Windows"),
            Err(PathError::VolumePrefix(_))
        ));
        assert!(matches!(VfsPath::parse("a\0b"), Err(PathError::Nul(_))));
    }

    #[test]
    fn rejects_paths_with_too_many_components() {
        let deep = "a/".repeat(MAX_COMPONENTS + 100) + "f.txt";
        assert!(matches!(
            VfsPath::parse(&deep),
            Err(PathError::TooManyComponents { .. })
        ));
        let long = "x".repeat(MAX_PATH_BYTES + 1);
        assert!(matches!(
            VfsPath::parse(&long),
            Err(PathError::TooLong { .. })
        ));
    }

    #[test]
    fn a_joined_path_is_held_to_the_ceilings() {
        let mut deep = VfsPath::root();
        for _ in 0..MAX_COMPONENTS {
            deep = deep.join("a").unwrap();
        }
        assert_eq!(deep.depth(), MAX_COMPONENTS);
        assert!(matches!(
            deep.join("b"),
            Err(PathError::TooManyComponents { count }) if count == MAX_COMPONENTS + 1
        ));

        let half = "x".repeat(MAX_PATH_BYTES / 2);
        let long = VfsPath::parse(&half).unwrap();
        assert!(matches!(
            long.join(&half),
            Err(PathError::TooLong { len }) if len == MAX_PATH_BYTES + 1
        ));
        let fits = "y".repeat(MAX_PATH_BYTES - half.len() - 1);
        assert_eq!(long.join(&fits).unwrap().as_str().len(), MAX_PATH_BYTES);
    }

    #[test]
    fn prefix_matching_is_component_wise() {
        let path = VfsPath::parse("a/bc").unwrap();
        assert!(!path.starts_with(&VfsPath::parse("a/b").unwrap()));
        assert!(path.starts_with(&VfsPath::parse("a").unwrap()));
        assert_eq!(
            path.strip_prefix(&VfsPath::parse("a").unwrap())
                .map(|p| p.as_str().to_owned()),
            Some("bc".into())
        );
    }

    #[test]
    fn native_resolution_stays_under_the_root() {
        let root = std::path::Path::new("/tmp/root");
        let native = VfsPath::parse("a/b").unwrap().to_native(root);
        assert!(native.starts_with(root));
    }
}
