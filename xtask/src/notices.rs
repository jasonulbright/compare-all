//! Dependency notices with pinned source records for incomplete crate packages.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt::Write, path::Path};

const PLACEHOLDER: &str = "Copyright (c) <year> <copyright holders>";

pub fn run(root: &Path) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let inventory_path = temporary.path().join("inventory.json");
    let output = std::process::Command::new("cargo")
        .current_dir(root)
        .args([
            "about",
            "generate",
            "--workspace",
            "--locked",
            "--fail",
            "--format",
            "json",
        ])
        .arg("--output-file")
        .arg(&inventory_path)
        .output()?;
    if !output.status.success() {
        bail!(
            "license inventory failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let inventory: Value = serde_json::from_slice(&std::fs::read(inventory_path)?)?;
    let directory = root.join("build/license-supplements");
    let index: Value = serde_json::from_slice(&std::fs::read(directory.join("index.json"))?)?;
    let mut supplements = BTreeMap::new();
    for record in array(&index)? {
        let hash = field(record, "sha256")?;
        let file = field(record, "file")?;
        if file != format!("{hash}.txt")
            || hash.len() != 64
            || !hash.bytes().all(|c| c.is_ascii_hexdigit())
        {
            bail!("invalid supplement filename");
        }
        let bytes = std::fs::read(directory.join(file))?;
        let mut digest = String::new();
        for byte in Sha256::digest(&bytes) {
            write!(digest, "{byte:02x}")?;
        }
        if digest != hash {
            bail!("license supplement checksum differs: {file}");
        }
        let key = (
            field(record, "name")?.to_owned(),
            field(record, "version")?.to_owned(),
        );
        let text = String::from_utf8(bytes)?;
        let source = field(record, "source")?.to_owned();
        let kind = field(record, "kind")?.to_owned();
        if supplements.insert(key, (kind, text, source)).is_some() {
            bail!("duplicate license supplement");
        }
    }
    let template = std::fs::read_to_string(root.join("build/license-notices.hbs"))?;
    let header = template
        .split_once("{{#each licenses}}")
        .context("notice template has no inventory marker")?
        .0;
    let rendered = render(&inventory, &supplements, header)?;
    std::fs::write(root.join("THIRD-PARTY-NOTICES.md"), rendered)?;
    Ok(())
}

type Supplements = BTreeMap<(String, String), (String, String, String)>;

fn render(inventory: &Value, supplements: &Supplements, header: &str) -> Result<String> {
    let mut notices = BTreeMap::<(String, String, String), Vec<String>>::new();
    for license in array(&inventory["licenses"])? {
        let original = field(license, "text")?;
        let id = field(license, "id")?;
        for usage in array(&license["used_by"])? {
            let name = field(&usage["crate"], "name")?;
            let version = field(&usage["crate"], "version")?;
            let mut text = original.to_owned();
            if original.contains(PLACEHOLDER) {
                if id != "MIT" {
                    bail!("unrecognized placeholder license: {id}");
                }
                let key = (name.to_owned(), version.to_owned());
                let (kind, upstream, source) = supplements
                    .get(&key)
                    .with_context(|| format!("missing license source for {name} {version}"))?;
                text = supplement_text(kind, upstream, source, original)?;
            }
            notices
                .entry((field(license, "name")?.to_owned(), id.to_owned(), text))
                .or_default()
                .push(format!("`{name} {version}`"));
        }
    }
    let mut rendered = header.to_owned();
    for ((name, id, text), mut packages) in notices {
        packages.sort();
        packages.dedup();
        write!(
            rendered,
            "## {name} ({id})\n\nUsed by: {}\n\n```text\n{text}\n```\n\n",
            packages.join(", ")
        )?;
    }
    Ok(rendered)
}

fn supplement_text(kind: &str, upstream: &str, source: &str, original: &str) -> Result<String> {
    match kind {
        "license" => Ok(upstream.to_owned()),
        "declaration" => {
            if upstream.to_ascii_lowercase().contains("copyright") {
                bail!("a source declaration contains a copyright notice requiring preservation");
            }
            Ok(format!("License declaration: {source}\n\n{upstream}\n\nMIT license terms (upstream supplies no copyright statement):\n\n{}", original.replace(&format!("{PLACEHOLDER}\n\n"), "")))
        }
        _ => bail!("unrecognized license supplement kind"),
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .with_context(|| format!("missing license field: {name}"))
}

fn array(value: &Value) -> Result<&[Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .context("missing license array")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_copyright_sources_fail_instead_of_emitting_placeholders() -> Result<()> {
        let inventory = serde_json::json!({"licenses": [{
            "name": "MIT License", "id": "MIT", "text": PLACEHOLDER,
            "used_by": [{"crate": {"name": "dependency", "version": "1.0.0"}}]
        }]});
        assert!(render(&inventory, &BTreeMap::new(), "").is_err());
        let supplements = BTreeMap::from([(
            ("dependency".into(), "1.0.0".into()),
            (
                "license".into(),
                "Copyright (c) Actual Author\nPermission is hereby granted".into(),
                "source".into(),
            ),
        )]);
        let rendered = render(&inventory, &supplements, "")?;
        assert!(rendered.contains("Copyright (c) Actual Author"));
        assert!(!rendered.contains(PLACEHOLDER));
        let changed_version = serde_json::json!({"licenses": [{
            "name": "MIT License", "id": "MIT", "text": PLACEHOLDER,
            "used_by": [{"crate": {"name": "dependency", "version": "2.0.0"}}]
        }]});
        assert!(render(&changed_version, &supplements, "").is_err());
        Ok(())
    }

    #[test]
    fn declarations_cannot_discard_an_upstream_copyright_line() {
        assert!(
            supplement_text("declaration", "Copyright (c) Author", "source", PLACEHOLDER).is_err()
        );
    }
}
