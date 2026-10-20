//! `cargo xtask pack-assets <dist>` writes the release files of the download
//! packs: `pack-<id>.zip`, `pack-<id>-preview.png` and `packs-index.json`,
//! for every folder under `assets/packs` that is not built into the program.
//! With `--check` it verifies those files in `<dist>` instead.
//!
//! The output depends only on the pack folders: entries are sorted, every
//! timestamp is fixed and the compressors are single-threaded.

use anyhow::{bail, Context};
use image::Rgba;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[path = "../../crates/ca-ui/src/pack/index.rs"]
#[allow(dead_code)]
mod index;

use index::{
    asset_name, check_entry_name, preview_name, IndexEntry, PackIndex, FORMAT, INDEX_NAME,
    MAX_PREVIEW_BYTES, MAX_ZIP_BYTES, MAX_ZIP_ENTRIES,
};

/// Icons per row of the preview image.
const COLUMNS: u32 = 8;
/// Most icons the preview image shows.
const ICONS_SHOWN: usize = 32;
/// Pixel size of the icons the preview image is made from.
const ICON_SIZE: u32 = 24;
/// Space around and between the icons, in pixels.
const GAP: u32 = 8;

pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let check = args.iter().any(|arg| arg == "--check");
    let dist = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .context("usage: cargo xtask pack-assets [--check] <dist>")?;
    let dist = Path::new(dist);
    let folders = download_folders(&root.join("assets").join("packs"))?;
    if check {
        check_dist(&folders, dist)?;
        println!("{INDEX_NAME} matches the files in {}", dist.display());
    } else {
        std::fs::create_dir_all(dist)?;
        let index = build(&folders, dist)?;
        println!("wrote {} packs and {INDEX_NAME}", index.packs.len());
    }
    Ok(())
}

/// The pack folders that are not built into the program, in name order.
fn download_folders(packs: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(packs).with_context(|| format!("read {}", packs.display()))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("a pack folder name is not text")?
            .to_owned();
        if path.is_dir() && !super::packart::DEFAULT_EMBEDDED.contains(&name.as_str()) {
            folders.push(path);
        }
    }
    folders.sort();
    Ok(folders)
}

