//! Reading and writing registry export files.
//!
//! Two forms exist. Version 4 is byte oriented text with the header `REGEDIT4`.
//! Version 5 is UTF-16 with the header `Windows Registry Editor Version 5.00`.
//! Detection runs through the shared text loader, so any encoding that loader
//! recognises is accepted and the header alone chooses the form.
//!
//! Byte ranges on the parsed items index the decoded text. They equal file
//! offsets only for a byte oriented encoding with no byte order mark.

use crate::bytes::to_u64;
use crate::error::{RecordError, Result};
use crate::limits::{Limits, RecordBudget};
use crate::record::{ByteRange, Record, RecordTree};
use crate::registry::value::{ValueData, ValueKind, ValueName};
use ca_text::{decode, encode, DecodeOptions, EncodingSpec, TextEncoding};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// Column the writer wraps a hexadecimal payload at.
const HEX_WRAP_COLUMN: usize = 76;

/// Header of a version 4 file.
const HEADER_V4: &str = "REGEDIT4";
/// Header of a version 5 file.
const HEADER_V5: &str = "Windows Registry Editor Version 5.00";

/// Which export form a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegFileVersion {
    /// `REGEDIT4`, written in a byte oriented encoding.
    V4,
    /// `Windows Registry Editor Version 5.00`, written in UTF-16.
    #[default]
    V5,
}

impl RegFileVersion {
    /// The header line of this form.
    #[must_use]
    pub const fn header(self) -> &'static str {
        match self {
            Self::V4 => HEADER_V4,
            Self::V5 => HEADER_V5,
        }
    }

    /// The encoding the writer uses for this form.
    fn encoding_spec(self) -> Result<EncodingSpec> {
        match self {
            Self::V5 => Ok(EncodingSpec {
                encoding: TextEncoding::Utf16Le,
                bom: true,
            }),
            Self::V4 => TextEncoding::from_label("windows-1252")
                .map(EncodingSpec::bare)
                .ok_or_else(|| RecordError::unsupported("the windows-1252 encoding is missing")),
        }
    }
}

/// One value entry inside a key block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegEntry {
    /// Name of the value.
    pub name: ValueName,
    /// Data of the value. Absent means the entry deletes the value.
    pub data: Option<ValueData>,
    /// Range of the entry in the decoded text.
    pub source: ByteRange,
}

impl RegEntry {
    /// True when the entry deletes the value instead of setting it.
    #[must_use]
    pub const fn is_delete(&self) -> bool {
        self.data.is_none()
    }
}

/// One key block: a `[path]` line and the entries under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegKeyBlock {
    /// Full key path, hive first.
    pub path: String,
    /// True when the block deletes the key, written `[-path]`.
    pub delete: bool,
    /// Value entries under the key.
    pub entries: Vec<RegEntry>,
    /// Range of the block in the decoded text.
    pub source: ByteRange,
}

/// A parsed registry export file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegFile {
    /// Which export form the file used.
    pub version: RegFileVersion,
    /// Key blocks in file order.
    pub keys: Vec<RegKeyBlock>,
}

impl RegFile {
    /// An empty file of the given form.
    #[must_use]
    pub const fn empty(version: RegFileVersion) -> Self {
        Self {
            version,
            keys: Vec::new(),
        }
    }

    /// Find a key block by path, ignoring case.
    #[must_use]
    pub fn key(&self, path: &str) -> Option<&RegKeyBlock> {
        self.keys
            .iter()
            .find(|block| block.path.eq_ignore_ascii_case(path))
    }

    /// Find a key block by path for editing, ignoring case.
    pub fn key_mut(&mut self, path: &str) -> Option<&mut RegKeyBlock> {
        self.keys
            .iter_mut()
            .find(|block| block.path.eq_ignore_ascii_case(path))
    }

