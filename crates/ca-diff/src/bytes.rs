//! Byte level alignment for hexadecimal comparison.
//!
//! All three modes are linear in the input size and allocate work proportional
//! to the number of differences, not to the square of the input, so several
//! hundred megabytes per side is a supported workload.

use crate::cancel::{check, Cancel, NeverCancel};
use crate::lines::HunkKind;
use crate::DiffError;
use imara_diff::intern::InternedInput;
use imara_diff::{diff, Algorithm, Sink};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// How byte positions on the two sides are matched up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ByteAlignment {
    /// Content defined chunking followed by a chunk level alignment. Detects
    /// insertions and deletions anywhere in the file.
    #[default]
    Complete,
    /// Compare in step and, on a mismatch, look ahead a bounded distance for a
    /// place where the two sides line up again. Cheaper than `Complete` and
    /// blind to shifts larger than the look-ahead window.
    Fast,
    /// Offset by offset with no insertion or deletion detection. The tail of
    /// the longer side is reported as an orphan run.
    None,
}

/// One aligned region of two byte streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteHunk {
    /// Region classification.
    pub kind: HunkKind,
    /// Byte range on the left side (end exclusive).
    pub left: Range<u64>,
    /// Byte range on the right side (end exclusive).
    pub right: Range<u64>,
}

/// Bytes scanned ahead of a mismatch when looking for a resynchronization
/// point in [`ByteAlignment::Fast`].
const RESYNC_WINDOW: usize = 64 * 1024;

/// Bytes that must agree before a resynchronization point is accepted.
const RESYNC_ANCHOR: usize = 16;

/// Average chunk size targeted by the content defined chunker.
const CHUNK_TARGET_MASK: u64 = 0x0000_07FF;

/// Smallest chunk the chunker will emit.
const CHUNK_MIN: usize = 512;

/// Largest chunk the chunker will emit, which bounds how coarse a reported
/// difference can be before edge trimming.
const CHUNK_MAX: usize = 16 * 1024;

/// Window the rolling hash is computed over.
const ROLL_WINDOW: usize = 48;

fn u64_of(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Align two byte streams.
#[must_use]
pub fn diff_bytes(left: &[u8], right: &[u8], alignment: ByteAlignment) -> Vec<ByteHunk> {
    diff_bytes_cancellable(left, right, alignment, &NeverCancel).unwrap_or_default()
}

/// Align two byte streams, abandoning the work when `cancel` is raised.
///
/// # Errors
///
/// Returns [`DiffError::Cancelled`] when the flag is raised before the
/// comparison finishes.
pub fn diff_bytes_cancellable(
    left: &[u8],
    right: &[u8],
    alignment: ByteAlignment,
    cancel: &dyn Cancel,
) -> Result<Vec<ByteHunk>, DiffError> {
    match alignment {
        ByteAlignment::None => unaligned(left, right, cancel),
        ByteAlignment::Fast => fast(left, right, cancel),
        ByteAlignment::Complete => complete(left, right, cancel),
    }
}

fn push(out: &mut Vec<ByteHunk>, kind: HunkKind, left: Range<u64>, right: Range<u64>) {
    if left.is_empty() && right.is_empty() {
        return;
    }
    if let Some(prev) = out.last_mut() {
        if prev.kind == kind && prev.left.end == left.start && prev.right.end == right.start {
            prev.left.end = left.end;
            prev.right.end = right.end;
            return;
        }
    }
    out.push(ByteHunk { kind, left, right });
}

/// Length of the common prefix of two slices.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let mut n = 0;
    let limit = a.len().min(b.len());
    while n + 8 <= limit {
        if a.get(n..n + 8) != b.get(n..n + 8) {
            break;
        }
        n += 8;
    }
    while n < limit && a.get(n) == b.get(n) {
        n += 1;
    }
    n
}

/// Length of the common suffix of two slices.
fn common_suffix(a: &[u8], b: &[u8]) -> usize {
    let mut n = 0;
    let limit = a.len().min(b.len());
    while n < limit && a.get(a.len() - 1 - n) == b.get(b.len() - 1 - n) {
        n += 1;
    }
    n
}

