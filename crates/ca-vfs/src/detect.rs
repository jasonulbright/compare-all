//! Recognising containers by their content and by their name.
//!
//! Content wins over the name: a file is opened as the format its bytes say it
//! is, and the extension only decides matters the bytes cannot, such as
//! whether a user has switched a format off or which compressed single stream
//! is meant to hold a tar.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use crate::error::VfsResult;

/// A container format this crate knows the name of.
///
/// Listing a format here does not mean it can be opened; ask
/// [`ArchiveFormat::is_supported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArchiveFormat {
    /// Zip and the formats that are zip containers under another name.
    Zip,
    /// 7z.
    SevenZip,
    /// An uncompressed tar.
    Tar,
    /// A tar inside a gzip stream.
    TarGz,
    /// A tar inside a bzip2 stream.
    TarBz2,
    /// A tar inside an xz stream.
    TarXz,
    /// A single file in a gzip stream.
    Gz,
    /// A single file in a bzip2 stream.
    Bz2,
    /// A single file in an xz stream.
    Xz,
    /// A snapshot written by this crate.
    Snapshot,
    /// Microsoft cabinet.
    Cab,
    /// RAR.
    Rar,
    /// A raw disc image.
    DiskImage,
    /// Compiled HTML help.
    Chm,
    /// Debian package.
    Deb,
    /// Red Hat package.
    Rpm,
    /// Windows imaging format.
    Wim,
}

impl ArchiveFormat {
    /// Every format the type can name.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::SevenZip,
            Self::Snapshot,
            Self::Bz2,
            Self::TarBz2,
            Self::Chm,
            Self::Deb,
            Self::DiskImage,
            Self::Gz,
            Self::TarGz,
            Self::Cab,
            Self::Rar,
            Self::Rpm,
            Self::Tar,
            Self::Wim,
            Self::Xz,
            Self::TarXz,
            Self::Zip,
        ]
    }

    /// Whether this build can open the format.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(
            self,
            Self::Zip
                | Self::SevenZip
                | Self::Tar
                | Self::TarGz
                | Self::TarBz2
                | Self::TarXz
                | Self::Gz
                | Self::Bz2
                | Self::Xz
                | Self::Snapshot
                | Self::Cab
                | Self::Rar
                | Self::DiskImage
                | Self::Deb
                | Self::Rpm
        )
    }

    /// Name shown in an archive types list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Zip => "Zip",
            Self::SevenZip => "7-zip",
            Self::Tar => "Tar",
            Self::TarGz => "GZipped Tar",
            Self::TarBz2 => "BZipped Tar",
            Self::TarXz => "XZipped Tar",
            Self::Gz => "GZip",
            Self::Bz2 => "BZip",
            Self::Xz => "Xz",
            Self::Snapshot => "Snapshot",
            Self::Cab => "Microsoft Cabinet",
            Self::Rar => "RAR",
            Self::DiskImage => "Disk Image",
            Self::Chm => "Compiled HTML Help",
            Self::Deb => "Debian Package",
            Self::Rpm => "Red Hat Package",
            Self::Wim => "Windows Imaging Format",
        }
    }

    /// The key a stored configuration names the format by. It never changes
    /// once written, whatever the label becomes.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::SevenZip => "7z",
            Self::Tar => "tar",
            Self::TarGz => "tgz",
            Self::TarBz2 => "tbz",
            Self::TarXz => "txz",
            Self::Gz => "gz",
            Self::Bz2 => "bz2",
            Self::Xz => "xz",
            Self::Snapshot => "snapshot",
            Self::Cab => "cab",
            Self::Rar => "rar",
            Self::DiskImage => "img",
            Self::Chm => "chm",
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::Wim => "wim",
        }
    }

    /// The format a stored key names.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::all().iter().copied().find(|format| format.id() == id)
    }

    /// The default masks as the options page shows them.
    #[must_use]
    pub fn default_mask_text(self) -> String {
        self.default_mask().join(";")
    }

    /// The masks a fresh configuration associates with the format.
    #[must_use]
    pub const fn default_mask(self) -> &'static [&'static str] {
        match self {
            Self::Zip => &["*.zip", "*.zipx", "*.jar", "*.ear", "*.war", "*.bcpkg"],
            Self::SevenZip => &["*.7z", "*.7z.001"],
            Self::Tar => &["*.tar"],
            Self::TarGz => &["*.tgz", "*.tar.gz"],
            Self::TarBz2 => &["*.tbz", "*.tbz2", "*.tar.bz2"],
            Self::TarXz => &["*.txz", "*.tar.xz"],
            Self::Gz => &["*.gz"],
            Self::Bz2 => &["*.bz", "*.bz2"],
            Self::Xz => &["*.xz"],
            Self::Snapshot => &["*.cass", "*.casn"],
            Self::Cab => &["*.cab"],
            Self::Rar => &["*.rar"],
            Self::DiskImage => &["*.img", "*.iso"],
            Self::Chm => &["*.chm"],
            Self::Deb => &["*.deb"],
            Self::Rpm => &["*.rpm"],
            Self::Wim => &["*.wim", "*.swm"],
        }
    }
}

