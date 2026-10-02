//! The resource directory and the version information block.

use std::collections::BTreeSet;

use crate::bytes::{slice, to_u64, utf16_le_string};
use crate::error::{RecordError, Result};
use crate::limits::{Limits, RecordBudget};
use crate::record::{ByteRange, Record, RecordTree, RecordValue};
use crate::version::pe::{self, FileFacts};
use crate::version::{LanguageChoice, VersionReadOptions};

/// Resource type identifier of a version block.
const RT_VERSION: u32 = 16;
/// Size of one resource directory header.
const RESOURCE_DIR_HEADER: usize = 16;
/// Size of one resource directory entry.
const RESOURCE_DIR_ENTRY: usize = 8;
/// Bit that marks a directory entry as pointing at another directory.
const SUBDIRECTORY_BIT: u32 = 0x8000_0000;
/// Largest number of resource entries read at one level.
const MAX_RESOURCE_ENTRIES: u32 = 65_535;
/// Signature of the fixed file information structure.
const FIXED_INFO_SIGNATURE: u32 = 0xFEEF_04BD;
/// Size of the fixed file information structure.
const FIXED_INFO_SIZE: usize = 52;

/// The fixed part of a version resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FixedFileInfo {
    /// File version, most significant half.
    pub file_version_ms: u32,
    /// File version, least significant half.
    pub file_version_ls: u32,
    /// Product version, most significant half.
    pub product_version_ms: u32,
    /// Product version, least significant half.
    pub product_version_ls: u32,
    /// Which flag bits carry meaning.
    pub file_flags_mask: u32,
    /// Flag bits.
    pub file_flags: u32,
    /// Target operating system.
    pub file_os: u32,
    /// Kind of file.
    pub file_type: u32,
    /// Sub kind of file.
    pub file_subtype: u32,
    /// File date, most significant half.
    pub file_date_ms: u32,
    /// File date, least significant half.
    pub file_date_ls: u32,
}

impl FixedFileInfo {
    /// The file version in dotted form.
    #[must_use]
    pub fn file_version(&self) -> String {
        dotted(self.file_version_ms, self.file_version_ls)
    }

    /// The product version in dotted form.
    #[must_use]
    pub fn product_version(&self) -> String {
        dotted(self.product_version_ms, self.product_version_ls)
    }
}

/// One string table of a version resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringTable {
    /// Identifier as the file writes it, for example `040904B0`.
    pub id: String,
    /// Language part of the identifier.
    pub language: u16,
    /// Code page part of the identifier.
    pub code_page: u16,
    /// Name and text of each entry, in file order.
    pub entries: Vec<(String, String, ByteRange)>,
}

/// A parsed version resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// The fixed part, absent when the block carries none.
    pub fixed: Option<FixedFileInfo>,
    /// String tables in file order.
    pub tables: Vec<StringTable>,
    /// Language and code page pairs the translation list names.
    pub translations: Vec<(u16, u16)>,
    /// Facts read from the headers.
    pub facts: FileFacts,
    /// Range of the version block in the file.
    pub source: ByteRange,
}

/// Read a version resource and build the record tree.
pub(crate) fn read_tree(bytes: &[u8], options: &VersionReadOptions) -> Result<RecordTree> {
    let info = read(bytes, options)?;
    Ok(build_tree(&info, options))
}

/// Read a version resource without building a tree.
///
/// # Errors
///
/// See [`crate::version::read`].
pub fn read(bytes: &[u8], options: &VersionReadOptions) -> Result<VersionInfo> {
    let limits = &options.limits;
    let image = pe::parse(bytes, limits)?;
    let Some(directory) = image.facts.resource else {
        return Err(RecordError::NotFound {
            path: "resource directory".to_owned(),
        });
    };
    let base = directory.file_offset;
    let leaves = find_version_leaves(bytes, base, limits)?;
    let Some(leaf) = pick_leaf(&leaves, &options.language) else {
        return Err(RecordError::NotFound {
            path: "version resource".to_owned(),
        });
    };
    let Some(offset) = image.offset_of(leaf.data_rva) else {
        return Err(RecordError::malformed(
            "version resource address",
            u64::from(leaf.data_rva),
        ));
    };
    let size = usize::try_from(leaf.size)
        .map_err(|_| RecordError::malformed("version resource size", to_u64(offset)))?;
    limits.check_value(to_u64(size))?;
    let block = slice(bytes, offset, size, "version resource")?;
    let mut info = parse_version_block(block, offset, limits)?;
    info.facts = image.facts.clone();
    info.source = ByteRange::new(to_u64(offset), to_u64(size));
    Ok(info)
}

