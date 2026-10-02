//! Name masks and non-name filters applied to scanned entries.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::scan::Entry;

/// Whether name comparisons distinguish case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseSensitivity {
    /// Insensitive on Windows, sensitive elsewhere.
    #[default]
    Platform,
    /// Always distinguish case.
    Sensitive,
    /// Never distinguish case.
    Insensitive,
}

impl CaseSensitivity {
    /// True when names compare without regard to case.
    #[must_use]
    pub fn ignores_case(self) -> bool {
        match self {
            Self::Platform => cfg!(windows),
            Self::Sensitive => false,
            Self::Insensitive => true,
        }
    }
}

/// Where a mask's segments are allowed to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The segments match at any depth.
    Anywhere,
    /// The segments match only directly under the base folder.
    Base,
}

/// One parsed name mask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    /// True when a match excludes rather than includes.
    pub exclude: bool,
    /// True when the mask matches folders, false when it matches files.
    pub folder: bool,
    /// Where the segments are allowed to start.
    pub anchor: Anchor,
    /// Wildcard pattern per path segment, outermost first.
    pub segments: Vec<String>,
}

impl Mask {
    /// Parse a single mask.
    ///
    /// A leading `-` excludes. A trailing separator makes the mask a folder
    /// mask. A leading `.` segment anchors the mask to the base folder; a
    /// leading `...` segment states the default any-depth behavior.
    ///
    /// Returns `None` for text that holds no segments.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (exclude, text) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let folder = text.ends_with('\\') || text.ends_with('/');
        let body = text.trim_end_matches(['\\', '/']);
        let mut segments: Vec<String> = body
            .split(['\\', '/'])
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let anchor = match segments.first().map(String::as_str) {
            Some(".") => {
                segments.remove(0);
                Anchor::Base
            }
            Some("...") => {
                segments.remove(0);
                Anchor::Anywhere
            }
            _ => Anchor::Anywhere,
        };
        if segments.is_empty() {
            return None;
        }
        Some(Self {
            exclude,
            folder,
            anchor,
            segments,
        })
    }

    /// Parse a semicolon or newline separated mask list.
    #[must_use]
    pub fn parse_list(text: &str) -> Vec<Self> {
        text.split([';', '\n', '\r'])
            .filter_map(Self::parse)
            .collect()
    }

    /// True when the mask matches `rel`, a path relative to the base folder.
    ///
    /// A folder mask matches the named folder and everything below it; a file
    /// mask matches only the final component.
    #[must_use]
    pub fn matches(&self, rel: &Path, ignore_case: bool) -> bool {
        let path: Vec<String> = rel
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        let count = self.segments.len();
        if path.len() < count {
            return false;
        }
        let last_start = path.len() - count;
        let mut starts: Vec<usize> = match self.anchor {
            Anchor::Base => vec![0],
            Anchor::Anywhere if self.folder => (0..=last_start).collect(),
            Anchor::Anywhere => vec![last_start],
        };
        if !self.folder {
            starts.retain(|start| start + count == path.len());
        }
        starts.into_iter().any(|start| {
            self.segments.iter().enumerate().all(|(offset, pattern)| {
                match path.get(start + offset) {
                    Some(name) => wildcard_match(pattern, name, ignore_case),
                    None => false,
                }
            })
        })
    }
}

/// The four name mask lists of a session, already parsed.
#[derive(Debug, Clone, Default)]
pub struct NameFilters {
    /// Masks that select which entries take part.
    pub include: Vec<Mask>,
    /// Masks that remove entries from the session.
    pub exclude: Vec<Mask>,
    /// Case rule for mask matching.
    pub case: CaseSensitivity,
}

impl NameFilters {
    /// Parse the toolbar form: one semicolon separated list where a leading
    /// `-` marks an exclusion and a trailing separator marks a folder mask.
    #[must_use]
    pub fn parse(spec: &str) -> Self {
        let mut filters = Self::default();
        for mask in Mask::parse_list(spec) {
            if mask.exclude {
                filters.exclude.push(mask);
            } else {
                filters.include.push(mask);
            }
        }
        filters
    }

