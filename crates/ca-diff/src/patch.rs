//! Reading unified and context diff files, and applying them to a text.
//!
//! The parser takes the patch as untrusted input. A stated line count is only
//! a number to count down, never a size to reserve, and the whole input, the
//! file count, the hunk count and the line count each stop at a ceiling in
//! [`PatchLimits`], so a hostile file ends in an error rather than in an
//! allocation it names.

/// Ceilings the parser checks before it accepts more input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchLimits {
    /// Largest patch accepted, in bytes.
    pub max_input_bytes: usize,
    /// Largest number of file sections one patch may hold.
    pub max_files: usize,
    /// Largest number of hunks over all file sections.
    pub max_hunks: usize,
    /// Largest number of hunk body lines over all file sections.
    pub max_lines: usize,
}

impl Default for PatchLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 << 20,
            max_files: 10_000,
            max_hunks: 1_000_000,
            max_lines: 4_000_000,
        }
    }
}

/// Why a patch could not be read or applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    /// The input passed one of the ceilings in [`PatchLimits`].
    #[error("the patch is larger than the {what} limit of {limit}")]
    LimitExceeded {
        /// The ceiling that was passed.
        what: &'static str,
        /// Its value.
        limit: usize,
    },
    /// A line does not have the form its place in the patch requires.
    #[error("line {line} of the patch is malformed: {detail}")]
    Malformed {
        /// One based line number in the patch.
        line: usize,
        /// What is wrong with it.
        detail: String,
    },
    /// The input holds no hunk in a format this parser reads.
    #[error("the file holds no unified or context diff hunk")]
    NoHunks,
}

/// What one body line of a hunk does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchLineKind {
    /// Present on both sides.
    Context,
    /// Present only on the original side.
    Removed,
    /// Present only on the patched side.
    Added,
}

/// One body line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchLine {
    /// What the line does.
    pub kind: PatchLineKind,
    /// The text, without its prefix and without its line ending.
    pub text: String,
    /// The line is the last of its side and has no line ending there.
    pub no_newline: bool,
}

/// One hunk: a run of lines around a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    /// First original line the hunk covers, one based as the header states it.
    pub old_start: usize,
    /// Number of original lines the hunk covers.
    pub old_count: usize,
    /// First patched line the hunk covers, one based as the header states it.
    pub new_start: usize,
    /// Number of patched lines the hunk covers.
    pub new_count: usize,
    /// The body, in order.
    pub lines: Vec<PatchLine>,
}

impl PatchHunk {
    /// The lines of the original side, in order.
    pub fn old_lines(&self) -> impl Iterator<Item = &PatchLine> {
        self.lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Added)
    }

    /// The lines of the patched side, in order.
    pub fn new_lines(&self) -> impl Iterator<Item = &PatchLine> {
        self.lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Removed)
    }
}

/// The hunks for one file pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilePatch {
    /// The original name the header states, without any timestamp.
    pub old_name: Option<String>,
    /// The patched name the header states, without any timestamp.
    pub new_name: Option<String>,
    /// The hunks, in file order.
    pub hunks: Vec<PatchHunk>,
}

impl FilePatch {
    /// The name to show for this file pair.
    #[must_use]
    pub fn display_name(&self) -> String {
        let pick = |name: &Option<String>| {
            name.as_deref()
                .filter(|name| *name != "/dev/null")
                .map(str::to_owned)
        };
        pick(&self.new_name)
            .or_else(|| pick(&self.old_name))
            .unwrap_or_default()
    }

    /// The original and patched sides this file section alone describes.
    ///
    /// A patch holds only the lines around each change, so the two texts are
    /// the hunks' own lines one after the other.
    #[must_use]
    pub fn reconstruct(&self) -> (String, String) {
        let mut old = String::new();
        let mut new = String::new();
        for hunk in &self.hunks {
            for line in &hunk.lines {
                let targets: &mut [&mut String] = match line.kind {
                    PatchLineKind::Context => &mut [&mut old, &mut new],
                    PatchLineKind::Removed => &mut [&mut old],
                    PatchLineKind::Added => &mut [&mut new],
                };
                for target in targets.iter_mut() {
                    target.push_str(&line.text);
                    target.push('\n');
                }
            }
        }
        (old, new)
    }
}

/// A parsed patch file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patch {
    /// Every file section, in patch order.
    pub files: Vec<FilePatch>,
}