fn folder_id(folder: &Path) -> anyhow::Result<&str> {
    folder
        .file_name()
        .and_then(|n| n.to_str())
        .context("a pack folder name is not text")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Every file below `folder` as a relative `/` path, sorted by byte order.
fn files_of(folder: &Path) -> anyhow::Result<Vec<String>> {
    fn walk(base: &Path, current: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(base, &path, out)?;
            } else if kind.is_file() {
                let relative = path.strip_prefix(base)?;
                let parts: Vec<&str> = relative
                    .components()
                    .map(|c| c.as_os_str().to_str().context("a file name is not text"))
                    .collect::<anyhow::Result<_>>()?;
                out.push(parts.join("/"));
            } else {
                bail!("{} is not a plain file or folder", path.display());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(folder, folder, &mut out)?;
    out.sort();
    Ok(out)
}

struct Manifest {
    name: String,
    version: String,
    modes: Vec<String>,
    chrome: Option<[u8; 3]>,
}

fn read_manifest(folder: &Path) -> anyhow::Result<Manifest> {
    let path = folder.join("pack.json");
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("{} has no {key}", path.display()))
    };
    let id = text("id")?;
    if id != folder_id(folder)? {
        bail!("{} names the id {id}, not its folder", path.display());
    }
    let modes: Vec<String> = value
        .get("modes")
        .and_then(serde_json::Value::as_array)
        .context("the pack names no modes")?
        .iter()
        .filter_map(|mode| mode.as_str().map(str::to_owned))
        .collect();
    let first = modes.first().context("the pack names no modes")?;
    let chrome = value
        .pointer(&format!("/colors/text/{first}/slots/chrome"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_color);
    Ok(Manifest {
        name: text("name")?,
        version: text("version")?,
        modes,
        chrome,
    })
}

fn parse_color(text: &str) -> Option<[u8; 3]> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// The zip of a pack folder: sorted entries, no folders, fixed timestamps.
fn zip_folder(folder: &Path) -> anyhow::Result<Vec<u8>> {
    let files = files_of(folder)?;
    if files.len() > MAX_ZIP_ENTRIES {
        bail!(
            "{} holds more than {MAX_ZIP_ENTRIES} files",
            folder.display()
        );
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(0o644);
        for name in &files {
            check_entry_name(name).map_err(anyhow::Error::msg)?;
            let path = name
                .split('/')
                .fold(folder.to_path_buf(), |path, part| path.join(part));
            writer.start_file(name.as_str(), options)?;
            writer.write_all(&std::fs::read(&path)?)?;
        }
        writer.finish()?;
    }
    Ok(cursor.into_inner())
}

/// An icon grid made from the 24 pixel icons of the pack.
fn preview(folder: &Path, manifest: &Manifest) -> anyhow::Result<Vec<u8>> {
    let directory = folder.join("icons").join(ICON_SIZE.to_string());
    let mut names: Vec<PathBuf> = std::fs::read_dir(&directory)
        .with_context(|| format!("read {}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        })
        .collect();
    names.sort();
    if names.is_empty() {
        bail!("{} holds no icons", directory.display());
    }
    let shown = names.len().min(ICONS_SHOWN);
    let chosen: Vec<&PathBuf> = (0..shown)
        .map(|n| &names[n * names.len() / shown])
        .collect();
    let background = manifest.chrome.unwrap_or_else(|| {
        if manifest.modes.first().is_some_and(|mode| mode == "light") {
            [0xF3, 0xF3, 0xF3]
        } else {
            [0x20, 0x21, 0x24]
        }
    });
    let columns = COLUMNS.min(u32::try_from(shown)?);
    let rows = u32::try_from(shown)?.div_ceil(COLUMNS);
    let cell = ICON_SIZE + GAP;
    let (width, height) = (columns * cell + GAP, rows * cell + GAP);
    let mut canvas = image::RgbaImage::from_pixel(
        width,
        height,
        Rgba([background[0], background[1], background[2], 255]),
    );
    for (position, path) in chosen.iter().enumerate() {
        let icon = image::open(path)
            .with_context(|| format!("read {}", path.display()))?
            .to_rgba8();
        if icon.dimensions() != (ICON_SIZE, ICON_SIZE) {
            bail!("{} is not {ICON_SIZE} pixels square", path.display());
        }
        let at = u32::try_from(position)?;
        let (left, top) = (GAP + (at % COLUMNS) * cell, GAP + (at / COLUMNS) * cell);
        for (x, y, pixel) in icon.enumerate_pixels() {
            let alpha = u32::from(pixel.0[3]);
            let below = canvas.get_pixel_mut(left + x, top + y);
            for channel in 0..3 {
                let blended = u32::from(pixel.0[channel]) * alpha
                    + u32::from(below.0[channel]) * (255 - alpha);
                below.0[channel] = u8::try_from((blended + 127) / 255)?;
            }
        }
    }
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for pixel in canvas.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_deflate_compression(png::DeflateCompression::Level(9));
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgb)?;
        writer.finish()?;
    }
    Ok(out)
}

fn build(folders: &[PathBuf], dist: &Path) -> anyhow::Result<PackIndex> {
    let mut packs = Vec::new();
    for folder in folders {
        let id = folder_id(folder)?.to_owned();
        let manifest = read_manifest(folder).with_context(|| id.clone())?;
        let zip = zip_folder(folder).with_context(|| id.clone())?;
        let image = preview(folder, &manifest).with_context(|| id.clone())?;
        std::fs::write(dist.join(asset_name(&id)), &zip)?;
        std::fs::write(dist.join(preview_name(&id)), &image)?;
        packs.push(IndexEntry {
            asset: asset_name(&id),
            size: zip.len() as u64,
            sha256: digest(&zip),
            preview: preview_name(&id),
            preview_size: image.len() as u64,
            preview_sha256: digest(&image),
            name: manifest.name,
            version: manifest.version,
            modes: manifest.modes,
            id,
        });
    }
    let index = PackIndex {
        format: FORMAT,
        packs,
    };
    let text = serde_json::to_string_pretty(&index)?;
    index::parse(text.as_bytes(), super::packart::DEFAULT_EMBEDDED.as_slice())
        .map_err(anyhow::Error::msg)?;
    std::fs::write(dist.join(INDEX_NAME), format!("{text}\n"))?;
    Ok(index)
}

fn read_limited(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("{} is larger than {limit} bytes", path.display());
    }
    Ok(bytes)
}