/// How a container should behave in a folder listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArchiveHandling {
    /// Always an ordinary file: it sorts with files and is never expanded.
    AsFiles,
    /// Sorts and compares as a file until it is opened, then behaves as a
    /// folder.
    #[default]
    AsFoldersOnceOpened,
    /// A folder from the start.
    AsFoldersAlways,
}

/// Which extensions map to which format, and how containers are handled.
///
/// Clearing a format's masks drops support for it: a file with that extension
/// is then an ordinary file even though the bytes would be recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveTypes {
    masks: BTreeMap<ArchiveFormat, Vec<String>>,
    handling: ArchiveHandling,
}

impl Default for ArchiveTypes {
    fn default() -> Self {
        let masks = ArchiveFormat::all()
            .iter()
            .map(|format| {
                let masks = format
                    .default_mask()
                    .iter()
                    .map(|mask| (*mask).to_owned())
                    .collect();
                (*format, masks)
            })
            .collect();
        Self {
            masks,
            handling: ArchiveHandling::default(),
        }
    }
}

impl ArchiveTypes {
    /// The masks currently associated with `format`.
    #[must_use]
    pub fn mask(&self, format: ArchiveFormat) -> &[String] {
        self.masks.get(&format).map_or(&[], Vec::as_slice)
    }

    /// Replace the masks for `format`. An empty list drops the format.
    pub fn set_mask<I, S>(&mut self, format: ArchiveFormat, masks: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let masks: Vec<String> = masks.into_iter().map(Into::into).collect();
        self.masks.insert(format, masks);
    }

    /// Apply masks stored as text, keyed by [`ArchiveFormat::id`].
    ///
    /// Each text holds masks separated by semicolons; a blank text drops the
    /// format. A key no format answers to is ignored, so a configuration
    /// written by a newer build still applies what this build knows.
    pub fn apply_stored<'a, I>(&mut self, stored: I)
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        for (id, text) in stored {
            if let Some(format) = ArchiveFormat::from_id(id) {
                self.set_mask(format, split_masks(text));
            }
        }
    }

    /// True when the format has at least one mask.
    #[must_use]
    pub fn is_enabled(&self, format: ArchiveFormat) -> bool {
        !self.mask(format).is_empty()
    }

    /// How containers behave in a listing.
    #[must_use]
    pub fn handling(&self) -> ArchiveHandling {
        self.handling
    }

    /// Set how containers behave in a listing.
    pub fn set_handling(&mut self, handling: ArchiveHandling) {
        self.handling = handling;
    }

    /// The format whose mask matches `name`, if any.
    ///
    /// Longer masks win, so `*.tar.gz` is preferred over `*.gz`.
    #[must_use]
    pub fn format_for_name(&self, name: &str) -> Option<ArchiveFormat> {
        let lower = name.to_ascii_lowercase();
        let mut best: Option<(usize, ArchiveFormat)> = None;
        for (format, masks) in &self.masks {
            for mask in masks {
                let mask = mask.to_ascii_lowercase();
                let matched = match mask.strip_prefix('*') {
                    Some(suffix) => lower.ends_with(suffix),
                    None => lower == mask,
                };
                if matched && best.is_none_or(|(len, _)| mask.len() > len) {
                    best = Some((mask.len(), *format));
                }
            }
        }
        best.map(|(_, format)| format)
    }

    /// Whether a listing should show `name` as a folder before it is opened.
    #[must_use]
    pub fn expands_by_default(&self, name: &str) -> bool {
        self.handling == ArchiveHandling::AsFoldersAlways
            && self
                .format_for_name(name)
                .is_some_and(ArchiveFormat::is_supported)
    }
}

