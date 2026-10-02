//! The local disk, presented through [`FileSystem`].

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::cancel::Cancel;
use crate::entry::{EntryKind, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{VfsError, VfsResult};
use crate::fs::{Capabilities, FileSystem, OpenFile};
use crate::path::VfsPath;
use crate::stored::{device_refusal, free_spelling, local_child, refuse, Stored};

/// A directory on disk and everything under it.
#[derive(Debug, Clone)]
pub struct LocalFs {
    root: PathBuf,
    follow_links: bool,
}

impl LocalFs {
    /// Present `root` as a file system.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            follow_links: false,
        }
    }

    /// Report the target's metadata for a link instead of the link's own.
    #[must_use]
    pub fn follow_links(mut self, follow: bool) -> Self {
        self.follow_links = follow;
        self
    }

    /// The directory this file system was built over.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a virtual path against the root.
    ///
    /// The path type forbids `..`, absolute paths and volume prefixes, so the
    /// result is always inside the root.
    fn native(&self, path: &VfsPath) -> PathBuf {
        path.to_native(&self.root)
    }

    /// The native path of `path`, or the refusal of a component a native
    /// path does not reach as itself.
    ///
    /// A DOS device name is a usable path component, and on Windows a path
    /// that holds one reaches the device.
    fn reachable(&self, path: &VfsPath) -> VfsResult<PathBuf> {
        if let Some(reason) = path.components().find_map(device_refusal) {
            return Err(VfsError::Refused {
                path: path.clone(),
                reason,
            });
        }
        Ok(self.native(path))
    }

    /// The items of the directory `dir`: each usable one with its path, and
    /// each refused one with the free spelling its row lists under.
    ///
    /// A refused row takes the first spelling no other item of the folder
    /// holds, so the same folder gives every refused row the same path in
    /// every listing.
    fn children(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Children> {
        let native = self.reachable(dir)?;
        let meta = fs::metadata(&native).map_err(|error| map_io(error, dir))?;
        if !meta.is_dir() {
            return Err(VfsError::NotADirectory { path: dir.clone() });
        }
        let mut usable = Vec::new();
        let mut refused = Vec::new();
        for item in fs::read_dir(&native).map_err(|error| map_io(error, dir))? {
            cancel.check()?;
            let item = item?;
            match local_child(dir, &item.file_name()) {
                Stored::Usable(child) => usable.push((child, item)),
                Stored::Refused { display, reason } => refused.push((display, reason, item)),
                Stored::Root => {}
            }
        }
        let mut placed = Vec::with_capacity(refused.len());
        if !refused.is_empty() {
            let mut taken: HashSet<String> = usable
                .iter()
                .filter_map(|(path, _): &(VfsPath, fs::DirEntry)| path.name())
                .map(str::to_lowercase)
                .collect();
            for (display, reason, item) in refused {
                placed.push((unclaimed(dir, display, &mut taken), reason, item));
            }
        }
        Ok(Children {
            usable,
            refused: placed,
        })
    }

    /// The refused row the listing of `path`'s directory shows at `path`.
    ///
    /// The row's path names no item on disk, so a lookup of it by path finds
    /// nothing; the listing of the directory is what holds it.
    fn refused_row(&self, path: &VfsPath) -> Option<VfsEntry> {
        let dir = path.parent()?;
        let children = self.children(&dir, &Cancel::new()).ok()?;
        children
            .refused
            .into_iter()
            .find(|(listed, _, _)| listed == path)
            .map(|(listed, reason, item)| Self::refused_entry(listed, &reason, &item))
    }

    fn entry_for(&self, path: VfsPath, native: &Path) -> VfsResult<VfsEntry> {
        let link_meta = fs::symlink_metadata(native).map_err(|error| map_io(error, &path))?;
        let is_link = link_meta.file_type().is_symlink();
        let meta = if is_link && self.follow_links {
            fs::metadata(native).unwrap_or(link_meta.clone())
        } else {
            link_meta.clone()
        };
        let name = path
            .name()
            .map(str::to_owned)
            .or_else(|| native.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        Ok(entry_of(path, name, is_link, &meta))
    }

    /// The row of a directory item whose name the path rules refuse.
    ///
    /// The row lists under the mapped name, which names no item on disk, so
    /// its facts come from the directory listing. A lookup by the stored name
    /// fails as well: the Win32 path layer drops a trailing dot or space and
    /// reaches another item or none.
    fn refused_entry(path: VfsPath, reason: &str, item: &fs::DirEntry) -> VfsEntry {
        let mut entry = match item.metadata() {
            Ok(meta) => {
                let name = path.name().unwrap_or_default().to_owned();
                entry_of(path, name, meta.file_type().is_symlink(), &meta)
            }
            Err(error) => {
                let mut entry = VfsEntry::file(path, 0);
                entry.error = Some(error.to_string());
                entry
            }
        };
        refuse(&mut entry, reason);
        entry
    }

    /// Write `content` beside the target then rename, so a reader never sees a
    /// half written file and an interrupted write leaves the old one intact.
    fn write_atomic(
        &self,
        target: &Path,
        content: &mut dyn Read,
        cancel: &Cancel,
    ) -> VfsResult<()> {
        let parent = target.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent)?;
        let mut chunk = vec![0u8; 64 * 1024];
        ca_io::replace_checked(
            target,
            |writer| {
                loop {
                    cancel.check()?;
                    let read = content.read(&mut chunk)?;
                    if read == 0 {
                        break;
                    }
                    writer.write_all(chunk.get(..read).unwrap_or_default())?;
                }
                Ok(())
            },
            || cancel.check(),
        )
    }
}

