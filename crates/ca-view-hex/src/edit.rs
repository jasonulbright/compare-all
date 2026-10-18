//! The editable byte content of one pane.
//!
//! Every change goes through one operation, a replacement of a byte range by
//! another run of bytes, so overwriting, inserting and deleting all reverse the
//! same way and the undo history is one stack of those operations.

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
thread_local! {
    static WHOLE_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has written a whole side anew.
#[cfg(test)]
pub(crate) fn whole_copies() -> usize {
    WHOLE_COPIES.with(std::cell::Cell::get)
}

fn count_whole_copy() {
    #[cfg(test)]
    WHOLE_COPIES.with(|copies| copies.set(copies.get() + 1));
}

/// How a typed byte enters the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditMode {
    /// A typed byte replaces the byte under the caret.
    #[default]
    Overwrite,
    /// A typed byte is inserted in front of the byte under the caret.
    Insert,
}

impl EditMode {
    /// The label the status line shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overwrite => "Overwrite",
            Self::Insert => "Insert",
        }
    }

    /// The other mode.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Overwrite => Self::Insert,
            Self::Insert => Self::Overwrite,
        }
    }
}

/// One reversible change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Offset the change starts at.
    pub at: u64,
    /// The bytes the change took out.
    pub removed: Vec<u8>,
    /// The bytes the change put in.
    pub inserted: Vec<u8>,
}

impl Edit {
    /// The change that reverses this one.
    fn reversed(&self) -> Self {
        Self {
            at: self.at,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }
}

/// The bytes of one side, with the history that reverses changes to them.
#[derive(Debug, Clone)]
pub struct ByteBuffer {
    bytes: std::sync::Arc<Vec<u8>>,
    /// Each entry is one undo step: edits in ascending order, each at an offset
    /// of the content as it was before the step.
    undo: Vec<Vec<Edit>>,
    redo: Vec<Vec<Edit>>,
    /// The depth of the undo stack when the content was last written.
    saved_depth: usize,
    read_only: bool,
    revision: u64,
}

impl Default for ByteBuffer {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ByteBuffer {
    /// A buffer over `bytes`.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: std::sync::Arc::new(bytes),
            undo: Vec::new(),
            redo: Vec::new(),
            saved_depth: 0,
            read_only: false,
            revision: NEXT_REVISION.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The content, shared without a copy.
    ///
    /// A reader that keeps the handle while the buffer is edited pays for one
    /// copy at that point; a reader that lets go pays for none.
    #[must_use]
    pub fn snapshot(&self) -> std::sync::Arc<Vec<u8>> {
        std::sync::Arc::clone(&self.bytes)
    }

    /// The content.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// How many bytes the content holds.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// True when the content holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// One byte, or `None` past the end.
    #[must_use]
    pub fn byte(&self, offset: u64) -> Option<u8> {
        self.bytes.get(usize::try_from(offset).ok()?).copied()
    }

    /// A run of bytes, clipped to what the content holds.
    #[must_use]
    pub fn slice(&self, start: u64, len: u32) -> &[u8] {
        let Ok(start) = usize::try_from(start) else {
            return &[];
        };
        let end = start.saturating_add(len as usize).min(self.bytes.len());
        self.bytes.get(start..end).unwrap_or_default()
    }

    /// True when the content may not be changed.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Refuse or allow changes.
    pub const fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// True when the content differs from what was last written.
    #[must_use]
    pub const fn is_modified(&self) -> bool {
        self.undo.len() != self.saved_depth
    }

    /// Record the content as written.
    pub const fn mark_saved(&mut self) {
        self.saved_depth = self.undo.len();
    }

    /// Identity of the bytes sent to an asynchronous save.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Mark only the submitted revision saved; later edits must remain dirty,
    /// including undo to the previous on-disk state after it was overwritten.
    pub const fn mark_saved_revision(&mut self, revision: u64) {
        self.saved_depth = if self.revision == revision {
            self.undo.len()
        } else {
            usize::MAX
        };
    }

    /// True when there is a change to reverse.
    #[must_use]
    pub const fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True when there is a reversed change to reapply.
    #[must_use]
    pub const fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Replace `len` bytes at `at` with `inserted`.
    ///
    /// Returns false when the content is read only or the range runs past the
    /// end, so a caller never has to check the bounds itself.
    pub fn replace(&mut self, at: u64, len: u64, inserted: &[u8]) -> bool {
        if self.read_only {
            return false;
        }
        let Some(edit) = plan_in(&self.bytes, at, len, inserted) else {
            return false;
        };
        self.commit(vec![edit]);
        true
    }