fn unaligned(left: &[u8], right: &[u8], cancel: &dyn Cancel) -> Result<Vec<ByteHunk>, DiffError> {
    let mut out = Vec::new();
    let common = left.len().min(right.len());
    let mut pos = 0usize;
    while pos < common {
        check(cancel)?;
        let a = left.get(pos..common).unwrap_or_default();
        let b = right.get(pos..common).unwrap_or_default();
        let same = common_prefix(a, b);
        if same > 0 {
            push(
                &mut out,
                HunkKind::Same,
                u64_of(pos)..u64_of(pos + same),
                u64_of(pos)..u64_of(pos + same),
            );
            pos += same;
            continue;
        }
        let mut end = pos;
        while end < common && left.get(end) != right.get(end) {
            end += 1;
        }
        push(
            &mut out,
            HunkKind::Changed,
            u64_of(pos)..u64_of(end),
            u64_of(pos)..u64_of(end),
        );
        pos = end;
    }
    if left.len() > common {
        push(
            &mut out,
            HunkKind::LeftOnly,
            u64_of(common)..u64_of(left.len()),
            u64_of(common)..u64_of(common),
        );
    } else if right.len() > common {
        push(
            &mut out,
            HunkKind::RightOnly,
            u64_of(common)..u64_of(common),
            u64_of(common)..u64_of(right.len()),
        );
    }
    Ok(out)
}

/// Total shift searched exhaustively before the windowed index is built.
const RESYNC_NEAR: usize = 64;

/// Resynchronizations that may go through the windowed index before the
/// comparison restarts under [`ByteAlignment::Complete`].
///
/// An index pass costs a window's worth of hashing, so a stream in which every
/// difference shifts further than [`RESYNC_NEAR`] would pay that cost once per
/// difference and end up far slower than the mode it is meant to undercut.
/// Chunk level alignment handles exactly that shape of input in one pass.
const RESYNC_INDEX_LIMIT: u32 = 32;

fn anchor(data: &[u8], at: usize) -> Option<&[u8]> {
    data.get(at..at + RESYNC_ANCHOR)
}

/// Where a resynchronization point was found, and at what cost.
enum Resync {
    /// Offsets found by the exhaustive near scan.
    Near(usize, usize),
    /// Offsets found through the windowed index.
    Indexed(usize, usize),
    /// No point within the window.
    None,
}

/// Find a pair of offsets at which `RESYNC_ANCHOR` bytes agree.
///
/// Small shifts are found by an exhaustive scan ordered by total shift. Larger
/// ones cost one index over the right look-ahead window; `index` is reused
/// across calls so the allocation is paid once per comparison.
fn find_resync<'a>(
    left: &'a [u8],
    right: &'a [u8],
    index: &mut std::collections::HashMap<&'a [u8], usize>,
) -> Resync {
    for total in 1..=RESYNC_NEAR {
        for li in 0..=total {
            let ri = total - li;
            if anchor(left, li).is_some() && anchor(left, li) == anchor(right, ri) {
                return Resync::Near(li, ri);
            }
        }
    }
    index.clear();
    let rw = right.len().min(RESYNC_WINDOW);
    for ri in 0..rw {
        if let Some(key) = anchor(right, ri) {
            index.entry(key).or_insert(ri);
        }
    }
    let lw = left.len().min(RESYNC_WINDOW);
    for li in 0..lw {
        if let Some(key) = anchor(left, li) {
            if let Some(&ri) = index.get(key) {
                return Resync::Indexed(li, ri);
            }
        }
    }
    Resync::None
}

fn fast(left: &[u8], right: &[u8], cancel: &dyn Cancel) -> Result<Vec<ByteHunk>, DiffError> {
    match fast_windowed(left, right, cancel)? {
        Some(hunks) => Ok(hunks),
        None => complete(left, right, cancel),
    }
}

