//! Stored entry names, and the listing row that stands in for a name the path
//! rules refuse.
//!
//! A container stores its names as free text. A name that [`VfsPath::parse`]
//! refuses still names a record the container holds, so every reader lists
//! such a record under a name the rules accept and marks the row refused: the
//! row counts in the listing and in a comparison, and nothing opens or
//! extracts it. Every reader maps a refused name the same way, here: each
//! character the rules refuse becomes `_`, and a clash with another name is
//! resolved by the listing tree like any other clash.

use crate::entry::VfsEntry;
use crate::path::{PathError, VfsPath, MAX_COMPONENTS, MAX_PATH_BYTES};

/// The character that takes the place of each refused character.
const REPLACEMENT: char = '_';

/// Most bytes of a stored name that a refusal quotes.
const QUOTED_BYTES: usize = 256;

/// Where one stored name lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stored {
    /// The name is a usable path.
    Usable(VfsPath),
    /// The name refers to the root of the container itself.
    Root,
    /// The rules refuse the name: the row lists at `display` and carries
    /// `reason` as its error.
    Refused {
        /// A path the rules accept.
        display: VfsPath,
        /// What the entry is stored as and why it is refused.
        reason: String,
    },
}

impl Stored {
    /// The path the row lists at and the refusal it carries, or `None` for
    /// the root, which lists no row.
    pub(crate) fn into_row(self) -> Option<(VfsPath, Option<String>)> {
        match self {
            Self::Usable(path) => Some((path, None)),
            Self::Root => None,
            Self::Refused { display, reason } => Some((display, Some(reason))),
        }
    }
}

/// Resolve a stored name that may hold several components.
///
/// A directory whose name refers to the root is the root itself. A file
/// whose name refers to the root names nothing a path can reach, so it is
/// refused.
pub(crate) fn stored_path(raw: &str, is_dir: bool) -> Stored {
    match VfsPath::parse(raw) {
        Ok(path) if !path.is_root() => Stored::Usable(path),
        Ok(_) if is_dir => Stored::Root,
        Ok(_) => Stored::Refused {
            display: placeholder(),
            reason: refusal(raw, "names the root of the container"),
        },
        Err(error) => Stored::Refused {
            display: display_path(raw),
            reason: refusal(raw, &why(&error)),
        },
    }
}

/// Resolve one stored name under `dir`, for a format that stores a single
/// component in each record.
pub(crate) fn stored_child(dir: &VfsPath, raw: &str) -> Stored {
    let why = match VfsPath::parse(raw) {
        Ok(part) if part.depth() == 1 => match dir.join(part.as_str()) {
            Ok(path) => return Stored::Usable(path),
            Err(error) => why(&error),
        },
        Ok(part) if part.is_root() => "names the root of the container".to_owned(),
        Ok(_) => "holds a path separator".to_owned(),
        Err(error) => why(&error),
    };
    let mut component = display_component(&raw.replace(['/', '\\'], "_"));
    if component.is_empty() {
        component.push(REPLACEMENT);
    }
    Stored::Refused {
        display: child_display(dir, &component),
        reason: refusal(raw, &why),
    }
}

/// The path a refused row whose component maps to `component` lists at.
fn child_display(dir: &VfsPath, component: &str) -> VfsPath {
    if dir.is_root() {
        display_path(component)
    } else {
        display_path(&format!("{dir}/{component}"))
    }
}

/// Resolve the name of one item of a local directory under `dir`.
///
/// A name that is not valid Unicode has no path of its own in this crate, so
/// its row lists under the name with `_` in place of each sequence that does
/// not decode. On Windows a DOS device name is refused as well, because a path
/// built from it reaches the device and not the item.
pub(crate) fn local_child(dir: &VfsPath, name: &std::ffi::OsStr) -> Stored {
    let Some(text) = name.to_str() else {
        let display = match stored_child(dir, &underscored(name)) {
            Stored::Usable(path) => path,
            Stored::Refused { display, .. } => display,
            Stored::Root => placeholder(),
        };
        return Stored::Refused {
            display: off_device(dir, display),
            reason: refusal(&name.to_string_lossy(), "is not valid Unicode"),
        };
    };
    match stored_child(dir, text) {
        Stored::Usable(_) if cfg!(windows) && is_device_name(text) => Stored::Refused {
            display: child_display(dir, &device_display(text)),
            reason: refusal(text, DEVICE),
        },
        Stored::Refused { display, reason } => Stored::Refused {
            display: off_device(dir, display),
            reason,
        },
        other => other,
    }
}

