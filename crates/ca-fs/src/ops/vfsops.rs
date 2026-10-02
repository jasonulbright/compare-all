//! The file system calls of a plan, routed to whatever source holds the path.
//!
//! The planner, the executor and the journal all speak in paths. A source that
//! is not the local disk is mounted under a path prefix of its own, so one
//! batch can copy between a folder, a container and a remote location without
//! any of those three modules knowing which is which.
//!
//! A container is rewritten in full for every change it accepts, so changes
//! aimed at one are queued and applied together. [`SourceOps::open_batch`]
//! starts the queue and [`SourceOps::commit_batch`] applies it with one
//! rewrite per container. A queue that is never committed changes nothing.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use ca_vfs::{ArchiveEdit, Cancel as VfsCancel, VfsError, VfsPath};

use crate::ops::fsops::{
    check_then_rename, write_new_through, AttributeChange, FileOps, ItemIdentity, RealFs,
    SyncWrite, TargetState,
};
use crate::source::{Source, SourceKind};

/// One source and the path prefix it answers for.
#[derive(Debug, Clone)]
pub struct Mount {
    /// Path every entry of this source sits under.
    pub prefix: PathBuf,
    /// The source itself.
    pub source: Source,
}

impl Mount {
    /// Mount `source` under `prefix`.
    #[must_use]
    pub fn new(prefix: impl Into<PathBuf>, source: Source) -> Self {
        Self {
            prefix: prefix.into(),
            source,
        }
    }

    /// A container mounted under the path of the container file itself.
    #[must_use]
    pub fn of_archive(source: Source) -> Self {
        let prefix = source.origin().to_path_buf();
        Self { prefix, source }
    }
}

/// Why one call could not be routed or carried out.
fn refuse(kind: io::ErrorKind, message: impl Into<String>) -> io::Error {
    io::Error::new(kind, message.into())
}

fn carry(error: &VfsError) -> io::Error {
    let kind = match error {
        VfsError::NotFound { .. } => io::ErrorKind::NotFound,
        VfsError::AlreadyExists { .. } => io::ErrorKind::AlreadyExists,
        VfsError::ReadOnly | VfsError::Unsupported { .. } => io::ErrorKind::Unsupported,
        VfsError::Cancelled => io::ErrorKind::Interrupted,
        _ => io::ErrorKind::Other,
    };
    refuse(kind, error.to_string())
}

/// [`FileOps`] over a set of mounted sources, with the local disk behind them.
///
/// A path under no mount reaches the real file system, so a batch that touches
/// one folder and one container needs only the container mounted.
pub struct SourceOps {
    state: std::sync::Arc<State>,
}

/// Everything the router owns, shared with each open destination.
struct State {
    mounts: Vec<Mount>,
    local: RealFs,
    /// Changes queued for each mount, by position in `mounts`.
    queued: Mutex<BTreeMap<usize, Vec<ArchiveEdit>>>,
    batching: Mutex<bool>,
    cancel: VfsCancel,
}

impl std::fmt::Debug for SourceOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceOps")
            .field("mounts", &self.state.mounts.len())
            .finish_non_exhaustive()
    }
}

impl SourceOps {
    /// Route every path under one of `mounts` to that source, and everything
    /// else to the local disk.
    #[must_use]
    pub fn new(mounts: Vec<Mount>) -> Self {
        Self {
            state: std::sync::Arc::new(State {
                mounts,
                local: RealFs,
                queued: Mutex::new(BTreeMap::new()),
                batching: Mutex::new(false),
                cancel: VfsCancel::new(),
            }),
        }
    }

    /// Stop every call this router still has to make.
    pub fn cancel(&self) {
        self.state.cancel.cancel();
    }

    /// Start queueing container changes instead of applying each one.
    pub fn open_batch(&self) {
        if let Ok(mut batching) = self.state.batching.lock() {
            *batching = true;
        }
    }

    /// Apply every queued change, one rewrite per container.
    ///
    /// An interrupted rewrite leaves the container as it was, because the
    /// rewrite is written beside it and renamed over it only once it is
    /// complete.
    ///
    /// # Errors
    /// Propagates the first container that refuses its batch. Containers
    /// already rewritten stay rewritten, and the error names the one that
    /// failed.
    pub fn commit_batch(&self) -> io::Result<()> {
        self.state.commit_batch()
    }