#[derive(Debug, Clone, Copy)]
struct Leaf {
    language: u16,
    data_rva: u32,
    size: u32,
}

fn pick_leaf(leaves: &[Leaf], choice: &LanguageChoice) -> Option<Leaf> {
    match choice {
        // The operating system resolves version information against the
        // neutral language first, so the listing does the same.
        LanguageChoice::Neutral => leaves
            .iter()
            .find(|leaf| leaf.language == 0)
            .or_else(|| leaves.first())
            .copied(),
        LanguageChoice::First | LanguageChoice::Unknown(_) => leaves.first().copied(),
        LanguageChoice::Specific { language, .. } => leaves
            .iter()
            .find(|leaf| leaf.language == *language)
            .or_else(|| leaves.first())
            .copied(),
    }
}

/// Walk the three levels of the resource directory and collect version leaves.
fn find_version_leaves(bytes: &[u8], base: usize, limits: &Limits) -> Result<Vec<Leaf>> {
    let mut budget = RecordBudget::new(limits);
    walk_version_leaves(bytes, base, &mut budget)
}

/// The walk behind [`find_version_leaves`], charging `budget` for every
/// directory entry it reads and every leaf it keeps.
///
/// Entries that name one shared child directory multiply the work by the
/// entry count of every level above it, so each directory is read at most
/// once in a walk and a second visit ends it with an error.
fn walk_version_leaves(bytes: &[u8], base: usize, budget: &mut RecordBudget) -> Result<Vec<Leaf>> {
    let mut visited: BTreeSet<usize> = BTreeSet::new();
    let mut out = Vec::new();
    for type_entry in directory_entries(bytes, base, budget, &mut visited)? {
        if type_entry.id != RT_VERSION || !type_entry.is_directory {
            continue;
        }
        let level2 = base
            .checked_add(type_entry.offset)
            .ok_or_else(|| RecordError::malformed("resource directory", to_u64(base)))?;
        for name_entry in directory_entries(bytes, level2, budget, &mut visited)? {
            if !name_entry.is_directory {
                continue;
            }
            let level3 = base
                .checked_add(name_entry.offset)
                .ok_or_else(|| RecordError::malformed("resource directory", to_u64(base)))?;
            for lang_entry in directory_entries(bytes, level3, budget, &mut visited)? {
                if lang_entry.is_directory {
                    continue;
                }
                let data_entry = base
                    .checked_add(lang_entry.offset)
                    .ok_or_else(|| RecordError::malformed("resource data entry", to_u64(base)))?;
                let raw = slice(bytes, data_entry, 16, "resource data entry")?;
                budget.spend()?;
                out.push(Leaf {
                    language: u16::try_from(lang_entry.id & 0xFFFF).unwrap_or(0),
                    data_rva: read_u32(raw, 0),
                    size: read_u32(raw, 4),
                });
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct DirEntry {
    id: u32,
    offset: usize,
    is_directory: bool,
}

fn directory_entries(
    bytes: &[u8],
    at: usize,
    budget: &mut RecordBudget,
    visited: &mut BTreeSet<usize>,
) -> Result<Vec<DirEntry>> {
    if !visited.insert(at) {
        return Err(RecordError::malformed(
            "resource directory named by more than one entry",
            to_u64(at),
        ));
    }
    let header = slice(bytes, at, RESOURCE_DIR_HEADER, "resource directory")?;
    let named = u32::from(u16::from_le_bytes([header[12], header[13]]));
    let ids = u32::from(u16::from_le_bytes([header[14], header[15]]));
    let total = named.saturating_add(ids);
    if total > MAX_RESOURCE_ENTRIES {
        return Err(RecordError::LimitExceeded {
            limit: "resourceEntries",
            allowed: u64::from(MAX_RESOURCE_ENTRIES),
            requested: u64::from(total),
        });
    }
    budget.spend_many(u64::from(total))?;
    let mut out = Vec::new();
    for index in 0..total {
        let position = at
            .checked_add(RESOURCE_DIR_HEADER)
            .and_then(|start| {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_mul(RESOURCE_DIR_ENTRY))
                    .and_then(|delta| start.checked_add(delta))
            })
            .ok_or_else(|| RecordError::malformed("resource directory", to_u64(at)))?;
        let raw = slice(bytes, position, RESOURCE_DIR_ENTRY, "resource directory")?;
        let id = read_u32(raw, 0);
        let pointer = read_u32(raw, 4);
        out.push(DirEntry {
            id,
            offset: usize::try_from(pointer & !SUBDIRECTORY_BIT).unwrap_or(0),
            is_directory: pointer & SUBDIRECTORY_BIT != 0,
        });
    }
    Ok(out)
}

/// One `VS_VERSIONINFO` style block header with its value and children.
#[derive(Debug, Clone)]
struct Block<'a> {
    key: String,
    value: &'a [u8],
    children: &'a [u8],
    value_offset: usize,
    total: usize,
}

fn parse_block(data: &[u8], at: usize) -> Result<Block<'_>> {
    let header = slice(data, 0, 6, "version block")?;
    let length = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let value_length = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let text_type = u16::from_le_bytes([header[4], header[5]]) == 1;
    if length < 6 || length > data.len() {
        return Err(RecordError::truncated(
            "version block",
            to_u64(at),
            to_u64(length),
            to_u64(data.len()),
        ));
    }
    let (key, taken) = utf16_le_string(data.get(6..length).unwrap_or_default());
    let header_end = align4(6usize.saturating_add(taken)).min(length);
    let value_bytes = if text_type {
        value_length.saturating_mul(2)
    } else {
        value_length
    };
    let value_end = header_end.saturating_add(value_bytes).min(length);
    let value = data.get(header_end..value_end).unwrap_or_default();
    let children_start = align4(value_end).min(length);
    let children = data.get(children_start..length).unwrap_or_default();
    Ok(Block {
        key,
        value,
        children,
        value_offset: at.saturating_add(header_end),
        total: length,
    })
}

/// Walk the sibling blocks of one child list.
fn children_of<'a>(
    data: &'a [u8],
    at: usize,
    budget: &mut RecordBudget,
) -> Result<Vec<(Block<'a>, usize)>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 6 <= data.len() {
        let rest = data.get(offset..).unwrap_or_default();
        // Alignment padding follows the last child, so a block that does not
        // parse ends the sibling list instead of failing the whole resource.
        let Ok(block) = parse_block(rest, at.saturating_add(offset)) else {
            break;
        };
        budget.spend()?;
        let length = block.total;
        out.push((block, at.saturating_add(offset)));
        // A zero or unaligned length would loop forever, so the step is forced
        // forward by at least one aligned block header.
        let step = align4(length).max(4);
        offset = offset.saturating_add(step);
    }
    Ok(out)
}