    /// Build from the four separate lists of the name filter tab. The lists
    /// supply the include or exclude role and the file or folder role; a mask
    /// that also carries its own markers keeps them.
    #[must_use]
    pub fn from_lists(
        include_files: &str,
        exclude_files: &str,
        include_folders: &str,
        exclude_folders: &str,
    ) -> Self {
        let mut filters = Self::default();
        let mut push = |text: &str, exclude: bool, folder: bool| {
            for mut mask in Mask::parse_list(text) {
                mask.exclude |= exclude;
                mask.folder |= folder;
                if mask.exclude {
                    filters.exclude.push(mask);
                } else {
                    filters.include.push(mask);
                }
            }
        };
        push(include_files, false, false);
        push(exclude_files, true, false);
        push(include_folders, false, true);
        push(exclude_folders, true, true);
        filters
    }

    /// True when an entry at `rel` takes part in the session.
    ///
    /// The base folder, whose relative path is empty, is always included.
    /// Include masks are kept apart by kind, so a file include list never hides
    /// the folders that contain matches. An excluded folder takes its contents
    /// with it, so a file inside it is refused as well.
    #[must_use]
    pub fn allows(&self, rel: &Path, is_dir: bool) -> bool {
        if rel.as_os_str().is_empty() {
            return true;
        }
        let ignore_case = self.case.ignores_case();
        if self.excluded(rel, is_dir, ignore_case) {
            return false;
        }
        let mut relevant = self
            .include
            .iter()
            .filter(|mask| mask.folder == is_dir)
            .peekable();
        if relevant.peek().is_none() {
            return true;
        }
        relevant.any(|mask| mask.matches(rel, ignore_case))
    }

    /// True when a folder must still be read because something below it may be
    /// included.
    ///
    /// A folder include mask names the folders that take part, not the route to
    /// them, so an ancestor of a possible match survives this test and is
    /// dropped later only when no descendant survives.
    #[must_use]
    pub fn allows_folder_traversal(&self, rel: &Path) -> bool {
        if rel.as_os_str().is_empty() {
            return true;
        }
        !self.excluded(rel, true, self.case.ignores_case())
    }

    fn excluded(&self, rel: &Path, is_dir: bool, ignore_case: bool) -> bool {
        if self
            .exclude
            .iter()
            .filter(|mask| mask.folder == is_dir)
            .any(|mask| mask.matches(rel, ignore_case))
        {
            return true;
        }
        if is_dir {
            return false;
        }
        let Some(parent) = rel.parent().filter(|parent| !parent.as_os_str().is_empty()) else {
            return false;
        };
        self.exclude
            .iter()
            .filter(|mask| mask.folder)
            .any(|mask| mask.matches(parent, ignore_case))
    }
}

/// A Windows attribute selectable by an attribute filter or an attribute
/// comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeKind {
    /// Read-only.
    ReadOnly,
    /// Hidden.
    Hidden,
    /// System.
    System,
    /// Archive.
    Archive,
}

impl AttributeKind {
    /// Read this attribute from a scanned entry.
    #[must_use]
    pub fn read(self, entry: &Entry) -> bool {
        match self {
            Self::ReadOnly => entry.attributes.read_only,
            Self::Hidden => entry.attributes.hidden,
            Self::System => entry.attributes.system,
            Self::Archive => entry.attributes.archive,
        }
    }
}

/// A point in time a date filter compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterTime {
    /// An absolute instant.
    Absolute(SystemTime),
    /// Whole days counted back from midnight of the day the comparison runs.
    DaysAgo(u32),
}

/// A POSIX file type an entry can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixFileType {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Block device.
    Block,
    /// Character device.
    Character,
    /// Named pipe.
    Fifo,
    /// Socket.
    Socket,
}