    /// Parse a registry export file from its bytes.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::LimitExceeded`] when the file or one value is
    /// over a limit, and [`RecordError::Malformed`] when the header is missing
    /// or a line does not match the grammar.
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Self> {
        limits.check_input(to_u64(bytes.len()))?;
        let decoded = decode(bytes, &DecodeOptions::default());
        Self::parse_text(&decoded.text, limits)
    }

    /// Parse a registry export file from already decoded text.
    ///
    /// # Errors
    ///
    /// Same as [`RegFile::parse`].
    pub fn parse_text(text: &str, limits: &Limits) -> Result<Self> {
        limits.check_input(to_u64(text.len()))?;
        let mut budget = RecordBudget::new(limits);
        let lines = split_lines(text);
        let mut index = 0usize;
        let version = read_header(&lines, &mut index)?;
        let mut keys: Vec<RegKeyBlock> = Vec::new();

        while index < lines.len() {
            let line = lines[index];
            let trimmed = line.text.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') {
                index += 1;
                continue;
            }
            if trimmed.starts_with('[') {
                let block = parse_key_line(line, trimmed, limits)?;
                keys.push(block);
                index += 1;
                continue;
            }
            let (joined, start, end, next) = join_continuations(&lines, index);
            index = next;
            let Some(block) = keys.last_mut() else {
                return Err(RecordError::malformed(
                    "registry export value outside a key block",
                    to_u64(start),
                ));
            };
            budget.spend()?;
            let entry = parse_entry(&joined, start, end, version, limits)?;
            block.source.len = to_u64(end).saturating_sub(block.source.start);
            block.entries.push(entry);
        }
        Ok(Self { version, keys })
    }

    /// Write the file back to bytes in its own form.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::Unsupported`] when the chosen encoding cannot
    /// represent a character the file holds, or a key or value name contains
    /// a control character that the export grammar cannot quote safely.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        for block in &self.keys {
            if block.path.chars().any(char::is_control) {
                return Err(RecordError::unsupported(
                    "a registry key path contains a control character and cannot be written as an export file",
                ));
            }
            if block.entries.iter().any(|entry| {
                matches!(&entry.name, ValueName::Named { name, .. } if name.chars().any(char::is_control))
            }) {
                return Err(RecordError::unsupported(
                    "a registry value name contains a control character and cannot be written as an export file",
                ));
            }
        }
        if self.version == RegFileVersion::V4 {
            for data in self
                .keys
                .iter()
                .flat_map(|block| block.entries.iter().filter_map(|entry| entry.data.as_ref()))
            {
                for text in legacy_text_values(data) {
                    if let Some(bad) = encode(text, self.version.encoding_spec()?).unmappable {
                        return Err(RecordError::unsupported(format!(
                            "the file encoding cannot represent the character {:?}",
                            bad.ch
                        )));
                    }
                }
            }
        }
        let text = self.to_text();
        let spec = self.version.encoding_spec()?;
        let outcome = encode(&text, spec);
        if let Some(bad) = outcome.unmappable {
            return Err(RecordError::unsupported(format!(
                "the file encoding cannot represent the character {:?}",
                bad.ch
            )));
        }
        Ok(outcome.bytes)
    }

    /// Write the file back to text, without applying an encoding.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(self.version.header());
        out.push_str("\r\n");
        for block in &self.keys {
            out.push_str("\r\n[");
            if block.delete {
                out.push('-');
            }
            out.push_str(&block.path);
            out.push_str("]\r\n");
            for entry in &block.entries {
                match &entry.name {
                    ValueName::Default => out.push('@'),
                    ValueName::Named { name, .. } => {
                        out.push('"');
                        out.push_str(&escape_string(name));
                        out.push('"');
                    }
                }
                out.push('=');
                match &entry.data {
                    None => out.push('-'),
                    Some(data) => write_data(&mut out, data, self.version),
                }
                out.push_str("\r\n");
            }
        }
        out
    }

    /// Build the record tree the comparison reads.
    ///
    /// Key paths become nested groups. A deletion block contributes a group
    /// marked by a record named `(Deleted)`, so a deletion is visible in the
    /// comparison instead of silently absent.
    #[must_use]
    pub fn to_tree(&self, root_name: &str) -> RecordTree {
        let mut root = RecordTree::new(String::new(), root_name.to_owned());
        for block in &self.keys {
            let Some(node) = ensure_path(&mut root, &block.path) else {
                continue;
            };
            node.source = Some(block.source);
            if block.delete {
                node.push(
                    Record::new(
                        block.path.clone(),
                        "(Deleted)",
                        "REG_KEY_DELETE",
                        crate::record::RecordValue::Empty,
                    )
                    .with_source(block.source),
                );
            }
            for entry in &block.entries {
                let (type_name, value, display) = match &entry.data {
                    None => (
                        "REG_VALUE_DELETE".to_owned(),
                        crate::record::RecordValue::Empty,
                        String::new(),
                    ),
                    Some(data) => (
                        data.kind().name(),
                        data.to_record_value(),
                        data.to_display(),
                    ),
                };
                node.push(
                    Record::new(
                        block.path.clone(),
                        entry.name.display().to_owned(),
                        type_name,
                        value,
                    )
                    .with_display(display)
                    .with_source(entry.source),
                );
            }
        }
        root.sort_by_name();
        root
    }
}

