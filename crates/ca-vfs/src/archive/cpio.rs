//! The cpio "newc" format, as a package payload carries it.
//!
//! Each record is a 110 byte ASCII header, a name padded to four bytes, and
//! content padded to four bytes. The walk seeks past content, so listing
//! costs one header read per entry whatever the content size.

use std::io::{Seek, SeekFrom};

use super::span::{backing_len, read_exact_or_corrupt, Span, SpanIndex};
use super::{unix_to_system_time, ArchiveBacking};
use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{VfsError, VfsResult};
use crate::path::VfsPath;
use crate::stored::{refuse, stored_path};
use crate::tree::RawEntry;

const HEADER_BYTES: u64 = 110;
const TRAILER: &[u8] = b"TRAILER!!!";
/// Most bytes a stored name may declare, including its terminator.
const MAX_NAME_BYTES: u64 = 64 * 1024;

const MODE_TYPE: u32 = 0o170_000;
const MODE_DIRECTORY: u32 = 0o040_000;
const MODE_REGULAR: u32 = 0o100_000;
const MODE_SYMLINK: u32 = 0o120_000;

fn pad4(value: u64) -> u64 {
    value.div_ceil(4) * 4
}

fn hex_field(header: &[u8], index: usize) -> VfsResult<u64> {
    let start = 6 + index * 8;
    let field = header
        .get(start..start + 8)
        .ok_or_else(|| VfsError::corrupt("cpio header truncated"))?;
    let text = std::str::from_utf8(field).map_err(|_| VfsError::corrupt("cpio header field"))?;
    u64::from_str_radix(text, 16).map_err(|_| VfsError::corrupt("cpio header field"))
}

