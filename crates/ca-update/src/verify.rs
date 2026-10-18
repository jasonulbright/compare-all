//! Checks on a downloaded release file.
//!
//! A response with status 200 proves nothing. A file passes only when its size
//! equals the size in the manifest, it has the format of its kind, its
//! SHA-256 equals the manifest value, and its Authenticode signer is the
//! release signer.
//!
//! The manifest and the SHA-256 come from the same host as the file, so the
//! signature is the only check an attacker on that path cannot satisfy. The
//! portable archive is therefore held to a fixed file set compiled into the
//! program: every program file in it is signer-checked, no other file may be
//! a program, and every signed program must carry the version the release
//! claims inside its signed sections, so an older signed build cannot be
//! staged under a newer version.

use crate::signature::{verify_signer, SignatureVerifier};
use crate::staging::AssetKind;
use crate::version::Version;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

const MSI_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const ZIP_MAGIC: [u8; 4] = *b"PK\x03\x04";
/// Largest unpacked size of a portable archive.
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
/// Most entries a portable archive may hold.
const MAX_ENTRIES: usize = 64;

/// Every file the portable archive may hold, in install order. Nothing else
/// is unpacked and nothing else is written into the program folder.
pub const PORTABLE_FILES: [&str; 6] = [
    "CHANGELOG.md",
    "LICENSE",
    "README.md",
    "THIRD-PARTY-NOTICES.md",
    "ca.exe",
    "compare-all.exe",
];

/// The program files of the portable archive. Each must be present, signed,
/// and carry the release version.
pub const PORTABLE_PROGRAMS: [&str; 2] = ["ca.exe", "compare-all.exe"];

/// Text that precedes the build version inside the program file. The version
/// ends at a NUL byte.
pub const VERSION_MARKER: &[u8] = b"CA-BUILD-VERSION=";

/// What the manifest promises about a file.
#[derive(Debug, Clone)]
pub struct Expected<'a> {
    /// Kind of file.
    pub kind: AssetKind,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256, lowercase hexadecimal.
    pub sha256: &'a str,
    /// Common name of the required signer.
    pub signer: &'a str,
    /// The release version the file belongs to.
    pub version: &'a Version,
}

/// The build version embedded in a program file.
///
/// Only the raw data of the PE sections is searched. The Authenticode hash
/// does not cover the attribute certificate table, and the system accepts
/// extra bytes inside it, so a marker there could be added to any old signed
/// build without breaking its signature.
///
/// # Errors
/// Returns why the file carries no single version.
pub fn embedded_version(bytes: &[u8]) -> Result<Version, String> {
    let mut found: Option<Version> = None;
    for section in pe_sections(bytes)? {
        markers_in(section, &mut found)?;
    }
    found.ok_or_else(|| "the program names no version".to_owned())
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *bytes.get(at)?,
        *bytes.get(at + 1)?,
        *bytes.get(at + 2)?,
        *bytes.get(at + 3)?,
    ]))
}

fn usize_at(bytes: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u32_at(bytes, at)?).ok()
}

/// The raw data of every section of a PE file. A file whose certificate table
/// does not end the file, or whose sections reach into that table or past the
/// end of the file, is refused.
fn pe_sections(bytes: &[u8]) -> Result<Vec<&[u8]>, String> {
    let bad = || "the program is not a valid Windows program file".to_owned();
    if bytes.get(..2) != Some(b"MZ".as_slice()) {
        return Err(bad());
    }
    let header = usize_at(bytes, 0x3C).ok_or_else(bad)?;
    if bytes.get(header..header.checked_add(4).ok_or_else(bad)?) != Some(b"PE\0\0".as_slice()) {
        return Err(bad());
    }
    let coff = header + 4;
    let count = usize::from(u16_at(bytes, coff + 2).ok_or_else(bad)?);
    let optional_size = usize::from(u16_at(bytes, coff + 16).ok_or_else(bad)?);
    let optional = coff + 20;
    let (directories, directory_count) = match u16_at(bytes, optional).ok_or_else(bad)? {
        0x10B => (
            optional + 96,
            usize_at(bytes, optional + 92).ok_or_else(bad)?,
        ),
        0x20B => (
            optional + 112,
            usize_at(bytes, optional + 108).ok_or_else(bad)?,
        ),
        _ => return Err(bad()),
    };
    let certificates_at = if directory_count > 4 {
        let security = directories + 4 * 8;
        let start = usize_at(bytes, security).ok_or_else(bad)?;
        let size = usize_at(bytes, security + 4).ok_or_else(bad)?;
        if size == 0 {
            bytes.len()
        } else if start.checked_add(size) == Some(bytes.len()) {
            start
        } else {
            return Err("the program has data after its signature".to_owned());
        }
    } else {
        bytes.len()
    };
    let table = optional + optional_size;
    let mut sections = Vec::with_capacity(count);
    for index in 0..count {
        let entry = table + index * 40;
        let size = usize_at(bytes, entry + 16).ok_or_else(bad)?;
        let start = usize_at(bytes, entry + 20).ok_or_else(bad)?;
        if size == 0 {
            continue;
        }
        let end = start.checked_add(size).ok_or_else(bad)?;
        if end > certificates_at {
            return Err(bad());
        }
        sections.push(bytes.get(start..end).ok_or_else(bad)?);
    }
    Ok(sections)
}

