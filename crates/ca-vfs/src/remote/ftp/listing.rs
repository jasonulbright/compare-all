//! Parsers for the two listing forms a file transfer server answers with.
//!
//! The machine-readable listing states a type, a size and a time in UTC, so it
//! parses exactly. The plain listing does not: it is meant to be read by a
//! person, its columns differ between server families, and the common Unix
//! form states a time as a local wall clock with no zone and no seconds, or as
//! a date alone once the file is more than six months old. Each entry
//! therefore carries how precise its time is, and the caller turns that into
//! [`TimeFidelity`](crate::entry::TimeFidelity) so the comparison layer can
//! allow a tolerance.
//!
//! No input is trusted. The parsers return `None` for a line they do not
//! understand and never fail on one, because a single unusual line must not
//! end a listing.

use crate::remote::timestamp::{civil_to_unix, month_number, parse_ftp_stamp, unix_to_civil};

/// What a listing line says an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListedKind {
    /// A leaf holding content.
    File,
    /// A node holding other entries.
    Directory,
    /// A link, whose target kind the caller decides.
    Link,
}

/// How precise a listed time is, and in which zone it is stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListedTime {
    /// An instant in UTC, to the second.
    Utc(i64),
    /// A wall clock in the server's zone, to the minute.
    LocalMinute(i64),
    /// A date in the server's zone; the time of day is not stated.
    LocalDay(i64),
}

impl ListedTime {
    /// The seconds the listing stated, before any zone offset is applied.
    #[must_use]
    pub const fn seconds(self) -> i64 {
        match self {
            Self::Utc(value) | Self::LocalMinute(value) | Self::LocalDay(value) => value,
        }
    }

    /// True when the value names an instant rather than a wall clock.
    #[must_use]
    pub const fn is_instant(self) -> bool {
        matches!(self, Self::Utc(_))
    }
}

/// One entry a listing line describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    /// Name as the server spelled it. Nothing validates it here.
    pub name: String,
    /// What the line says the entry is.
    pub kind: ListedKind,
    /// Size in bytes where the line states one.
    pub size: Option<u64>,
    /// Modification time where the line states one.
    pub modified: Option<ListedTime>,
    /// Target of a link, where the line states one.
    pub link_target: Option<String>,
    /// Unix permission bits where the line states them.
    pub unix_mode: Option<u32>,
    /// Owner and group names where the line states them.
    pub owner: Option<String>,
}

impl ListedEntry {
    /// An entry with a name and a kind and nothing else.
    fn bare(name: String, kind: ListedKind) -> Self {
        Self {
            name,
            kind,
            size: None,
            modified: None,
            link_target: None,
            unix_mode: None,
            owner: None,
        }
    }
}

/// Parse one line of the machine-readable listing.
///
/// The line is a semicolon separated fact list, a space, then the name. The
/// name may hold spaces and semicolons, so it is taken as everything after the
/// first space that follows the facts.
#[must_use]
pub fn parse_machine_line(line: &str) -> Option<ListedEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (facts, name) = line.split_once(' ')?;
    if name.is_empty() {
        return None;
    }
    let mut entry = ListedEntry::bare(name.to_owned(), ListedKind::File);
    for fact in facts.split(';') {
        let Some((key, value)) = fact.split_once('=') else {
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "type" => {
                entry.kind = match value.to_ascii_lowercase().as_str() {
                    "dir" | "cdir" | "pdir" => ListedKind::Directory,
                    "file" => ListedKind::File,
                    other if other.starts_with("os.unix=slink") => ListedKind::Link,
                    _ => ListedKind::File,
                };
                if matches!(value.to_ascii_lowercase().as_str(), "cdir" | "pdir") {
                    // The listing describes the folder itself or its parent.
                    return None;
                }
            }
            "size" | "sizd" => entry.size = value.parse().ok(),
            "modify" => entry.modified = parse_ftp_stamp(value).map(ListedTime::Utc),
            "unix.mode" => entry.unix_mode = u32::from_str_radix(value, 8).ok(),
            "unix.owner" | "unix.uid" => entry.owner = Some(value.to_owned()),
            _ => {}
        }
    }
    if entry.name == "." || entry.name == ".." {
        return None;
    }
    Some(entry)
}