/// Largest number of path segments a key contributes to the tree.
///
/// A deeper path is grouped at this depth. The cap bounds the descent, so a
/// hostile path cannot drive it without end.
const MAX_KEY_SEGMENTS: usize = 256;

/// Find or create the group chain for a key path.
fn ensure_path<'a>(root: &'a mut RecordTree, path: &str) -> Option<&'a mut RecordTree> {
    let parts: Vec<&str> = path
        .split('\\')
        .filter(|part| !part.is_empty())
        .take(MAX_KEY_SEGMENTS)
        .collect();
    descend(root, &parts, String::new())
}

fn descend<'a>(
    node: &'a mut RecordTree,
    parts: &[&str],
    walked: String,
) -> Option<&'a mut RecordTree> {
    let Some((part, rest)) = parts.split_first() else {
        return Some(node);
    };
    let mut full = walked;
    if !full.is_empty() {
        full.push('\\');
    }
    full.push_str(part);
    let existing = node
        .children
        .iter()
        .position(|child| child.name.eq_ignore_ascii_case(part));
    let index = if let Some(index) = existing {
        index
    } else {
        node.children
            .push(RecordTree::new(full.clone(), (*part).to_owned()));
        node.children.len().saturating_sub(1)
    };
    node.children
        .get_mut(index)
        .and_then(|child| descend(child, rest, full))
}

#[derive(Debug, Clone, Copy)]
struct SourceLine<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

fn split_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let mut end = index;
            if end > start && bytes.get(end - 1) == Some(&b'\r') {
                end -= 1;
            }
            out.push(SourceLine {
                start,
                end: index + 1,
                text: text.get(start..end).unwrap_or_default(),
            });
            start = index + 1;
        }
    }
    if start < bytes.len() {
        out.push(SourceLine {
            start,
            end: bytes.len(),
            text: text.get(start..).unwrap_or_default(),
        });
    }
    out
}

