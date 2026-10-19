//! Reading the files of a pack and decoding its icons.
//!
//! The loader takes manifest bytes and a file source, so a pack built into the
//! program and a pack in a user folder go through the same checks. Nothing in
//! this module touches a window: decoding runs on a worker and the finished
//! pixels are uploaded later, once per icon and size.

use super::manifest::{check_path, PackManifest, ICON_SIZES};
use super::{PackError, PackModes};
use crate::icons::Icon;
use ca_session::options::PaletteOptions;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Largest icon file accepted, in bytes.
pub const MAX_FILE_BYTES: usize = 128 * 1024;
/// Largest sum of icon file sizes accepted, in bytes.
pub const MAX_TOTAL_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Where the files of one pack are read from.
pub trait PackFiles {
    /// The bytes of the file at a checked relative path, or nothing when the
    /// pack has no such file.
    ///
    /// # Errors
    /// A message when the file is larger than `limit` or cannot be read safely.
    fn read(&self, path: &str, limit: usize) -> Result<Option<Vec<u8>>, PackError>;
}

/// Files held in memory, for packs built into the program.
#[derive(Debug, Clone, Default)]
pub struct MemoryFiles {
    files: BTreeMap<String, Cow<'static, [u8]>>,
}

impl MemoryFiles {
    /// An empty source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one file.
    pub fn insert(&mut self, path: &str, bytes: impl Into<Cow<'static, [u8]>>) {
        self.files.insert(path.to_owned(), bytes.into());
    }
}

impl PackFiles for MemoryFiles {
    fn read(&self, path: &str, limit: usize) -> Result<Option<Vec<u8>>, PackError> {
        let Some(bytes) = self.files.get(path) else {
            return Ok(None);
        };
        if bytes.len() > limit {
            return Err(too_large(path, limit));
        }
        Ok(Some(bytes.to_vec()))
    }
}

/// Files in a pack folder.
#[derive(Debug, Clone)]
pub struct DirectoryFiles {
    root: PathBuf,
}

impl DirectoryFiles {
    /// A source over `root`.
    ///
    /// # Errors
    /// A message when the folder cannot be resolved.
    pub fn new(root: &Path) -> Result<Self, PackError> {
        let root = std::fs::canonicalize(root)
            .map_err(|error| PackError::new(format!("The pack folder cannot be read: {error}.")))?;
        Ok(Self { root })
    }
}

impl PackFiles for DirectoryFiles {
    fn read(&self, path: &str, limit: usize) -> Result<Option<Vec<u8>>, PackError> {
        check_path(path)?;
        let joined = path
            .split('/')
            .fold(self.root.clone(), |folder, part| folder.join(part));
        let Ok(resolved) = std::fs::canonicalize(&joined) else {
            return Ok(None);
        };
        if !resolved.starts_with(&self.root) {
            return Err(PackError::new(format!(
                "The file \"{path}\" leaves the pack folder."
            )));
        }
        let meta = std::fs::symlink_metadata(&joined).map_err(|error| {
            PackError::new(format!("The file \"{path}\" cannot be read: {error}."))
        })?;
        if !meta.is_file() {
            return Err(PackError::new(format!("\"{path}\" is not a plain file.")));
        }
        if meta.len() > limit as u64 {
            return Err(too_large(path, limit));
        }
        let file = std::fs::File::open(&resolved).map_err(|error| {
            PackError::new(format!("The file \"{path}\" cannot be read: {error}."))
        })?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                PackError::new(format!("The file \"{path}\" cannot be read: {error}."))
            })?;
        if bytes.len() > limit {
            return Err(too_large(path, limit));
        }
        Ok(Some(bytes))
    }
}

fn too_large(path: &str, limit: usize) -> PackError {
    PackError::new(format!("The file \"{path}\" is larger than {limit} bytes."))
}

/// A pack with its icons decoded, ready to paint.
pub struct LoadedPack {
    manifest: PackManifest,
    images: HashMap<(Icon, u32), egui::ColorImage>,
    textures: Mutex<HashMap<(Icon, u32), egui::TextureHandle>>,
}