/// The result of applying one file section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The patched text.
    pub text: String,
    /// Indexes of the hunks whose original lines were not found.
    pub rejected: Vec<usize>,
    /// Indexes of the hunks applied at a line other than the one stated.
    pub moved: Vec<usize>,
}

/// Read a unified or context diff under the default ceilings.
///
/// # Errors
///
/// Returns [`PatchError`] when the input passes a ceiling, when a hunk is
/// malformed or cut short, or when the input holds no hunk.
pub fn parse_patch(input: &str) -> Result<Patch, PatchError> {
    parse_patch_with(input, &PatchLimits::default())
}

/// Read a unified or context diff under `limits`.
///
/// # Errors
///
/// As [`parse_patch`].
pub fn parse_patch_with(input: &str, limits: &PatchLimits) -> Result<Patch, PatchError> {
    if input.len() > limits.max_input_bytes {
        return Err(PatchError::LimitExceeded {
            what: "input size",
            limit: limits.max_input_bytes,
        });
    }
    let lines: Vec<&str> = input
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let mut parser = Parser {
        lines: &lines,
        at: 0,
        limits,
        hunks: 0,
        body_lines: 0,
        patch: Patch::default(),
        pending_old: None,
    };
    parser.run()?;
    if parser.patch.files.iter().all(|file| file.hunks.is_empty()) {
        return Err(PatchError::NoHunks);
    }
    parser.patch.files.retain(|file| !file.hunks.is_empty());
    Ok(parser.patch)
}

struct Parser<'a> {
    lines: &'a [&'a str],
    at: usize,
    limits: &'a PatchLimits,
    hunks: usize,
    body_lines: usize,
    patch: Patch,
    /// A `---` or `***` name seen before its partner line.
    pending_old: Option<String>,
}

