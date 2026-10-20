//! The pack list a release publishes and the rules every entry and every zip
//! entry name must pass.
//!
//! This file uses only `serde` and the standard library, so the release tool
//! compiles the same source and a published list is judged by the code that
//! the program runs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The only list format this build reads.
pub const FORMAT: u32 = 1;
/// File name of the list among the release files.
pub const INDEX_NAME: &str = "packs-index.json";
/// Largest list accepted, in bytes.
pub const MAX_INDEX_BYTES: usize = 256 * 1024;
/// Most entries one list holds.
pub const MAX_ENTRIES: usize = 64;
/// Largest pack zip accepted, in bytes.
pub const MAX_ZIP_BYTES: u64 = 4 * 1024 * 1024;
/// Largest preview image accepted, in bytes.
pub const MAX_PREVIEW_BYTES: u64 = 512 * 1024;
/// Most entries one pack zip holds.
pub const MAX_ZIP_ENTRIES: usize = 1024;
/// Longest name of a zip entry, in characters.
pub const MAX_ENTRY_NAME_CHARS: usize = 128;
/// Longest pack identifier, in characters.
pub const MAX_ID_CHARS: usize = 64;
/// Longest pack name, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// Longest version text, in characters.
pub const MAX_VERSION_CHARS: usize = 32;
/// File inside an installed folder that marks a pack the pack browser put
/// there.
pub const MARKER_FILE: &str = ".downloaded";

/// The release file that holds the zip of pack `id`.
#[must_use]
pub fn asset_name(id: &str) -> String {
    format!("pack-{id}.zip")
}

/// The release file that holds the preview image of pack `id`.
#[must_use]
pub fn preview_name(id: &str) -> String {
    format!("pack-{id}-preview.png")
}

/// One download pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Pack identifier.
    pub id: String,
    /// Name to show.
    pub name: String,
    /// Version of the pack.
    pub version: String,
    /// `light`, `dark` or both.
    pub modes: Vec<String>,
    /// Release file that holds the zip.
    pub asset: String,
    /// Size of the zip, in bytes.
    pub size: u64,
    /// SHA-256 of the zip, lower case hexadecimal.
    pub sha256: String,
    /// Release file that holds the preview image.
    pub preview: String,
    /// Size of the preview image, in bytes.
    pub preview_size: u64,
    /// SHA-256 of the preview image, lower case hexadecimal.
    pub preview_sha256: String,
}

/// The list of download packs of one release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackIndex {
    /// List format.
    pub format: u32,
    /// The packs, in the order the list gives them.
    pub packs: Vec<IndexEntry>,
}

/// Read and check a list. `builtin` holds the identifiers of the packs built
/// into the program, which a list may not name.
///
/// Unknown fields are ignored. Any other violation refuses the whole list.
///
/// # Errors
/// A message naming the first rule the list breaks.
pub fn parse(bytes: &[u8], builtin: &[&str]) -> Result<PackIndex, String> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(format!(
            "The pack list is larger than {MAX_INDEX_BYTES} bytes."
        ));
    }
    let mut index: PackIndex = serde_json::from_slice(bytes)
        .map_err(|error| format!("The pack list is not valid: {error}."))?;
    if index.format != FORMAT {
        return Err(if index.format > FORMAT {
            format!(
                "The pack list uses format {}. This program reads format {FORMAT}.",
                index.format
            )
        } else {
            format!("Pack list format {} is not supported.", index.format)
        });
    }
    if index.packs.len() > MAX_ENTRIES {
        return Err(format!(
            "The pack list names more than {MAX_ENTRIES} packs."
        ));
    }
    let mut seen = BTreeSet::new();
    for entry in &mut index.packs {
        check_entry(entry, builtin)?;
        if !seen.insert(entry.id.clone()) {
            return Err(format!("The pack list names \"{}\" twice.", entry.id));
        }
    }
    Ok(index)
}