impl std::fmt::Debug for LoadedPack {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedPack")
            .field("id", &self.manifest.id)
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

impl LoadedPack {
    /// The pack identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// The pack name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// The modes the pack paints in.
    #[must_use]
    pub fn modes(&self) -> PackModes {
        self.manifest.pack_modes()
    }

    /// The colors the pack states.
    #[must_use]
    pub const fn colors(&self) -> &PaletteOptions {
        &self.manifest.colors
    }

    /// How many decoded icon images the pack holds.
    #[must_use]
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Whether the pack supplies any image for `icon`.
    #[must_use]
    pub fn has_icon(&self, icon: Icon) -> bool {
        ICON_SIZES
            .iter()
            .any(|size| self.images.contains_key(&(icon, *size)))
    }

    /// The pack size used to paint `icon` at `target` pixels: the smallest
    /// size that is not smaller than the target, else the largest.
    #[must_use]
    pub fn size_for(&self, icon: Icon, target: u32) -> Option<u32> {
        let sizes: Vec<u32> = ICON_SIZES
            .iter()
            .copied()
            .filter(|size| self.images.contains_key(&(icon, *size)))
            .collect();
        sizes
            .iter()
            .copied()
            .find(|size| *size >= target)
            .or_else(|| sizes.last().copied())
    }

    /// The texture for `icon` at `target` pixels, uploaded on first use.
    #[must_use]
    pub fn texture(
        &self,
        ctx: &egui::Context,
        icon: Icon,
        target: u32,
    ) -> Option<egui::TextureHandle> {
        let size = self.size_for(icon, target)?;
        let mut textures = self.textures.lock().ok()?;
        if let Some(handle) = textures.get(&(icon, size)) {
            return Some(handle.clone());
        }
        let image = self.images.get(&(icon, size))?;
        let handle = ctx.load_texture(
            format!("pack/{}/{}/{size}", self.manifest.id, icon.id()),
            image.clone(),
            egui::TextureOptions::LINEAR,
        );
        textures.insert((icon, size), handle.clone());
        Some(handle)
    }
}

/// Read, check and decode a pack.
///
/// A missing icon file is not an error: that icon falls back to the built-in
/// set. A file that breaks a limit refuses the whole pack.
///
/// # Errors
/// A message naming the first limit or rule the pack breaks.
pub fn load(manifest_bytes: &[u8], files: &dyn PackFiles) -> Result<LoadedPack, PackError> {
    let manifest = PackManifest::parse(manifest_bytes)?;
    let mut images = HashMap::new();
    let mut total_bytes = 0usize;
    for icon in Icon::ALL {
        for size in &manifest.icons.sizes {
            let Some(path) = manifest.icon_path(*icon, *size) else {
                continue;
            };
            check_path(&path)?;
            let Some(bytes) = files.read(&path, MAX_FILE_BYTES)? else {
                continue;
            };
            total_bytes += bytes.len();
            if total_bytes > MAX_TOTAL_FILE_BYTES {
                return Err(PackError::new(format!(
                    "The icon files are larger than {MAX_TOTAL_FILE_BYTES} bytes together."
                )));
            }
            images.insert((*icon, *size), decode(&path, &bytes, *size)?);
        }
    }
    Ok(LoadedPack {
        manifest,
        images,
        textures: Mutex::new(HashMap::new()),
    })
}

/// Decode a PNG that must be exactly `size` pixels square.
fn decode(path: &str, bytes: &[u8], size: u32) -> Result<egui::ColorImage, PackError> {
    let fail = |what: &str| PackError::new(format!("The file \"{path}\" {what}."));
    let reader = || image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let (width, height) = reader()
        .into_dimensions()
        .map_err(|_| fail("is not a PNG image"))?;
    if width != size || height != size {
        return Err(PackError::new(format!(
            "The file \"{path}\" is {width} by {height} pixels. It must be {size} by {size}."
        )));
    }
    let mut limits = image::Limits::no_limits();
    limits.max_image_width = Some(size);
    limits.max_image_height = Some(size);
    limits.max_alloc = Some(1 << 20);
    let mut decoder = reader();
    decoder.limits(limits);
    let decoded = decoder
        .decode()
        .map_err(|_| fail("could not be decoded"))?
        .to_rgba8();
    let edge = size as usize;
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [edge, edge],
        decoded.as_raw(),
    ))
}