/// `display`, with its last component moved off a DOS device name on
/// Windows, so the path of a refused row never reaches a device.
fn off_device(dir: &VfsPath, display: VfsPath) -> VfsPath {
    match display.name() {
        Some(name) if cfg!(windows) && is_device_name(name) => {
            child_display(dir, &device_display(name))
        }
        _ => display,
    }
}

/// Why a path of this platform does not reach the local directory item
/// `name` as itself, and the one component its row lists under.
///
/// Windows drops a dot or a space at the end of a path component, and reads
/// a DOS device name, with or without an extension, as the device. A path
/// built from such a name reaches another item, a device or nothing. Every
/// other platform reaches every name as itself.
#[must_use]
pub fn platform_refusal(name: &std::ffi::OsStr) -> Option<(String, String)> {
    if !cfg!(windows) {
        return None;
    }
    let text = name.to_string_lossy();
    if text.ends_with(['.', ' ']) {
        let mut component = display_component(&underscored(name));
        if component.is_empty() {
            component.push(REPLACEMENT);
        }
        if is_device_name(&component) {
            component = device_display(&component);
        }
        let reason = refusal(&text, &why(&PathError::InvalidComponent(String::new())));
        return Some((component, reason));
    }
    if is_device_name(&text) {
        return Some((device_display(&text), refusal(&text, DEVICE)));
    }
    None
}

/// `base`, or the first `base~N` that `taken` does not hold, compared without
/// case. The spelling returned joins `taken`.
///
/// A volume that folds case resolves a spelling that differs from an item only
/// by case to that item, so a free spelling has to be free in every case.
#[must_use]
pub fn free_spelling<S: std::hash::BuildHasher>(
    base: &str,
    taken: &mut std::collections::HashSet<String, S>,
) -> String {
    if taken.insert(base.to_lowercase()) {
        return base.to_owned();
    }
    let mut counter = 1usize;
    loop {
        let spelling = format!("{base}~{counter}");
        if taken.insert(spelling.to_lowercase()) {
            return spelling;
        }
        counter += 1;
    }
}

/// The refusal text of a DOS device name.
const DEVICE: &str = "is a device name on Windows";

/// Why a local path that holds the component `component` does not reach an
/// item, or `None` when it does: on Windows a DOS device name reaches the
/// device.
pub(crate) fn device_refusal(component: &str) -> Option<String> {
    (cfg!(windows) && is_device_name(component)).then(|| refusal(component, DEVICE))
}

/// True when Windows reads `name` as a DOS device: the text before the first
/// dot, less the spaces at its end, is a reserved device name.
fn is_device_name(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    // The longest device name is `COM` or `LPT` and a superscript digit.
    if base.len() > 5 {
        return false;
    }
    let base = base.to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let Some(rest) = base
        .strip_prefix("COM")
        .or_else(|| base.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut chars = rest.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// A device name with `_` after the text before its first dot, which no
/// longer names a device.
fn device_display(name: &str) -> String {
    let cut = name.find('.').unwrap_or(name.len());
    let (base, rest) = name.split_at(cut);
    format!("{base}{REPLACEMENT}{rest}")
}

/// The name with `_` in place of each sequence that does not decode.
fn underscored(name: &std::ffi::OsStr) -> String {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        char::decode_utf16(name.encode_wide())
            .map(|unit| unit.unwrap_or(REPLACEMENT))
            .collect()
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut out = String::new();
        for chunk in name.as_bytes().utf8_chunks() {
            out.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                out.push(REPLACEMENT);
            }
        }
        out
    }
    #[cfg(not(any(windows, unix)))]
    {
        name.to_string_lossy().replace('\u{fffd}', "_")
    }
}

/// The refusal of an entry stored under `stored` in a directory the rules
/// refuse, for a format that names the directory in a record of its own.
pub(crate) fn refused_by_parent(stored: &str) -> String {
    refusal(stored, "lies in a directory whose stored name is refused")
}

/// Mark `entry` refused for `reason`, keeping any text it already carries.
pub(crate) fn refuse(entry: &mut VfsEntry, reason: &str) {
    entry.refused = true;
    entry.error = Some(match entry.error.take() {
        Some(existing) => format!("{existing}; {reason}"),
        None => reason.to_owned(),
    });
}

