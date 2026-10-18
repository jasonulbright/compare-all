//! Checks on a downloaded release file.
//!
//! A response with status 200 proves nothing. A file passes only when its size
//! equals the size in the manifest, it has the format of its kind, its
//! SHA-256 equals the manifest value, and its Authenticode signer is the
//! release signer. For the portable archive the signer checks apply to every
//! program file inside.

use crate::signature::{verify_signer, SignatureVerifier};
use crate::staging::AssetKind;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

const MSI_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const ZIP_MAGIC: [u8; 4] = *b"PK\x03\x04";
/// Largest unpacked size of a portable archive.
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
/// Most entries a portable archive may hold.
const MAX_ENTRIES: usize = 64;

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
            for path in files.iter().filter(|path| is_program_file(path)) {
                verify_signer(signer, path, expected.signer)?;
            }
            Ok(())
        }
    }
}

fn is_program_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("dll"))
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

/// Unpack a portable archive into `destination`. Only plain file names are
/// accepted; a folder, a path or an unexpected number of entries fails the
/// whole archive.
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
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("the archive is not valid: {error}"))?;
        let name = entry.name().to_owned();
        if entry.is_dir() || !crate::manifest::is_plain_file_name(&name) {
            return Err(format!("the archive entry {name:?} is not a plain file"));
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err("the archive unpacks to more than allowed".to_owned());
        }
        let target = destination.join(&name);
        let mut out =
            std::fs::File::create(&target).map_err(|error| format!("cannot unpack: {error}"))?;
        let copied = std::io::copy(&mut (&mut entry).take(MAX_UNPACKED_BYTES), &mut out)
            .map_err(|error| format!("cannot unpack: {error}"))?;
        if copied != entry.size() {
            return Err(format!("the archive entry {name:?} has the wrong size"));
        }
        written.push(target);
    }
    if !written.iter().any(|path| {
        path.file_name()
            .is_some_and(|name| name == "compare-all.exe")
    }) {
        return Err("the archive holds no compare-all.exe".to_owned());
    }
    Ok(written)
}