impl UnixFileType {
    /// The type a name states, ignoring case.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "regular" | "file" => Some(Self::Regular),
            "directory" | "dir" => Some(Self::Directory),
            "symlink" | "link" => Some(Self::Symlink),
            "block" => Some(Self::Block),
            "character" | "char" => Some(Self::Character),
            "fifo" | "pipe" => Some(Self::Fifo),
            "socket" => Some(Self::Socket),
            _ => None,
        }
    }

    /// The mode bits this type occupies.
    const fn bits(self) -> u32 {
        match self {
            Self::Fifo => 0o010_000,
            Self::Character => 0o020_000,
            Self::Directory => 0o040_000,
            Self::Block => 0o060_000,
            Self::Regular => 0o100_000,
            Self::Symlink => 0o120_000,
            Self::Socket => 0o140_000,
        }
    }

    /// True when the entry carries this type.
    ///
    /// A platform that reports no mode word answers false for every type, so a
    /// type filter excludes nothing there.
    #[must_use]
    pub fn matches(self, entry: &Entry) -> bool {
        match entry.attributes.unix_mode {
            Some(mode) => mode & 0o170_000 == self.bits(),
            None => false,
        }
    }
}

/// One item of the non-name filter list. A matching entry is excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherFilter {
    /// Exclude entries modified before the given point.
    ModifiedOlderThan(FilterTime),
    /// Exclude entries modified after the given point.
    ModifiedNewerThan(FilterTime),
    /// Exclude entries smaller than the given size in bytes.
    SmallerThan(u64),
    /// Exclude entries larger than the given size in bytes.
    LargerThan(u64),
    /// Exclude entries carrying the attribute.
    AttributeSet(AttributeKind),
    /// Exclude entries not carrying the attribute.
    AttributeNotSet(AttributeKind),
    /// Exclude entries of this POSIX file type.
    UnixFileTypeIs(UnixFileType),
    /// Exclude entries that are not of this POSIX file type.
    UnixFileTypeIsNot(UnixFileType),
}

/// The clock and local offset a date filter resolves against.
#[derive(Debug, Clone, Copy)]
pub struct FilterContext {
    /// The instant the comparison runs.
    pub now: SystemTime,
    /// Seconds to add to UTC to reach local time.
    pub local_offset_seconds: i32,
}

impl Default for FilterContext {
    fn default() -> Self {
        Self {
            now: SystemTime::now(),
            local_offset_seconds: 0,
        }
    }
}

impl FilterContext {
    /// Resolve a filter time to seconds since the Unix epoch.
    ///
    /// A day count resolves against midnight local time of the day the
    /// comparison runs, so a filter written in days does not drift with the
    /// time of day the session is opened.
    #[must_use]
    pub fn resolve(&self, time: FilterTime) -> i64 {
        match time {
            FilterTime::Absolute(instant) => unix_seconds(instant),
            FilterTime::DaysAgo(days) => {
                let offset = i64::from(self.local_offset_seconds);
                let local = unix_seconds(self.now) + offset;
                let midnight = local.div_euclid(86_400) * 86_400;
                midnight - offset - i64::from(days) * 86_400
            }
        }
    }
}

/// One exclusion that reads the bytes of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFilter {
    /// Text searched for.
    pub text: String,
    /// Exclude entries that do not hold the text rather than those that do.
    pub not_containing: bool,
}

/// The non-name filter settings of a session.
#[derive(Debug, Clone)]
pub struct OtherFilters {
    /// Filter items; an entry matching any of them is excluded.
    pub items: Vec<OtherFilter>,
    /// Exclusions that read file contents.
    pub content: Vec<ContentFilter>,
    /// Exclude entries carrying both the hidden and system attributes.
    pub exclude_protected_system: bool,
    /// Exclude entries carrying the hidden attribute.
    pub exclude_hidden: bool,
    /// Left base folder, so a content filter can reach the bytes.
    pub left_root: std::path::PathBuf,
    /// Right base folder, so a content filter can reach the bytes.
    pub right_root: std::path::PathBuf,
    /// Greatest number of bytes a content filter reads from one file.
    pub content_read_limit: u64,
}

