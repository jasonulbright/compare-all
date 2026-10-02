//! Debian packages, read only.
//!
//! A package is an `ar` container. Its tar members, `control.tar.*` and
//! `data.tar.*`, are each decoded once into a temporary file and shown
//! expanded as a folder named after the member; every other member is shown
//! as the file it is. An `ar` container that is not a package lists the same
//! way.

use std::io::{Seek, SeekFrom};

use super::span::{
    backing_len, decode_to_temp, range_reader, read_exact_or_corrupt, Compression, Span, SpanIndex,
};
use super::{tar_format, unix_to_system_time, ArchiveBacking};
use crate::cancel::Cancel;
use crate::entry::{EntryKind, VfsEntry};
use crate::error::{VfsError, VfsResult};
use crate::limits::{Budget, Limits};
use crate::path::VfsPath;
use crate::stored::{refuse, stored_child, stored_path};
use crate::tree::RawEntry;

const GLOBAL_HEADER: &[u8] = b"!<arch>\n";
const MEMBER_HEADER_BYTES: u64 = 60;
/// Most members read from one container.
const MAX_MEMBERS: usize = 65_536;
/// Most bytes a name stored after a BSD header may declare.
const MAX_LONG_NAME: u64 = 4096;
/// Most bytes of a GNU long name table that are read. A member that names an
/// entry of a larger table keeps its reference as its name and lists refused.
const MAX_NAME_TABLE: u64 = 1024 * 1024;

/// One member of an `ar` container.
#[derive(Debug, Clone)]
pub(crate) struct Member {
    /// Name as stored, with the GNU terminator removed.
    pub name: String,
    /// Where the content starts in the container.
    pub offset: u64,
    /// Content length.
    pub size: u64,
    /// Modification time in seconds since the Unix epoch.
    pub mtime: Option<i64>,
    /// Permission bits.
    pub mode: Option<u32>,
}

fn field(header: &[u8], start: usize, len: usize) -> &str {
    header
        .get(start..start + len)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .map_or("", str::trim_end)
}

/// Read the member headers of an `ar` container.
///
/// # Errors
/// Returns [`VfsError::Corrupt`] when the global header or a member header is
/// wrong or a member runs past the container.
pub(crate) fn members(backing: &ArchiveBacking, cancel: &Cancel) -> VfsResult<Vec<Member>> {
    let len = backing_len(backing)?;
    let mut reader = backing.reader()?;
    let mut global = [0u8; 8];
    read_exact_or_corrupt(&mut reader, &mut global, "ar header")?;
    if global != GLOBAL_HEADER {
        return Err(VfsError::corrupt("ar global header is wrong"));
    }
    let mut position = GLOBAL_HEADER.len() as u64;
    let mut out = Vec::new();
    let mut name_table: Option<Vec<u8>> = None;
    while position + MEMBER_HEADER_BYTES <= len {
        cancel.check()?;
        if out.len() >= MAX_MEMBERS {
            return Err(VfsError::corrupt("ar container holds too many members"));
        }
        let mut header = [0u8; 60];
        reader.seek(SeekFrom::Start(position))?;
        read_exact_or_corrupt(&mut reader, &mut header, "ar member header")?;
        if header.get(58..60) != Some(b"`\n".as_slice()) {
            return Err(VfsError::corrupt("ar member header is wrong"));
        }
        let size: u64 = field(&header, 48, 10)
            .parse()
            .map_err(|_| VfsError::corrupt("ar member size"))?;
        let mut offset = position + MEMBER_HEADER_BYTES;
        let mut content = size;
        let raw_name = field(&header, 0, 16).to_owned();
        let name = if let Some(count) = raw_name.strip_prefix("#1/") {
            let count: u64 = count
                .parse()
                .map_err(|_| VfsError::corrupt("ar long name length"))?;
            if count > MAX_LONG_NAME || count > size {
                return Err(VfsError::corrupt("ar long name length"));
            }
            let mut name = vec![0u8; usize::try_from(count).unwrap_or(0)];
            read_exact_or_corrupt(&mut reader, &mut name, "ar long name")?;
            offset += count;
            content -= count;
            String::from_utf8_lossy(&name)
                .trim_end_matches('\0')
                .to_owned()
        } else if raw_name == "//" {
            if size <= MAX_NAME_TABLE && offset.saturating_add(size) <= len {
                let mut table = vec![0u8; usize::try_from(size).unwrap_or(0)];
                read_exact_or_corrupt(&mut reader, &mut table, "ar long name table")?;
                name_table = Some(table);
            }
            "/".to_owned()
        } else if let Some(at) = long_name_offset(&raw_name) {
            name_table
                .as_deref()
                .and_then(|table| long_name(table, at))
                .unwrap_or(raw_name)
        } else {
            raw_name.strip_suffix('/').unwrap_or(&raw_name).to_owned()
        };
        let end = offset
            .checked_add(content)
            .ok_or_else(|| VfsError::corrupt("ar member size"))?;
        if end > len {
            return Err(VfsError::corrupt("ar member runs past the container"));
        }
        out.push(Member {
            name,
            offset,
            size: content,
            mtime: field(&header, 16, 12).parse().ok(),
            mode: u32::from_str_radix(field(&header, 40, 8), 8).ok(),
        });
        position = end + (end % 2);
    }
    if position < len {
        return Err(VfsError::corrupt(
            "ar container ends inside a member header",
        ));
    }
    Ok(out)
}