/// Add each well-formed marker in `bytes` to `found`.
fn markers_in(bytes: &[u8], found: &mut Option<Version>) -> Result<(), String> {
    let mut rest = bytes;
    while let Some(at) = find(rest, VERSION_MARKER) {
        let after = &rest[at + VERSION_MARKER.len()..];
        rest = after;
        let digits: Vec<u8> = after
            .iter()
            .take(32)
            .take_while(|byte| byte.is_ascii_digit() || **byte == b'.')
            .copied()
            .collect();
        if after.get(digits.len()) != Some(&0) {
            continue;
        }
        let Some(version) = std::str::from_utf8(&digits).ok().and_then(Version::parse) else {
            continue;
        };
        match found {
            Some(earlier) if *earlier != version => {
                return Err("the program names more than one version".to_owned());
            }
            _ => *found = Some(version),
        }
    }
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// SHA-256 of a file as lowercase hexadecimal.
///
/// # Errors
/// Returns why the file could not be read.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Lowercase hexadecimal of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// One unpacked release file and the SHA-256 of the bytes written to it. The
/// file stays open until this value is dropped; on Windows that handle shares
/// reading only, so no other process can change, rename or delete the file,
/// or rename its folder, between its check and its copy.
#[derive(Debug)]
struct HeldFile {
    name: String,
    file: std::fs::File,
    sha256: Vec<u8>,
}

/// The checked files of a portable archive, held open from their check to
/// their copy into the program folder.
#[derive(Debug, Default)]
pub struct Unpacked {
    files: Vec<HeldFile>,
}

impl Unpacked {
    /// Hold every file of [`PORTABLE_FILES`] that exists in `folder`.
    ///
    /// # Errors
    /// Returns why a file could not be opened or read.
    pub fn hold_folder(folder: &Path) -> Result<Self, String> {
        let mut files = Vec::new();
        for name in PORTABLE_FILES {
            let path = folder.join(name);
            if !path.is_file() {
                continue;
            }
            let file = hold(&path).map_err(|error| format!("cannot open {name}: {error}"))?;
            let sha256 = digest_from_start(&file, &mut std::io::sink())?;
            files.push(HeldFile {
                name: name.to_owned(),
                file,
                sha256,
            });
        }
        Ok(Self { files })
    }

    /// The names of the held files, in install order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        PORTABLE_FILES
            .iter()
            .copied()
            .filter(|name| self.files.iter().any(|held| held.name == *name))
    }

    /// Copy the held file `name` to `destination`, a new file. The bytes
    /// copied must be the bytes that were checked.
    ///
    /// # Errors
    /// Returns why the copy failed or the bytes differ.
    pub fn copy_to(&self, name: &str, destination: &Path) -> Result<(), String> {
        let held = self
            .files
            .iter()
            .find(|held| held.name == name)
            .ok_or_else(|| format!("{name} was not checked"))?;
        let mut out = std::fs::File::create(destination)
            .map_err(|error| format!("cannot copy {name}: {error}"))?;
        let copied = digest_from_start(&held.file, &mut out)?;
        out.sync_all()
            .map_err(|error| format!("cannot copy {name}: {error}"))?;
        if copied != held.sha256 {
            return Err(format!("{name} changed after its check"));
        }
        Ok(())
    }

    fn bytes_of(&self, name: &str) -> Result<Vec<u8>, String> {
        let held = self
            .files
            .iter()
            .find(|held| held.name == name)
            .ok_or_else(|| format!("the archive holds no {name}"))?;
        let mut bytes = Vec::new();
        let digest = digest_from_start(&held.file, &mut bytes)?;
        if digest != held.sha256 {
            return Err(format!("{name} changed after it was unpacked"));
        }
        Ok(bytes)
    }
}