/// List every record of the archive held in `plain`, placing each under
/// `prefix` and recording its content as a span of `source`.
///
/// # Errors
/// Returns [`VfsError::Unsupported`] for a cpio variant other than newc, and
/// [`VfsError::Corrupt`] when a header does not parse or runs past the data.
pub(crate) fn list(
    plain: &ArchiveBacking,
    source: usize,
    prefix: &VfsPath,
    index: &mut SpanIndex,
    cancel: &Cancel,
) -> VfsResult<Vec<RawEntry>> {
    let len = backing_len(plain)?;
    let mut reader = plain.reader()?;
    let mut position = 0u64;
    let mut out = Vec::new();

    loop {
        cancel.check()?;
        if position + HEADER_BYTES > len {
            return Err(VfsError::corrupt("cpio archive ends without a trailer"));
        }
        let mut header = [0u8; 110];
        reader.seek(SeekFrom::Start(position))?;
        read_exact_or_corrupt(&mut reader, &mut header, "cpio header")?;
        let magic = header.get(0..6).unwrap_or_default();
        if magic != b"070701" && magic != b"070702" {
            return Err(VfsError::unsupported(
                "cpio variant other than newc in a package payload",
            ));
        }
        let mode = u32::try_from(hex_field(&header, 1)?).unwrap_or(0);
        let uid = u32::try_from(hex_field(&header, 2)?).ok();
        let gid = u32::try_from(hex_field(&header, 3)?).ok();
        let mtime = hex_field(&header, 5)?;
        let size = hex_field(&header, 6)?;
        let name_len = hex_field(&header, 11)?;
        if name_len == 0 || name_len > MAX_NAME_BYTES {
            return Err(VfsError::corrupt("cpio name length out of range"));
        }
        let name_end = position + HEADER_BYTES + name_len;
        let data_start = pad4(name_end);
        let data_end = data_start
            .checked_add(size)
            .ok_or_else(|| VfsError::corrupt("cpio size out of range"))?;
        if data_end > len {
            return Err(VfsError::corrupt("cpio entry runs past the archive"));
        }
        let mut raw_name = vec![0u8; usize::try_from(name_len).unwrap_or(0)];
        read_exact_or_corrupt(&mut reader, &mut raw_name, "cpio name")?;
        if raw_name.last() == Some(&0) {
            raw_name.pop();
        }
        position = pad4(data_end);
        if raw_name == TRAILER {
            break;
        }

        let text = String::from_utf8_lossy(&raw_name).into_owned();
        let kind = if mode & MODE_TYPE == MODE_DIRECTORY {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        let Some((path, refusal)) = place(prefix, &text, kind == EntryKind::Directory) else {
            continue;
        };
        let file_type = mode & MODE_TYPE;
        let name = path.name().unwrap_or_default().to_owned();
        let mut entry = VfsEntry {
            path,
            name: name.clone(),
            kind,
            size: if kind == EntryKind::Directory {
                0
            } else {
                size
            },
            size_is_exact: true,
            modified: i64::try_from(mtime).ok().and_then(unix_to_system_time),
            time_fidelity: TimeFidelity::Utc,
            created: None,
            attributes: Some(VfsAttributes {
                read_only: mode & 0o200 == 0,
                hidden: name.starts_with('.'),
                system: false,
                archive: false,
                windows_bits: None,
                unix_mode: Some(mode & 0o7777),
                uid,
                gid,
            }),
            crc32: None,
            link: (file_type == MODE_SYMLINK).then_some(VfsLinkKind::FileLink),
            version_info: None,
            error: (kind == EntryKind::File
                && file_type != MODE_REGULAR
                && file_type != MODE_SYMLINK)
                .then(|| "a device, pipe or socket entry holds no content".to_owned()),
            refused: false,
        };
        let locator = match refusal {
            Some(reason) => {
                refuse(&mut entry, &reason);
                None
            }
            None => (kind == EntryKind::File)
                .then(|| index.push(Span::single(source, data_start, size))),
        };
        out.push(RawEntry { entry, locator });
    }
    Ok(out)
}

/// The path a stored name takes under `prefix`, with the refusal its row
/// carries when the rules refuse the name, or `None` for the root itself.
fn place(prefix: &VfsPath, raw: &str, is_dir: bool) -> Option<(VfsPath, Option<String>)> {
    let trimmed = raw.strip_prefix("./").unwrap_or(raw);
    let (relative, refusal) = stored_path(trimmed, is_dir).into_row()?;
    if prefix.is_root() {
        return Some((relative, refusal));
    }
    let (path, past_ceiling) = stored_path(&format!("{prefix}/{relative}"), is_dir).into_row()?;
    Some((path, refusal.or(past_ceiling)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn record(name: &str, mode: u32, data: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            1,
            mode,
            0,
            0,
            1,
            1_700_000_000,
            data.len(),
            0,
            0,
            0,
            0,
            name.len() + 1,
            0
        )
        .into_bytes();
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out
    }

    #[test]
    fn newc_records_list_with_their_content() {
        let mut bytes = record("./usr", 0o040_755, b"");
        bytes.extend(record("./usr/a.txt", 0o100_644, b"hello"));
        bytes.extend(record("../escape", 0o100_644, b"x"));
        bytes.extend(record("TRAILER!!!", 0, b""));
        let backing = ArchiveBacking::from_bytes(bytes);
        let mut index = SpanIndex::default();
        index.sources.push(backing.clone());
        let entries = list(&backing, 0, &VfsPath::root(), &mut index, &Cancel::new()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.entry.path.as_str()).collect();
        assert_eq!(names, vec!["usr", "usr/a.txt", "__/escape"]);
        assert_eq!(entries[1].entry.size, 5);
        let escape = &entries[2];
        assert!(
            escape.entry.refused,
            "a name that leaves the root is refused"
        );
        assert!(escape.locator.is_none(), "and has no content to read");
    }

    #[test]
    fn a_missing_trailer_is_corrupt() {
        let backing = ArchiveBacking::from_bytes(record("a", 0o100_644, b"x"));
        let mut index = SpanIndex::default();
        let error = list(&backing, 0, &VfsPath::root(), &mut index, &Cancel::new()).unwrap_err();
        assert!(matches!(error, VfsError::Corrupt { .. }));
    }
}
