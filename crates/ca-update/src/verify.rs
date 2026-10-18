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
//! a program, and the signed `compare-all.exe` must carry the version the
//! release claims, so an older signed build cannot be staged under a newer
//! version.

use crate::signature::{verify_signer, SignatureVerifier};
use crate::staging::AssetKind;
use crate::version::Version;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

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

/// The program files of the portable archive. Each must be present and signed.
pub const PORTABLE_PROGRAMS: [&str; 2] = ["ca.exe", "compare-all.exe"];

/// The program whose embedded version must equal the release version.
pub const MAIN_PROGRAM: &str = "compare-all.exe";

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
/// # Errors
/// Returns why the file carries no single version.
pub fn embedded_version(bytes: &[u8]) -> Result<Version, String> {
    let mut found: Option<Version> = None;
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
            Some(earlier) if earlier != version => {
                return Err("the program names more than one version".to_owned());
            }
            _ => found = Some(version),
        }
    }
    found.ok_or_else(|| "the program names no version".to_owned())
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
        AssetKind::Msi => verify_signer(signer, file, expected.signer),
        AssetKind::Zip => {
            let files = extract_release_zip(file, scratch)?;
            for path in &files {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if PORTABLE_PROGRAMS.contains(&name.as_str()) {
                    verify_signer(signer, path, expected.signer)?;
                } else if is_program_image(path)? {
                    return Err(format!("the archive file {name} is a program"));
                }
            }
            let main = std::fs::read(scratch.join(MAIN_PROGRAM))
                .map_err(|error| format!("cannot read {MAIN_PROGRAM}: {error}"))?;
            let inside = embedded_version(&main)?;
            if inside != *expected.version {
                return Err(format!(
                    "the archive holds version {inside}, the release is {}",
                    expected.version
                ));
            }
            Ok(())
        }
    }
}

/// True when the file starts like a Windows program or library.
fn is_program_image(path: &Path) -> Result<bool, String> {
    let mut head = [0u8; 2];
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let read = file.read(&mut head).map_err(|error| error.to_string())?;
    Ok(read == 2 && head == *b"MZ")
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
/// number of entries fails the whole archive.
///
/// # Errors
/// Returns why the archive is not a release archive.
pub fn extract_release_zip(zip_path: &Path, destination: &Path) -> Result<Vec<PathBuf>, String> {
    let file = std::fs::File::open(zip_path).map_err(|error| error.to_string())?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("the archive is not valid: {error}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err("the archive holds too many entries".to_owned());
    }
    std::fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    let mut written = Vec::new();
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
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("cannot unpack: {error}"))?;
        let copied = std::io::copy(&mut (&mut entry).take(MAX_UNPACKED_BYTES), &mut out)
            .map_err(|error| format!("cannot unpack: {error}"))?;
        if copied != entry.size() {
            return Err(format!("the archive entry {name:?} has the wrong size"));
        }
        written.push(target);
    }
    for program in PORTABLE_PROGRAMS {
        if !seen.iter().any(|name| name == program) {
            return Err(format!("the archive holds no {program}"));
        }
    }
    Ok(written)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_version_is_read_and_must_be_single() {
        let version = |text: &str| Version::parse(text).unwrap();
        assert_eq!(
            embedded_version(b"MZ..CA-BUILD-VERSION=2026.10.18.0007\0..").unwrap(),
            version("2026.10.18.0007")
        );
        assert_eq!(
            embedded_version(
                b"CA-BUILD-VERSION=\x01CA-BUILD-VERSION=2026.10.18.0007\0CA-BUILD-VERSION=2026.10.18.0007\0"
            )
            .unwrap(),
            version("2026.10.18.0007")
        );
        assert!(embedded_version(b"MZ no marker").is_err());
        assert!(embedded_version(b"CA-BUILD-VERSION=2026.10.18.0007").is_err());
        assert!(embedded_version(
            b"CA-BUILD-VERSION=2026.10.18.0007\0CA-BUILD-VERSION=2026.01.01.0001\0"
        )
        .is_err());
    }
}