impl Default for OtherFilters {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            content: Vec::new(),
            exclude_protected_system: true,
            exclude_hidden: false,
            left_root: std::path::PathBuf::new(),
            right_root: std::path::PathBuf::new(),
            content_read_limit: 4 * 1024 * 1024,
        }
    }
}

impl OtherFilters {
    /// True when the entry survives every non-name filter.
    #[must_use]
    pub fn allows(&self, entry: &Entry, context: &FilterContext) -> bool {
        if self.exclude_protected_system && entry.attributes.hidden && entry.attributes.system {
            return false;
        }
        if self.exclude_hidden && entry.attributes.hidden {
            return false;
        }
        if self
            .items
            .iter()
            .any(|item| excludes(*item, entry, context))
        {
            return false;
        }
        self.content.is_empty() || self.content_allows(entry)
    }

    /// True when no content exclusion matches.
    ///
    /// A file that cannot be read holds no text, so a "contains" rule leaves it
    /// in and a "does not contain" rule takes it out. A directory is never read.
    fn content_allows(&self, entry: &Entry) -> bool {
        if entry.is_dir {
            return true;
        }
        let text = self.read_head(entry);
        !self.content.iter().any(|filter| {
            let found = !filter.text.is_empty() && text.contains(&filter.text);
            found != filter.not_containing
        })
    }

    fn read_head(&self, entry: &Entry) -> String {
        use std::io::Read;
        for root in [&self.left_root, &self.right_root] {
            if root.as_os_str().is_empty() {
                continue;
            }
            let Ok(file) = std::fs::File::open(root.join(&entry.rel)) else {
                continue;
            };
            let mut buffer = Vec::new();
            if file
                .take(self.content_read_limit)
                .read_to_end(&mut buffer)
                .is_err()
            {
                continue;
            }
            return String::from_utf8_lossy(&buffer).into_owned();
        }
        String::new()
    }
}

fn excludes(item: OtherFilter, entry: &Entry, context: &FilterContext) -> bool {
    match item {
        OtherFilter::ModifiedOlderThan(time) => entry
            .modified
            .is_some_and(|modified| unix_seconds(modified) < context.resolve(time)),
        OtherFilter::ModifiedNewerThan(time) => entry
            .modified
            .is_some_and(|modified| unix_seconds(modified) > context.resolve(time)),
        OtherFilter::SmallerThan(size) => !entry.is_dir && entry.size < size,
        OtherFilter::LargerThan(size) => !entry.is_dir && entry.size > size,
        OtherFilter::AttributeSet(kind) => kind.read(entry),
        OtherFilter::AttributeNotSet(kind) => !kind.read(entry),
        OtherFilter::UnixFileTypeIs(kind) => kind.matches(entry),
        OtherFilter::UnixFileTypeIsNot(kind) => {
            entry.attributes.unix_mode.is_some() && !kind.matches(entry)
        }
    }
}

/// Seconds since the Unix epoch, negative for earlier instants.
#[must_use]
pub fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => i64::try_from(delta.as_secs()).unwrap_or(i64::MAX),
        Err(err) => -i64::try_from(err.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Build a [`SystemTime`] from seconds since the Unix epoch when it fits the
/// range this platform can represent.
#[must_use]
pub fn from_unix_seconds(seconds: i64) -> Option<SystemTime> {
    let magnitude = Duration::from_secs(seconds.unsigned_abs());
    if seconds < 0 {
        UNIX_EPOCH.checked_sub(magnitude)
    } else {
        UNIX_EPOCH.checked_add(magnitude)
    }
}

/// Parse a size written as a number with an optional `B`, `KB`, `MB` or `GB`
/// unit. Units are binary multiples.
///
/// # Errors
/// Returns the offending text when the number or the unit cannot be read.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, unit) = trimmed.split_at(split);
    let value: u64 = digits.trim().parse().map_err(|_| text.to_owned())?;
    let multiplier = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1_u64,
        "K" | "KB" => 1_024,
        "M" | "MB" => 1_024 * 1_024,
        "G" | "GB" => 1_024 * 1_024 * 1_024,
        _ => return Err(text.to_owned()),
    };
    value.checked_mul(multiplier).ok_or_else(|| text.to_owned())
}