/// Open `path` for reading so that, on Windows, other processes may only read
/// it while the handle lives.
fn hold(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path)
}

/// Read `file` from its start into `out` and return the SHA-256 of the bytes.
fn digest_from_start(
    file: &std::fs::File,
    out: &mut dyn std::io::Write,
) -> Result<Vec<u8>, String> {
    use std::io::Seek;
    let mut reader = file;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])
            .map_err(|error| error.to_string())?;
    }
    Ok(hasher.finalize().to_vec())
}

/// Check `file` against `expected`. `scratch` is an empty folder the check may
/// fill; the portable archive is unpacked there to check its program files.
///
/// # Errors
/// Returns the first check the file fails.
pub fn verify_file(
    file: &Path,
    expected: &Expected<'_>,
    signer: &dyn SignatureVerifier,
    scratch: &Path,
) -> Result<(), String> {
    verify_and_hold(file, expected, signer, scratch).map(drop)
}

/// [`verify_file`], keeping the unpacked files of a portable archive held
/// open for the copy that follows. An installer gives `None`.
///
/// # Errors
/// Returns the first check the file fails.
pub fn verify_and_hold(
    file: &Path,
    expected: &Expected<'_>,
    signer: &dyn SignatureVerifier,
    scratch: &Path,
) -> Result<Option<Unpacked>, String> {
    let size = std::fs::metadata(file)
        .map_err(|error| format!("cannot read the download: {error}"))?
        .len();
    if size != expected.size {
        return Err(format!(
            "the download has {size} bytes, the release lists {}",
            expected.size
        ));
    }
    check_format(file, expected.kind)?;
    let actual = sha256_file(file)?;
    if actual != expected.sha256 {
        return Err("the SHA-256 of the download does not match the release".to_owned());
    }
    match expected.kind {
        AssetKind::Msi => verify_signer(signer, file, expected.signer).map(|()| None),
        AssetKind::Zip => {
            let unpacked = unpack_release_zip(file, scratch)?;
            for held in &unpacked.files {
                let name = held.name.as_str();
                if PORTABLE_PROGRAMS.contains(&name) {
                    verify_signer(signer, &scratch.join(name), expected.signer)?;
                } else if unpacked.bytes_of(name)?.starts_with(b"MZ") {
                    return Err(format!("the archive file {name} is a program"));
                }
            }
            for program in PORTABLE_PROGRAMS {
                let bytes = unpacked.bytes_of(program)?;
                let inside =
                    embedded_version(&bytes).map_err(|error| format!("{program}: {error}"))?;
                if inside != *expected.version {
                    return Err(format!(
                        "the archive holds {program} {inside}, the release is {}",
                        expected.version
                    ));
                }
            }
            Ok(Some(unpacked))
        }
    }
}

/// A writer that keeps the SHA-256 of what passes through it.
struct Hashing<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: std::io::Write> std::io::Write for Hashing<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn check_format(file: &Path, kind: AssetKind) -> Result<(), String> {
    let mut head = [0u8; 8];
    let mut handle = std::fs::File::open(file).map_err(|error| error.to_string())?;
    let read = handle.read(&mut head).map_err(|error| error.to_string())?;
    let head = &head[..read];
    let good = match kind {
        AssetKind::Msi => head.starts_with(&MSI_MAGIC),
        AssetKind::Zip => head.starts_with(&ZIP_MAGIC),
    };
    if good {
        return Ok(());
    }
    let looks_like_page = head
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'<');
    Err(if looks_like_page {
        "the download is a web page, not the release file".to_owned()
    } else {
        "the download is not the release file".to_owned()
    })
}