/// The look-ahead alignment proper. Returns `None` when the input needs more
/// than [`RESYNC_INDEX_LIMIT`] index passes, which is the signal to align by
/// chunk instead.
fn fast_windowed(
    left: &[u8],
    right: &[u8],
    cancel: &dyn Cancel,
) -> Result<Option<Vec<ByteHunk>>, DiffError> {
    let mut out = Vec::new();
    let mut index: std::collections::HashMap<&[u8], usize> = std::collections::HashMap::new();
    let mut indexed = 0u32;
    let (mut li, mut ri) = (0usize, 0usize);
    while li < left.len() && ri < right.len() {
        check(cancel)?;
        let a = left.get(li..).unwrap_or_default();
        let b = right.get(ri..).unwrap_or_default();
        let same = common_prefix(a, b);
        if same > 0 {
            push(
                &mut out,
                HunkKind::Same,
                u64_of(li)..u64_of(li + same),
                u64_of(ri)..u64_of(ri + same),
            );
            li += same;
            ri += same;
            continue;
        }
        let (dl, dr) = match find_resync(a, b, &mut index) {
            Resync::Near(dl, dr) => (dl, dr),
            Resync::Indexed(dl, dr) => {
                indexed += 1;
                if indexed > RESYNC_INDEX_LIMIT {
                    return Ok(None);
                }
                (dl, dr)
            }
            Resync::None => (a.len(), b.len()),
        };
        let kind = match (dl == 0, dr == 0) {
            (true, false) => HunkKind::RightOnly,
            (false, true) => HunkKind::LeftOnly,
            _ => HunkKind::Changed,
        };
        push(
            &mut out,
            kind,
            u64_of(li)..u64_of(li + dl),
            u64_of(ri)..u64_of(ri + dr),
        );
        li += dl;
        ri += dr;
    }
    push(
        &mut out,
        HunkKind::LeftOnly,
        u64_of(li)..u64_of(left.len()),
        u64_of(ri)..u64_of(ri),
    );
    push(
        &mut out,
        HunkKind::RightOnly,
        u64_of(li)..u64_of(li),
        u64_of(ri)..u64_of(right.len()),
    );
    Ok(Some(out))
}

/// Split `data` at content defined boundaries so that an insertion only
/// perturbs the chunks around it.
fn chunk(data: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut hash: u64 = 0;
    let mut pos = 0usize;
    while pos < data.len() {
        let byte = data.get(pos).copied().unwrap_or(0);
        hash = (hash << 1).wrapping_add(u64::from(byte));
        if pos >= ROLL_WINDOW {
            let old = data.get(pos - ROLL_WINDOW).copied().unwrap_or(0);
            hash = hash.wrapping_sub(u64::from(old) << ROLL_WINDOW);
        }
        pos += 1;
        let len = pos - start;
        if (len >= CHUNK_MIN && (hash & CHUNK_TARGET_MASK) == 0) || len >= CHUNK_MAX {
            out.push(start..pos);
            start = pos;
            hash = 0;
        }
    }
    if start < data.len() {
        out.push(start..data.len());
    }
    out
}

struct ChunkCollector {
    changes: Vec<(Range<u32>, Range<u32>)>,
}

impl Sink for ChunkCollector {
    type Out = Vec<(Range<u32>, Range<u32>)>;

    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        self.changes.push((before, after));
    }

    fn finish(self) -> Self::Out {
        self.changes
    }
}

/// Byte offset of chunk index `i`, where `chunks.len()` maps to the end.
fn chunk_start(chunks: &[Range<usize>], index: u32, total: usize) -> usize {
    chunks.get(index as usize).map_or(total, |r| r.start)
}

fn complete(left: &[u8], right: &[u8], cancel: &dyn Cancel) -> Result<Vec<ByteHunk>, DiffError> {
    if left.is_empty() || right.is_empty() {
        return unaligned(left, right, cancel);
    }
    check(cancel)?;
    let lc = chunk(left);
    let rc = chunk(right);
    check(cancel)?;
    let lk: Vec<&[u8]> = lc
        .iter()
        .map(|r| left.get(r.clone()).unwrap_or_default())
        .collect();
    let rk: Vec<&[u8]> = rc
        .iter()
        .map(|r| right.get(r.clone()).unwrap_or_default())
        .collect();
    let mut input: InternedInput<&[u8]> = InternedInput::default();
    input.update_before(lk.iter().copied());
    input.update_after(rk.iter().copied());
    let changes = diff(
        Algorithm::Histogram,
        &input,
        ChunkCollector {
            changes: Vec::new(),
        },
    );

    let mut out = Vec::new();
    let (mut lcur, mut rcur) = (0usize, 0usize);
    for (before, after) in changes {
        check(cancel)?;
        let ls = chunk_start(&lc, before.start, left.len());
        let rs = chunk_start(&rc, after.start, right.len());
        push(
            &mut out,
            HunkKind::Same,
            u64_of(lcur)..u64_of(ls),
            u64_of(rcur)..u64_of(rs),
        );
        let le = chunk_start(&lc, before.end, left.len());
        let re = chunk_start(&rc, after.end, right.len());
        emit_refined(&mut out, left, right, ls..le, rs..re);
        lcur = le;
        rcur = re;
    }
    push(
        &mut out,
        HunkKind::Same,
        u64_of(lcur)..u64_of(left.len()),
        u64_of(rcur)..u64_of(right.len()),
    );
    Ok(out)
}