/// Parse one line of a plain listing, in either the Unix or the Windows form.
///
/// `now` is the reference used to choose a year for a Unix line that states
/// only a month and a day.
#[must_use]
pub fn parse_plain_line(line: &str, now: i64) -> Option<ListedEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return None;
    }
    if line.starts_with("total ") || line.ends_with(':') {
        return None;
    }
    parse_unix_line(line, now).or_else(|| parse_windows_line(line))
}

/// Parse a Unix long listing line.
fn parse_unix_line(line: &str, now: i64) -> Option<ListedEntry> {
    let first = line.split_whitespace().next()?;
    let kind = match first.as_bytes().first()? {
        b'd' => ListedKind::Directory,
        b'l' => ListedKind::Link,
        b'-' | b'b' | b'c' | b'p' | b's' => ListedKind::File,
        _ => return None,
    };
    if first.len() < 10 {
        return None;
    }

    let tokens: Vec<(usize, &str)> = token_offsets(line);
    // The date starts at the first month name that is followed by a day and by
    // a time or a year. Scanning for it tolerates the missing owner or group
    // column that some servers leave out.
    let mut date_at = None;
    for index in 2..tokens.len().saturating_sub(2) {
        let (_, token) = tokens.get(index)?;
        if month_number(token).is_none() || token.len() != 3 {
            continue;
        }
        let (_, day) = tokens.get(index + 1)?;
        let (_, third) = tokens.get(index + 2)?;
        if day.parse::<u32>().is_err() {
            continue;
        }
        if looks_like_clock(third) || third.parse::<i64>().is_ok() {
            date_at = Some(index);
            break;
        }
    }
    let date_at = date_at?;
    let size = tokens
        .get(date_at.checked_sub(1)?)
        .and_then(|(_, token)| token.parse::<u64>().ok());

    let month = month_number(tokens.get(date_at)?.1)?;
    let day: u32 = tokens.get(date_at + 1)?.1.parse().ok()?;
    let third = tokens.get(date_at + 2)?.1;

    let (modified, name_token) = if looks_like_clock(third) {
        let (hour, minute, second) = parse_clock(third)?;
        // A full timestamp column states the year after the clock. Anything
        // else after a clock is the start of the name.
        let stated_year = tokens
            .get(date_at + 3)
            .and_then(|(_, token)| token.parse::<i64>().ok())
            .filter(|year| (1000..=9999).contains(year))
            .filter(|_| third.matches(':').count() == 2);
        if let Some(year) = stated_year {
            (
                Some(ListedTime::LocalMinute(civil_to_unix(
                    year, month, day, hour, minute, second,
                ))),
                date_at + 4,
            )
        } else {
            // A clock with no year means the file is inside a six month
            // window around now; a date ahead of now by more than a day
            // belongs to the year before.
            let (this_year, ..) = unix_to_civil(now);
            let candidate = civil_to_unix(this_year, month, day, hour, minute, second);
            let chosen = if candidate > now + 86_400 {
                civil_to_unix(this_year - 1, month, day, hour, minute, second)
            } else {
                candidate
            };
            (Some(ListedTime::LocalMinute(chosen)), date_at + 3)
        }
    } else {
        let year: i64 = third.parse().ok()?;
        if !(1000..=9999).contains(&year) {
            return None;
        }
        (
            Some(ListedTime::LocalDay(civil_to_unix(
                year, month, day, 0, 0, 0,
            ))),
            date_at + 3,
        )
    };

    let name_start = tokens.get(name_token)?.0;
    let raw_name = line.get(name_start..)?.to_owned();
    let (name, link_target) = match raw_name.split_once(" -> ") {
        Some((name, target)) if kind == ListedKind::Link => {
            (name.to_owned(), Some(target.to_owned()))
        }
        _ => (raw_name, None),
    };
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }

    let owner = tokens.get(2).map(|(_, token)| (*token).to_owned());
    Some(ListedEntry {
        name,
        kind,
        size: if kind == ListedKind::Directory {
            None
        } else {
            size
        },
        modified,
        link_target,
        unix_mode: parse_mode(first),
        owner,
    })
}

