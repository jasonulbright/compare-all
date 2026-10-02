//! Every workspace crate outside `ca_ui::theme` should be free of literal
//! color constructors; `theme` is the one place a value is assigned.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::path::{Path, PathBuf};

/// Names of `Color32` constructors and constants that assign a literal color.
const NEEDLES: &[&str] = &[
    "Color32::from_rgb",
    "Color32::from_rgba_unmultiplied",
    "Color32::from_rgba_premultiplied",
    "Color32::WHITE",
    "Color32::BLACK",
    "Color32::RED",
    "Color32::GREEN",
    "Color32::BLUE",
    "Color32::YELLOW",
    "Color32::GRAY",
    "Color32::TRANSPARENT",
];

/// Files allowed to name a color outside `ca_ui::theme`.
///
/// The picture view turns an already-decoded image pixel into a paintable
/// color and paints an already-colored texture with a full-opacity white tint.
/// Neither is a color choice, so neither belongs in a theme table. The list
/// holds file names rather than line numbers, so editing the file elsewhere
/// does not break the test.
const ALLOWED_FILES: &[&str] = &["ca-view-picture/src/lib.rs"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ca-ui manifest dir sits two levels under the workspace root")
        .to_path_buf()
}

fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Every `crates/*/src` file, as a path relative to `crates/`.
fn source_files() -> Vec<PathBuf> {
    let crates_dir = workspace_root().join("crates");
    let Ok(members) = std::fs::read_dir(&crates_dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for member in members.flatten() {
        let src = member.path().join("src");
        if src.is_dir() {
            visit(&src, &mut files);
        }
    }
    files
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(&crates_dir)
                .ok()
                .map(std::path::Path::to_path_buf)
        })
        .collect()
}

#[test]
fn no_module_outside_the_theme_holds_a_literal_color32() {
    let crates_dir = workspace_root().join("crates");
    let mut hits = Vec::new();
    for relative in source_files() {
        // The theme is the one place allowed to assign a color.
        if relative
            .components()
            .any(|part| part.as_os_str() == "theme")
        {
            continue;
        }
        let full = crates_dir.join(&relative);
        let Ok(text) = std::fs::read_to_string(&full) else {
            continue;
        };
        let name = relative.to_string_lossy().replace('\\', "/");
        if ALLOWED_FILES.contains(&name.as_str()) {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            if NEEDLES.iter().any(|needle| line.contains(needle)) {
                hits.push(format!("{name}:{}", index + 1));
            }
        }
    }
    hits.sort();
    assert!(
        hits.is_empty(),
        "a color constructor appeared outside ca_ui::theme and outside the allowed files: {hits:#?}"
    );
}