fn check_entry(entry: &mut IndexEntry, builtin: &[&str]) -> Result<(), String> {
    let id = entry.id.clone();
    let refuse = |what: &str| Err(format!("The pack list entry \"{id}\" {what}."));
    if !valid_id(&entry.id) {
        return Err(format!(
            "The pack list holds an identifier \"{}\" that is not a pack identifier.",
            entry.id.chars().take(MAX_ID_CHARS + 1).collect::<String>()
        ));
    }
    if builtin.contains(&entry.id.as_str()) {
        return refuse("names a pack that is built into the program");
    }
    if !printable(&entry.name, MAX_NAME_CHARS) {
        return refuse("has a name that is empty, too long or not printable");
    }
    if !printable(&entry.version, MAX_VERSION_CHARS) {
        return refuse("has a version that is empty, too long or not printable");
    }
    let modes: BTreeSet<&str> = entry.modes.iter().map(String::as_str).collect();
    if entry.modes.is_empty()
        || modes.len() != entry.modes.len()
        || modes.iter().any(|mode| !matches!(*mode, "light" | "dark"))
    {
        return refuse("has modes that are not light, dark or both");
    }
    if entry.asset != asset_name(&entry.id) {
        return refuse("names a zip file other than its own");
    }
    if entry.preview != preview_name(&entry.id) {
        return refuse("names a preview file other than its own");
    }
    if entry.size == 0 || entry.size > MAX_ZIP_BYTES {
        return refuse("has a zip size outside the limit");
    }
    if entry.preview_size == 0 || entry.preview_size > MAX_PREVIEW_BYTES {
        return refuse("has a preview size outside the limit");
    }
    let Some(sha256) = normalized_hash(&entry.sha256) else {
        return refuse("has a checksum that is not 64 hexadecimal digits");
    };
    let Some(preview_sha256) = normalized_hash(&entry.preview_sha256) else {
        return refuse("has a preview checksum that is not 64 hexadecimal digits");
    };
    entry.sha256 = sha256;
    entry.preview_sha256 = preview_sha256;
    Ok(())
}