impl<'a> Parser<'a> {
    fn line(&self, index: usize) -> Option<&'a str> {
        self.lines.get(index).copied()
    }

    fn malformed(index: usize, detail: impl Into<String>) -> PatchError {
        PatchError::Malformed {
            line: index + 1,
            detail: detail.into(),
        }
    }

    fn run(&mut self) -> Result<(), PatchError> {
        while let Some(line) = self.line(self.at) {
            if let Some(name) = line.strip_prefix("--- ") {
                if self
                    .line(self.at + 1)
                    .is_some_and(|next| next.starts_with("+++ "))
                {
                    let new = self.line(self.at + 1).and_then(|l| l.strip_prefix("+++ "));
                    self.start_file(Some(header_name(name)), new.map(header_name))?;
                    self.at += 2;
                    continue;
                }
                if let Some(old) = self.pending_old.take() {
                    self.start_file(Some(old), Some(header_name(name)))?;
                    self.at += 1;
                    continue;
                }
            }
            if let Some(name) = line.strip_prefix("*** ") {
                let is_range = parse_context_range(line, "*** ", " ****").is_some();
                if !is_range
                    && self
                        .line(self.at + 1)
                        .is_some_and(|next| next.starts_with("--- "))
                {
                    self.pending_old = Some(header_name(name));
                    self.at += 1;
                    continue;
                }
            }
            if line.starts_with("@@ ") {
                self.ensure_file()?;
                self.unified_hunk()?;
                continue;
            }
            if line.starts_with("***************") {
                self.ensure_file()?;
                self.context_hunk()?;
                continue;
            }
            self.at += 1;
        }
        Ok(())
    }

    fn start_file(&mut self, old: Option<String>, new: Option<String>) -> Result<(), PatchError> {
        if self.patch.files.len() >= self.limits.max_files {
            return Err(PatchError::LimitExceeded {
                what: "file count",
                limit: self.limits.max_files,
            });
        }
        self.patch.files.push(FilePatch {
            old_name: old,
            new_name: new,
            hunks: Vec::new(),
        });
        Ok(())
    }

    fn ensure_file(&mut self) -> Result<(), PatchError> {
        if self.patch.files.is_empty() {
            self.start_file(None, None)?;
        }
        Ok(())
    }

    fn count_hunk(&mut self) -> Result<(), PatchError> {
        self.hunks += 1;
        if self.hunks > self.limits.max_hunks {
            return Err(PatchError::LimitExceeded {
                what: "hunk count",
                limit: self.limits.max_hunks,
            });
        }
        Ok(())
    }

    fn count_line(&mut self) -> Result<(), PatchError> {
        self.body_lines += 1;
        if self.body_lines > self.limits.max_lines {
            return Err(PatchError::LimitExceeded {
                what: "line count",
                limit: self.limits.max_lines,
            });
        }
        Ok(())
    }

    fn push_hunk(&mut self, hunk: PatchHunk) {
        if let Some(file) = self.patch.files.last_mut() {
            file.hunks.push(hunk);
        }
    }

    fn unified_hunk(&mut self) -> Result<(), PatchError> {
        self.count_hunk()?;
        let header_at = self.at;
        let header = self.line(header_at).unwrap_or_default();
        let (old_start, old_count, new_start, new_count) = parse_unified_header(header)
            .ok_or_else(|| Self::malformed(header_at, "the hunk header has no line ranges"))?;
        self.at += 1;
        let mut old_left = old_count;
        let mut new_left = new_count;
        let mut body = Vec::new();
        while old_left > 0 || new_left > 0 {
            let index = self.at;
            let Some(line) = self.line(index) else {
                return Err(Self::malformed(
                    index,
                    "the hunk ends before its stated length",
                ));
            };
            let at_end = index + 1 == self.lines.len();
            let (kind, text) = match line.as_bytes().first() {
                Some(b' ') => (PatchLineKind::Context, line.get(1..).unwrap_or_default()),
                Some(b'-') => (PatchLineKind::Removed, line.get(1..).unwrap_or_default()),
                Some(b'+') => (PatchLineKind::Added, line.get(1..).unwrap_or_default()),
                Some(b'\\') => {
                    self.at += 1;
                    mark_no_newline(&mut body);
                    continue;
                }
                // Some tools strip the single space from an empty context line.
                None if !at_end => (PatchLineKind::Context, ""),
                _ => {
                    return Err(Self::malformed(
                        index,
                        "the hunk ends before its stated length",
                    ));
                }
            };
            let fits = match kind {
                PatchLineKind::Context => old_left > 0 && new_left > 0,
                PatchLineKind::Removed => old_left > 0,
                PatchLineKind::Added => new_left > 0,
            };
            if !fits {
                return Err(Self::malformed(
                    index,
                    "the hunk holds more lines than it states",
                ));
            }
            self.count_line()?;
            if kind != PatchLineKind::Added {
                old_left -= 1;
            }
            if kind != PatchLineKind::Removed {
                new_left -= 1;
            }
            body.push(PatchLine {
                kind,
                text: text.to_owned(),
                no_newline: false,
            });
            self.at += 1;
        }
        while self
            .line(self.at)
            .is_some_and(|line| line.starts_with('\\'))
        {
            mark_no_newline(&mut body);
            self.at += 1;
        }
        self.push_hunk(PatchHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines: body,
        });
        Ok(())
    }

    fn context_hunk(&mut self) -> Result<(), PatchError> {
        self.count_hunk()?;
        self.at += 1;
        let old_at = self.at;
        let old_header = self.line(old_at).unwrap_or_default();
        let old_range = parse_context_range(old_header, "*** ", " ****")
            .ok_or_else(|| Self::malformed(old_at, "the original range line is missing"))?;
        self.at += 1;
        let old =
            self.context_section(|line| parse_context_range(line, "--- ", " ----").is_some())?;
        let new_at = self.at;
        let new_header = self.line(new_at).unwrap_or_default();
        let new_range = parse_context_range(new_header, "--- ", " ----")
            .ok_or_else(|| Self::malformed(new_at, "the patched range line is missing"))?;
        self.at += 1;
        let new = self.context_section(|line| {
            line.starts_with("***************")
                || line.starts_with("*** ")
                || line.starts_with("--- ")
                || line.starts_with("diff ")
        })?;
        let lines = merge_context(old, new);
        let old_count = lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Added)
            .count();
        let new_count = lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Removed)
            .count();
        // An empty side names the line before it rather than a first line.
        let start = |(first, last): (usize, usize), count: usize| {
            if count == 0 {
                first.min(last)
            } else {
                first
            }
        };
        self.push_hunk(PatchHunk {
            old_start: start(old_range, old_count),
            old_count,
            new_start: start(new_range, new_count),
            new_count,
            lines,
        });
        Ok(())
    }

    /// Read one side of a context hunk up to the line `ends` accepts.
    fn context_section(
        &mut self,
        ends: impl Fn(&str) -> bool,
    ) -> Result<Vec<(u8, PatchLine)>, PatchError> {
        let mut out: Vec<(u8, PatchLine)> = Vec::new();
        while let Some(line) = self.line(self.at) {
            if ends(line) {
                break;
            }
            let marker = line.as_bytes().first().copied();
            match marker {
                Some(b' ' | b'-' | b'+' | b'!') => {
                    if line.as_bytes().get(1).is_some_and(|byte| *byte != b' ') {
                        break;
                    }
                    self.count_line()?;
                    out.push((
                        marker.unwrap_or(b' '),
                        PatchLine {
                            kind: PatchLineKind::Context,
                            text: line.get(2..).unwrap_or_default().to_owned(),
                            no_newline: false,
                        },
                    ));
                }
                Some(b'\\') => {
                    if let Some((_, last)) = out.last_mut() {
                        last.no_newline = true;
                    }
                }
                _ => break,
            }
            self.at += 1;
        }
        Ok(out)
    }
}