fn parse_version_block(block: &[u8], file_offset: usize, limits: &Limits) -> Result<VersionInfo> {
    let mut budget = RecordBudget::new(limits);
    let root = parse_block(block, file_offset)?;
    if !root.key.eq_ignore_ascii_case("VS_VERSION_INFO") {
        return Err(RecordError::malformed(
            "version block key",
            to_u64(file_offset),
        ));
    }
    let fixed = parse_fixed(root.value);
    let mut tables = Vec::new();
    let mut translations = Vec::new();
    let root_children = root.children;
    let children_at = file_offset + (root.total - root_children.len());

    for (section, section_at) in children_of(root_children, children_at, &mut budget)? {
        if section.key.eq_ignore_ascii_case("StringFileInfo") {
            let table_at = section_at + (section.total - section.children.len());
            for (table, entry_at) in children_of(section.children, table_at, &mut budget)? {
                let (language, code_page) = split_table_id(&table.key);
                let mut entries = Vec::new();
                let inner_at = entry_at + (table.total - table.children.len());
                for (item, item_at) in children_of(table.children, inner_at, &mut budget)? {
                    let (text, _) = utf16_le_string(item.value);
                    limits.check_name(to_u64(item.key.len()))?;
                    entries.push((
                        item.key.clone(),
                        text,
                        ByteRange::new(to_u64(item_at), to_u64(item.total)),
                    ));
                }
                tables.push(StringTable {
                    id: table.key.clone(),
                    language,
                    code_page,
                    entries,
                });
            }
        } else if section.key.eq_ignore_ascii_case("VarFileInfo") {
            let var_at = section_at + (section.total - section.children.len());
            for (var, _) in children_of(section.children, var_at, &mut budget)? {
                if !var.key.eq_ignore_ascii_case("Translation") {
                    continue;
                }
                let mut offset = 0usize;
                while offset + 4 <= var.value.len() {
                    let raw = var.value.get(offset..offset + 4).unwrap_or_default();
                    translations.push((
                        u16::from_le_bytes([raw[0], raw[1]]),
                        u16::from_le_bytes([raw[2], raw[3]]),
                    ));
                    offset += 4;
                }
            }
        }
    }

    Ok(VersionInfo {
        fixed,
        tables,
        translations,
        facts: FileFacts {
            machine: crate::version::Machine(0),
            is_64_bit: false,
            time_stamp: 0,
            subsystem: crate::version::Subsystem(0),
            dll_characteristics: 0,
            has_signature_block: false,
            resource: None,
        },
        source: ByteRange::new(to_u64(root.value_offset), to_u64(root.total)),
    })
}