/// Trim the bytes a chunk level change shares at its edges so the reported
/// difference is no coarser than it has to be.
fn emit_refined(
    out: &mut Vec<ByteHunk>,
    left: &[u8],
    right: &[u8],
    lr: Range<usize>,
    rr: Range<usize>,
) {
    let a = left.get(lr.clone()).unwrap_or_default();
    let b = right.get(rr.clone()).unwrap_or_default();
    let pre = common_prefix(a, b);
    let suf = common_suffix(
        a.get(pre..).unwrap_or_default(),
        b.get(pre..).unwrap_or_default(),
    );
    let (ls, le) = (lr.start + pre, lr.end - suf);
    let (rs, re) = (rr.start + pre, rr.end - suf);
    push(
        out,
        HunkKind::Same,
        u64_of(lr.start)..u64_of(ls),
        u64_of(rr.start)..u64_of(rs),
    );
    let kind = match (ls >= le, rs >= re) {
        (true, true) => HunkKind::Same,
        (true, false) => HunkKind::RightOnly,
        (false, true) => HunkKind::LeftOnly,
        (false, false) => HunkKind::Changed,
    };
    push(out, kind, u64_of(ls)..u64_of(le), u64_of(rs)..u64_of(re));
    push(
        out,
        HunkKind::Same,
        u64_of(le)..u64_of(lr.end),
        u64_of(re)..u64_of(rr.end),
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const MODES: [ByteAlignment; 3] = [
        ByteAlignment::None,
        ByteAlignment::Fast,
        ByteAlignment::Complete,
    ];

    fn covers(hunks: &[ByteHunk], ll: u64, rl: u64) {
        let mut cl = 0;
        let mut cr = 0;
        for h in hunks {
            assert_eq!(h.left.start, cl, "left gap at {h:?}");
            assert_eq!(h.right.start, cr, "right gap at {h:?}");
            cl = h.left.end;
            cr = h.right.end;
        }
        assert_eq!(cl, ll);
        assert_eq!(cr, rl);
    }

    fn rebuild(hunks: &[ByteHunk], right: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for h in hunks {
            let start = usize::try_from(h.right.start).unwrap_or(usize::MAX);
            let end = usize::try_from(h.right.end).unwrap_or(usize::MAX);
            out.extend_from_slice(right.get(start..end).unwrap_or_default());
        }
        out
    }

    #[test]
    fn identical_inputs_are_one_same_run() {
        for mode in MODES {
            let data = vec![7u8; 5000];
            let hunks = diff_bytes(&data, &data, mode);
            assert_eq!(hunks.len(), 1, "{mode:?}");
            assert_eq!(hunks[0].kind, HunkKind::Same);
        }
    }

    #[test]
    fn single_byte_change_is_isolated() {
        let mut a = vec![0u8; 4096];
        for (i, b) in a.iter_mut().enumerate() {
            *b = u8::try_from(i % 251).unwrap_or(0);
        }
        let mut b = a.clone();
        if let Some(slot) = b.get_mut(2000) {
            *slot ^= 0xFF;
        }
        for mode in MODES {
            let hunks = diff_bytes(&a, &b, mode);
            covers(&hunks, 4096, 4096);
            let changed: Vec<_> = hunks.iter().filter(|h| h.kind != HunkKind::Same).collect();
            assert_eq!(changed.len(), 1, "{mode:?}");
            assert_eq!(changed[0].left, 2000..2001, "{mode:?}");
        }
    }

    #[test]
    fn unaligned_mode_never_reports_an_offset_shift() {
        let a = b"abcdef".to_vec();
        let b = b"Xabcdef".to_vec();
        let hunks = diff_bytes(&a, &b, ByteAlignment::None);
        covers(&hunks, 6, 7);
        assert!(hunks.iter().all(|h| h.kind != HunkKind::Same));
    }

    #[test]
    fn insertion_is_detected_by_aligning_modes() {
        let mut a = Vec::new();
        for i in 0..20_000u32 {
            a.extend_from_slice(&i.to_le_bytes());
        }
        let mut b = a.clone();
        let inserted = vec![0xABu8; 1024];
        b.splice(40_000..40_000, inserted.iter().copied());
        for mode in [ByteAlignment::Fast, ByteAlignment::Complete] {
            let hunks = diff_bytes(&a, &b, mode);
            covers(&hunks, u64_of(a.len()), u64_of(b.len()));
            assert_eq!(rebuild(&hunks, &b), b, "{mode:?}");
            let inserted_bytes: u64 = hunks
                .iter()
                .filter(|h| h.kind != HunkKind::Same)
                .map(|h| h.right.end - h.right.start)
                .sum();
            assert!(inserted_bytes < 8192, "{mode:?} reported {inserted_bytes}");
        }
    }

    #[test]
    fn truncation_is_reported_as_an_orphan_tail() {
        let a = vec![3u8; 10_000];
        let b = vec![3u8; 4_000];
        for mode in MODES {
            let hunks = diff_bytes(&a, &b, mode);
            covers(&hunks, 10_000, 4_000);
            assert_eq!(hunks.last().unwrap().kind, HunkKind::LeftOnly, "{mode:?}");
        }
    }

    #[test]
    fn empty_sides() {
        for mode in MODES {
            assert!(diff_bytes(&[], &[], mode).is_empty());
            let hunks = diff_bytes(b"abc", &[], mode);
            covers(&hunks, 3, 0);
            assert_eq!(hunks[0].kind, HunkKind::LeftOnly);
        }
    }

    #[test]
    fn chunking_stays_within_its_documented_bounds() {
        let mut data = Vec::new();
        for i in 0..200_000u32 {
            data.extend_from_slice(&i.wrapping_mul(2_654_435_761).to_le_bytes());
        }
        let chunks = chunk(&data);
        assert!(chunks.len() > 1);
        for c in chunks.iter().take(chunks.len() - 1) {
            assert!(c.end - c.start >= CHUNK_MIN);
            assert!(c.end - c.start <= CHUNK_MAX);
        }
    }

    /// Many small insertions, each shifting further than the near scan reaches,
    /// which is the shape that makes the windowed index pathological.
    fn shifted_insertions(total: usize, every: usize, run: usize) -> (Vec<u8>, Vec<u8>) {
        let mut a = Vec::with_capacity(total);
        let mut seed = 0x1234_5678u32;
        for _ in 0..total {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            a.push(u8::try_from(seed >> 24).unwrap_or(0));
        }
        let mut b = Vec::with_capacity(total + total / every * run);
        for (i, byte) in a.iter().enumerate() {
            if i % every == 0 {
                b.extend(std::iter::repeat_n(0xEEu8, run));
            }
            b.push(*byte);
        }
        (a, b)
    }

    #[test]
    fn fast_is_not_dramatically_slower_than_complete() {
        let (a, b) = shifted_insertions(4 << 20, 1024, 100);
        let start = std::time::Instant::now();
        let complete_hunks = diff_bytes(&a, &b, ByteAlignment::Complete);
        let complete_time = start.elapsed();
        covers(&complete_hunks, u64_of(a.len()), u64_of(b.len()));

        let start = std::time::Instant::now();
        let fast_hunks = diff_bytes(&a, &b, ByteAlignment::Fast);
        let fast_time = start.elapsed();
        covers(&fast_hunks, u64_of(a.len()), u64_of(b.len()));
        assert_eq!(rebuild(&fast_hunks, &b), b);

        assert!(
            fast_time < complete_time * 8 + std::time::Duration::from_secs(1),
            "fast took {fast_time:?} against complete's {complete_time:?}"
        );
    }

    #[test]
    fn a_raised_flag_stops_the_byte_pass() {
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(true);
        for mode in MODES {
            assert_eq!(
                diff_bytes_cancellable(b"abcd", b"abce", mode, &flag),
                Err(crate::DiffError::Cancelled),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn large_inputs_stay_linear() {
        let mut a = vec![0u8; 8 << 20];
        for (i, b) in a.iter_mut().enumerate() {
            *b = u8::try_from(i % 253).unwrap_or(0);
        }
        let mut b = a.clone();
        b.splice(0..0, [1u8, 2, 3]);
        let hunks = diff_bytes(&a, &b, ByteAlignment::Complete);
        covers(&hunks, u64_of(a.len()), u64_of(b.len()));
        assert!(hunks.len() < 32);
    }
}