/// Mark the last line of the side the marker follows.
fn mark_no_newline(body: &mut [PatchLine]) {
    if let Some(last) = body.last_mut() {
        last.no_newline = true;
    }
}

/// Merge the two sides of a context hunk into one ordered body.
fn merge_context(old: Vec<(u8, PatchLine)>, new: Vec<(u8, PatchLine)>) -> Vec<PatchLine> {
    let mut out = Vec::with_capacity(old.len() + new.len());
    let mut old = old.into_iter().peekable();
    let mut new = new.into_iter().peekable();
    let with = |mut line: PatchLine, kind| {
        line.kind = kind;
        line
    };
    loop {
        match (
            old.peek().map(|entry| entry.0),
            new.peek().map(|entry| entry.0),
        ) {
            (None, None) => break,
            (Some(b'-'), _) => {
                if let Some((_, line)) = old.next() {
                    out.push(with(line, PatchLineKind::Removed));
                }
            }
            (_, Some(b'+')) => {
                if let Some((_, line)) = new.next() {
                    out.push(with(line, PatchLineKind::Added));
                }
            }
            (Some(b'!'), _) | (_, Some(b'!')) => {
                while old.peek().is_some_and(|entry| entry.0 == b'!') {
                    if let Some((_, line)) = old.next() {
                        out.push(with(line, PatchLineKind::Removed));
                    }
                }
                while new.peek().is_some_and(|entry| entry.0 == b'!') {
                    if let Some((_, line)) = new.next() {
                        out.push(with(line, PatchLineKind::Added));
                    }
                }
            }
            (Some(_), Some(_)) => {
                let _ = new.next();
                if let Some((_, line)) = old.next() {
                    out.push(line);
                }
            }
            (Some(_), None) => {
                if let Some((_, line)) = old.next() {
                    out.push(line);
                }
            }
            (None, Some(_)) => {
                if let Some((_, line)) = new.next() {
                    out.push(line);
                }
            }
        }
    }
    out
}

/// The name part of a `---`, `+++` or `***` header, without a timestamp.
fn header_name(rest: &str) -> String {
    let name = rest.split('\t').next().unwrap_or(rest).trim_end();
    name.to_owned()
}

/// `@@ -a,b +c,d @@` as its four numbers. A missing count is one.
fn parse_unified_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some((old_start, old_count, new_start, new_count))
}

fn parse_range(text: &str) -> Option<(usize, usize)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// `*** a,b ****` as its two numbers. A single number stands for both.
fn parse_context_range(line: &str, prefix: &str, suffix: &str) -> Option<(usize, usize)> {
    let inner = line.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if let Some((first, last)) = inner.split_once(',') {
        return Some((first.parse().ok()?, last.parse().ok()?));
    }
    let only = inner.parse::<usize>().ok()?;
    Some((only, only))
}