/// Match one path component against one wildcard pattern.
///
/// `?` matches a single character, `*` matches zero or more, `[az]`, `[a-z]`
/// and `[!az]` match a single character by set, and `[[]` matches a literal
/// bracket. `*.*` matches every name, including names with no extension. When
/// the pattern's last character is a period, `?` and `*` stop matching periods
/// and the trailing period itself matches the empty extension.
#[must_use]
pub fn wildcard_match(pattern: &str, name: &str, ignore_case: bool) -> bool {
    if pattern == "*.*" {
        return true;
    }
    let fold = |text: &str| {
        if ignore_case {
            text.to_lowercase()
        } else {
            text.to_owned()
        }
    };
    let pattern_text = fold(pattern);
    let name_text = fold(name);
    let mut pattern_chars: Vec<char> = pattern_text.chars().collect();
    let name_chars: Vec<char> = name_text.chars().collect();
    let trailing_period = pattern_chars.last() == Some(&'.');
    if trailing_period {
        pattern_chars.pop();
        let stripped: &[char] = if name_chars.last() == Some(&'.') {
            &name_chars[..name_chars.len() - 1]
        } else {
            &name_chars
        };
        return match_from(&tokenize(&pattern_chars), stripped, true);
    }
    match_from(&tokenize(&pattern_chars), &name_chars, false)
}

enum Token {
    /// Zero or more characters.
    Star,
    /// Exactly one character.
    Any,
    /// One character drawn from a bracket set.
    Set(CharSet),
    /// One literal character.
    Literal(char),
    /// A bracket that never closes, which matches nothing.
    Unterminated,
}

fn tokenize(pattern: &[char]) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some((head, tail)) = rest.split_first() {
        match head {
            '*' => {
                // Runs of stars behave as one, which keeps the match linear.
                if !matches!(tokens.last(), Some(Token::Star)) {
                    tokens.push(Token::Star);
                }
                rest = tail;
            }
            '?' => {
                tokens.push(Token::Any);
                rest = tail;
            }
            '[' => {
                if let Some((set, after)) = parse_set(tail) {
                    tokens.push(Token::Set(set));
                    rest = after;
                } else {
                    tokens.push(Token::Unterminated);
                    rest = &[];
                }
            }
            literal => {
                tokens.push(Token::Literal(*literal));
                rest = tail;
            }
        }
    }
    tokens
}

fn token_matches(token: &Token, candidate: char, no_period: bool) -> bool {
    match token {
        Token::Star | Token::Unterminated => false,
        Token::Any => !(no_period && candidate == '.'),
        Token::Set(set) => set.matches(candidate),
        Token::Literal(literal) => *literal == candidate,
    }
}

/// Match a token sequence against a name with one remembered star, so a
/// pattern of many stars costs time proportional to pattern times name rather
/// than exponential in the number of stars.
fn match_from(tokens: &[Token], name: &[char], no_period: bool) -> bool {
    let mut token_index = 0;
    let mut name_index = 0;
    let mut star: Option<usize> = None;
    let mut star_name = 0;

    while name_index < name.len() {
        if token_index < tokens.len()
            && token_matches(&tokens[token_index], name[name_index], no_period)
        {
            token_index += 1;
            name_index += 1;
        } else if token_index < tokens.len() && matches!(tokens[token_index], Token::Star) {
            star = Some(token_index);
            star_name = name_index;
            token_index += 1;
        } else if let Some(star_index) = star {
            if no_period && name[star_name] == '.' {
                return false;
            }
            star_name += 1;
            name_index = star_name;
            token_index = star_index + 1;
        } else {
            return false;
        }
    }

    tokens[token_index..]
        .iter()
        .all(|token| matches!(token, Token::Star))
}