/// Masks separated by semicolons, trimmed, with blanks dropped.
#[must_use]
pub fn split_masks(text: &str) -> Vec<String> {
    text.split(';')
        .map(str::trim)
        .filter(|mask| !mask.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The magic bytes at the start of a container, where a format has them.
///
/// `None` means the bytes name no format this crate knows. A tar has no magic
/// at offset zero, so it is recognised by the field at offset 257.
#[must_use]
pub fn format_from_magic(head: &[u8]) -> Option<ArchiveFormat> {
    const TAR_MAGIC_OFFSET: usize = 257;

    if head.starts_with(crate::snapshot::MAGIC) {
        return Some(ArchiveFormat::Snapshot);
    }
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        return Some(ArchiveFormat::Zip);
    }
    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(ArchiveFormat::SevenZip);
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        return Some(ArchiveFormat::Gz);
    }
    if head.starts_with(b"BZh") {
        return Some(ArchiveFormat::Bz2);
    }
    if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(ArchiveFormat::Xz);
    }
    if head.starts_with(b"Rar!\x1a\x07") {
        return Some(ArchiveFormat::Rar);
    }
    if head.starts_with(b"MSCF") {
        return Some(ArchiveFormat::Cab);
    }
    if head.starts_with(b"ITSF") {
        return Some(ArchiveFormat::Chm);
    }
    if head.starts_with(b"!<arch>") {
        return Some(ArchiveFormat::Deb);
    }
    if head.starts_with(&[0xed, 0xab, 0xee, 0xdb]) {
        return Some(ArchiveFormat::Rpm);
    }
    if head.starts_with(b"MSWIM\x00\x00\x00") {
        return Some(ArchiveFormat::Wim);
    }
    if head
        .get(TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + 5)
        .is_some_and(|field| field == b"ustar")
    {
        return Some(ArchiveFormat::Tar);
    }
    None
}

/// Number of leading bytes [`format_from_magic`] can use.
pub const MAGIC_WINDOW: usize = 512;

/// Decide the format of an open container.
///
/// The magic bytes decide first. A single compressed stream is then probed: if
/// what it decompresses to begins with a tar header the container is the
/// matching tar variant, so a `.tgz` renamed to `.gz` still opens as folders.
/// Only when the bytes name nothing does `name` decide, through `types`.
///
/// # Errors
/// Returns [`crate::VfsError::Io`] when the container cannot be read or
/// rewound.
pub fn detect<R: Read + Seek>(
    reader: &mut R,
    name: &str,
    types: &ArchiveTypes,
) -> VfsResult<Option<ArchiveFormat>> {
    let mut head = vec![0u8; MAGIC_WINDOW];
    reader.seek(SeekFrom::Start(0))?;
    let read = read_up_to(reader, &mut head)?;
    head.truncate(read);
    reader.seek(SeekFrom::Start(0))?;

    let by_magic = format_from_magic(&head);
    let format = match by_magic {
        Some(format) => Some(refine_stream(format, reader, name)?),
        None => {
            // A disc image carries its magic 32 KiB in, past the window, and
            // a name ending in ".img" says nothing about what the bytes hold.
            if crate::archive::iso_format::is_iso(reader)? {
                Some(ArchiveFormat::DiskImage)
            } else {
                types
                    .format_for_name(name)
                    .filter(|format| *format != ArchiveFormat::DiskImage)
            }
        }
    };

    Ok(format.filter(|format| types.is_enabled(*format)))
}