/// The path a refused name lists at.
///
/// Both slash kinds separate components, and empty and `.` components drop,
/// as they do in [`VfsPath::parse`]. Each leading separator becomes `_`, so an
/// absolute name never lists as the relative one it would otherwise equal.
/// In each component every colon and control character becomes `_`, and so
/// does each dot or space of a trailing run of them, which turns `..` into
/// `__`. Past [`MAX_COMPONENTS`] the remaining components join the last one,
/// and past [`MAX_PATH_BYTES`] the text is cut, so the result always parses.
pub(crate) fn display_path(raw: &str) -> VfsPath {
    let body = raw.trim_start_matches(['/', '\\']);
    let lead = raw.len() - body.len();
    let mut parts: Vec<String> = Vec::new();
    for (index, part) in body.split(['/', '\\']).enumerate() {
        let mut text = String::new();
        if index == 0 {
            text.extend(std::iter::repeat_n(REPLACEMENT, lead));
        }
        text.push_str(part);
        if text.is_empty() || text == "." {
            continue;
        }
        parts.push(display_component(&text));
    }
    if parts.len() > MAX_COMPONENTS {
        let rest = parts.split_off(MAX_COMPONENTS - 1).join("_");
        parts.push(rest);
    }
    let mut text = parts.join("/");
    if text.len() > MAX_PATH_BYTES {
        let mut cut = MAX_PATH_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        while text.ends_with('/') {
            text.pop();
        }
        let last = text.rfind('/').map_or(0, |at| at + 1);
        let fixed = display_component(text.get(last..).unwrap_or_default());
        text.truncate(last);
        text.push_str(&fixed);
    }
    match VfsPath::parse(&text) {
        Ok(path) if !path.is_root() => path,
        _ => placeholder(),
    }
}

/// One component with every refused character replaced.
fn display_component(part: &str) -> String {
    let mut out: String = part
        .chars()
        .map(|ch| {
            if ch == ':' || ch < ' ' {
                REPLACEMENT
            } else {
                ch
            }
        })
        .collect();
    let kept = out.trim_end_matches(['.', ' ']).len();
    let trailing = out.len() - kept;
    out.truncate(kept);
    out.extend(std::iter::repeat_n(REPLACEMENT, trailing));
    out
}

/// The row name for a stored name that maps to nothing at all.
fn placeholder() -> VfsPath {
    VfsPath::parse("_").unwrap_or_default()
}

fn refusal(raw: &str, why: &str) -> String {
    format!(
        "the entry is stored as {}, which {why}; it cannot be opened or extracted",
        quoted(raw)
    )
}

fn quoted(raw: &str) -> String {
    if raw.len() <= QUOTED_BYTES {
        return format!("{raw:?}");
    }
    let mut cut = QUOTED_BYTES;
    while !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{:?}...", raw.get(..cut).unwrap_or_default())
}