struct CharSet {
    negated: bool,
    singles: Vec<char>,
    ranges: Vec<(char, char)>,
}

impl CharSet {
    fn matches(&self, candidate: char) -> bool {
        let hit = self.singles.contains(&candidate)
            || self
                .ranges
                .iter()
                .any(|(low, high)| candidate >= *low && candidate <= *high);
        hit != self.negated
    }
}

/// Read a bracket set starting just after the opening bracket. The first
/// character is always literal, so `[[]` reads as the set holding a bracket.
fn parse_set(pattern: &[char]) -> Option<(CharSet, &[char])> {
    let mut index = 0;
    let negated = pattern.first() == Some(&'!');
    if negated {
        index += 1;
    }
    let mut set = CharSet {
        negated,
        singles: Vec::new(),
        ranges: Vec::new(),
    };
    let mut first = true;
    while index < pattern.len() {
        let current = pattern[index];
        if current == ']' && !first {
            return Some((set, &pattern[index + 1..]));
        }
        first = false;
        if pattern.get(index + 1) == Some(&'-') {
            if let Some(high) = pattern.get(index + 2) {
                if *high != ']' {
                    set.ranges.push((current, *high));
                    index += 3;
                    continue;
                }
            }
        }
        set.singles.push(current);
        index += 1;
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        from_unix_seconds, parse_size, wildcard_match, AttributeKind, CaseSensitivity,
        FilterContext, FilterTime, Mask, NameFilters, OtherFilter, OtherFilters,
    };
    use crate::scan::{Attributes, Entry};
    use std::path::{Path, PathBuf};
    use std::time::UNIX_EPOCH;

    fn entry(name: &str, size: u64, modified_seconds: i64, attributes: Attributes) -> Entry {
        Entry {
            rel: PathBuf::from(name),
            name: name.to_owned(),
            is_dir: false,
            size,
            modified: Some(from_unix_seconds(modified_seconds).unwrap_or(UNIX_EPOCH)),
            created: None,
            attributes,
            link: None,
            error: None,
            listing_incomplete: false,
            refused: false,
        }
    }

    #[test]
    fn wildcard_table() {
        let cases = [
            ("*.txt", "a.txt", true),
            ("*.txt", "a.bak", false),
            ("?.txt", "a.txt", true),
            ("?.txt", "ab.txt", false),
            ("[az].txt", "a.txt", true),
            ("[az].txt", "b.txt", false),
            ("[a-z].txt", "q.txt", true),
            ("[a-z].txt", "0.txt", false),
            ("[!az].txt", "b.txt", true),
            ("[!az].txt", "a.txt", false),
            ("[[]*", "[draft].txt", true),
            ("*.*", "readme", true),
            ("*.*", "a.txt", true),
            ("*.", "readme", true),
            ("*.", "a.txt", false),
            ("abc?.bak", "abc1.bak", true),
        ];
        for (pattern, name, expected) in cases {
            assert_eq!(
                wildcard_match(pattern, name, false),
                expected,
                "{pattern} against {name}"
            );
        }
    }

    #[test]
    fn many_stars_do_not_blow_up() {
        let pattern = "*a*a*a*a*a*a*a*a*a*b";
        let name = "a".repeat(36);
        assert!(!wildcard_match(pattern, &name, false));
        assert!(wildcard_match(pattern, &format!("{name}b"), false));
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn many_stars_match_within_the_budget() {
        let pattern = "*a*a*a*a*a*a*a*a*a*b";
        let name = "a".repeat(36);
        let start = std::time::Instant::now();
        assert!(!wildcard_match(pattern, &name, false));
        assert!(start.elapsed() < std::time::Duration::from_millis(50));
    }

    #[test]
    fn a_folder_exclude_mask_takes_the_files_inside_with_it() {
        let filters = NameFilters::parse("-Backup\\");
        assert!(!filters.allows(Path::new("Backup"), true));
        assert!(!filters.allows(&Path::new("Backup").join("a.txt"), false));
        assert!(!filters.allows(&Path::new("Backup").join("deep").join("a.txt"), false));
        assert!(filters.allows(Path::new("a.txt"), false));
        // A file mask never judges a folder.
        assert!(NameFilters::parse("-a.txt").allows(Path::new("a.txt"), true));
    }

    #[test]
    fn an_include_folder_mask_leaves_ancestors_traversable() {
        let filters = NameFilters::from_lists("", "", "Source", "");
        assert!(!filters.allows(Path::new("a"), true));
        assert!(filters.allows_folder_traversal(Path::new("a")));
        assert!(filters.allows(&Path::new("a").join("Source"), true));
        let excluded = NameFilters::from_lists("", "", "Source", "a");
        assert!(!excluded.allows_folder_traversal(Path::new("a")));
    }

    #[test]
    fn case_rule_controls_matching() {
        assert!(!wildcard_match("*.TXT", "a.txt", false));
        assert!(wildcard_match("*.TXT", "a.txt", true));
        assert!(CaseSensitivity::Insensitive.ignores_case());
        assert!(!CaseSensitivity::Sensitive.ignores_case());
    }

    #[test]
    fn mask_forms_parse() {
        let file = Mask::parse("f").unwrap();
        assert!(!file.exclude && !file.folder);
        let folder = Mask::parse("p\\").unwrap();
        assert!(folder.folder && !folder.exclude);
        let excluded_folder = Mask::parse("-p\\").unwrap();
        assert!(excluded_folder.folder && excluded_folder.exclude);
        assert!(Mask::parse("-f").unwrap().exclude);
        assert_eq!(Mask::parse(".\\f").unwrap().segments, vec!["f".to_owned()]);
        assert_eq!(
            Mask::parse("...\\Windows\\*.txt").unwrap().segments,
            vec!["Windows".to_owned(), "*.txt".to_owned()]
        );
        assert!(Mask::parse("   ").is_none());
    }

    #[test]
    fn mask_list_splits_on_semicolons() {
        let masks = Mask::parse_list("*.pas;*.dfm;*.dpr");
        assert_eq!(masks.len(), 3);
    }

    #[test]
    fn path_qualified_masks() {
        let base_only = Mask::parse(".\\f.txt").unwrap();
        assert!(base_only.matches(Path::new("f.txt"), false));
        assert!(!base_only.matches(&Path::new("sub").join("f.txt"), false));

        let any_depth = Mask::parse("...\\Windows\\*.txt").unwrap();
        assert!(any_depth.matches(&Path::new("a").join("Windows").join("n.txt"), false));
        assert!(any_depth.matches(&Path::new("Windows").join("n.txt"), false));
        assert!(!any_depth.matches(&Path::new("Windows").join("n.bin"), false));

        let relative = Mask::parse("p\\f.txt").unwrap();
        assert!(relative.matches(&Path::new("x").join("p").join("f.txt"), false));
        assert!(!relative.matches(&Path::new("x").join("f.txt"), false));

        let simple = Mask::parse("f.txt").unwrap();
        assert!(simple.matches(&Path::new("deep").join("f.txt"), false));
    }

    #[test]
    fn folder_mask_covers_subtree() {
        let mask = Mask::parse("Source\\").unwrap();
        assert!(mask.matches(Path::new("Source"), false));
        assert!(mask.matches(&Path::new("Source").join("inner"), false));
        assert!(!mask.matches(Path::new("Other"), false));
    }

    #[test]
    fn include_and_exclude_interact() {
        let filters = NameFilters::parse("*.pas;*.dfm;-x*.pas;-Backup\\");
        assert!(filters.allows(Path::new("a.pas"), false));
        assert!(!filters.allows(Path::new("x1.pas"), false));
        assert!(!filters.allows(Path::new("a.txt"), false));
        assert!(!filters.allows(Path::new("Backup"), true));
        assert!(filters.allows(Path::new("Source"), true));
        assert!(filters.allows(Path::new(""), false));
    }

    #[test]
    fn four_list_form_assigns_roles() {
        let filters = NameFilters::from_lists("*.txt", "-secret.txt", "Source", "Backup");
        assert!(filters.allows(Path::new("a.txt"), false));
        assert!(!filters.allows(Path::new("secret.txt"), false));
        assert!(filters.allows(Path::new("Source"), true));
        assert!(!filters.allows(Path::new("Other"), true));
        assert!(!filters.allows(Path::new("Backup"), true));
    }

    #[test]
    fn size_filters_and_parsing() {
        assert_eq!(parse_size("10").unwrap(), 10);
        assert_eq!(parse_size("2 KB").unwrap(), 2048);
        assert_eq!(parse_size("1MB").unwrap(), 1_048_576);
        assert!(parse_size("1 furlong").is_err());

        let filters = OtherFilters {
            items: vec![OtherFilter::LargerThan(100)],
            exclude_protected_system: false,
            exclude_hidden: false,
            ..OtherFilters::default()
        };
        let context = FilterContext::default();
        assert!(!filters.allows(&entry("big", 200, 0, Attributes::default()), &context));
        assert!(filters.allows(&entry("small", 10, 0, Attributes::default()), &context));
    }

    #[test]
    fn days_ago_counts_from_local_midnight() {
        let context = FilterContext {
            now: from_unix_seconds(86_400 * 10 + 3_600 * 13).unwrap_or(UNIX_EPOCH),
            local_offset_seconds: 0,
        };
        assert_eq!(context.resolve(FilterTime::DaysAgo(0)), 86_400 * 10);
        assert_eq!(context.resolve(FilterTime::DaysAgo(3)), 86_400 * 7);
        assert_eq!(context.resolve(FilterTime::Absolute(UNIX_EPOCH)), 0);
    }

    #[test]
    fn date_filters_exclude_by_age() {
        let context = FilterContext {
            now: from_unix_seconds(86_400 * 10).unwrap_or(UNIX_EPOCH),
            local_offset_seconds: 0,
        };
        let older = OtherFilters {
            items: vec![OtherFilter::ModifiedOlderThan(FilterTime::DaysAgo(2))],
            exclude_protected_system: false,
            exclude_hidden: false,
            ..OtherFilters::default()
        };
        assert!(!older.allows(
            &entry("old", 1, 86_400 * 5, Attributes::default()),
            &context
        ));
        assert!(older.allows(
            &entry("fresh", 1, 86_400 * 9, Attributes::default()),
            &context
        ));
    }

    #[test]
    fn attribute_filters_and_protected_default() {
        let hidden_system = Attributes {
            hidden: true,
            system: true,
            ..Attributes::default()
        };
        let context = FilterContext::default();
        let defaults = OtherFilters::default();
        assert!(defaults.exclude_protected_system);
        assert!(!defaults.allows(&entry("p", 1, 0, hidden_system), &context));

        let by_attribute = OtherFilters {
            items: vec![OtherFilter::AttributeSet(AttributeKind::ReadOnly)],
            exclude_protected_system: false,
            exclude_hidden: false,
            ..OtherFilters::default()
        };
        let read_only = Attributes {
            read_only: true,
            ..Attributes::default()
        };
        assert!(!by_attribute.allows(&entry("r", 1, 0, read_only), &context));
        assert!(by_attribute.allows(&entry("w", 1, 0, Attributes::default()), &context));

        let unset = OtherFilters {
            items: vec![OtherFilter::AttributeNotSet(AttributeKind::Archive)],
            exclude_protected_system: false,
            exclude_hidden: false,
            ..OtherFilters::default()
        };
        assert!(!unset.allows(&entry("w", 1, 0, Attributes::default()), &context));
    }
}
