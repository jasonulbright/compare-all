//! The update manifest a release publishes beside its files.
//!
//! This file depends on `serde` and `serde_json` only: the repository tooling
//! includes it by path to write and check the manifest of a release.

use serde::{Deserialize, Serialize};

/// File name of the manifest in a release.
pub const MANIFEST_NAME: &str = "update-manifest.json";

/// The only manifest layout this build reads.
pub const SCHEMA: u32 = 1;

/// Largest manifest this build reads.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestAsset {
    /// File name in the release.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 as 64 lowercase hexadecimal digits.
    pub sha256: String,
}

/// What a release says about its own files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Layout number of this document.
    pub schema: u32,
    /// Release version, `YYYY.MM.DD.NNNN`.
    pub version: String,
    /// Lowest updater protocol number that may apply this release.
    pub minimum_updater_version: u32,
    /// The Windows files of the release.
    pub assets: Vec<ManifestAsset>,
}

/// Name of the installer of `version`.
#[must_use]
pub fn msi_name(version: &str) -> String {
    format!("compare-all-{version}-x64.msi")
}

/// Name of the portable archive of `version`.
#[must_use]
pub fn zip_name(version: &str) -> String {
    format!("compare-all-{version}-windows-x64.zip")
}

/// True for 64 lowercase hexadecimal digits.
#[must_use]
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// True for a file name without any folder part that Windows stores under
/// exactly that name.
///
/// Windows drops a trailing dot or space from a name, so `a.dll.` is written
/// as `a.dll`, and it opens a device for a reserved base name such as `NUL`
/// or `COM1.txt`. Both are refused.
#[must_use]
pub fn is_plain_file_name(name: &str) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let base = name.split('.').next().unwrap_or_default();
    let numbered_device = match base.as_bytes() {
        [a, b, c, digit] => {
            let prefix = [*a, *b, *c];
            (prefix.eq_ignore_ascii_case(b"COM") || prefix.eq_ignore_ascii_case(b"LPT"))
                && digit.is_ascii_digit()
        }
        _ => false,
    };
    !name.is_empty()
        && !name.ends_with('.')
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !DEVICES
            .iter()
            .any(|device| base.eq_ignore_ascii_case(device))
        && !numbered_device
}

impl Manifest {
    /// Read and validate a manifest.
    ///
    /// # Errors
    /// Returns why the document is not a manifest this build can use.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err("the manifest is too large".to_owned());
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("the manifest is not valid: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Check the fields a consumer relies on.
    ///
    /// # Errors
    /// Returns the first field that is not usable.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "the manifest layout {} is not supported",
                self.schema
            ));
        }
        if !self
            .version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
            || self.version.split('.').count() != 4
        {
            return Err("the manifest version is not YYYY.MM.DD.NNNN".to_owned());
        }
        for asset in &self.assets {
            if !is_plain_file_name(&asset.name) {
                return Err(format!(
                    "the asset name {:?} is not a file name",
                    asset.name
                ));
            }
            if !is_sha256(&asset.sha256) {
                return Err(format!("the checksum of {} is not SHA-256", asset.name));
            }
        }
        Ok(())
    }

    /// The asset called `name`.
    #[must_use]
    pub fn asset(&self, name: &str) -> Option<&ManifestAsset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            schema: SCHEMA,
            version: "2026.10.18.0007".to_owned(),
            minimum_updater_version: 1,
            assets: vec![ManifestAsset {
                name: msi_name("2026.10.18.0007"),
                size: 10,
                sha256: "a".repeat(64),
            }],
        }
    }

    #[test]
    fn a_written_manifest_reads_back() {
        let bytes = serde_json::to_vec(&sample()).unwrap();
        assert_eq!(Manifest::parse(&bytes).unwrap(), sample());
    }

    #[test]
    fn a_manifest_with_a_bad_field_is_refused() {
        let mut bad = sample();
        bad.assets[0].sha256 = "A".repeat(64);
        assert!(Manifest::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        let mut bad = sample();
        bad.assets[0].name = "..\\evil.msi".to_owned();
        assert!(Manifest::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        let mut bad = sample();
        bad.schema = 2;
        assert!(Manifest::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        assert!(Manifest::parse(b"<html>").is_err());
    }

    #[test]
    fn names_windows_stores_differently_are_not_plain() {
        for bad in [
            "",
            ".",
            "..",
            "a.dll.",
            "a.dll ",
            ".hidden",
            "a:b",
            "C:x.exe",
            "a\\b",
            "a/b",
            "/a",
            "NUL",
            "nul.txt",
            "CON",
            "Prn.md",
            "aux",
            "COM1",
            "com9.exe",
            "LPT1.dll",
            "lpt0",
            "\u{e9}.exe",
        ] {
            assert!(!is_plain_file_name(bad), "{bad:?}");
        }
        for good in [
            "compare-all.exe",
            "README.md",
            "THIRD-PARTY-NOTICES.md",
            "COM10.txt",
            "console.log",
            "nullable.txt",
        ] {
            assert!(is_plain_file_name(good), "{good:?}");
        }
    }
}