    /// Replace every `len` byte run starting at `offsets` with `inserted`, as
    /// one undo step.
    ///
    /// Returns how many runs were replaced. Offsets that overlap an earlier
    /// run or reach past the end are skipped, so the step never corrupts the
    /// content.
    pub fn replace_all(&mut self, offsets: &[u64], len: u64, inserted: &[u8]) -> usize {
        if self.read_only {
            return 0;
        }
        let Some(edits) = plan_all(&self.bytes, offsets, len, inserted, &mut Pace::endless())
        else {
            return 0;
        };
        let count = edits.len();
        if count > 0 {
            self.commit(edits);
        }
        count
    }

    fn commit(&mut self, edits: Vec<Edit>) {
        if self.saved_depth > self.undo.len() {
            self.saved_depth = usize::MAX;
        }
        self.apply_step(&edits);
        self.undo.push(edits);
        self.redo.clear();
    }

    /// The step that reverses `edits`, in the offsets of the content after it.
    fn reversed_step(edits: &[Edit]) -> Vec<Edit> {
        let mut shift: i128 = 0;
        edits
            .iter()
            .map(|edit| {
                let at = u64::try_from(i128::from(edit.at) + shift).unwrap_or(edit.at);
                shift += edit.inserted.len() as i128 - edit.removed.len() as i128;
                Edit {
                    at,
                    ..edit.reversed()
                }
            })
            .collect()
    }

    /// Apply one step in a single pass over the content.
    fn apply_step(&mut self, edits: &[Edit]) {
        if let [edit] = edits {
            self.apply(edit);
            return;
        }
        count_whole_copy();
        let Some(bytes) = rebuilt(&self.bytes, edits, &mut Pace::endless()) else {
            return;
        };
        self.bytes = std::sync::Arc::new(bytes);
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
    }

    fn apply(&mut self, edit: &Edit) {
        let Ok(start) = usize::try_from(edit.at) else {
            return;
        };
        let end = start
            .saturating_add(edit.removed.len())
            .min(self.bytes.len());
        if start > self.bytes.len() {
            return;
        }
        if std::sync::Arc::strong_count(&self.bytes) > 1 {
            count_whole_copy();
        }
        std::sync::Arc::make_mut(&mut self.bytes).splice(start..end, edit.inserted.iter().copied());
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
    }

    /// Put `bytes` in at `at`, in the given mode.
    ///
    /// Overwriting past the end of the content appends instead, which is what
    /// lets a caret at the end of the file be typed into.
    pub fn write(&mut self, at: u64, bytes: &[u8], mode: EditMode) -> bool {
        let taken = match mode {
            EditMode::Insert => 0,
            EditMode::Overwrite => (self.len().saturating_sub(at)).min(bytes.len() as u64),
        };
        self.replace(at, taken, bytes)
    }

    /// Take `len` bytes out at `at`.
    pub fn delete(&mut self, at: u64, len: u64) -> bool {
        let len = len.min(self.len().saturating_sub(at));
        if len == 0 {
            return false;
        }
        self.replace(at, len, &[])
    }

    /// Reverse the last change and report where it was. A read-only buffer
    /// changes nothing.
    pub fn undo(&mut self) -> Option<u64> {
        if self.read_only {
            return None;
        }
        let step = self.undo.pop()?;
        let back = Self::reversed_step(&step);
        self.apply_step(&back);
        let at = step.first().map_or(0, |edit| edit.at);
        self.redo.push(step);
        Some(at)
    }

    /// Reapply the last reversed change and report where it was. A read-only
    /// buffer changes nothing.
    pub fn redo(&mut self) -> Option<u64> {
        if self.read_only {
            return None;
        }
        let step = self.redo.pop()?;
        self.apply_step(&step);
        let at = step.first().map_or(0, |edit| edit.at);
        self.undo.push(step);
        Some(at)
    }
}

/// A replace all step and the content it leaves, built from a snapshot of a
/// buffer by a worker so the frame thread only installs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStep {
    revision: u64,
    edits: Vec<Edit>,
    bytes: std::sync::Arc<Vec<u8>>,
}

impl PreparedStep {
    /// Plan the replacement of every `len` byte run at `offsets` in `source`,
    /// the content of revision `revision`, and build the content after it.
    ///
    /// Offsets are taken as [`ByteBuffer::replace_all`] takes them. `stop` is
    /// asked at regular intervals of planned edits and copied bytes; once it
    /// answers true the work ends and `None` is returned.
    #[must_use]
    pub fn replace_all(
        source: &[u8],
        revision: u64,
        offsets: &[u64],
        len: u64,
        inserted: &[u8],
        stop: &dyn Fn() -> bool,
    ) -> Option<Self> {
        let mut pace = Pace::new(stop);
        let edits = plan_all(source, offsets, len, inserted, &mut pace)?;
        let bytes = std::sync::Arc::new(rebuilt(source, &edits, &mut pace)?);
        Some(Self {
            revision,
            edits,
            bytes,
        })
    }