fn parse_fixed(value: &[u8]) -> Option<FixedFileInfo> {
    if value.len() < FIXED_INFO_SIZE || read_u32(value, 0) != FIXED_INFO_SIGNATURE {
        return None;
    }
    Some(FixedFileInfo {
        file_version_ms: read_u32(value, 8),
        file_version_ls: read_u32(value, 12),
        product_version_ms: read_u32(value, 16),
        product_version_ls: read_u32(value, 20),
        file_flags_mask: read_u32(value, 24),
        file_flags: read_u32(value, 28),
        file_os: read_u32(value, 32),
        file_type: read_u32(value, 36),
        file_subtype: read_u32(value, 40),
        file_date_ms: read_u32(value, 44),
        file_date_ls: read_u32(value, 48),
    })
}

fn build_tree(info: &VersionInfo, options: &VersionReadOptions) -> RecordTree {
    let mut root = RecordTree::new(String::new(), "Version".to_owned());
    root.source = Some(info.source);

    if let Some(fixed) = info.fixed {
        root.push_child(fixed_group(&fixed));
    }

    if !info.tables.is_empty() {
        let mut strings = RecordTree::new("StringFileInfo".to_owned(), "StringFileInfo".to_owned());
        for table in &info.tables {
            let path = format!("StringFileInfo\\{}", table.id);
            let mut group = RecordTree::new(path.clone(), table.id.clone());
            for (name, text, range) in &table.entries {
                group.push(
                    Record::new(&path, name, "STRING", RecordValue::Text(text.clone()))
                        .with_source(*range),
                );
            }
            strings.push_child(group);
        }
        root.push_child(strings);
    }

    if !info.translations.is_empty() {
        let path = "VarFileInfo";
        let mut group = RecordTree::new(path.to_owned(), path.to_owned());
        let items: Vec<String> = info
            .translations
            .iter()
            .map(|(language, code_page)| format!("{language:04X}{code_page:04X}"))
            .collect();
        group.push(
            Record::new(
                path,
                "Translation",
                "TRANSLATION",
                RecordValue::TextList(items.clone()),
            )
            .with_display(items.join(", ")),
        );
        root.push_child(group);
    }

    if options.include_file_facts {
        root.push_child(facts_group(&info.facts, options.include_signature_presence));
    }

    root.sort_by_name();
    root
}