fn read_header(lines: &[SourceLine<'_>], index: &mut usize) -> Result<RegFileVersion> {
    while let Some(line) = lines.get(*index) {
        let trimmed = line.text.trim().trim_start_matches('\u{feff}').trim();
        *index += 1;
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.eq_ignore_ascii_case(HEADER_V4) {
            return Ok(RegFileVersion::V4);
        }
        if trimmed
            .to_ascii_lowercase()
            .starts_with(&HEADER_V5.to_ascii_lowercase())
        {
            return Ok(RegFileVersion::V5);
        }
        return Err(RecordError::malformed(
            "registry export header",
            to_u64(line.start),
        ));
    }
    Err(RecordError::malformed("registry export header", 0))
}

fn parse_key_line(line: SourceLine<'_>, trimmed: &str, limits: &Limits) -> Result<RegKeyBlock> {
    let Some(close) = trimmed.rfind(']') else {
        return Err(RecordError::malformed(
            "registry export key line",
            to_u64(line.start),
        ));
    };
    let inner = trimmed.get(1..close).unwrap_or_default();
    let delete = inner.starts_with('-');
    let path = if delete {
        inner.get(1..).unwrap_or_default()
    } else {
        inner
    };
    if path.is_empty() {
        return Err(RecordError::malformed(
            "registry export key path",
            to_u64(line.start),
        ));
    }
    limits.check_name(to_u64(path.len()))?;
    Ok(RegKeyBlock {
        path: path.to_owned(),
        delete,
        entries: Vec::new(),
        source: ByteRange::new(to_u64(line.start), to_u64(line.end - line.start)),
    })
}

/// Join a value line with the lines a trailing backslash continues it onto.
fn join_continuations(
    lines: &[SourceLine<'_>],
    start_index: usize,
) -> (String, usize, usize, usize) {
    let mut joined = String::new();
    let mut index = start_index;
    let start = lines.get(start_index).map_or(0, |line| line.start);
    let mut end = start;
    while let Some(line) = lines.get(index) {
        let piece = if index == start_index {
            line.text
        } else {
            line.text.trim_start()
        };
        let trimmed = piece.trim_end();
        end = line.end;
        index += 1;
        if let Some(head) = trimmed.strip_suffix('\\') {
            joined.push_str(head.trim_end());
            continue;
        }
        joined.push_str(trimmed);
        break;
    }
    (joined, start, end, index.max(start_index + 1))
}

fn parse_entry(
    line: &str,
    start: usize,
    end: usize,
    version: RegFileVersion,
    limits: &Limits,
) -> Result<RegEntry> {
    let source = ByteRange::new(to_u64(start), to_u64(end.saturating_sub(start)));
    let trimmed = line.trim();
    let (name, rest) = split_name(trimmed, start, limits)?;
    let data_text = rest.trim();
    if data_text == "-" {
        return Ok(RegEntry {
            name,
            data: None,
            source,
        });
    }
    let data = parse_data(data_text, start, version, limits)?;
    Ok(RegEntry {
        name,
        data: Some(data),
        source,
    })
}

fn split_name<'a>(line: &'a str, start: usize, limits: &Limits) -> Result<(ValueName, &'a str)> {
    if let Some(rest) = line.strip_prefix('@') {
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            return Err(RecordError::malformed(
                "registry export default value",
                to_u64(start),
            ));
        };
        return Ok((ValueName::Default, rest));
    }
    if !line.starts_with('"') {
        return Err(RecordError::malformed(
            "registry export value name",
            to_u64(start),
        ));
    }
    let mut name = String::new();
    let mut chars = line.char_indices().skip(1);
    let mut closed = None;
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some((_, next @ ('\\' | '"'))) => name.push(next),
                Some((_, next)) => {
                    name.push('\\');
                    name.push(next);
                }
                None => {
                    return Err(RecordError::malformed(
                        "registry export value name",
                        to_u64(start),
                    ))
                }
            },
            '"' => {
                closed = Some(index);
                break;
            }
            other => name.push(other),
        }
    }
    let Some(close) = closed else {
        return Err(RecordError::malformed(
            "registry export value name",
            to_u64(start),
        ));
    };
    limits.check_name(to_u64(name.len()))?;
    let rest = line.get(close + 1..).unwrap_or_default().trim_start();
    let Some(rest) = rest.strip_prefix('=') else {
        return Err(RecordError::malformed(
            "registry export value separator",
            to_u64(start),
        ));
    };
    Ok((ValueName::from_raw(&name), rest))
}