    /// How many runs the step replaces.
    #[must_use]
    pub fn count(&self) -> usize {
        self.edits.len()
    }
}

impl ByteBuffer {
    /// Make `step` the content as one undo step, without reading the bytes.
    ///
    /// Returns how many runs were replaced, 0 for a read-only buffer, or
    /// `None` when the content is no longer the revision the step was built
    /// from.
    pub fn install(&mut self, step: PreparedStep) -> Option<usize> {
        if step.revision != self.revision {
            return None;
        }
        if self.read_only || step.edits.is_empty() {
            return Some(0);
        }
        if self.saved_depth > self.undo.len() {
            self.saved_depth = usize::MAX;
        }
        let count = step.edits.len();
        self.bytes = step.bytes;
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        self.undo.push(step.edits);
        self.redo.clear();
        Some(count)
    }
}

/// How many planned edits pass between two questions to the stop check.
const EDITS_PER_CHECK: usize = 4096;
/// How many copied bytes pass between two questions to the stop check.
const BYTES_PER_CHECK: usize = 1 << 20;

/// Asks a stop check after every fixed amount of work, so a cancelled
/// preparation ends without building the whole output first.
///
/// The byte count carries over between copies; a per-copy count lets many
/// copies each shorter than the interval run without any check.
struct Pace<'a> {
    stop: &'a dyn Fn() -> bool,
    edits: usize,
    bytes: usize,
}

impl<'a> Pace<'a> {
    fn new(stop: &'a dyn Fn() -> bool) -> Self {
        Self {
            stop,
            edits: 0,
            bytes: 0,
        }
    }

    /// A pace whose check never asks the work to end.
    fn endless() -> Pace<'static> {
        Pace::new(&never)
    }

    /// Count one edit; true when the work must end.
    fn edit(&mut self) -> bool {
        self.edits += 1;
        if self.edits < EDITS_PER_CHECK {
            return false;
        }
        self.edits = 0;
        (self.stop)()
    }

    /// Count `copied` bytes; true when the work must end.
    fn bytes(&mut self, copied: usize) -> bool {
        self.bytes = self.bytes.saturating_add(copied);
        if self.bytes < BYTES_PER_CHECK {
            return false;
        }
        self.bytes %= BYTES_PER_CHECK;
        (self.stop)()
    }

    /// Append `part` to `out`, asking the stop check each time the total
    /// copied reaches the interval; false when the work must end.
    fn copy(&mut self, out: &mut Vec<u8>, part: &[u8]) -> bool {
        let mut rest = part;
        while !rest.is_empty() {
            let room = BYTES_PER_CHECK.saturating_sub(self.bytes).max(1);
            let Some((piece, after)) = rest.split_at_checked(room.min(rest.len())) else {
                break;
            };
            out.extend_from_slice(piece);
            rest = after;
            if self.bytes(piece.len()) {
                return false;
            }
        }
        true
    }
}

const fn never() -> bool {
    false
}

/// The edits replacing every `len` byte run at `offsets`, in ascending order,
/// with runs that overlap an earlier one or reach past the end left out.
fn plan_all(
    bytes: &[u8],
    offsets: &[u64],
    len: u64,
    inserted: &[u8],
    pace: &mut Pace<'_>,
) -> Option<Vec<Edit>> {
    let mut sorted = offsets.to_vec();
    sorted.sort_unstable();
    let mut edits = Vec::with_capacity(sorted.len());
    let mut free_from = 0u64;
    for at in sorted {
        if pace.edit() {
            return None;
        }
        if at < free_from {
            continue;
        }
        let Some(edit) = plan_in(bytes, at, len, inserted) else {
            continue;
        };
        free_from = at.saturating_add(len.max(1));
        let copied = edit.removed.len().saturating_add(edit.inserted.len());
        edits.push(edit);
        if pace.bytes(copied) {
            return None;
        }
    }
    Some(edits)
}