/// Unpack a portable archive into `destination`. Only the names of
/// [`PORTABLE_FILES`] are accepted, each once; a folder, a path, another name,
/// a name repeated in any letter case, a missing program or an unexpected
/// number of entries fails the whole archive. Each file is held open from
/// the moment it is written.
///
/// # Errors
/// Returns why the archive is not a release archive.
pub fn unpack_release_zip(zip_path: &Path, destination: &Path) -> Result<Unpacked, String> {
    let file = std::fs::File::open(zip_path).map_err(|error| error.to_string())?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("the archive is not valid: {error}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err("the archive holds too many entries".to_owned());
    }
    std::fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    let mut unpacked = Unpacked::default();
    let mut seen: Vec<String> = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("the archive is not valid: {error}"))?;
        let name = entry.name().to_owned();
        if entry.is_dir() || !crate::manifest::is_plain_file_name(&name) {
            return Err(format!("the archive entry {name:?} is not a plain file"));
        }
        if !PORTABLE_FILES.contains(&name.as_str()) {
            return Err(format!("the archive entry {name:?} is not a release file"));
        }
        let folded = name.to_ascii_lowercase();
        if seen.contains(&folded) {
            return Err(format!("the archive holds {name:?} twice"));
        }
        seen.push(folded);
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err("the archive unpacks to more than allowed".to_owned());
        }
        let target = destination.join(&name);
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("cannot unpack: {error}"))?;
        let mut out = Hashing {
            inner: out,
            hasher: Sha256::new(),
        };
        let copied = std::io::copy(&mut (&mut entry).take(MAX_UNPACKED_BYTES), &mut out)
            .map_err(|error| format!("cannot unpack: {error}"))?;
        if copied != entry.size() {
            return Err(format!("the archive entry {name:?} has the wrong size"));
        }
        let written = out.hasher.finalize().to_vec();
        drop(out.inner);
        let file = hold(&target).map_err(|error| format!("cannot unpack: {error}"))?;
        // The file was closed for a moment between the write and the hold.
        if digest_from_start(&file, &mut std::io::sink())? != written {
            return Err(format!("{name} changed while it was unpacked"));
        }
        unpacked.files.push(HeldFile {
            name,
            file,
            sha256: written,
        });
    }
    for program in PORTABLE_PROGRAMS {
        if !seen.iter().any(|name| name == program) {
            return Err(format!("the archive holds no {program}"));
        }
    }
    Ok(unpacked)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[path = "../../../tests/support/pe.rs"]
    mod fixture;
    use fixture::pe;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn the_embedded_version_is_read_and_must_be_single() {
        assert_eq!(
            embedded_version(&pe(&[b"..CA-BUILD-VERSION=2026.10.18.0007\0.."], b"", None)).unwrap(),
            version("2026.10.18.0007")
        );
        assert_eq!(
            embedded_version(&pe(
                &[
                    b"CA-BUILD-VERSION=\x01CA-BUILD-VERSION=2026.10.18.0007\0",
                    b"CA-BUILD-VERSION=2026.10.18.0007\0"
                ],
                b"",
                Some(b"signature")
            ))
            .unwrap(),
            version("2026.10.18.0007")
        );
        assert!(embedded_version(&pe(&[b"no marker"], b"", None)).is_err());
        assert!(embedded_version(&pe(&[b"CA-BUILD-VERSION=2026.10.18.0007x"], b"", None)).is_err());
        assert!(embedded_version(&pe(
            &[b"CA-BUILD-VERSION=2026.10.18.0007\0CA-BUILD-VERSION=2026.01.01.0001\0"],
            b"",
            None
        ))
        .is_err());
        assert!(embedded_version(b"CA-BUILD-VERSION=2026.10.18.0007\0").is_err());
    }

    #[test]
    fn a_version_outside_the_signed_sections_is_not_read() {
        let tag = b"CA-BUILD-VERSION=2099.01.01.0001\0";
        // A signed build without a marker, with one added to the certificate
        // table, which the Authenticode hash does not cover.
        assert!(embedded_version(&pe(&[b"signed code"], b"", Some(tag))).is_err());
        // A marker of its own wins over one in the certificate table.
        assert_eq!(
            embedded_version(&pe(
                &[b"CA-BUILD-VERSION=2026.10.02.0001\0"],
                b"",
                Some(tag)
            ))
            .unwrap(),
            version("2026.10.02.0001")
        );
        // Bytes after the last section belong to no section.
        assert!(embedded_version(&pe(&[b"signed code"], tag, None)).is_err());
        assert!(embedded_version(&pe(&[b"signed code"], tag, Some(b"signature"))).is_err());
        // A certificate table that does not end the file.
        let mut trailing = pe(
            &[b"CA-BUILD-VERSION=2026.10.02.0001\0"],
            b"",
            Some(b"signature"),
        );
        trailing.extend_from_slice(tag);
        assert!(embedded_version(&trailing).is_err());
    }
}
