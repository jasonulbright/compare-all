//! `cargo xtask update-manifest <dist>` writes the update manifest of a
//! release from the files in `<dist>`; with `--check` it verifies the manifest
//! against those files instead.

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::path::Path;

#[path = "../../crates/ca-update/src/manifest.rs"]
#[allow(dead_code)]
mod manifest;

use manifest::{msi_name, zip_name, Manifest, ManifestAsset, MANIFEST_NAME, SCHEMA};

/// Protocol number of the updater that reads this manifest layout.
const MINIMUM_UPDATER_VERSION: u32 = 1;

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let check = args.iter().any(|arg| arg == "--check");
    let dist = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .context("usage: cargo xtask update-manifest [--check] <dist>")?;
    let dist = Path::new(dist);
    if check {
        check_dist(dist)?;
        println!("{MANIFEST_NAME} matches the files in {}", dist.display());
    } else {
        let manifest = build(dist)?;
        let text = serde_json::to_string_pretty(&manifest)?;
        std::fs::write(dist.join(MANIFEST_NAME), format!("{text}\n"))?;
        println!("wrote {MANIFEST_NAME} for {}", manifest.version);
    }
    Ok(())
}

fn version_of(dist: &Path) -> anyhow::Result<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dist)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(version) = name
            .strip_prefix("compare-all-")
            .and_then(|rest| rest.strip_suffix("-x64.msi"))
        {
            found.push(version.to_owned());
        }
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        _ => bail!("expected exactly one installer in {}", dist.display()),
    }
}

fn describe(dist: &Path, name: &str) -> anyhow::Result<ManifestAsset> {
    let path = dist.join(name);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let sha256 = hex(&Sha256::digest(&bytes));
    let sum_path = dist.join(format!("{name}.sha256"));
    let sum = std::fs::read_to_string(&sum_path)
        .with_context(|| format!("read {}", sum_path.display()))?;
    let listed = sum.split_whitespace().next().unwrap_or_default();
    if listed != sha256 {
        bail!("{name}.sha256 does not match the file");
    }
    Ok(ManifestAsset {
        name: name.to_owned(),
        size: bytes.len() as u64,
        sha256,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

pub fn build(dist: &Path) -> anyhow::Result<Manifest> {
    let version = version_of(dist)?;
    let manifest = Manifest {
        schema: SCHEMA,
        assets: vec![
            describe(dist, &msi_name(&version))?,
            describe(dist, &zip_name(&version))?,
        ],
        version,
        minimum_updater_version: MINIMUM_UPDATER_VERSION,
    };
    manifest.validate().map_err(anyhow::Error::msg)?;
    Ok(manifest)
}

pub fn check_dist(dist: &Path) -> anyhow::Result<()> {
    let path = dist.join(MANIFEST_NAME);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let found = Manifest::parse(&bytes).map_err(anyhow::Error::msg)?;
    let expected = build(dist)?;
    if found != expected {
        bail!(
            "{MANIFEST_NAME} does not match the uploaded files: manifest {found:?}, files {expected:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn dist_with(version: &str, zip_bytes: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            (msi_name(version), b"msi bytes".as_slice()),
            (zip_name(version), zip_bytes),
        ] {
            std::fs::write(dir.path().join(&name), bytes).unwrap();
            std::fs::write(
                dir.path().join(format!("{name}.sha256")),
                format!("{}  {name}\n", hex(&Sha256::digest(bytes))),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn a_written_manifest_matches_its_files_and_a_changed_file_does_not() {
        let dir = dist_with("2026.10.18.0007", b"zip bytes");
        run(&[dir.path().to_string_lossy().into_owned()]).unwrap();
        check_dist(dir.path()).unwrap();
        std::fs::write(dir.path().join(zip_name("2026.10.18.0007")), b"changed").unwrap();
        assert!(check_dist(dir.path()).is_err());
    }

    #[test]
    fn a_checksum_file_that_disagrees_stops_the_manifest() {
        let dir = dist_with("2026.10.18.0007", b"zip bytes");
        std::fs::write(
            dir.path()
                .join(format!("{}.sha256", msi_name("2026.10.18.0007"))),
            format!("{}  x\n", "0".repeat(64)),
        )
        .unwrap();
        assert!(build(dir.path()).is_err());
    }
}