fn plan_in(bytes: &[u8], at: u64, len: u64, inserted: &[u8]) -> Option<Edit> {
    let start = usize::try_from(at).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    if end > bytes.len() {
        return None;
    }
    Some(Edit {
        at,
        removed: bytes.get(start..end)?.to_vec(),
        inserted: inserted.to_vec(),
    })
}

/// `old` with the ascending `edits` applied, in a single pass.
fn rebuilt(old: &[u8], edits: &[Edit], pace: &mut Pace<'_>) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(old.len());
    let mut from = 0usize;
    for edit in edits {
        if pace.edit() {
            return None;
        }
        let Ok(at) = usize::try_from(edit.at) else {
            continue;
        };
        let at = at.clamp(from, old.len());
        if !pace.copy(&mut out, old.get(from..at).unwrap_or_default()) {
            return None;
        }
        if !pace.copy(&mut out, &edit.inserted) {
            return None;
        }
        from = at.saturating_add(edit.removed.len()).min(old.len());
    }
    if !pace.copy(&mut out, old.get(from..).unwrap_or_default()) {
        return None;
    }
    Some(out)
}

/// The two hexadecimal digits of a byte.
#[must_use]
pub fn hex_digits(byte: u8) -> [char; 2] {
    const DIGITS: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
    ];
    [DIGITS[(byte >> 4) as usize], DIGITS[(byte & 0x0F) as usize]]
}

/// The value of one hexadecimal digit.
#[must_use]
pub const fn hex_value(key: char) -> Option<u8> {
    match key {
        '0'..='9' => Some(key as u8 - b'0'),
        'a'..='f' => Some(key as u8 - b'a' + 10),
        'A'..='F' => Some(key as u8 - b'A' + 10),
        _ => None,
    }
}

/// A run of bytes written as space separated pairs of hexadecimal digits.
#[must_use]
pub fn to_hex_text(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for byte in bytes {
        if !out.is_empty() {
            out.push(' ');
        }
        let digits = hex_digits(*byte);
        out.push(digits[0]);
        out.push(digits[1]);
    }
    out
}