/// Compare the files in `dist` with the list and with the pack folders.
fn check_dist(folders: &[PathBuf], dist: &Path) -> anyhow::Result<()> {
    let path = dist.join(INDEX_NAME);
    let bytes = read_limited(&path, index::MAX_INDEX_BYTES as u64)?;
    let index = index::parse(&bytes, super::packart::DEFAULT_EMBEDDED.as_slice())
        .map_err(anyhow::Error::msg)?;
    let expected: Vec<&str> = folders
        .iter()
        .map(|folder| folder_id(folder))
        .collect::<anyhow::Result<_>>()?;
    let listed: Vec<&str> = index.packs.iter().map(|entry| entry.id.as_str()).collect();
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    if sorted != expected {
        bail!("{INDEX_NAME} lists {listed:?}, the pack folders are {expected:?}");
    }
    for entry in &index.packs {
        let zip = read_limited(&dist.join(&entry.asset), MAX_ZIP_BYTES)?;
        if zip.len() as u64 != entry.size || digest(&zip) != entry.sha256 {
            bail!("{} does not match {INDEX_NAME}", entry.asset);
        }
        let image = read_limited(&dist.join(&entry.preview), MAX_PREVIEW_BYTES)?;
        if image.len() as u64 != entry.preview_size || digest(&image) != entry.preview_sha256 {
            bail!("{} does not match {INDEX_NAME}", entry.preview);
        }
        check_zip(&zip, &entry.id).with_context(|| entry.asset.clone())?;
    }
    for item in std::fs::read_dir(dist)? {
        let name = item?.file_name().to_string_lossy().into_owned();
        let pack_file = name.starts_with("pack-")
            && Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip") || e.eq_ignore_ascii_case("png"));
        let known = index
            .packs
            .iter()
            .any(|entry| entry.asset == name || entry.preview == name);
        if pack_file && !known {
            bail!("{name} is not listed in {INDEX_NAME}");
        }
    }
    Ok(())
}