    /// The queued changes of one mount, for a caller that reports what a batch
    /// still holds.
    #[must_use]
    pub fn pending(&self, index: usize) -> usize {
        self.state.pending(index)
    }
}

impl State {
    fn commit_batch(&self) -> io::Result<()> {
        let queued = {
            let mut queued = self
                .queued
                .lock()
                .map_err(|_| refuse(io::ErrorKind::Other, "queue lock poisoned"))?;
            std::mem::take(&mut *queued)
        };
        if let Ok(mut batching) = self.batching.lock() {
            *batching = false;
        }
        for (index, edits) in queued {
            let Some(mount) = self.mounts.get(index) else {
                continue;
            };
            let Some(archive) = mount.source.as_archive() else {
                continue;
            };
            archive
                .apply_edits(&edits, &self.cancel)
                .map_err(|error| carry(&error))?;
        }
        Ok(())
    }

    /// The mount a path belongs to, and the path inside it.
    fn route(&self, path: &Path) -> Option<(usize, &Mount, VfsPath)> {
        for (index, mount) in self.mounts.iter().enumerate() {
            if let Ok(rest) = path.strip_prefix(&mount.prefix) {
                let mut inside = VfsPath::root();
                for part in rest.components() {
                    let text = part.as_os_str().to_string_lossy();
                    inside = inside.join(&text).ok()?;
                }
                return Some((index, mount, inside));
            }
        }
        None
    }

    fn is_batching(&self) -> bool {
        self.batching.lock().map(|flag| *flag).unwrap_or(false)
    }

    /// Queue one change, or apply it now when no batch is open.
    fn edit(&self, index: usize, mount: &Mount, edit: ArchiveEdit) -> io::Result<()> {
        if mount.source.as_archive().is_none() || !self.is_batching() {
            return self.apply_now(mount, &edit);
        }
        let mut queued = self
            .queued
            .lock()
            .map_err(|_| refuse(io::ErrorKind::Other, "queue lock poisoned"))?;
        let entry = queued.entry(index).or_default();
        // The rewrite reads every edit against the listing the batch started
        // from, where a name the batch itself wrote does not exist, so a
        // delete alone would leave the queued write to be stored.
        if let ArchiveEdit::Delete { path } = &edit {
            entry.retain(|queued| match queued {
                ArchiveEdit::WriteFile { path: written, .. }
                | ArchiveEdit::CreateDir { path: written } => !written.starts_with(path),
                ArchiveEdit::Delete { .. } | ArchiveEdit::Rename { .. } => true,
            });
        }
        // A write under a temporary name followed by a rename is one stored
        // entry, because the rewrite that lands it is already all-or-nothing.
        if let ArchiveEdit::Rename { from, to } = &edit {
            if let Some(pending) = entry.iter_mut().find(|queued| match queued {
                ArchiveEdit::WriteFile { path, .. } => path == from,
                _ => false,
            }) {
                if let ArchiveEdit::WriteFile { path, .. } = pending {
                    *path = to.clone();
                }
                return Ok(());
            }
        }
        entry.push(edit);
        Ok(())
    }

    /// Give the write the batch queued for `inside` the modification time
    /// `time`, and say whether one was queued.
    fn stamp_queued_write(&self, index: usize, inside: &VfsPath, time: SystemTime) -> bool {
        let Ok(mut queued) = self.queued.lock() else {
            return false;
        };
        let Some(pending) = queued.get_mut(&index).and_then(|entry| {
            entry.iter_mut().rev().find(|queued| match queued {
                ArchiveEdit::WriteFile { path, .. } => path == inside,
                _ => false,
            })
        }) else {
            return false;
        };
        if let ArchiveEdit::WriteFile { modified, .. } = pending {
            *modified = Some(time);
        }
        true
    }

    fn apply_now(&self, mount: &Mount, edit: &ArchiveEdit) -> io::Result<()> {
        let fs = mount.source.file_system();
        let result = match edit {
            ArchiveEdit::CreateDir { path } => fs.create_dir(path, &self.cancel),
            ArchiveEdit::WriteFile { path, content, .. } => {
                let mut bytes = content.as_slice();
                fs.write_file(path, &mut bytes, &self.cancel)
            }
            ArchiveEdit::Delete { path } => fs.delete(path, &self.cancel),
            ArchiveEdit::Rename { from, to } => fs.rename(from, to, &self.cancel),
        };
        result.map_err(|error| carry(&error))
    }