fn normalized_hash(text: &str) -> Option<String> {
    (text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

fn printable(text: &str, limit: usize) -> bool {
    let chars = text.chars().count();
    chars > 0 && chars <= limit && !text.chars().any(char::is_control)
}

/// The pack identifier rule: 1 to 64 characters of `a-z`, `0-9` and `-`, not
/// starting with `-`, and not `default`.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_ID_CHARS
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !id.starts_with('-')
        && id != "default"
}

/// The rule for the name of a zip entry: forward slashes, plain lower case
/// names of `a-z`, `0-9`, `-`, `_` and `.`, no empty or all-dot component, no
/// device name, at most 128 characters, and not the marker file.
///
/// # Errors
/// A message naming the entry.
pub fn check_entry_name(name: &str) -> Result<(), String> {
    let plain = |component: &str| {
        !component.is_empty()
            && component.bytes().any(|byte| byte != b'.')
            && component.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
    };
    let shown: String = name.chars().take(MAX_ENTRY_NAME_CHARS).collect();
    if name.chars().count() > MAX_ENTRY_NAME_CHARS
        || !name.split('/').all(plain)
        || name.split('/').any(is_device_name)
        || name == MARKER_FILE
    {
        return Err(format!(
            "The pack holds the file path \"{shown}\", which is not a plain relative path."
        ));
    }
    Ok(())
}

fn is_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|digit| matches!(digit.as_bytes(), [b'1'..=b'9']))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn entry(id: &str) -> IndexEntry {
        IndexEntry {
            id: id.to_owned(),
            name: "Neon Tile".to_owned(),
            version: "1.0.0".to_owned(),
            modes: vec!["dark".to_owned()],
            asset: asset_name(id),
            size: 10,
            sha256: "a".repeat(64),
            preview: preview_name(id),
            preview_size: 10,
            preview_sha256: "B".repeat(64),
        }
    }

    fn text(packs: &[IndexEntry]) -> Vec<u8> {
        serde_json::to_vec(&PackIndex {
            format: 1,
            packs: packs.to_vec(),
        })
        .unwrap()
    }

    #[test]
    fn a_good_list_parses_and_hashes_are_lower_case() {
        let index = parse(&text(&[entry("neon-tile")]), &["luminous-dark"]).unwrap();
        assert_eq!(index.packs[0].preview_sha256, "b".repeat(64));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&text(&[entry("neon-tile")])).unwrap();
        value["later"] = serde_json::json!(1);
        value["packs"][0]["extra"] = serde_json::json!("x");
        assert!(parse(&serde_json::to_vec(&value).unwrap(), &[]).is_ok());
    }

    #[test]
    fn the_list_limits_are_enforced() {
        let many: Vec<IndexEntry> = (0..65).map(|n| entry(&format!("p{n}"))).collect();
        assert!(parse(&text(&many[..64]), &[]).is_ok());
        assert!(parse(&text(&many), &[]).is_err());
        assert!(parse(&vec![b' '; MAX_INDEX_BYTES + 1], &[]).is_err());
        assert!(parse(&text(&[entry("a"), entry("a")]), &[]).is_err());
        assert!(parse(&text(&[entry("neon")]), &["neon"]).is_err());
        let change = |edit: &dyn Fn(&mut IndexEntry)| {
            let mut broken = entry("neon");
            edit(&mut broken);
            parse(&text(&[broken]), &[])
        };
        assert!(change(&|e| e.size = MAX_ZIP_BYTES).is_ok());
        assert!(change(&|e| e.size = MAX_ZIP_BYTES + 1).is_err());
        assert!(change(&|e| e.size = 0).is_err());
        assert!(change(&|e| e.preview_size = MAX_PREVIEW_BYTES).is_ok());
        assert!(change(&|e| e.preview_size = MAX_PREVIEW_BYTES + 1).is_err());
        assert!(change(&|e| e.sha256 = "g".repeat(64)).is_err());
        assert!(change(&|e| e.sha256 = "a".repeat(63)).is_err());
        assert!(change(&|e| e.preview_sha256.clear()).is_err());
        assert!(change(&|e| e.id = "Neon".to_owned()).is_err());
        assert!(change(&|e| e.id = "default".to_owned()).is_err());
        assert!(change(&|e| e.id = "a".repeat(65)).is_err());
        assert!(change(&|e| e.asset = "../pack-neon.zip".to_owned()).is_err());
        assert!(change(&|e| e.preview = "other.png".to_owned()).is_err());
        assert!(change(&|e| e.modes.clear()).is_err());
        assert!(change(&|e| e.modes = vec!["dark".to_owned(), "dark".to_owned()]).is_err());
        assert!(change(&|e| e.modes = vec!["sepia".to_owned()]).is_err());
        assert!(change(&|e| e.modes = vec!["light".to_owned(), "dark".to_owned()]).is_ok());
        assert!(change(&|e| e.name = "x".repeat(81)).is_err());
        assert!(change(&|e| e.version = "1\n2".to_owned()).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&text(&[entry("a")])).unwrap();
        value["format"] = serde_json::json!(2);
        assert!(parse(&serde_json::to_vec(&value).unwrap(), &[])
            .unwrap_err()
            .contains("reads format 1"));
    }

    #[test]
    fn entry_names_follow_the_rules() {
        for good in ["pack.json", "icons/16/a-b_c.png", "a.b.c"] {
            assert!(check_entry_name(good).is_ok(), "{good}");
        }
        for bad in [
            "../x",
            "a/../b",
            "/abs",
            "a//b",
            "a/",
            "C:/x",
            "c:x",
            "a\\b",
            "\\\\server\\share",
            "CON",
            "con.txt",
            "icons/nul.png",
            "Lpt1",
            "A.png",
            ".",
            "..",
            "a b",
            ".downloaded",
            &"a".repeat(129),
        ] {
            assert!(check_entry_name(bad).is_err(), "{bad}");
        }
        assert!(check_entry_name(&"a".repeat(128)).is_ok());
        assert!(check_entry_name("com10.png").is_ok());
    }
}