impl FileSystem for LocalFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: true,
            supports_timestamps: true,
            supports_attributes: true,
            stored_crc: false,
            random_access: true,
            content_available: true,
        }
    }

    fn root_label(&self) -> String {
        self.root.display().to_string()
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        let children = self.children(dir, cancel)?;
        let mut out = Vec::with_capacity(children.usable.len() + children.refused.len());
        for (child, item) in children.usable {
            match self.entry_for(child.clone(), &item.path()) {
                Ok(entry) => out.push(entry),
                Err(error) => {
                    let mut entry = VfsEntry::file(child, 0);
                    entry.error = Some(error.to_string());
                    out.push(entry);
                }
            }
        }
        for (path, reason, item) in children.refused {
            out.push(Self::refused_entry(path, &reason, &item));
        }
        Ok(out)
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        let native = self.reachable(path)?;
        match self.entry_for(path.clone(), &native) {
            Err(VfsError::NotFound { .. }) => self
                .refused_row(path)
                .ok_or_else(|| VfsError::NotFound { path: path.clone() }),
            other => other,
        }
    }

    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile> {
        cancel.check()?;
        let native = self.reachable(path)?;
        let meta = match fs::metadata(&native) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(match self.refused_row(path) {
                    Some(row) => VfsError::Refused {
                        path: path.clone(),
                        reason: row.error.unwrap_or_default(),
                    },
                    None => VfsError::NotFound { path: path.clone() },
                });
            }
            Err(error) => return Err(map_io(error, path)),
        };
        if meta.is_dir() {
            return Err(VfsError::IsADirectory { path: path.clone() });
        }
        let file = fs::File::open(&native).map_err(|error| map_io(error, path))?;
        Ok(OpenFile::seekable(file, Some(meta.len())))
    }

    fn create_dir(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        cancel.check()?;
        fs::create_dir_all(self.reachable(path)?)?;
        Ok(())
    }

    fn write_file(&self, path: &VfsPath, content: &mut dyn Read, cancel: &Cancel) -> VfsResult<()> {
        if path.is_root() {
            return Err(VfsError::IsADirectory { path: path.clone() });
        }
        let native = self.reachable(path)?;
        self.write_atomic(&native, content, cancel)
    }

    fn delete(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        cancel.check()?;
        if path.is_root() {
            return Err(VfsError::unsupported("deleting the root of a file system"));
        }
        let native = self.reachable(path)?;
        let meta = fs::symlink_metadata(&native).map_err(|error| map_io(error, path))?;
        if meta.is_dir() {
            fs::remove_dir_all(&native)?;
        } else {
            fs::remove_file(&native)?;
        }
        Ok(())
    }

    fn rename(&self, from: &VfsPath, to: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        cancel.check()?;
        let source = self.reachable(from)?;
        let target = self.reachable(to)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&source, &target).map_err(|error| map_io(error, from))?;
        Ok(())
    }

    /// Resolve a link to the place it points at.
    ///
    /// Only a link is resolved. A plain directory is left alone, because
    /// resolving one would turn a path the caller gave into whatever the
    /// volume happens to mount there and cost a syscall per directory in the
    /// tree for nothing.
    fn link_identity(&self, path: &VfsPath) -> Option<String> {
        let native = self.native(path);
        let meta = fs::symlink_metadata(&native).ok()?;
        if !meta.file_type().is_symlink() {
            return Some(native.to_string_lossy().into_owned());
        }
        let resolved = fs::canonicalize(&native).ok()?;
        Some(resolved.to_string_lossy().into_owned())
    }
}