fn fixed_group(fixed: &FixedFileInfo) -> RecordTree {
    let path = "FixedFileInfo";
    let mut group = RecordTree::new(path.to_owned(), path.to_owned());
    group.push(Record::new(
        path,
        "File Version",
        "VERSION",
        RecordValue::Text(fixed.file_version()),
    ));
    group.push(Record::new(
        path,
        "Product Version",
        "VERSION",
        RecordValue::Text(fixed.product_version()),
    ));
    group.push(
        Record::new(
            path,
            "File Flags",
            "FLAGS",
            RecordValue::TextList(file_flag_names(fixed.file_flags & fixed.file_flags_mask)),
        )
        .with_display(join_or_none(&file_flag_names(
            fixed.file_flags & fixed.file_flags_mask,
        ))),
    );
    group.push(
        Record::new(
            path,
            "Operating System",
            "ENUM",
            RecordValue::Integer(i128::from(fixed.file_os)),
        )
        .with_display(file_os_name(fixed.file_os)),
    );
    group.push(
        Record::new(
            path,
            "File Type",
            "ENUM",
            RecordValue::Integer(i128::from(fixed.file_type)),
        )
        .with_display(file_type_name(fixed.file_type)),
    );
    group.push(
        Record::new(
            path,
            "File Subtype",
            "ENUM",
            RecordValue::Integer(i128::from(fixed.file_subtype)),
        )
        .with_display(format!("0x{:08X}", fixed.file_subtype)),
    );
    group.push(
        Record::new(
            path,
            "File Date",
            "DATE",
            RecordValue::Integer(i128::from(
                (u64::from(fixed.file_date_ms) << 32) | u64::from(fixed.file_date_ls),
            )),
        )
        .with_display(format!(
            "0x{:08X}{:08X}",
            fixed.file_date_ms, fixed.file_date_ls
        )),
    );
    group
}

fn facts_group(facts: &FileFacts, include_signature: bool) -> RecordTree {
    let path = "File";
    let mut group = RecordTree::new(path.to_owned(), path.to_owned());
    group.push(Record::new(
        path,
        "Processor",
        "ENUM",
        RecordValue::Text(facts.machine.name()),
    ));
    group.push(Record::new(
        path,
        "Word Size",
        "ENUM",
        RecordValue::Text(facts.word_size().to_owned()),
    ));
    group.push(
        Record::new(
            path,
            "Time Stamp",
            "TIME",
            RecordValue::Integer(i128::from(facts.time_stamp)),
        )
        .with_display(format!("0x{:08X}", facts.time_stamp)),
    );
    group.push(Record::new(
        path,
        "Subsystem",
        "ENUM",
        RecordValue::Text(facts.subsystem.name()),
    ));
    let flags = facts.dll_flag_names();
    group.push(
        Record::new(
            path,
            "DLL Flags",
            "FLAGS",
            RecordValue::TextList(flags.clone()),
        )
        .with_display(join_or_none(&flags)),
    );
    if include_signature {
        group.push(Record::new(
            path,
            "Signature Block",
            "PRESENCE",
            RecordValue::Text(
                if facts.has_signature_block {
                    "present"
                } else {
                    "absent"
                }
                .to_owned(),
            ),
        ));
    }
    group
}

fn split_table_id(id: &str) -> (u16, u16) {
    let language = id
        .get(..4)
        .and_then(|part| u16::from_str_radix(part, 16).ok())
        .unwrap_or(0);
    let code_page = id
        .get(4..8)
        .and_then(|part| u16::from_str_radix(part, 16).ok())
        .unwrap_or(0);
    (language, code_page)
}

fn file_flag_names(flags: u32) -> Vec<String> {
    const NAMES: [(u32, &str); 6] = [
        (0x01, "Debug"),
        (0x02, "Prerelease"),
        (0x04, "Patched"),
        (0x08, "Private build"),
        (0x10, "Information inferred"),
        (0x20, "Special build"),
    ];
    NAMES
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| (*name).to_owned())
        .collect()
}

fn file_os_name(value: u32) -> String {
    let high = match value & 0xFFFF_0000 {
        0x0001_0000 => Some("DOS"),
        0x0002_0000 => Some("OS/2 16-bit"),
        0x0003_0000 => Some("OS/2 32-bit"),
        0x0004_0000 => Some("Windows NT"),
        _ => None,
    };
    let low = match value & 0x0000_FFFF {
        0x0001 => Some("Windows 16-bit"),
        0x0002 => Some("Presentation Manager 16-bit"),
        0x0003 => Some("Presentation Manager 32-bit"),
        0x0004 => Some("Windows 32-bit"),
        _ => None,
    };
    match (high, low) {
        (Some(high), Some(low)) => format!("{high} / {low}"),
        (Some(name), None) | (None, Some(name)) => name.to_owned(),
        (None, None) => format!("0x{value:08X}"),
    }
}