    fn pending(&self, index: usize) -> usize {
        self.queued
            .lock()
            .ok()
            .and_then(|queued| queued.get(&index).map(Vec::len))
            .unwrap_or(0)
    }
}

impl FileOps for SourceOps {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        let Some((_, mount, inside)) = self.state.route(path) else {
            return self.state.local.probe(path);
        };
        let entry = mount
            .source
            .file_system()
            .metadata(&inside)
            .map_err(|error| carry(&error))?;
        Ok(TargetState {
            is_dir: entry.is_dir(),
            is_link: entry.is_link(),
            read_only: !mount.source.is_writable(),
            hidden: entry.attributes.is_some_and(|bits| bits.hidden),
            size: if entry.is_dir() { 0 } else { entry.size },
            modified: entry.modified,
            created: entry.created,
        })
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let Some((index, mount, inside)) = self.state.route(path) else {
            return self.state.local.create_dir(path);
        };
        self.state
            .edit(index, mount, ArchiveEdit::CreateDir { path: inside })
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let Some((_, mount, inside)) = self.state.route(path) else {
            return self.state.local.open_read(path);
        };
        let open = mount
            .source
            .file_system()
            .open(&inside, &self.state.cancel)
            .map_err(|error| carry(&error))?;
        Ok(Box::new(open))
    }

    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        let Some((index, mount, inside)) = self.state.route(path) else {
            return self.state.local.create_new(path);
        };
        if !mount.source.is_writable() {
            return Err(refuse(
                io::ErrorKind::Unsupported,
                mount
                    .source
                    .refusal()
                    .unwrap_or_else(|| "the source refuses writes".to_owned()),
            ));
        }
        if mount.source.file_system().metadata(&inside).is_ok() {
            return Err(refuse(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            ));
        }
        Ok(Box::new(BufferedEntry {
            ops: std::sync::Arc::clone(&self.state),
            index,
            path: inside,
            bytes: Vec::new(),
            written: false,
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        match (self.state.route(from), self.state.route(to)) {
            (None, None) => self.state.local.rename(from, to),
            (Some((index, mount, inside_from)), Some((other, _, inside_to))) if index == other => {
                self.state.edit(
                    index,
                    mount,
                    ArchiveEdit::Rename {
                        from: inside_from,
                        to: inside_to,
                    },
                )
            }
            _ => Err(refuse(
                io::ErrorKind::CrossesDevices,
                "a rename cannot cross two sources",
            )),
        }
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.state.route(from).is_none() && self.state.route(to).is_none() {
            return self.state.local.rename_no_replace(from, to);
        }
        check_then_rename(self, from, to)
    }

    fn write_new(
        &self,
        target: &Path,
        reader: &mut dyn Read,
        buffer_size: usize,
    ) -> io::Result<()> {
        if self.state.route(target).is_none() {
            return self.state.local.write_new(target, reader, buffer_size);
        }
        write_new_through(self, target, reader, buffer_size)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let Some((index, mount, inside)) = self.state.route(path) else {
            return self.state.local.remove_file(path);
        };
        self.state
            .edit(index, mount, ArchiveEdit::Delete { path: inside })
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        let Some((index, mount, inside)) = self.state.route(path) else {
            return self.state.local.remove_dir(path);
        };
        self.state
            .edit(index, mount, ArchiveEdit::Delete { path: inside })
    }

    fn move_to_trash(&self, path: &Path) -> io::Result<()> {
        if self.state.route(path).is_none() {
            return self.state.local.move_to_trash(path);
        }
        Err(refuse(
            io::ErrorKind::Unsupported,
            "a source other than a local folder has no recycle bin",
        ))
    }

    /// A container takes a time only with the write the open batch queued for
    /// the same path: a rewrite for a time alone would cost a whole container.
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        let Some((index, _, inside)) = self.state.route(path) else {
            return self.state.local.set_modified(path, time);
        };
        if self.state.stamp_queued_write(index, &inside, time) {
            return Ok(());
        }
        Err(refuse(
            io::ErrorKind::Unsupported,
            "this source does not accept a written time stamp",
        ))
    }

    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        if self.state.route(path).is_none() {
            return self.state.local.set_created(path, time);
        }
        Err(refuse(
            io::ErrorKind::Unsupported,
            "this source does not accept a written creation time",
        ))
    }

    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()> {
        if self.state.route(path).is_none() {
            return self.state.local.set_attributes(path, change);
        }
        Err(refuse(
            io::ErrorKind::Unsupported,
            "this source does not accept written attributes",
        ))
    }

    fn same_volume(&self, from: &Path, to: &Path) -> bool {
        match (self.state.route(from), self.state.route(to)) {
            (None, None) => self.state.local.same_volume(from, to),
            (Some((left, _, _)), Some((right, _, _))) => left == right,
            _ => false,
        }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let Some((_, mount, inside)) = self.state.route(path) else {
            return self.state.local.read_dir(path);
        };
        let listed = mount
            .source
            .file_system()
            .list(&inside, &self.state.cancel)
            .map_err(|error| carry(&error))?;
        Ok(listed
            .into_iter()
            .map(|entry| {
                let mut out = mount.prefix.clone();
                for part in entry.path.components() {
                    out.push(part);
                }
                out
            })
            .collect())
    }

    /// A mounted path names one entry and no link stands between it and the
    /// root, so the path is already the real location.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        if self.state.route(path).is_none() {
            return std::fs::canonicalize(path);
        }
        Ok(path.to_path_buf())
    }

    /// An entry of a mounted source has no number of its own; its path is
    /// its identity, which [`FileOps::canonicalize`] states.
    fn identity(&self, path: &Path) -> io::Result<Option<ItemIdentity>> {
        if self.state.route(path).is_none() {
            return self.state.local.identity(path);
        }
        Ok(None)
    }

    fn volume_identity(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        if self.state.route(path).is_none() {
            return self.state.local.volume_identity(path);
        }
        Ok(None)
    }

    fn keeps_case(&self, path: &Path) -> bool {
        match self.state.route(path) {
            Some((_, mount, _)) => mount.source.kind() == SourceKind::Archive,
            None => self.state.local.keeps_case(path),
        }
    }

    fn identity_is_path(&self, path: &Path) -> bool {
        self.state.route(path).is_some()
    }
}

