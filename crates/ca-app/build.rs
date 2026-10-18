//! Embeds the build version as `CA_VERSION` and, on Windows, the application
//! icon and the version information as resources of the `compare-all`
//! executable.

#[path = "../../xtask/src/version.rs"]
mod version;

#[path = "../../xtask/src/resource.rs"]
mod resource;

use resource::{append, append_version, new_resource_file, VersionInfo, LANGUAGE};
use std::path::{Path, PathBuf};

const ICON: &str = "../../assets/icon/compare-all.ico";

fn main() {
    // The build date is an input cargo cannot see, so it is supplied as an
    // environment variable and tracked as one. Nothing else makes the date
    // reach the binary without recompiling the crate on every invocation.
    println!("cargo:rerun-if-env-changed=CA_BUILD_DATE");
    println!("cargo:rerun-if-changed=../../BUILD_NUMBER");
    println!("cargo:rerun-if-changed={ICON}");
    let version = version::compute();
    println!("cargo:rustc-env=CA_VERSION={version}");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        if let Err(error) = embed_resources(&version) {
            println!("cargo:warning=the resources are not embedded: {error}");
        }
    }
}

/// Writes the icon and the version information as a compiled resource file
/// and hands it to the linker.
///
/// The MSVC linker takes a `.res` file as an input and converts it itself, so
/// no resource compiler has to be installed.
fn embed_resources(version: &str) -> Result<(), String> {
    let ico = std::fs::read(ICON).map_err(|error| format!("cannot read {ICON}: {error}"))?;
    let mut res = resource_file(&ico)?;
    append_version(
        &mut res,
        &VersionInfo {
            version,
            description: "Compare All",
            product: "Compare All",
            company: "Jason Ulbright",
            file_name: "compare-all.exe",
        },
    )?;
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|error| error.to_string())?);
    let path = out.join("compare-all.res");
    std::fs::write(&path, res).map_err(|error| error.to_string())?;
    link(&path);
    Ok(())
}

fn link(path: &Path) {
    println!("cargo:rustc-link-arg-bin=compare-all={}", path.display());
}

/// Resource type of one icon image.
const RT_ICON: u16 = 3;
/// Resource type of the directory that groups the images of one icon.
const RT_GROUP_ICON: u16 = 14;
/// Moveable, pure and discardable, as a resource compiler marks icon data.
const ICON_FLAGS: u16 = 0x1010;
/// Moveable, pure and discardable, as a resource compiler marks an icon group.
const GROUP_FLAGS: u16 = 0x1030;

/// A `.res` file with one icon group whose images are those of `ico`.
fn resource_file(ico: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "the ICO file is malformed".to_owned();
    let u16_at =
        |at: usize| -> Option<u16> { Some(u16::from_le_bytes([*ico.get(at)?, *ico.get(at + 1)?])) };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes([
            *ico.get(at)?,
            *ico.get(at + 1)?,
            *ico.get(at + 2)?,
            *ico.get(at + 3)?,
        ]))
    };
    if u16_at(0) != Some(0) || u16_at(2) != Some(1) {
        return Err(bad());
    }
    let count = u16_at(4).ok_or_else(bad)?;
    let mut res = new_resource_file();
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&count.to_le_bytes());
    for index in 0..count {
        let entry = 6 + usize::from(index) * 16;
        let header = ico.get(entry..entry + 8).ok_or_else(bad)?;
        let length = u32_at(entry + 8).ok_or_else(bad)?;
        let offset = u32_at(entry + 12).ok_or_else(bad)?;
        let start = usize::try_from(offset).map_err(|_| bad())?;
        let end = start + usize::try_from(length).map_err(|_| bad())?;
        let image = ico.get(start..end).ok_or_else(bad)?;
        let id = index + 1;
        append(&mut res, RT_ICON, id, ICON_FLAGS, LANGUAGE, image);
        group.extend_from_slice(header);
        group.extend_from_slice(&length.to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    append(&mut res, RT_GROUP_ICON, 1, GROUP_FLAGS, LANGUAGE, &group);
    Ok(res)
}