/// Every fact a local listing states about one item.
fn entry_of(path: VfsPath, name: String, is_link: bool, meta: &fs::Metadata) -> VfsEntry {
    let kind = if meta.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    let link = is_link.then(|| {
        if meta.is_dir() {
            VfsLinkKind::DirectoryLink
        } else {
            VfsLinkKind::FileLink
        }
    });
    VfsEntry {
        path,
        name,
        kind,
        size: if meta.is_dir() { 0 } else { meta.len() },
        size_is_exact: true,
        modified: meta.modified().ok(),
        time_fidelity: crate::entry::TimeFidelity::Utc,
        created: meta.created().ok(),
        attributes: Some(attributes_of(meta)),
        crc32: None,
        link,
        version_info: None,
        error: None,
        refused: false,
    }
}

/// Every item of one directory, split by whether a path reaches it.
struct Children {
    usable: Vec<(VfsPath, fs::DirEntry)>,
    refused: Vec<(VfsPath, String, fs::DirEntry)>,
}

/// `display`, or the first `display~N` that no other item of the folder
/// holds.
fn unclaimed(dir: &VfsPath, display: VfsPath, taken: &mut HashSet<String>) -> VfsPath {
    let base = display.name().unwrap_or_default().to_owned();
    let spelling = free_spelling(&base, taken);
    if spelling == base {
        return display;
    }
    dir.join(&spelling).unwrap_or(display)
}

/// Turn a missing-file error into the typed variant callers match on.
fn map_io(error: std::io::Error, path: &VfsPath) -> VfsError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return VfsError::NotFound { path: path.clone() };
    }
    VfsError::Io(error)
}

#[cfg(windows)]
fn attributes_of(meta: &fs::Metadata) -> VfsAttributes {
    use std::os::windows::fs::MetadataExt;

    const READ_ONLY: u32 = 0x0000_0001;
    const HIDDEN: u32 = 0x0000_0002;
    const SYSTEM: u32 = 0x0000_0004;
    const ARCHIVE: u32 = 0x0000_0020;

    let bits = meta.file_attributes();
    VfsAttributes {
        read_only: bits & READ_ONLY != 0,
        hidden: bits & HIDDEN != 0,
        system: bits & SYSTEM != 0,
        archive: bits & ARCHIVE != 0,
        windows_bits: Some(bits),
        unix_mode: None,
        uid: None,
        gid: None,
    }
}

#[cfg(unix)]
fn attributes_of(meta: &fs::Metadata) -> VfsAttributes {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let mode = meta.permissions().mode();
    VfsAttributes {
        read_only: mode & 0o200 == 0,
        hidden: false,
        system: false,
        archive: false,
        windows_bits: None,
        unix_mode: Some(meta.mode()),
        uid: Some(meta.uid()),
        gid: Some(meta.gid()),
    }
}

#[cfg(not(any(windows, unix)))]
fn attributes_of(_meta: &fs::Metadata) -> VfsAttributes {
    VfsAttributes::default()
}