/// The table offset a GNU member name `/123` refers to.
fn long_name_offset(raw_name: &str) -> Option<usize> {
    let digits = raw_name.strip_prefix('/')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The name that starts at `at` in a GNU long name table.
///
/// GNU ends each name with `/` and a line feed; other writers end it with a
/// line feed or a NUL alone. `None` when `at` lies outside the table or names
/// an empty name, so the reference stays the member's name and lists refused.
fn long_name(table: &[u8], at: usize) -> Option<String> {
    let rest = table.get(at..)?;
    let end = rest
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\0'))
        .unwrap_or(rest.len());
    let bytes = rest.get(..end)?;
    let bytes = bytes.strip_suffix(b"/").unwrap_or(bytes);
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(bytes).into_owned())
}

/// True for the members GNU `ar` keeps its symbol table and its long name
/// table in, as [`members`] names them. They are part of the container format
/// and hold no entry of it.
fn is_ar_table(name: &str) -> bool {
    matches!(name, "" | "/" | "/SYM64")
}

/// The compression of a tar member, when the member is a tar.
fn tar_member(name: &str) -> Option<Compression> {
    const SUFFIXES: [(&str, Compression); 6] = [
        (".tar", Compression::None),
        (".tar.gz", Compression::Gzip),
        (".tar.bz2", Compression::Bzip2),
        (".tar.xz", Compression::Xz),
        (".tar.lzma", Compression::Lzma),
        (".tar.zst", Compression::Zstd),
    ];
    let lower = name.to_ascii_lowercase();
    SUFFIXES
        .iter()
        .find(|(suffix, _)| lower.len() > suffix.len() && lower.ends_with(suffix))
        .map(|(_, compression)| *compression)
}

/// Build the listing: plain members as files, tar members expanded.
///
/// # Errors
/// Returns [`VfsError::Corrupt`] when the container or a tar inside it does
/// not parse, and [`VfsError::LimitExceeded`] when a tar member expands past
/// a ceiling.
pub(crate) fn list(
    backing: &ArchiveBacking,
    limits: &Limits,
    scan: &Budget,
    cancel: &Cancel,
) -> VfsResult<(SpanIndex, Vec<RawEntry>)> {
    let members = members(backing, cancel)?;
    let mut index = SpanIndex::default();
    index.sources.push(backing.clone());
    let mut out = Vec::new();

    for member in members {
        cancel.check()?;
        if is_ar_table(&member.name) {
            continue;
        }
        let Some((path, refusal)) = stored_child(&VfsPath::root(), &member.name).into_row() else {
            continue;
        };
        let modified = member.mtime.and_then(unix_to_system_time);
        let mut unexpanded: Option<String> = None;
        let tar = tar_member(&member.name).filter(|_| refusal.is_none());
        if let Some(compression) = tar {
            match expand_tar(
                backing,
                &member,
                compression,
                &path,
                limits,
                scan,
                cancel,
                &mut index,
            ) {
                Ok(mut entries) => {
                    let mut folder = VfsEntry::directory(path);
                    folder.modified = modified;
                    out.push(RawEntry::from(folder));
                    out.append(&mut entries);
                    continue;
                }
                Err(VfsError::Unsupported { what }) => unexpanded = Some(what),
                Err(other) => return Err(other),
            }
        }
        let mut entry = VfsEntry::file(path, member.size);
        entry.modified = modified;
        entry.error = unexpanded.map(|what| format!("not expanded: {what}"));
        if let Some(mode) = member.mode {
            entry.attributes = Some(crate::entry::VfsAttributes {
                read_only: mode & 0o200 == 0,
                hidden: false,
                system: false,
                archive: false,
                windows_bits: None,
                unix_mode: Some(mode & 0o7777),
                uid: None,
                gid: None,
            });
        }
        let locator = match refusal {
            Some(reason) => {
                refuse(&mut entry, &reason);
                None
            }
            None => Some(index.push(Span::single(0, member.offset, member.size))),
        };
        out.push(RawEntry { entry, locator });
    }
    Ok((index, out))
}