fn parse_data(
    text: &str,
    start: usize,
    version: RegFileVersion,
    limits: &Limits,
) -> Result<ValueData> {
    if text.starts_with('"') {
        let value = parse_quoted(text, start)?;
        limits.check_value(to_u64(value.len()))?;
        return Ok(ValueData::Sz(value));
    }
    let lower = text.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix("dword:") {
        let digits = digits.trim();
        let value = u32::from_str_radix(digits, 16)
            .map_err(|_| RecordError::malformed("registry export dword", to_u64(start)))?;
        return Ok(ValueData::Dword(value));
    }
    if let Some(rest) = lower.strip_prefix("hex") {
        let (kind, payload) = split_hex_kind(rest, start)?;
        let bytes = parse_hex_bytes(payload, start, limits)?;
        if version == RegFileVersion::V4 && matches!(kind, ValueKind::ExpandSz | ValueKind::MultiSz)
        {
            return value_from_v4_raw(kind, &bytes, limits);
        }
        return ValueData::from_raw(kind, &bytes, limits);
    }
    Err(RecordError::malformed(
        "registry export value data",
        to_u64(start),
    ))
}

fn split_hex_kind(rest: &str, start: usize) -> Result<(ValueKind, &str)> {
    if let Some(after) = rest.strip_prefix('(') {
        let Some(close) = after.find(')') else {
            return Err(RecordError::malformed(
                "registry export hex type",
                to_u64(start),
            ));
        };
        let digits = after.get(..close).unwrap_or_default().trim();
        let code = u32::from_str_radix(digits, 16)
            .map_err(|_| RecordError::malformed("registry export hex type", to_u64(start)))?;
        let tail = after.get(close + 1..).unwrap_or_default();
        let Some(payload) = tail.strip_prefix(':') else {
            return Err(RecordError::malformed(
                "registry export hex separator",
                to_u64(start),
            ));
        };
        return Ok((ValueKind::from_code(code), payload));
    }
    let Some(payload) = rest.strip_prefix(':') else {
        return Err(RecordError::malformed(
            "registry export hex separator",
            to_u64(start),
        ));
    };
    Ok((ValueKind::Binary, payload))
}

fn parse_hex_bytes(payload: &str, start: usize, limits: &Limits) -> Result<Vec<u8>> {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    // One byte needs at least two characters with its separator, so the token
    // count bounds the payload before any allocation happens.
    let upper = to_u64(trimmed.len()).saturating_add(1) / 2;
    limits.check_value(upper)?;
    let mut out = Vec::new();
    for token in trimmed.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token.len() > 2 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RecordError::malformed(
                "registry export hex byte",
                to_u64(start),
            ));
        }
        let value = u8::from_str_radix(token, 16)
            .map_err(|_| RecordError::malformed("registry export hex byte", to_u64(start)))?;
        out.push(value);
    }
    limits.check_value(to_u64(out.len()))?;
    Ok(out)
}

fn parse_quoted(text: &str, start: usize) -> Result<String> {
    let mut out = String::new();
    let mut chars = text.char_indices().skip(1);
    while let Some((_, ch)) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some((_, next @ ('\\' | '"'))) => out.push(next),
                Some((_, next)) => {
                    out.push('\\');
                    out.push(next);
                }
                None => {
                    return Err(RecordError::malformed(
                        "registry export string",
                        to_u64(start),
                    ))
                }
            },
            '"' => return Ok(out),
            other => out.push(other),
        }
    }
    Err(RecordError::malformed(
        "registry export string",
        to_u64(start),
    ))
}