/// Apply one file section to `original`.
///
/// Each hunk is looked for at the line its header states, then at the nearest
/// line after the previous hunk where its original lines match. A hunk found
/// nowhere is left out and reported in [`Applied::rejected`]. The line ending
/// style of `original` is kept, and so is a missing final line ending unless a
/// hunk that reaches the end states otherwise.
#[must_use]
pub fn apply_patch(original: &str, file: &FilePatch) -> Applied {
    let ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines: Vec<&str> = original
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let mut final_newline = original.ends_with('\n');
    if final_newline || original.is_empty() {
        lines.pop();
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut next = 0usize;
    let mut rejected = Vec::new();
    let mut moved = Vec::new();
    for (index, hunk) in file.hunks.iter().enumerate() {
        let wanted: Vec<&str> = hunk.old_lines().map(|line| line.text.as_str()).collect();
        let stated = if hunk.old_count == 0 {
            hunk.old_start
        } else {
            hunk.old_start.saturating_sub(1)
        };
        let Some(at) = locate(&lines, &wanted, stated, next) else {
            rejected.push(index);
            continue;
        };
        if at != stated {
            moved.push(index);
        }
        out.extend(
            lines
                .get(next..at)
                .unwrap_or_default()
                .iter()
                .map(|line| (*line).to_owned()),
        );
        out.extend(hunk.new_lines().map(|line| line.text.clone()));
        next = at + wanted.len();
        if next >= lines.len() {
            if hunk.new_lines().last().is_some_and(|line| line.no_newline) {
                final_newline = false;
            } else if hunk.old_lines().last().is_some_and(|line| line.no_newline)
                || (lines.is_empty() && hunk.new_lines().next().is_some())
            {
                final_newline = true;
            }
        }
    }
    out.extend(
        lines
            .get(next..)
            .unwrap_or_default()
            .iter()
            .map(|line| (*line).to_owned()),
    );
    let mut text = out.join(ending);
    if final_newline && !out.is_empty() {
        text.push_str(ending);
    }
    Applied {
        text,
        rejected,
        moved,
    }
}

/// Where `wanted` sits in `lines`, at or after `from`, nearest to `stated`.
fn locate(lines: &[&str], wanted: &[&str], stated: usize, from: usize) -> Option<usize> {
    let fits = |at: usize| {
        at >= from
            && at
                .checked_add(wanted.len())
                .and_then(|end| lines.get(at..end))
                .is_some_and(|slice| slice == wanted)
    };
    if fits(stated) {
        return Some(stated);
    }
    let last = lines.len().checked_sub(wanted.len())?;
    let centre = stated.min(last);
    for distance in 0..=last {
        let after = centre.saturating_add(distance);
        if after <= last && fits(after) {
            return Some(after);
        }
        if let Some(before) = centre.checked_sub(distance) {
            if fits(before) {
                return Some(before);
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const UNIFIED: &str = "\
--- a/file.txt\t2024-01-01 00:00:00
+++ b/file.txt\t2024-01-02 00:00:00
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
";

    #[test]
    fn a_unified_hunk_reads_and_applies() {
        let patch = parse_patch(UNIFIED).unwrap();
        assert_eq!(patch.files.len(), 1);
        let file = &patch.files[0];
        assert_eq!(file.old_name.as_deref(), Some("a/file.txt"));
        assert_eq!(file.display_name(), "b/file.txt");
        let applied = apply_patch("one\ntwo\nthree\n", file);
        assert_eq!(applied.text, "one\nTWO\nthree\n");
        assert!(applied.rejected.is_empty());
        assert!(applied.moved.is_empty());
    }

    #[test]
    fn crlf_patch_and_crlf_target_keep_crlf() {
        let patch = parse_patch(&UNIFIED.replace('\n', "\r\n")).unwrap();
        let applied = apply_patch("one\r\ntwo\r\nthree\r\n", &patch.files[0]);
        assert_eq!(applied.text, "one\r\nTWO\r\nthree\r\n");
    }

    #[test]
    fn a_patch_without_a_trailing_newline_still_reads() {
        let patch = parse_patch(UNIFIED.trim_end_matches('\n')).unwrap();
        assert_eq!(patch.files[0].hunks[0].lines.len(), 4);
    }

    #[test]
    fn no_newline_markers_decide_the_final_line_ending() {
        let text = "\
--- a
+++ b
@@ -1,2 +1,2 @@
 one
-two
\\ No newline at end of file
+two
";
        let patch = parse_patch(text).unwrap();
        let applied = apply_patch("one\ntwo", &patch.files[0]);
        assert_eq!(applied.text, "one\ntwo\n");

        let removed = "\
--- a
+++ b
@@ -1,2 +1,2 @@
 one
-two
+two
\\ No newline at end of file
";
        let patch = parse_patch(removed).unwrap();
        let applied = apply_patch("one\ntwo\n", &patch.files[0]);
        assert_eq!(applied.text, "one\ntwo");
    }

    #[test]
    fn a_hunk_moved_by_earlier_lines_is_found_and_reported() {
        let patch = parse_patch(UNIFIED).unwrap();
        let applied = apply_patch("zero\nzero\none\ntwo\nthree\n", &patch.files[0]);
        assert_eq!(applied.text, "zero\nzero\none\nTWO\nthree\n");
        assert_eq!(applied.moved, vec![0]);
    }

    #[test]
    fn a_hunk_that_matches_nowhere_is_rejected_and_the_text_kept() {
        let patch = parse_patch(UNIFIED).unwrap();
        let applied = apply_patch("alpha\nbeta\n", &patch.files[0]);
        assert_eq!(applied.text, "alpha\nbeta\n");
        assert_eq!(applied.rejected, vec![0]);
    }

    #[test]
    fn an_empty_context_line_without_its_space_is_context() {
        let text = "--- a\n+++ b\n@@ -1,3 +1,3 @@\n one\n\n-x\n+y\n";
        let patch = parse_patch(text).unwrap();
        let applied = apply_patch("one\n\nx\n", &patch.files[0]);
        assert_eq!(applied.text, "one\n\ny\n");
    }

    #[test]
    fn a_new_file_applies_to_an_empty_text() {
        let text = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n";
        let patch = parse_patch(text).unwrap();
        assert_eq!(patch.files[0].display_name(), "b/new.txt");
        assert_eq!(apply_patch("", &patch.files[0]).text, "a\nb\n");
    }

    #[test]
    fn several_files_are_separate_sections() {
        let text = format!(
            "diff --git a/x b/x\nindex 1..2\n{UNIFIED}diff --git a/y b/y\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-p\n+q\n"
        );
        let patch = parse_patch(&text).unwrap();
        assert_eq!(patch.files.len(), 2);
        assert_eq!(patch.files[1].new_name.as_deref(), Some("b/y"));
        assert_eq!(apply_patch("p\n", &patch.files[1]).text, "q\n");
    }

    #[test]
    fn a_context_diff_reads_and_applies() {
        let text = "\
*** a/file.txt\t2024-01-01
--- b/file.txt\t2024-01-02
***************
*** 1,4 ****
  one
! two
  three
- four
--- 1,4 ----
  one
! TWO
  three
+ five
";
        let patch = parse_patch(text).unwrap();
        let file = &patch.files[0];
        assert_eq!(file.old_name.as_deref(), Some("a/file.txt"));
        let applied = apply_patch("one\ntwo\nthree\nfour\n", file);
        assert_eq!(applied.text, "one\nTWO\nthree\nfive\n");
    }

    #[test]
    fn a_context_diff_with_an_omitted_side_reads() {
        let text = "\
*** a
--- b
***************
*** 1,2 ****
--- 1,3 ----
  one
+ added
  two
";
        let patch = parse_patch(text).unwrap();
        let applied = apply_patch("one\ntwo\n", &patch.files[0]);
        assert_eq!(applied.text, "one\nadded\ntwo\n");
    }

    #[test]
    fn reconstruct_gives_both_sides_of_the_hunks() {
        let patch = parse_patch(UNIFIED).unwrap();
        let (old, new) = patch.files[0].reconstruct();
        assert_eq!(old, "one\ntwo\nthree\n");
        assert_eq!(new, "one\nTWO\nthree\n");
    }

    // Hostile input.

    #[test]
    fn text_with_no_hunk_is_refused() {
        assert_eq!(parse_patch(""), Err(PatchError::NoHunks));
        assert_eq!(parse_patch("hello\nworld\n"), Err(PatchError::NoHunks));
        assert_eq!(parse_patch("--- a\n+++ b\n"), Err(PatchError::NoHunks));
    }

    #[test]
    fn a_stated_length_far_past_the_body_is_malformed_not_reserved() {
        let text = "--- a\n+++ b\n@@ -1,18446744073709551615 +1,99999999999 @@\n one\n";
        assert!(matches!(
            parse_patch(text),
            Err(PatchError::Malformed { .. })
        ));
    }

    #[test]
    fn a_number_too_large_for_the_platform_is_malformed() {
        let text = "--- a\n+++ b\n@@ -1,999999999999999999999999 +1 @@\n one\n";
        assert!(matches!(
            parse_patch(text),
            Err(PatchError::Malformed { .. })
        ));
    }

    #[test]
    fn a_body_longer_than_stated_is_malformed() {
        let text = "--- a\n+++ b\n@@ -1 +1 @@\n-a\n-b\n+c\n";
        assert!(matches!(
            parse_patch(text),
            Err(PatchError::Malformed { .. })
        ));
    }

    #[test]
    fn the_ceilings_stop_the_parser() {
        let small = PatchLimits {
            max_input_bytes: 10,
            ..PatchLimits::default()
        };
        assert!(matches!(
            parse_patch_with(UNIFIED, &small),
            Err(PatchError::LimitExceeded { .. })
        ));
        let few_lines = PatchLimits {
            max_lines: 2,
            ..PatchLimits::default()
        };
        assert!(matches!(
            parse_patch_with(UNIFIED, &few_lines),
            Err(PatchError::LimitExceeded { .. })
        ));
        let few_hunks = PatchLimits {
            max_hunks: 1,
            ..PatchLimits::default()
        };
        let two = format!("{UNIFIED}@@ -5 +5 @@\n-a\n+b\n");
        assert!(matches!(
            parse_patch_with(&two, &few_hunks),
            Err(PatchError::LimitExceeded { .. })
        ));
        let few_files = PatchLimits {
            max_files: 1,
            ..PatchLimits::default()
        };
        let files = format!("{UNIFIED}{UNIFIED}");
        assert!(matches!(
            parse_patch_with(&files, &few_files),
            Err(PatchError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn a_context_range_that_runs_backwards_does_not_underflow() {
        let text = "*** a\n--- b\n***************\n*** 5,1 ****\n--- 5,1 ----\n";
        let _ = parse_patch(text);
        let text = "*** a\n--- b\n***************\n*** 18446744073709551615 ****\n--- 1 ----\n";
        let _ = parse_patch(text);
    }

    #[test]
    fn stray_markers_and_truncated_headers_never_panic() {
        for text in [
            "@@",
            "@@ -",
            "@@ -1 +1",
            "@@ -a,b +c,d @@",
            "***************",
            "***************\n*** 1 ****",
            "--- a\n+++ b\n@@ -1 +1 @@\n\\ No newline at end of file\n",
            "\\\n\\\n+++ \n--- \n",
            "*** \n--- \n***************\n*** 1,2 ****\n! \n!\n--- 1,2 ----\n",
            "\u{feff}--- a\n+++ b\n@@ -1 +1 @@\n-\u{fffd}\n+\u{0}\n",
        ] {
            let _ = parse_patch(text);
        }
    }

    #[test]
    fn applying_a_hunk_past_the_end_of_the_text_rejects_it() {
        let text = "--- a\n+++ b\n@@ -1000000,2 +1000000,2 @@\n x\n-y\n+z\n";
        let patch = parse_patch(text).unwrap();
        let applied = apply_patch("a\n", &patch.files[0]);
        assert_eq!(applied.rejected, vec![0]);
        assert_eq!(applied.text, "a\n");
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_text_never_panics(text in "(?s).{0,400}") {
            if let Ok(patch) = parse_patch(&text) {
                for file in &patch.files {
                    let _ = apply_patch(&text, file);
                    let _ = file.reconstruct();
                }
            }
        }

        #[test]
        fn arbitrary_hunk_lines_never_panic(
            body in proptest::collection::vec("[ +\\-\\\\!*@]{0,2}[a-c]{0,3}", 0..30),
            a in 0usize..6, b in 0usize..6, c in 0usize..6, d in 0usize..6,
        ) {
            let unified = format!("--- x\n+++ y\n@@ -{a},{b} +{c},{d} @@\n{}", body.join("\n"));
            if let Ok(patch) = parse_patch(&unified) {
                let _ = apply_patch("a\nb\nc\n", &patch.files[0]);
            }
            let context = format!(
                "*** x\n--- y\n***************\n*** {a},{b} ****\n{}\n--- {c},{d} ----\n{}",
                body.join("\n"),
                body.join("\n")
            );
            if let Ok(patch) = parse_patch(&context) {
                let _ = apply_patch("a\nb\nc\n", &patch.files[0]);
            }
        }
    }
}