/// Decode one tar member and list it under `folder`.
#[allow(clippy::too_many_arguments)]
fn expand_tar(
    backing: &ArchiveBacking,
    member: &Member,
    compression: Compression,
    folder: &VfsPath,
    limits: &Limits,
    scan: &Budget,
    cancel: &Cancel,
    index: &mut SpanIndex,
) -> VfsResult<Vec<RawEntry>> {
    let raw = range_reader(backing, member.offset, member.size)?;
    let decoder = compression.decoder(raw)?;
    let plain = decode_to_temp(decoder, &member.name, limits, scan, cancel)?;
    let source = index.sources.len();
    index.sources.push(plain.clone());

    let listing = Budget::new(limits.max_archive_bytes);
    let inner = tar_format::list(&plain, None, limits, &listing, cancel)?;
    let mut out = Vec::with_capacity(inner.len());
    for item in inner {
        let mut entry = item.entry;
        let placed = format!("{folder}/{}", entry.path);
        let Some((path, past_ceiling)) = stored_path(&placed, entry.is_dir()).into_row() else {
            continue;
        };
        path.name().unwrap_or_default().clone_into(&mut entry.name);
        entry.path = path;
        if let Some(reason) = past_ceiling {
            refuse(&mut entry, &reason);
        }
        let locator = match (entry.kind, item.locator, entry.refused) {
            (EntryKind::File, Some(offset), false) => {
                Some(index.push(Span::single(source, offset, entry.size)))
            }
            _ => None,
        };
        out.push(RawEntry { entry, locator });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn tar_members_are_recognised_by_suffix() {
        assert_eq!(tar_member("data.tar.xz"), Some(Compression::Xz));
        assert_eq!(tar_member("control.tar.gz"), Some(Compression::Gzip));
        assert_eq!(tar_member("data.tar"), Some(Compression::None));
        assert_eq!(tar_member("data.tar.zst"), Some(Compression::Zstd));
        assert_eq!(tar_member("debian-binary"), None);
        assert_eq!(tar_member("notes.gz"), None);
    }

    #[test]
    fn the_tables_of_a_gnu_ar_container_list_no_row() {
        let mut bytes = GLOBAL_HEADER.to_vec();
        let members: [(&str, &[u8]); 3] = [("/", b"0000"), ("//", b"x"), ("notes.txt", b"hello")];
        for (name, body) in members {
            let header = format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                name,
                0,
                0,
                0,
                644,
                body.len()
            );
            bytes.extend_from_slice(header.as_bytes());
            bytes.extend_from_slice(body);
            if bytes.len() % 2 == 1 {
                bytes.push(b'\n');
            }
        }
        let (_, entries) = list(
            &ArchiveBacking::from_bytes(bytes),
            &Limits::default(),
            &Budget::new(u64::MAX),
            &Cancel::new(),
        )
        .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.entry.path.as_str()).collect();
        assert_eq!(names, ["notes.txt"]);
    }

    #[test]
    fn a_member_past_the_end_is_corrupt() {
        let mut bytes = GLOBAL_HEADER.to_vec();
        bytes.extend_from_slice(
            format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                "a", 0, 0, 0, 644, 99
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"short");
        let error = members(&ArchiveBacking::from_bytes(bytes), &Cancel::new()).unwrap_err();
        assert!(matches!(error, VfsError::Corrupt { .. }));
    }

    #[test]
    fn bytes_too_short_for_a_member_header_after_the_last_member_are_corrupt() {
        let mut bytes = GLOBAL_HEADER.to_vec();
        bytes.extend_from_slice(
            format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", "a", 0, 0, 0, 644, 2).as_bytes(),
        );
        bytes.extend_from_slice(b"ok");
        let complete = members(&ArchiveBacking::from_bytes(bytes.clone()), &Cancel::new()).unwrap();
        assert_eq!(complete.len(), 1);
        bytes.extend_from_slice(b"b               0   ");
        let error = members(&ArchiveBacking::from_bytes(bytes), &Cancel::new()).unwrap_err();
        assert!(matches!(error, VfsError::Corrupt { .. }), "{error}");
    }
}