/// The rules the program applies to a downloaded pack, on the names and the id.
fn check_zip(zip: &[u8], id: &str) -> anyhow::Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip))?;
    if archive.len() > MAX_ZIP_ENTRIES {
        bail!("more than {MAX_ZIP_ENTRIES} entries");
    }
    let mut manifest = None;
    for position in 0..archive.len() {
        let mut member = archive.by_index(position)?;
        if !member.is_file() {
            bail!("an entry is not a regular file");
        }
        let name = member.name()?.into_owned();
        check_entry_name(&name).map_err(anyhow::Error::msg)?;
        if name == "pack.json" {
            let mut bytes = Vec::new();
            (&mut member).take(256 * 1024 + 1).read_to_end(&mut bytes)?;
            manifest = Some(bytes);
        }
    }
    let manifest = manifest.context("the zip has no pack.json")?;
    let value: serde_json::Value = serde_json::from_slice(&manifest)?;
    if value.get("id").and_then(serde_json::Value::as_str) != Some(id) {
        bail!("pack.json names another id than the list");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn pack(root: &Path, id: &str, modes: &str) -> PathBuf {
        let folder = root.join(id);
        std::fs::create_dir_all(folder.join("icons/24")).unwrap();
        std::fs::write(
            folder.join("pack.json"),
            format!(
                r##"{{"format":1,"id":"{id}","name":"Name {id}","version":"1.0.0","modes":{modes},"colors":{{"text":{{"dark":{{"slots":{{"chrome":"#102030"}}}}}}}}}}"##
            ),
        )
        .unwrap();
        for icon in ["a", "b", "c"] {
            let image = image::RgbaImage::from_pixel(24, 24, Rgba([200, 10, 10, 128]));
            image
                .save_with_format(
                    folder.join(format!("icons/24/{icon}.png")),
                    image::ImageFormat::Png,
                )
                .unwrap();
        }
        folder
    }

    #[test]
    fn built_in_folders_are_not_download_packs() {
        let dir = tempfile::tempdir().unwrap();
        pack(dir.path(), "luminous-dark", r#"["dark"]"#);
        pack(dir.path(), "bold-outline", r#"["dark"]"#);
        pack(dir.path(), "neon", r#"["dark"]"#);
        let folders = download_folders(dir.path()).unwrap();
        assert_eq!(folders.len(), 1);
        assert!(folders[0].ends_with("neon"));
    }

    #[test]
    fn the_output_is_the_same_on_every_run_and_passes_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let packs = dir.path().join("packs");
        let folders = vec![
            pack(&packs, "neon", r#"["dark"]"#),
            pack(&packs, "sea", r#"["light","dark"]"#),
        ];
        let (first, second) = (dir.path().join("one"), dir.path().join("two"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        build(&folders, &first).unwrap();
        build(&folders, &second).unwrap();
        for name in [
            "packs-index.json",
            "pack-neon.zip",
            "pack-neon-preview.png",
            "pack-sea.zip",
            "pack-sea-preview.png",
        ] {
            assert_eq!(
                std::fs::read(first.join(name)).unwrap(),
                std::fs::read(second.join(name)).unwrap(),
                "{name}"
            );
        }
        check_dist(&folders, &first).unwrap();
    }

    #[test]
    fn a_changed_file_a_stray_file_or_a_missing_pack_fails_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let packs = dir.path().join("packs");
        let folders = vec![pack(&packs, "neon", r#"["dark"]"#)];
        let dist = dir.path().join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        build(&folders, &dist).unwrap();
        check_dist(&folders, &dist).unwrap();

        std::fs::write(dist.join("pack-extra.zip"), b"x").unwrap();
        assert!(check_dist(&folders, &dist).is_err());
        std::fs::remove_file(dist.join("pack-extra.zip")).unwrap();

        let more = vec![folders[0].clone(), pack(&packs, "sea", r#"["dark"]"#)];
        assert!(check_dist(&more, &dist).is_err());

        let zip = dist.join("pack-neon.zip");
        let mut bytes = std::fs::read(&zip).unwrap();
        bytes.push(0);
        std::fs::write(&zip, bytes).unwrap();
        assert!(check_dist(&folders, &dist).is_err());
    }

    #[test]
    fn a_file_with_an_unsafe_name_stops_the_build() {
        let dir = tempfile::tempdir().unwrap();
        let folder = pack(dir.path(), "neon", r#"["dark"]"#);
        std::fs::write(folder.join("Notes.TXT"), b"x").unwrap();
        assert!(zip_folder(&folder).is_err());
    }

    #[test]
    fn the_preview_is_a_grid_of_the_icons() {
        let dir = tempfile::tempdir().unwrap();
        let folder = pack(dir.path(), "neon", r#"["dark"]"#);
        let manifest = read_manifest(&folder).unwrap();
        let bytes = preview(&folder, &manifest).unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (3 * 32 + 8, 40));
        assert_eq!(image.get_pixel(0, 0).0, [0x10, 0x20, 0x30]);
    }
}