fn why(error: &PathError) -> String {
    match error {
        PathError::Absolute(_) => "starts at a root".to_owned(),
        PathError::VolumePrefix(_) => {
            "holds a colon, the mark of a drive or a data stream on Windows".to_owned()
        }
        PathError::ParentComponent(_) => "climbs above the root of the container".to_owned(),
        PathError::Nul(_) => "holds a NUL character".to_owned(),
        PathError::InvalidComponent(_) => {
            "holds a control character, or a name that ends with a dot or a space".to_owned()
        }
        PathError::TooManyComponents { .. } => {
            format!("has more than {MAX_COMPONENTS} components")
        }
        PathError::TooLong { .. } => format!("is longer than {MAX_PATH_BYTES} bytes"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn display(raw: &str) -> String {
        match stored_path(raw, false) {
            Stored::Refused { display, .. } => display.as_str().to_owned(),
            other => panic!("{raw:?} was not refused: {other:?}"),
        }
    }

    #[test]
    fn each_refused_character_becomes_an_underscore() {
        let cases = [
            ("log-12:30.txt", "log-12_30.txt"),
            ("trailing-dot.", "trailing-dot_"),
            ("trailing-space ", "trailing-space_"),
            ("sub/12:00/x.txt", "sub/12_00/x.txt"),
            ("notes/2024-01-01T12:30.log", "notes/2024-01-01T12_30.log"),
            ("a. .", "a___"),
            ("tab\there", "tab_here"),
            ("nul\0here", "nul_here"),
            ("../escaped.txt", "__/escaped.txt"),
            ("..\\escaped.txt", "__/escaped.txt"),
            ("a/../../escaped.txt", "a/__/__/escaped.txt"),
            ("/absolute.txt", "_absolute.txt"),
            (
                "C:/Windows/system32/escaped.txt",
                "C_/Windows/system32/escaped.txt",
            ),
            ("C:\\evil.txt", "C_/evil.txt"),
            (
                "\\\\server\\share\\escaped.txt",
                "__server/share/escaped.txt",
            ),
            ("/", "_"),
        ];
        for (raw, expected) in cases {
            assert_eq!(display(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn a_usable_name_resolves_as_itself_and_the_root_lists_no_directory() {
        assert_eq!(
            stored_path("a/b.txt", false),
            Stored::Usable(VfsPath::parse("a/b.txt").unwrap())
        );
        assert_eq!(stored_path("./", true), Stored::Root);
        assert_eq!(display(""), "_");
        assert_eq!(display("."), "_");
    }

    #[test]
    fn the_reason_quotes_the_stored_name() {
        let Stored::Refused { reason, .. } = stored_path("log-12:30.txt", false) else {
            panic!("not refused");
        };
        assert!(reason.contains("\"log-12:30.txt\""), "{reason}");
        assert!(reason.contains("cannot be opened"), "{reason}");
    }

    #[test]
    fn a_name_past_the_ceilings_still_lists_under_a_usable_path() {
        let deep = "a/".repeat(2000) + "f:.txt";
        let path = VfsPath::parse(&display(&deep)).unwrap();
        assert_eq!(path.depth(), MAX_COMPONENTS);

        let long = "x".repeat(MAX_PATH_BYTES + 100) + ":";
        let path = VfsPath::parse(&display(&long)).unwrap();
        assert!(path.as_str().len() <= MAX_PATH_BYTES);

        let cut_at_a_dot = ".".repeat(MAX_PATH_BYTES + 10);
        assert!(VfsPath::parse(&display(&cut_at_a_dot)).is_ok());

        let long_quote = "y".repeat(10_000) + ":";
        let Stored::Refused { reason, .. } = stored_path(&long_quote, false) else {
            panic!("not refused");
        };
        assert!(reason.len() < 1_000, "{} bytes", reason.len());
    }

    #[test]
    fn a_device_name_is_matched_on_the_text_before_its_first_dot() {
        for name in [
            "nul",
            "NUL",
            "con.txt",
            "aux .c",
            "prn",
            "com0",
            "COM9.tar.gz",
            "lpt1",
            "com\u{b9}",
            "LPT\u{b3}.log",
        ] {
            assert!(is_device_name(name), "{name:?}");
        }
        for name in [
            "null",
            "com10",
            "console",
            "nul_",
            "anul",
            "lpt",
            ".nul",
            "com\u{b4}",
        ] {
            assert!(!is_device_name(name), "{name:?}");
        }
        assert_eq!(device_display("nul"), "nul_");
        assert_eq!(device_display("con.txt"), "con_.txt");
        assert!(!is_device_name(&device_display("COM9.tar.gz")));
    }

    #[test]
    fn a_free_spelling_is_free_in_every_case() {
        let mut taken: std::collections::HashSet<String> =
            ["a_", "a_~1"].into_iter().map(str::to_owned).collect();
        assert_eq!(free_spelling("A_", &mut taken), "A_~2");
        assert_eq!(free_spelling("b_", &mut taken), "b_");
        assert_eq!(free_spelling("B_", &mut taken), "B_~1");
    }

    #[test]
    fn a_child_name_is_one_component_whatever_it_holds() {
        let dir = VfsPath::parse("d").unwrap();
        assert_eq!(
            stored_child(&dir, "ok.txt"),
            Stored::Usable(VfsPath::parse("d/ok.txt").unwrap())
        );
        for (raw, expected) in [
            ("a:b", "d/a_b"),
            ("x/y", "d/x_y"),
            ("..", "d/__"),
            (".", "d/_"),
            ("", "d/_"),
        ] {
            let Stored::Refused { display, .. } = stored_child(&dir, raw) else {
                panic!("{raw:?} was not refused");
            };
            assert_eq!(display.as_str(), expected, "{raw:?}");
        }
        let Stored::Refused { display, .. } = stored_child(&VfsPath::root(), "a:b") else {
            panic!("not refused");
        };
        assert_eq!(display.as_str(), "a_b");
    }
}
