//! Finding packs, loading the selected one off the frame thread, and handing
//! the result to the painting code.

use super::load::{self, DirectoryFiles, LoadedPack, MemoryFiles, PackFiles};
use super::manifest::{PackManifest, MANIFEST_FILE, MAX_MANIFEST_BYTES};
use super::{install_active, install_catalog, PackError, PackModes};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

/// Most pack folders one scan reads.
pub const MAX_INSTALLED_PACKS: usize = 64;

/// A pack compiled into the program.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinPack {
    /// The bytes of `pack.json`.
    pub manifest: &'static [u8],
    /// Every other file, by relative path.
    pub files: &'static [(&'static str, &'static [u8])],
}

/// The packs compiled into the program.
pub const BUILTIN: &[BuiltinPack] = &[];

/// One line of the pack list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    /// Identifier stored in the options; empty for a refused pack.
    pub id: String,
    /// Name to show.
    pub name: String,
    /// Version text, empty for a refused pack.
    pub version: String,
    /// The modes the pack paints in.
    pub modes: PackModes,
    /// The pack is compiled into the program.
    pub built_in: bool,
    /// Why the pack cannot be used, when it cannot.
    pub problem: Option<String>,
}

/// Every pack found, the built-in look first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// The entries, in list order.
    pub entries: Vec<PackEntry>,
    /// The scan has finished.
    pub scanned: bool,
}

impl Catalog {
    /// The entry with `id`.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&PackEntry> {
        if id.is_empty() {
            return None;
        }
        self.entries.iter().find(|entry| entry.id == id)
    }
}

#[derive(Debug, Clone)]
enum Source {
    Builtin(BuiltinPack),
    Folder(PathBuf),
}

struct Scan {
    entries: Vec<PackEntry>,
    sources: BTreeMap<String, Source>,
}

enum ScanState {
    Idle,
    Running(Receiver<Scan>),
    Done,
}

struct Loading {
    id: String,
    receiver: Receiver<Result<LoadedPack, PackError>>,
}

/// Owns the pack list and the selected pack for one window.
pub struct PackHost {
    directory: PathBuf,
    builtin: &'static [BuiltinPack],
    scan: ScanState,
    sources: BTreeMap<String, Source>,
    entries: Vec<PackEntry>,
    failed: BTreeMap<String, String>,
    loading: Option<Loading>,
    active: Option<Arc<LoadedPack>>,
    published: Option<Arc<Catalog>>,
}

impl PackHost {
    /// A host over the packs folder `directory` and the compiled-in packs.
    #[must_use]
    pub fn new(directory: PathBuf, builtin: &'static [BuiltinPack]) -> Self {
        Self {
            directory,
            builtin,
            scan: ScanState::Idle,
            sources: BTreeMap::new(),
            entries: Vec::new(),
            failed: BTreeMap::new(),
            loading: None,
            active: None,
            published: None,
        }
    }

    /// Look for packs again on the next frame.
    pub fn rescan(&mut self) {
        if !matches!(self.scan, ScanState::Running(_)) {
            self.scan = ScanState::Idle;
        }
    }