/// Parse a Windows long listing line.
fn parse_windows_line(line: &str) -> Option<ListedEntry> {
    let tokens: Vec<(usize, &str)> = token_offsets(line);
    let (_, date) = tokens.first()?;
    let (_, clock) = tokens.get(1)?;
    let (_, third) = tokens.get(2)?;

    let mut date_parts = date.split(['-', '/']);
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let year_text = date_parts.next()?;
    if date_parts.next().is_some() {
        return None;
    }
    let year: i64 = year_text.parse().ok()?;
    let year = match year_text.len() {
        2 => {
            if year < 70 {
                2000 + year
            } else {
                1900 + year
            }
        }
        4 => year,
        _ => return None,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let upper = clock.to_ascii_uppercase();
    let (digits, shift) = if let Some(rest) = upper.strip_suffix("PM") {
        (rest, 12)
    } else if let Some(rest) = upper.strip_suffix("AM") {
        (rest, 0)
    } else {
        (upper.as_str(), 0)
    };
    let (hour, minute, second) = parse_clock(digits)?;
    let hour = if shift == 12 {
        if hour == 12 {
            12
        } else {
            hour + 12
        }
    } else if hour == 12 && upper.ends_with("AM") {
        0
    } else {
        hour
    };
    if hour > 23 || minute > 59 {
        return None;
    }

    let (kind, size) = if third.eq_ignore_ascii_case("<DIR>") {
        (ListedKind::Directory, None)
    } else {
        (ListedKind::File, Some(third.parse::<u64>().ok()?))
    };

    let name_start = tokens.get(3)?.0;
    let name = line.get(name_start..)?.to_owned();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(ListedEntry {
        name,
        kind,
        size,
        modified: Some(ListedTime::LocalMinute(civil_to_unix(
            year, month, day, hour, minute, second,
        ))),
        link_target: None,
        unix_mode: None,
        owner: None,
    })
}

/// Byte offset and text of each whitespace separated token.
fn token_offsets(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in line.char_indices() {
        if ch.is_whitespace() {
            if let Some(at) = start.take() {
                if let Some(text) = line.get(at..index) {
                    out.push((at, text));
                }
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(at) = start {
        if let Some(text) = line.get(at..) {
            out.push((at, text));
        }
    }
    out
}

/// True for `HH:MM` and `HH:MM:SS`.
fn looks_like_clock(token: &str) -> bool {
    let mut parts = token.split(':');
    let Some(hour) = parts.next() else {
        return false;
    };
    let Some(minute) = parts.next() else {
        return false;
    };
    if !(1..=2).contains(&hour.len()) || minute.len() != 2 {
        return false;
    }
    let second = parts.next();
    if parts.next().is_some() {
        return false;
    }
    if second.is_some_and(|value| value.len() != 2) {
        return false;
    }
    token
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b':')
}

/// Hour, minute and second of `HH:MM` or `HH:MM:SS`.
fn parse_clock(token: &str) -> Option<(u32, u32, u32)> {
    let mut parts = token.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = match parts.next() {
        Some(value) => value.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

/// The permission bits of a `rwxrwxrwx` column.
fn parse_mode(column: &str) -> Option<u32> {
    let bytes = column.as_bytes().get(1..10)?;
    let mut mode = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        let bit = 8 - u32::try_from(index).ok()?;
        let set = matches!(
            (index % 3, byte),
            (0, b'r') | (1, b'w') | (2, b'x' | b's' | b't')
        );
        if set {
            mode |= 1 << bit;
        }
    }
    Some(mode)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    const NOW: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

    #[test]
    fn the_machine_listing_states_an_instant() {
        let entry =
            parse_machine_line("type=file;size=1234;modify=20240102030405; report.txt").unwrap();
        assert_eq!(entry.name, "report.txt");
        assert_eq!(entry.kind, ListedKind::File);
        assert_eq!(entry.size, Some(1234));
        assert_eq!(
            entry.modified,
            Some(ListedTime::Utc(civil_to_unix(2024, 1, 2, 3, 4, 5)))
        );
        assert!(parse_machine_line("type=cdir;modify=20240102030405; .").is_none());
    }

    #[test]
    fn a_unix_line_with_a_clock_is_minute_precision() {
        let entry = parse_plain_line(
            "-rw-r--r--   1 owner group     1234 Jan  2 03:04 report.txt",
            NOW,
        )
        .unwrap();
        assert_eq!(entry.name, "report.txt");
        assert_eq!(entry.size, Some(1234));
        assert_eq!(entry.unix_mode, Some(0o644));
        assert!(matches!(entry.modified, Some(ListedTime::LocalMinute(_))));
    }

    #[test]
    fn a_unix_line_with_a_year_is_day_precision() {
        let entry = parse_plain_line(
            "drwxr-xr-x   2 owner group     4096 Feb 10  2019 sub dir",
            NOW,
        )
        .unwrap();
        assert_eq!(entry.name, "sub dir");
        assert_eq!(entry.kind, ListedKind::Directory);
        assert_eq!(
            entry.modified,
            Some(ListedTime::LocalDay(civil_to_unix(2019, 2, 10, 0, 0, 0)))
        );
    }

    #[test]
    fn a_unix_link_line_carries_its_target() {
        let entry = parse_plain_line(
            "lrwxrwxrwx   1 owner group        7 Jan  2 03:04 link -> target",
            NOW,
        )
        .unwrap();
        assert_eq!(entry.kind, ListedKind::Link);
        assert_eq!(entry.name, "link");
        assert_eq!(entry.link_target.as_deref(), Some("target"));
    }

    #[test]
    fn a_windows_line_parses_both_shapes() {
        let file = parse_plain_line("01-02-24  03:04PM              1234 report.txt", NOW).unwrap();
        assert_eq!(file.size, Some(1234));
        assert_eq!(
            file.modified,
            Some(ListedTime::LocalMinute(civil_to_unix(2024, 1, 2, 15, 4, 0)))
        );
        let dir =
            parse_plain_line("01-02-2024  09:00AM       <DIR>          sub dir", NOW).unwrap();
        assert_eq!(dir.kind, ListedKind::Directory);
        assert_eq!(dir.name, "sub dir");
    }

    #[test]
    fn a_line_that_is_not_a_listing_is_skipped_rather_than_failing() {
        assert!(parse_plain_line("total 12", NOW).is_none());
        assert!(parse_plain_line("/var/www:", NOW).is_none());
        assert!(parse_plain_line("", NOW).is_none());
        assert!(parse_plain_line("garbage garbage garbage", NOW).is_none());
    }

    #[test]
    fn a_full_timestamp_column_still_parses() {
        let entry = parse_plain_line(
            "-rw-r--r--   1 owner group     1234 Jan  2 03:04:05 2024 report.txt",
            NOW,
        )
        .unwrap();
        assert_eq!(entry.name, "report.txt");
        assert_eq!(
            entry.modified,
            Some(ListedTime::LocalMinute(civil_to_unix(2024, 1, 2, 3, 4, 5)))
        );
    }

    #[test]
    fn a_clock_ahead_of_now_belongs_to_the_year_before() {
        let entry = parse_plain_line("-rw-r--r--   1 o g  1 Dec 31 23:00 late.txt", NOW).unwrap();
        let Some(ListedTime::LocalMinute(seconds)) = entry.modified else {
            panic!("expected a minute precision time");
        };
        assert_eq!(unix_to_civil(seconds).0, 2025);
    }
}
