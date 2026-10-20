//! A local transport and pack builders for the pack download tests. Nothing
//! here opens a network connection.

#![allow(clippy::unwrap_used, dead_code, missing_docs)]

use ca_ui::pack::index::{asset_name, preview_name, IndexEntry, PackIndex};
use ca_ui::pack::remote::{hex_digest, FetchError, PackLocations, PackServices, PackTransport};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub const DOWNLOAD_BASE: &str = "https://example.test/releases/download";
pub const PAGE_BASE: &str = "https://example.test/releases/tag";
pub const VERSION: &str = "2026.10.20.0001";

#[derive(Default)]
pub struct FakeTransport {
    files: Mutex<BTreeMap<String, Result<Vec<u8>, FetchError>>>,
    pub requests: Mutex<Vec<String>>,
}

impl FakeTransport {
    pub fn put(&self, url: &str, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(url.to_owned(), Ok(bytes));
    }

    pub fn fail(&self, url: &str, error: FetchError) {
        self.files
            .lock()
            .unwrap()
            .insert(url.to_owned(), Err(error));
    }

    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError> {
        self.requests.lock().unwrap().push(url.to_owned());
        let found = self.files.lock().unwrap().get(url).cloned();
        match found {
            None => Err(FetchError::NotFound),
            Some(Err(error)) => Err(error),
            Some(Ok(bytes)) if bytes.len() as u64 > max_bytes => Err(FetchError::Failed(
                "the response is larger than allowed".to_owned(),
            )),
            Some(Ok(bytes)) => Ok(bytes),
        }
    }
}

impl PackTransport for FakeTransport {
    fn document(
        &self,
        url: &str,
        max_bytes: u64,
        _cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, FetchError> {
        self.get(url, max_bytes)
    }

    fn file(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
        _cancel: &Arc<AtomicBool>,
    ) -> Result<(), FetchError> {
        let bytes = self.get(url, max_bytes)?;
        std::fs::write(destination, bytes).map_err(|error| FetchError::Failed(error.to_string()))
    }
}

pub fn locations() -> PackLocations {
    PackLocations::for_release(DOWNLOAD_BASE, PAGE_BASE, VERSION)
}

pub fn services(transport: &Arc<FakeTransport>, directory: &Path) -> Arc<PackServices> {
    Arc::new(PackServices {
        transport: Arc::clone(transport) as Arc<dyn PackTransport>,
        locations: locations(),
        directory: directory.to_path_buf(),
    })
}

pub fn manifest(id: &str, name: &str) -> Vec<u8> {
    format!(
        r#"{{"format":1,"id":"{id}","name":"{name}","version":"1.0.0","modes":["dark"],"icons":{{"sizes":[16]}}}}"#
    )
    .into_bytes()
}

pub fn png(size: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(size, size, image::Rgba([10, 200, 30, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// A zip with the given entries, written under the names as given.
pub fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

/// A good zip for pack `id`.
pub fn good_zip(id: &str, name: &str) -> Vec<u8> {
    zip_of(&[
        ("pack.json", manifest(id, name)),
        ("icons/16/next-difference.png", png(16)),
    ])
}

pub fn entry_for(id: &str, name: &str, zip: &[u8], preview: &[u8]) -> IndexEntry {
    IndexEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        version: "1.0.0".to_owned(),
        modes: vec!["dark".to_owned()],
        asset: asset_name(id),
        size: zip.len() as u64,
        sha256: hex_digest(zip),
        preview: preview_name(id),
        preview_size: preview.len() as u64,
        preview_sha256: hex_digest(preview),
    }
}

/// Serve a good pack and its list entry; returns the entry.
pub fn serve_pack(transport: &FakeTransport, id: &str, name: &str, zip: Vec<u8>) -> IndexEntry {
    let preview = png(32);
    let entry = entry_for(id, name, &zip, &preview);
    let locations = locations();
    transport.put(&locations.file_url(&entry.asset), zip);
    transport.put(&locations.file_url(&entry.preview), preview);
    entry
}

pub fn serve_index(transport: &FakeTransport, entries: &[IndexEntry]) {
    let index = PackIndex {
        format: 1,
        packs: entries.to_vec(),
    };
    transport.put(
        &locations().index_url(),
        serde_json::to_vec(&index).unwrap(),
    );
}

/// Names of the entries of the packs folder, sorted.
pub fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|listing| {
            listing
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