    /// Advance scanning and loading, and return the pack in force.
    ///
    /// `wanted` is the identifier stored in the options. An identifier that
    /// names no usable pack selects the built-in look. All reading and
    /// decoding runs on worker threads; this call only checks for results.
    pub fn update(&mut self, ctx: &egui::Context, wanted: &str) -> Option<Arc<LoadedPack>> {
        self.poll_scan(ctx);
        if matches!(self.scan, ScanState::Done) {
            self.select(ctx, wanted);
        }
        self.publish(ctx);
        self.active.clone()
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        if matches!(self.scan, ScanState::Idle) {
            let (sender, receiver) = channel();
            let directory = self.directory.clone();
            let builtin = self.builtin;
            let repaint = ctx.clone();
            let started = std::thread::Builder::new()
                .name("theme-pack-scan".to_owned())
                .spawn(move || {
                    let _ = sender.send(scan(builtin, &directory));
                    repaint.request_repaint();
                });
            self.scan = if started.is_ok() {
                ScanState::Running(receiver)
            } else {
                ScanState::Done
            };
        }
        if let ScanState::Running(receiver) = &self.scan {
            match receiver.try_recv() {
                Ok(result) => {
                    self.entries = result.entries;
                    self.sources = result.sources;
                    self.failed.clear();
                    self.scan = ScanState::Done;
                }
                Err(TryRecvError::Disconnected) => self.scan = ScanState::Done,
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    fn select(&mut self, ctx: &egui::Context, wanted: &str) {
        if let Some(loading) = &self.loading {
            match loading.receiver.try_recv() {
                Ok(Ok(pack)) => {
                    let id = loading.id.clone();
                    self.loading = None;
                    if id == wanted {
                        self.active = Some(Arc::new(pack));
                        install_active(ctx, self.active.clone());
                    }
                }
                Ok(Err(error)) => {
                    let id = loading.id.clone();
                    self.loading = None;
                    self.failed.insert(id, error.message().to_owned());
                }
                Err(TryRecvError::Disconnected) => {
                    let id = loading.id.clone();
                    self.loading = None;
                    self.failed
                        .insert(id, "The pack could not be loaded.".to_owned());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let usable = !wanted.is_empty()
            && self.sources.contains_key(wanted)
            && !self.failed.contains_key(wanted);
        if !usable {
            self.loading = None;
            if self.active.take().is_some() {
                install_active(ctx, None);
            }
            return;
        }
        if self.active.as_ref().is_some_and(|pack| pack.id() == wanted) {
            self.loading = None;
            return;
        }
        if self
            .loading
            .as_ref()
            .is_some_and(|loading| loading.id == wanted)
        {
            return;
        }
        let Some(source) = self.sources.get(wanted).cloned() else {
            return;
        };
        let (sender, receiver) = channel();
        let repaint = ctx.clone();
        let started = std::thread::Builder::new()
            .name("theme-pack-load".to_owned())
            .spawn(move || {
                let _ = sender.send(load_source(&source));
                repaint.request_repaint();
            });
        if started.is_ok() {
            self.loading = Some(Loading {
                id: wanted.to_owned(),
                receiver,
            });
        } else {
            self.failed.insert(
                wanted.to_owned(),
                "The pack could not be loaded.".to_owned(),
            );
        }
    }

    fn publish(&mut self, ctx: &egui::Context) {
        let mut entries = self.entries.clone();
        for entry in &mut entries {
            if let Some(problem) = self.failed.get(&entry.id) {
                entry.problem = Some(problem.clone());
            }
        }
        let catalog = Catalog {
            entries,
            scanned: matches!(self.scan, ScanState::Done),
        };
        if self.published.as_deref() != Some(&catalog) {
            let shared = Arc::new(catalog);
            install_catalog(ctx, Arc::clone(&shared));
            self.published = Some(shared);
        }
    }
}

fn load_source(source: &Source) -> Result<LoadedPack, PackError> {
    match source {
        Source::Builtin(pack) => load::load(pack.manifest, &memory_files(pack)),
        Source::Folder(folder) => {
            let files = DirectoryFiles::new(folder)?;
            let manifest = read_manifest(&files)?;
            load::load(&manifest, &files)
        }
    }
}

fn memory_files(pack: &BuiltinPack) -> MemoryFiles {
    let mut files = MemoryFiles::new();
    for (path, bytes) in pack.files {
        files.insert(path, *bytes);
    }
    files
}

fn read_manifest(files: &dyn PackFiles) -> Result<Vec<u8>, PackError> {
    files
        .read(MANIFEST_FILE, MAX_MANIFEST_BYTES)?
        .ok_or_else(|| PackError::new(format!("The folder has no {MANIFEST_FILE} file.")))
}

fn scan(builtin: &[BuiltinPack], directory: &Path) -> Scan {
    let mut entries = vec![PackEntry {
        id: String::new(),
        name: "Default".to_owned(),
        version: String::new(),
        modes: PackModes::BOTH,
        built_in: true,
        problem: None,
    }];
    let mut sources = BTreeMap::new();
    for pack in builtin {
        match PackManifest::parse(pack.manifest) {
            Ok(manifest) => add(
                &mut entries,
                &mut sources,
                &manifest,
                true,
                Source::Builtin(*pack),
            ),
            Err(error) => entries.push(refused("built-in pack", &error, true)),
        }
    }
    for folder in installed_folders(directory) {
        let name = folder
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let manifest = DirectoryFiles::new(&folder)
            .and_then(|files| read_manifest(&files))
            .and_then(|bytes| PackManifest::parse(&bytes));
        match manifest {
            Ok(manifest) => add(
                &mut entries,
                &mut sources,
                &manifest,
                false,
                Source::Folder(folder),
            ),
            Err(error) => entries.push(refused(&name, &error, false)),
        }
    }
    Scan { entries, sources }
}

fn add(
    entries: &mut Vec<PackEntry>,
    sources: &mut BTreeMap<String, Source>,
    manifest: &PackManifest,
    built_in: bool,
    source: Source,
) {
    let taken = sources.contains_key(&manifest.id);
    entries.push(PackEntry {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        modes: manifest.pack_modes(),
        built_in,
        problem: taken.then(|| "Another pack already uses this identifier.".to_owned()),
    });
    if !taken {
        sources.insert(manifest.id.clone(), source);
    }
}

fn refused(name: &str, error: &PackError, built_in: bool) -> PackEntry {
    PackEntry {
        id: String::new(),
        name: name.to_owned(),
        version: String::new(),
        modes: PackModes {
            light: false,
            dark: false,
        },
        built_in,
        problem: Some(error.message().to_owned()),
    }
}

/// The pack folders inside `directory`, in name order, without following links.
fn installed_folders(directory: &Path) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    folders.truncate(MAX_INSTALLED_PACKS);
    folders
}