/// The bytes a run of hexadecimal digits stands for.
///
/// Spaces and commas separate pairs, and a lone digit is refused rather than
/// silently taken as the high or the low half of a byte.
#[must_use]
pub fn from_hex_text(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;
    for key in text.chars() {
        if key.is_whitespace() || key == ',' {
            if high.is_some() {
                return None;
            }
            continue;
        }
        let value = hex_value(key)?;
        match high.take() {
            Some(first) => out.push((first << 4) | value),
            None => high = Some(value),
        }
    }
    if high.is_some() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{from_hex_text, hex_digits, to_hex_text, ByteBuffer, EditMode};

    fn buffer() -> ByteBuffer {
        ByteBuffer::new(vec![0x00, 0x11, 0x22, 0x33])
    }

    #[test]
    fn overwriting_keeps_the_length() {
        let mut bytes = buffer();
        assert!(bytes.write(1, &[0xAA], EditMode::Overwrite));
        assert_eq!(bytes.bytes(), &[0x00, 0xAA, 0x22, 0x33]);
        assert!(bytes.is_modified());
    }

    #[test]
    fn inserting_grows_the_content() {
        let mut bytes = buffer();
        assert!(bytes.write(2, &[0xAA, 0xBB], EditMode::Insert));
        assert_eq!(bytes.bytes(), &[0x00, 0x11, 0xAA, 0xBB, 0x22, 0x33]);
        assert_eq!(bytes.len(), 6);
    }

    #[test]
    fn overwriting_at_the_end_appends() {
        let mut bytes = buffer();
        assert!(bytes.write(4, &[0xAA], EditMode::Overwrite));
        assert_eq!(bytes.len(), 5);
        assert_eq!(bytes.byte(4), Some(0xAA));
    }

    #[test]
    fn deleting_takes_bytes_out_and_clips_at_the_end() {
        let mut bytes = buffer();
        assert!(bytes.delete(1, 2));
        assert_eq!(bytes.bytes(), &[0x00, 0x33]);
        assert!(bytes.delete(1, 99));
        assert_eq!(bytes.bytes(), &[0x00]);
        assert!(!bytes.delete(1, 1));
    }

    #[test]
    fn undo_and_redo_walk_the_history_back_and_forth() {
        let mut bytes = buffer();
        bytes.write(0, &[0xEE], EditMode::Overwrite);
        bytes.write(1, &[0xFF], EditMode::Insert);
        assert_eq!(bytes.bytes(), &[0xEE, 0xFF, 0x11, 0x22, 0x33]);
        assert_eq!(bytes.undo(), Some(1));
        assert_eq!(bytes.bytes(), &[0xEE, 0x11, 0x22, 0x33]);
        assert_eq!(bytes.undo(), Some(0));
        assert_eq!(bytes.bytes(), &[0x00, 0x11, 0x22, 0x33]);
        assert!(!bytes.can_undo());
        assert_eq!(bytes.redo(), Some(0));
        assert_eq!(bytes.byte(0), Some(0xEE));
        assert_eq!(bytes.redo(), Some(1));
        assert_eq!(bytes.bytes(), &[0xEE, 0xFF, 0x11, 0x22, 0x33]);
        assert!(!bytes.can_redo());
    }

    #[test]
    fn a_new_change_drops_the_reapply_history() {
        let mut bytes = buffer();
        bytes.write(0, &[0xEE], EditMode::Overwrite);
        bytes.undo();
        assert!(bytes.can_redo());
        bytes.write(0, &[0xDD], EditMode::Overwrite);
        assert!(!bytes.can_redo());
    }

    #[test]
    fn editing_after_undo_does_not_reuse_a_discarded_save_point() {
        let mut bytes = buffer();
        bytes.write(0, &[0xEE], EditMode::Overwrite);
        bytes.mark_saved();
        bytes.undo();
        bytes.write(0, &[0xDD], EditMode::Overwrite);
        assert!(bytes.is_modified(), "the disk still contains EE, not DD");
        bytes.undo();
        assert!(bytes.is_modified());
        bytes.redo();
        assert!(bytes.is_modified());
    }

    #[test]
    fn undoing_back_to_the_written_content_clears_the_modified_flag() {
        let mut bytes = buffer();
        bytes.write(0, &[0xEE], EditMode::Overwrite);
        assert!(bytes.is_modified());
        bytes.undo();
        assert!(!bytes.is_modified());
        bytes.redo();
        assert!(bytes.is_modified());
        bytes.mark_saved();
        assert!(!bytes.is_modified());
    }

    #[test]
    fn replace_all_is_one_undo_step_and_skips_overlaps() {
        let mut bytes = ByteBuffer::new(vec![1, 1, 1, 2, 1, 1]);
        assert_eq!(bytes.replace_all(&[4, 0, 1, 9], 2, &[7, 7, 7]), 2);
        assert_eq!(bytes.bytes(), &[7, 7, 7, 1, 2, 7, 7, 7]);
        assert!(bytes.is_modified());
        assert_eq!(bytes.undo(), Some(0));
        assert_eq!(bytes.bytes(), &[1, 1, 1, 2, 1, 1]);
        assert!(!bytes.is_modified());
        bytes.redo();
        assert_eq!(bytes.bytes(), &[7, 7, 7, 1, 2, 7, 7, 7]);
        assert_eq!(bytes.replace_all(&[0], 3, &[]), 1);
        assert_eq!(bytes.bytes(), &[1, 2, 7, 7, 7]);
        bytes.undo();
        bytes.undo();
        assert_eq!(bytes.bytes(), &[1, 1, 1, 2, 1, 1]);
    }

    #[test]
    fn a_prepared_step_installs_only_over_the_revision_it_was_built_from() {
        let mut bytes = ByteBuffer::new(vec![1, 1, 1, 2, 1, 1]);
        let step = super::PreparedStep::replace_all(
            bytes.bytes(),
            bytes.revision(),
            &[4, 0, 1, 9],
            2,
            &[7, 7, 7],
            &|| false,
        );
        assert!(step.is_some());
        let Some(step) = step else { return };
        assert_eq!(step.count(), 2);
        let stale = step.clone();
        assert_eq!(bytes.install(step), Some(2));
        assert_eq!(bytes.bytes(), &[7, 7, 7, 1, 2, 7, 7, 7]);
        assert_eq!(bytes.install(stale), None);
        assert_eq!(bytes.undo(), Some(0));
        assert_eq!(bytes.bytes(), &[1, 1, 1, 2, 1, 1]);
        assert!(!bytes.is_modified());

        let mut locked = ByteBuffer::new(vec![1]);
        locked.set_read_only(true);
        let step = super::PreparedStep::replace_all(
            locked.bytes(),
            locked.revision(),
            &[0],
            1,
            &[2],
            &|| false,
        );
        assert!(step.is_some());
        let Some(step) = step else { return };
        assert_eq!(locked.install(step), Some(0));
        assert_eq!(locked.bytes(), &[1]);
    }

    #[test]
    fn copied_bytes_are_counted_across_spans_shorter_than_one_check() {
        use std::cell::Cell;
        let span = super::BYTES_PER_CHECK - 1;
        let spans = 8usize;
        let mut source = Vec::with_capacity((span + 1) * spans);
        let mut offsets = Vec::with_capacity(spans);
        for _ in 0..spans {
            source.resize(source.len() + span, 0);
            offsets.push(source.len() as u64);
            source.push(1);
        }
        let checks = Cell::new(0usize);
        let step = super::PreparedStep::replace_all(&source, 0, &offsets, 1, &[2], &|| {
            checks.set(checks.get() + 1);
            false
        });
        assert_eq!(step.map(|step| step.count()), Some(spans));
        let unchanged = span * spans;
        assert!(checks.get() >= unchanged / super::BYTES_PER_CHECK);

        let checks = Cell::new(0usize);
        let stopped = super::PreparedStep::replace_all(&source, 0, &offsets, 1, &[2], &|| {
            checks.set(checks.get() + 1);
            true
        });
        assert!(stopped.is_none());
        assert_eq!(checks.get(), 1);
    }

    #[test]
    fn replacement_bytes_count_toward_the_copy_checks() {
        use std::cell::Cell;
        let inserted = vec![9u8; super::BYTES_PER_CHECK / 2];
        let checks = Cell::new(0usize);
        let step = super::PreparedStep::replace_all(
            &[1, 1, 1, 1],
            0,
            &[0, 1, 2, 3],
            1,
            &inserted,
            &|| {
                checks.set(checks.get() + 1);
                false
            },
        );
        assert_eq!(step.map(|step| step.count()), Some(4));
        // Planning clones each replacement once and the rebuild copies it once.
        assert!(checks.get() >= 4);
    }

    #[test]
    fn a_read_only_buffer_refuses_every_change() {
        let mut bytes = buffer();
        bytes.set_read_only(true);
        assert!(!bytes.write(0, &[0xEE], EditMode::Overwrite));
        assert!(!bytes.delete(0, 1));
        assert_eq!(bytes.bytes(), &[0x00, 0x11, 0x22, 0x33]);
    }

    #[test]
    fn a_range_past_the_end_is_refused_rather_than_clipped() {
        let mut bytes = buffer();
        assert!(!bytes.replace(3, 4, &[0xEE]));
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn a_slice_stops_at_the_end_of_the_content() {
        let bytes = buffer();
        assert_eq!(bytes.slice(2, 8), &[0x22, 0x33]);
        assert_eq!(bytes.slice(9, 4), &[] as &[u8]);
    }

    #[test]
    fn a_byte_renders_as_two_upper_case_digits() {
        assert_eq!(hex_digits(0x0A), ['0', 'A']);
        assert_eq!(hex_digits(0xFF), ['F', 'F']);
        assert_eq!(to_hex_text(&[0x01, 0xAB]), "01 AB");
        assert_eq!(to_hex_text(&[]), "");
    }

    #[test]
    fn hexadecimal_text_round_trips_through_bytes() {
        assert_eq!(from_hex_text("01 ab"), Some(vec![0x01, 0xAB]));
        assert_eq!(from_hex_text("01AB"), Some(vec![0x01, 0xAB]));
        assert_eq!(from_hex_text("01,AB"), Some(vec![0x01, 0xAB]));
        assert_eq!(from_hex_text(""), Some(Vec::new()));
        assert_eq!(from_hex_text("0"), None);
        assert_eq!(from_hex_text("0 1"), None);
        assert_eq!(from_hex_text("zz"), None);
    }

    #[test]
    fn the_mode_toggles_both_ways_and_names_itself() {
        assert_eq!(EditMode::Insert.toggled(), EditMode::Overwrite);
        assert_eq!(EditMode::Overwrite.toggled(), EditMode::Insert);
        assert_eq!(EditMode::Insert.label(), "Insert");
    }
}