fn file_type_name(value: u32) -> String {
    match value {
        1 => "Application".to_owned(),
        2 => "Dynamic link library".to_owned(),
        3 => "Driver".to_owned(),
        4 => "Font".to_owned(),
        5 => "Virtual device".to_owned(),
        7 => "Static library".to_owned(),
        other => format!("0x{other:08X}"),
    }
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items.join(", ")
}

fn dotted(high: u32, low: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        high >> 16,
        high & 0xFFFF,
        low >> 16,
        low & 0xFFFF
    )
}

const fn align4(value: usize) -> usize {
    value.wrapping_add(3) & !3
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let mut raw = [0u8; 4];
    for (slot, byte) in raw.iter_mut().zip(data.iter().skip(offset)) {
        *slot = *byte;
    }
    u32::from_le_bytes(raw)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn directory(out: &mut Vec<u8>, count: u16) {
        out.extend_from_slice(&[0u8; 14]);
        out.extend_from_slice(&count.to_le_bytes());
    }

    fn entry(out: &mut Vec<u8>, id: u32, offset: u32, is_directory: bool) {
        out.extend_from_slice(&id.to_le_bytes());
        let pointer = if is_directory {
            offset | SUBDIRECTORY_BIT
        } else {
            offset
        };
        out.extend_from_slice(&pointer.to_le_bytes());
    }

    /// Three levels of `count` entries each, where every entry of a level
    /// names the one directory of the next level.
    fn shared_levels(count: u16) -> Vec<u8> {
        let level = 16 + 8 * u32::from(count);
        let mut out = Vec::new();
        directory(&mut out, count);
        for _ in 0..count {
            entry(&mut out, RT_VERSION, level, true);
        }
        directory(&mut out, count);
        for index in 0..count {
            entry(&mut out, u32::from(index), 2 * level, true);
        }
        directory(&mut out, count);
        for _ in 0..count {
            entry(&mut out, 0x0409, 3 * level, false);
        }
        out.extend_from_slice(&[0u8; 16]);
        out
    }

    /// One version type entry, `names` second level entries, and a third
    /// level directory of `leaves` entries under each of them.
    fn distinct_levels(names: u16, leaves: u16) -> Vec<u8> {
        let third = 16 + 8 * u32::from(leaves);
        let third_start = 16 + 8 + 16 + 8 * u32::from(names);
        let data_at = third_start + u32::from(names) * third;
        let mut out = Vec::new();
        directory(&mut out, 1);
        entry(&mut out, RT_VERSION, 24, true);
        directory(&mut out, names);
        for index in 0..names {
            entry(
                &mut out,
                u32::from(index),
                third_start + u32::from(index) * third,
                true,
            );
        }
        for _ in 0..names {
            directory(&mut out, leaves);
            for _ in 0..leaves {
                entry(&mut out, 0x0409, data_at, false);
            }
        }
        out.extend_from_slice(&[0u8; 16]);
        out
    }

    #[test]
    fn a_directory_named_by_more_than_one_entry_ends_the_walk_after_one_visit() {
        let bytes = shared_levels(u16::MAX);
        let mut budget = RecordBudget::new(&Limits::default());
        let error = walk_version_leaves(&bytes, 0, &mut budget).unwrap_err();
        assert!(matches!(error, RecordError::Malformed { .. }), "{error}");
        let per_level = u64::from(u16::MAX);
        assert!(
            budget.used() <= 4 * per_level,
            "{} entries and leaves charged",
            budget.used()
        );
    }

    #[test]
    fn every_entry_and_leaf_of_one_walk_draws_on_one_budget() {
        let bytes = distinct_levels(50, 50);
        let mut budget = RecordBudget::new(&Limits::default());
        let leaves = walk_version_leaves(&bytes, 0, &mut budget).unwrap();
        assert_eq!(leaves.len(), 2500);

        let limits = Limits {
            max_records: 1_000,
            ..Limits::default()
        };
        let mut budget = RecordBudget::new(&limits);
        let error = walk_version_leaves(&bytes, 0, &mut budget).unwrap_err();
        assert!(error.is_limit(), "{error}");
    }
}