/// Promote a single compressed stream to its tar variant when it holds a tar.
fn refine_stream<R: Read + Seek>(
    format: ArchiveFormat,
    reader: &mut R,
    name: &str,
) -> VfsResult<ArchiveFormat> {
    const TARRED_SUFFIXES: [&str; 4] = [".tgz", ".tbz", ".tbz2", ".txz"];

    let (plain, tarred) = match format {
        ArchiveFormat::Gz => (ArchiveFormat::Gz, ArchiveFormat::TarGz),
        ArchiveFormat::Bz2 => (ArchiveFormat::Bz2, ArchiveFormat::TarBz2),
        ArchiveFormat::Xz => (ArchiveFormat::Xz, ArchiveFormat::TarXz),
        other => return Ok(other),
    };

    reader.seek(SeekFrom::Start(0))?;
    let mut probe = vec![0u8; MAGIC_WINDOW];
    let read = {
        let mut decoder = crate::archive::single::decoder(plain, &mut *reader)?;
        read_up_to(&mut decoder, &mut probe).unwrap_or(0)
    };
    reader.seek(SeekFrom::Start(0))?;
    probe.truncate(read);

    if format_from_magic(&probe) == Some(ArchiveFormat::Tar) {
        return Ok(tarred);
    }
    // A short tar member can leave the probe empty; fall back to the name.
    let lower = name.to_ascii_lowercase();
    if TARRED_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(*suffix))
    {
        return Ok(tarred);
    }
    Ok(plain)
}

/// Fill as much of `buf` as the reader has, treating a short read as the end.
fn read_up_to<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let slice = buf.get_mut(filled..).unwrap_or_default();
        match reader.read(slice) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn longest_mask_wins() {
        let types = ArchiveTypes::default();
        assert_eq!(
            types.format_for_name("a.tar.gz"),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(types.format_for_name("a.gz"), Some(ArchiveFormat::Gz));
        assert_eq!(types.format_for_name("a.txt"), None);
    }

    #[test]
    fn clearing_a_mask_drops_the_format() {
        let mut types = ArchiveTypes::default();
        types.set_mask(ArchiveFormat::Zip, Vec::<String>::new());
        assert!(!types.is_enabled(ArchiveFormat::Zip));
        assert_eq!(types.format_for_name("a.zip"), None);
    }

    #[test]
    fn handling_controls_default_expansion() {
        let mut types = ArchiveTypes::default();
        assert!(!types.expands_by_default("a.zip"));
        types.set_handling(ArchiveHandling::AsFoldersAlways);
        assert!(types.expands_by_default("a.zip"));
        assert!(!types.expands_by_default("a.chm"));
    }

    #[test]
    fn stored_masks_apply_by_key_and_a_blank_drops_the_format() {
        let mut types = ArchiveTypes::default();
        types.apply_stored([("cab", " *.cab ; *.msu "), ("rar", ""), ("future", "*.new")]);
        assert_eq!(types.mask(ArchiveFormat::Cab), ["*.cab", "*.msu"]);
        assert_eq!(
            types.format_for_name("update.msu"),
            Some(ArchiveFormat::Cab)
        );
        assert!(!types.is_enabled(ArchiveFormat::Rar));
        assert!(types.is_enabled(ArchiveFormat::Zip));
    }

    #[test]
    fn every_format_round_trips_its_key() {
        for format in ArchiveFormat::all() {
            assert_eq!(ArchiveFormat::from_id(format.id()), Some(*format));
        }
    }

    #[test]
    fn magic_beats_extension() {
        let mut bytes = std::io::Cursor::new(b"PK\x03\x04rest".to_vec());
        let format = detect(&mut bytes, "not-an-archive.txt", &ArchiveTypes::default()).unwrap();
        assert_eq!(format, Some(ArchiveFormat::Zip));
    }
}