fn escape_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\\' || ch == '"' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn write_data(out: &mut String, data: &ValueData, version: RegFileVersion) {
    match data {
        ValueData::Sz(text) if !text.chars().any(char::is_control) => {
            out.push('"');
            out.push_str(&escape_string(text));
            out.push('"');
        }
        ValueData::Dword(value) => {
            let _ = write!(out, "dword:{value:08x}");
        }
        other => {
            let kind = other.kind();
            if kind == ValueKind::Binary {
                out.push_str("hex:");
            } else {
                let _ = write!(out, "hex({:x}):", kind.code());
            }
            let raw = match (version, other) {
                (
                    RegFileVersion::V4,
                    ValueData::Preserved {
                        legacy_v4_bytes: Some(bytes),
                        ..
                    },
                ) => bytes.clone(),
                (
                    RegFileVersion::V4,
                    ValueData::Preserved {
                        kind: ValueKind::ExpandSz | ValueKind::MultiSz,
                        decoded,
                        ..
                    },
                ) => legacy_raw(decoded, version),
                (RegFileVersion::V4, ValueData::ExpandSz(_) | ValueData::MultiSz(_)) => {
                    legacy_raw(other, version)
                }
                _ => other.to_raw(),
            };
            write_hex(out, &raw);
        }
    }
}

fn value_from_v4_raw(kind: ValueKind, bytes: &[u8], limits: &Limits) -> Result<ValueData> {
    limits.check_value(to_u64(bytes.len()))?;
    let encoding = TextEncoding::from_label("windows-1252")
        .ok_or_else(|| RecordError::unsupported("the windows-1252 encoding is missing"))?;
    let decode_bytes = |input: &[u8]| {
        decode(
            input,
            &DecodeOptions {
                forced: Some(encoding),
                ..DecodeOptions::default()
            },
        )
        .text
    };
    let decoded = match kind {
        ValueKind::ExpandSz => {
            let payload = bytes.strip_suffix(&[0]).unwrap_or(bytes);
            ValueData::ExpandSz(decode_bytes(payload))
        }
        ValueKind::MultiSz => {
            let mut items = Vec::new();
            for part in bytes.split(|byte| *byte == 0) {
                if part.is_empty() {
                    break;
                }
                items.push(decode_bytes(part));
            }
            ValueData::MultiSz(items)
        }
        _ => return ValueData::from_raw(kind, bytes, limits),
    };
    if legacy_raw(&decoded, RegFileVersion::V4) == bytes {
        Ok(decoded)
    } else {
        let registry_bytes = decode_bytes(bytes)
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        Ok(ValueData::Preserved {
            kind,
            bytes: registry_bytes,
            decoded: Box::new(decoded),
            legacy_v4_bytes: Some(bytes.to_vec()),
        })
    }
}

fn legacy_text_values(data: &ValueData) -> Vec<&str> {
    match data {
        ValueData::ExpandSz(text) => vec![text],
        ValueData::MultiSz(items) => items.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    }
}

fn legacy_raw(data: &ValueData, version: RegFileVersion) -> Vec<u8> {
    let encode_text = |text: &str| {
        encode(
            text,
            version
                .encoding_spec()
                .unwrap_or_else(|_| EncodingSpec::bare(TextEncoding::Utf8)),
        )
        .bytes
    };
    match data {
        ValueData::ExpandSz(text) => {
            let mut bytes = encode_text(text);
            bytes.push(0);
            bytes
        }
        ValueData::MultiSz(items) => {
            let mut bytes = if items.is_empty() {
                vec![0]
            } else {
                Vec::new()
            };
            for item in items {
                bytes.extend(encode_text(item));
                bytes.push(0);
            }
            bytes.push(0);
            bytes
        }
        other => other.to_raw(),
    }
}

/// Write a payload as comma separated hexadecimal, wrapped with continuations.
fn write_hex(out: &mut String, bytes: &[u8]) {
    let mut column = out.len() - out.rfind('\n').map_or(0, |index| index + 1);
    for (index, byte) in bytes.iter().enumerate() {
        let piece = format!("{byte:02x}");
        if index > 0 {
            out.push(',');
            column += 1;
            if column >= HEX_WRAP_COLUMN {
                out.push_str("\\\r\n  ");
                column = 2;
            }
        }
        out.push_str(&piece);
        column += piece.len();
    }
}