/// A destination inside a source that takes its bytes in one piece.
///
/// A container stores an entry whole, so the bytes are held until the writer is
/// flushed and then handed over as one change.
struct BufferedEntry {
    ops: std::sync::Arc<State>,
    index: usize,
    path: VfsPath,
    bytes: Vec<u8>,
    written: bool,
}

impl BufferedEntry {
    fn hand_over(&mut self) -> io::Result<()> {
        if self.written {
            return Ok(());
        }
        self.written = true;
        let Some(mount) = self.ops.mounts.get(self.index) else {
            return Err(refuse(io::ErrorKind::NotFound, "the source is gone"));
        };
        self.ops.edit(
            self.index,
            mount,
            ArchiveEdit::WriteFile {
                path: self.path.clone(),
                content: std::mem::take(&mut self.bytes),
                modified: None,
            },
        )
    }
}

impl Write for BufferedEntry {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.hand_over()
    }
}

impl SyncWrite for BufferedEntry {
    fn sync_data(&mut self) -> io::Result<()> {
        self.hand_over()
    }
}

/// A source whose kind cannot take part in a destructive step, with the text
/// that states why.
#[must_use]
pub fn write_refusal(source: &Source) -> Option<String> {
    match source.kind() {
        SourceKind::LocalFolder | SourceKind::Archive | SourceKind::Remote => source.refusal(),
        SourceKind::Snapshot => Some("a recorded listing holds no files to change".to_owned()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{Mount, SourceOps};
    use crate::ops::fsops::FileOps;
    use crate::source::Source;

    #[test]
    fn a_path_under_no_mount_reaches_the_local_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"payload").unwrap();
        let ops = SourceOps::new(Vec::new());
        assert_eq!(ops.probe(&file).unwrap().size, 7);
    }

    #[test]
    fn a_rename_across_two_sources_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ops = SourceOps::new(vec![Mount::new(
            dir.path().join("mounted"),
            Source::local(dir.path()),
        )]);
        let error = ops
            .rename(&dir.path().join("mounted/a"), &dir.path().join("b"))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::CrossesDevices);
    }
}
