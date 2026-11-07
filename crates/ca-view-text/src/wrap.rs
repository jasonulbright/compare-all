//! Measured word-wrap rows for Text Compare.
//!
//! A layout worker reads buffer snapshots and builds line break points. The
//! frame thread only reads the breaks for rows in the viewport.

use crate::lines::{self, LineMetrics};
use ca_text::{EditSnapshot, TextBuffer};
use ca_ui::worker::Cancel;
use std::ops::Range;
use std::sync::Arc;

/// Maximum line count and wrapped-segment count for one layout.
const MAX_WRAP_LINES: usize = 2_000_000;
const MAX_WRAP_SEGMENTS: usize = 10_000_000;
const MAX_WRAP_VISUAL_LINES: u64 = 20_000_000;
const EXACT_COLUMN_LIMIT: usize = 64 * 1024;

/// Why the wrap-layout worker stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// The input changed while the worker was running.
    Cancelled,
    /// The comparison exceeds the configured layout limits.
    TooLarge,
}

/// The break columns for every line in one pane.
#[derive(Debug, Default)]
pub struct SideWraps {
    /// Offsets into `ends`, with one extra sentinel after the last line.
    offsets: Vec<usize>,
    /// One-past-last display column of each wrapped segment.
    ends: Vec<u32>,
    /// Character offset at each segment end for direct byte-column lines.
    char_ends: Vec<u32>,
    /// Offsets into `source_ends`, with one extra sentinel after the last line.
    source_offsets: Vec<u32>,
    /// Source byte offsets at the segment ends of non-direct lines.
    source_ends: Vec<u32>,
    /// Lines whose columns intentionally follow the >64 KiB byte fallback.
    byte_columns: Vec<bool>,
}

impl SideWraps {
    fn build(buffer: &TextBuffer, width: usize, cancel: &Cancel) -> Result<Self, BuildError> {
        let line_count = buffer.len_lines() as usize;
        if line_count > MAX_WRAP_LINES {
            return Err(BuildError::TooLarge);
        }
        let mut wraps = Self {
            offsets: Vec::with_capacity(line_count.saturating_add(1)),
            ends: Vec::new(),
            char_ends: Vec::new(),
            source_offsets: Vec::with_capacity(line_count.saturating_add(1)),
            source_ends: Vec::new(),
            byte_columns: Vec::with_capacity(line_count),
        };
        wraps.offsets.push(0);
        wraps.source_offsets.push(0);
        for line in 0..line_count {
            if cancel.is_cancelled() {
                return Err(BuildError::Cancelled);
            }
            let line_number = u32::try_from(line).map_err(|_| BuildError::TooLarge)?;
            let slice = buffer.line(line_number).ok_or(BuildError::TooLarge)?;
            let mut content_chars = slice.len_chars();
            if content_chars > 0 && slice.char(content_chars - 1) == '\n' {
                content_chars -= 1;
                if content_chars > 0 && slice.char(content_chars - 1) == '\r' {
                    content_chars -= 1;
                }
            } else if content_chars > 0 && slice.char(content_chars - 1) == '\r' {
                content_chars -= 1;
            }
            let content = slice.slice(..content_chars);
            if content.len_bytes() > EXACT_COLUMN_LIMIT {
                u32::try_from(content.len_bytes()).map_err(|_| BuildError::TooLarge)?;
                push_chunked_byte_ends(
                    &mut wraps.ends,
                    &mut wraps.char_ends,
                    content.chunks(),
                    width,
                    cancel,
                )?;
                wraps.byte_columns.push(true);
            } else {
                let text = content.to_string();
                let metrics = lines::measure(&text);
                let before = wraps.ends.len();
                push_line_ends(&mut wraps.ends, &text, metrics, width, cancel)?;
                if metrics.direct {
                    wraps.char_ends.extend_from_slice(&wraps.ends[before..]);
                } else {
                    wraps.char_ends.resize(wraps.ends.len(), 0);
                    wraps
                        .source_ends
                        .extend(lines::byte_offsets_at_columns(&text, &wraps.ends[before..]));
                }
                wraps.byte_columns.push(false);
            }
            wraps.offsets.push(wraps.ends.len());
            wraps
                .source_offsets
                .push(u32::try_from(wraps.source_ends.len()).map_err(|_| BuildError::TooLarge)?);
        }
        Ok(wraps)
    }

    /// The number of visual rows for one line. An absent line has no rows.
    #[must_use]
    pub fn line_count(&self, line: u32) -> usize {
        self.ends_for(line).len()
    }

    /// The display-column range of one visual row of a line.
    #[must_use]
    pub fn segment(&self, line: u32, row: usize) -> Option<Range<usize>> {
        let ends = self.ends_for(line);
        let end = usize::try_from(*ends.get(row)?).ok()?;
        let start = if row == 0 {
            0
        } else {
            usize::try_from(*ends.get(row - 1)?).ok()?
        };
        Some(start..end)
    }

    /// The end columns for a line, or `None` when the line is outside the
    /// buffer snapshot this layout measured.
    #[must_use]
    pub fn ends_for(&self, line: u32) -> &[u32] {
        let line = line as usize;
        let Some(start) = self.offsets.get(line).copied() else {
            return &[];
        };
        let Some(end) = self.offsets.get(line.saturating_add(1)).copied() else {
            return &[];
        };
        self.ends.get(start..end).unwrap_or(&[])
    }

    /// Character-index range of one direct byte-column segment.
    #[must_use]
    pub fn character_segment(&self, line: u32, row: usize) -> Option<Range<usize>> {
        let ends = self.char_ends_for(line);
        let end = usize::try_from(*ends.get(row)?).ok()?;
        let start = if row == 0 {
            0
        } else {
            usize::try_from(*ends.get(row - 1)?).ok()?
        };
        Some(start..end)
    }

    /// Source byte range for one wrapped segment.
    #[must_use]
    pub fn source_byte_segment(&self, line: u32, row: usize) -> Option<Range<usize>> {
        let ends = self.source_ends_for(line);
        let end = usize::try_from(*ends.get(row)?).ok()?;
        let start = if row == 0 {
            0
        } else {
            usize::try_from(*ends.get(row - 1)?).ok()?
        };
        Some(start..end)
    }

    /// True when this line uses the renderer's large-line byte columns.
    #[must_use]
    pub fn uses_byte_columns(&self, line: u32) -> bool {
        self.byte_columns
            .get(usize::try_from(line).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(false)
    }

    fn char_ends_for(&self, line: u32) -> &[u32] {
        let line = line as usize;
        let Some(start) = self.offsets.get(line).copied() else {
            return &[];
        };
        let Some(end) = self.offsets.get(line.saturating_add(1)).copied() else {
            return &[];
        };
        self.char_ends.get(start..end).unwrap_or(&[])
    }

    /// Source byte end offsets for the wrapped segments of one line.
    #[must_use]
    pub fn source_ends_for(&self, line: u32) -> &[u32] {
        let line_index = usize::try_from(line).unwrap_or(usize::MAX);
        let Some(start) = self.source_offsets.get(line_index).copied() else {
            return &[];
        };
        let Some(end) = self
            .source_offsets
            .get(line_index.saturating_add(1))
            .copied()
        else {
            return &[];
        };
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        let end = usize::try_from(end).unwrap_or(usize::MAX);
        let source = self.source_ends.get(start..end).unwrap_or(&[]);
        if source.len() == self.line_count(line) {
            source
        } else {
            self.ends_for(line)
        }
    }
}

/// The row heights and break points for a comparison at one pane width.
#[derive(Debug)]
pub struct WrapLayout {
    /// Pane width in display columns.
    pub columns: usize,
    /// The row height in visual lines, by model row.
    pub row_lines: Vec<u32>,
    /// Prefix sums for the unfiltered rows, built by the layout worker.
    pub all_prefix: Arc<Vec<u64>>,
    /// Filtered rows used when this layout was requested.
    pub initial_visible: Arc<crate::model::Visible>,
    /// Prefix sums for `initial_visible`, built by the layout worker.
    pub initial_prefix: Arc<Vec<u64>>,
    /// Left-pane line breaks.
    pub left: SideWraps,
    /// Right-pane line breaks.
    pub right: SideWraps,
    /// The line pairs this layout was measured from.
    pub model_lines: Arc<Vec<(Option<u32>, Option<u32>)>>,
}

/// Build a variable-height layout without reading a file or measuring text on
/// the frame thread.
///
/// # Errors
///
/// Returns [`BuildError::Cancelled`] when the input changes during layout and
/// [`BuildError::TooLarge`] when the buffers or resulting layout exceed the
/// configured limits.
pub fn build(
    left: &EditSnapshot,
    right: &EditSnapshot,
    model_lines: Arc<Vec<(Option<u32>, Option<u32>)>>,
    visible: Arc<crate::model::Visible>,
    columns: usize,
    cancel: &Cancel,
) -> Result<WrapLayout, BuildError> {
    let columns = columns.max(1);
    if model_lines.len() > MAX_WRAP_LINES {
        return Err(BuildError::TooLarge);
    }
    let left = SideWraps::build(left.buffer(), columns, cancel)?;
    let right = SideWraps::build(right.buffer(), columns, cancel)?;
    let mut row_lines = Vec::with_capacity(model_lines.len());
    let mut all_prefix = Vec::with_capacity(model_lines.len().saturating_add(1));
    all_prefix.push(0_u64);
    let mut total_visual_lines = 0_u64;
    for (left_line, right_line) in model_lines.iter().copied() {
        if cancel.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        let left_count = left_line.map_or(1, |line| left.line_count(line).max(1));
        let right_count = right_line.map_or(1, |line| right.line_count(line).max(1));
        #[allow(clippy::cast_possible_truncation)]
        let height =
            u32::try_from(left_count.max(right_count)).map_err(|_| BuildError::TooLarge)?;
        total_visual_lines = total_visual_lines.saturating_add(u64::from(height));
        if total_visual_lines > MAX_WRAP_VISUAL_LINES {
            return Err(BuildError::TooLarge);
        }
        row_lines.push(height);
        all_prefix.push(total_visual_lines);
    }
    let all_prefix = Arc::new(all_prefix);
    let initial_prefix = if matches!(&*visible, crate::model::Visible::All(count) if *count == row_lines.len())
    {
        Arc::clone(&all_prefix)
    } else {
        let mut prefix = Vec::with_capacity(visible.len().saturating_add(1));
        prefix.push(0_u64);
        for position in 0..visible.len() {
            if cancel.is_cancelled() {
                return Err(BuildError::Cancelled);
            }
            let Some(row) = visible.row_at(position) else {
                break;
            };
            let height = row_lines.get(row).copied().unwrap_or(1).max(1);
            prefix.push(
                prefix
                    .last()
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(u64::from(height)),
            );
        }
        Arc::new(prefix)
    };
    Ok(WrapLayout {
        columns,
        row_lines,
        all_prefix,
        initial_visible: visible,
        initial_prefix,
        left,
        right,
        model_lines,
    })
}

/// Build the visual-height prefix for a filtered view on a worker.
///
/// # Errors
///
/// Returns [`BuildError::Cancelled`] when the worker is cancelled.
pub fn visible_prefix(
    row_lines: &[u32],
    visible: &crate::model::Visible,
    cancel: &Cancel,
) -> Result<Vec<u64>, BuildError> {
    let mut prefix = Vec::with_capacity(visible.len().saturating_add(1));
    prefix.push(0_u64);
    for position in 0..visible.len() {
        if cancel.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        let Some(row) = visible.row_at(position) else {
            break;
        };
        let height = row_lines.get(row).copied().unwrap_or(1).max(1);
        prefix.push(
            prefix
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(u64::from(height)),
        );
    }
    Ok(prefix)
}

fn push_line_ends(
    output: &mut Vec<u32>,
    text: &str,
    metrics: LineMetrics,
    width: usize,
    cancel: &Cancel,
) -> Result<(), BuildError> {
    if metrics.columns == 0 {
        if output.len() >= MAX_WRAP_SEGMENTS {
            return Err(BuildError::TooLarge);
        }
        output.push(0);
        return Ok(());
    }
    if metrics.direct {
        push_byte_ends(output, text, width, cancel)
    } else {
        // Non-direct lines are bounded by `lines::measure`. Expanding a tab
        // once gives every later segment a display-column string.
        let expanded = lines::window(text, metrics, 0, metrics.columns as usize);
        push_character_ends(output, expanded.as_ref(), width, cancel)
    }
}

fn push_character_ends(
    output: &mut Vec<u32>,
    text: &str,
    width: usize,
    cancel: &Cancel,
) -> Result<(), BuildError> {
    let characters: Vec<char> = text.chars().collect();
    let mut start = 0usize;
    while start < characters.len() {
        if cancel.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        if output.len() >= MAX_WRAP_SEGMENTS {
            return Err(BuildError::TooLarge);
        }
        let limit = start.saturating_add(width).min(characters.len());
        let end = if limit == characters.len() {
            limit
        } else if !characters[limit - 1].is_whitespace() && characters[limit].is_whitespace() {
            // Keep a word that fills the pane on this row. Its following
            // whitespace stays at the start of the next visual row.
            limit
        } else {
            characters[start..limit]
                .iter()
                .rposition(|character| character.is_whitespace())
                .map_or(limit, |space| start + space + 1)
        };
        let end = end.max(start.saturating_add(1));
        output.push(u32::try_from(end).unwrap_or(u32::MAX));
        start = end;
    }
    Ok(())
}

fn push_byte_ends(
    output: &mut Vec<u32>,
    text: &str,
    width: usize,
    cancel: &Cancel,
) -> Result<(), BuildError> {
    let mut start = 0usize;
    while start < text.len() {
        if cancel.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        if output.len() >= MAX_WRAP_SEGMENTS {
            return Err(BuildError::TooLarge);
        }
        let mut limit = start.saturating_add(width).min(text.len());
        while limit > start && !text.is_char_boundary(limit) {
            limit -= 1;
        }
        if limit == start {
            limit = text[start..]
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(offset, _)| start + offset);
        }
        let segment = text.get(start..limit).ok_or(BuildError::TooLarge)?;
        let next_is_space = text
            .get(limit..)
            .and_then(|tail| tail.chars().next())
            .is_some_and(char::is_whitespace);
        let previous_is_space = segment.chars().next_back().is_some_and(char::is_whitespace);
        let end = if limit == text.len() || (!previous_is_space && next_is_space) {
            limit
        } else {
            segment
                .char_indices()
                .filter_map(|(offset, character)| {
                    character
                        .is_whitespace()
                        .then_some(start + offset + character.len_utf8())
                })
                .next_back()
                .filter(|space_end| *space_end > start)
                .unwrap_or(limit)
        };
        output.push(u32::try_from(end).unwrap_or(u32::MAX));
        start = end;
    }
    Ok(())
}

fn push_chunked_byte_ends<'a>(
    output: &mut Vec<u32>,
    char_output: &mut Vec<u32>,
    chunks: impl IntoIterator<Item = &'a str>,
    width: usize,
    cancel: &Cancel,
) -> Result<(), BuildError> {
    let width = width.max(1);
    let mut pending = Vec::<(usize, usize, char)>::new();
    let mut pending_start = 0usize;
    let mut byte = 0usize;
    let mut character_index = 0usize;
    for chunk in chunks {
        for character in chunk.chars() {
            if cancel.is_cancelled() {
                return Err(BuildError::Cancelled);
            }
            let end = byte.saturating_add(character.len_utf8());
            if let Some(&(last_end, last_character, previous)) = pending.last() {
                if end.saturating_sub(pending_start) > width {
                    if output.len() >= MAX_WRAP_SEGMENTS {
                        return Err(BuildError::TooLarge);
                    }
                    let fit_end = last_end;
                    let (break_end, break_character) =
                        if !previous.is_whitespace() && character.is_whitespace() {
                            (fit_end, last_character)
                        } else {
                            pending
                                .iter()
                                .rev()
                                .find(|(_, _, candidate)| candidate.is_whitespace())
                                .map_or((fit_end, last_character), |(end, character, _)| {
                                    (*end, *character)
                                })
                        };
                    output.push(u32::try_from(break_end).unwrap_or(u32::MAX));
                    char_output.push(u32::try_from(break_character).unwrap_or(u32::MAX));
                    pending.retain(|(candidate_end, _, _)| *candidate_end > break_end);
                    pending_start = break_end;
                }
            }
            if pending.is_empty() {
                pending_start = byte;
            }
            character_index = character_index.saturating_add(1);
            pending.push((end, character_index, character));
            byte = end;
        }
    }
    if !pending.is_empty() {
        if output.len() >= MAX_WRAP_SEGMENTS {
            return Err(BuildError::TooLarge);
        }
        output.push(u32::try_from(byte).unwrap_or(u32::MAX));
        char_output.push(u32::try_from(character_index).unwrap_or(u32::MAX));
    } else if output.is_empty() {
        output.push(0);
        char_output.push(0);
    }
    Ok(())
}

/// Prefix sums of the visual-line heights currently shown by a filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisibleHeights {
    prefix: Arc<Vec<u64>>,
}

#[allow(clippy::cast_precision_loss)]
impl VisibleHeights {
    /// Build display-order prefix sums from the model rows selected by a filter.
    #[must_use]
    pub fn new(layout: &WrapLayout, visible: &crate::model::Visible) -> Self {
        if std::ptr::eq(layout.initial_visible.as_ref(), visible) {
            return Self {
                prefix: layout.initial_prefix.clone(),
            };
        }
        if matches!(visible, crate::model::Visible::All(count) if *count == layout.row_lines.len())
        {
            return Self {
                prefix: layout.all_prefix.clone(),
            };
        }
        let mut prefix = Vec::with_capacity(visible.len().saturating_add(1));
        prefix.push(0_u64);
        for position in 0..visible.len() {
            let Some(row) = visible.row_at(position) else {
                break;
            };
            let height = layout.row_lines.get(row).copied().unwrap_or(1).max(1);
            prefix.push(
                prefix
                    .last()
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(u64::from(height)),
            );
        }
        Self {
            prefix: Arc::new(prefix),
        }
    }

    /// Use a prefix accumulated while selecting filtered rows.
    #[must_use]
    pub(crate) fn from_prefix(prefix: Vec<u64>) -> Self {
        Self {
            prefix: Arc::new(prefix),
        }
    }

    /// The number of shown rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.prefix.len().saturating_sub(1)
    }

    /// Total height in visual lines.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.prefix.last().copied().unwrap_or(0)
    }

    /// Height in visual lines at one display position.
    #[must_use]
    pub fn row_height(&self, position: usize) -> u64 {
        self.prefix
            .get(position.saturating_add(1))
            .copied()
            .unwrap_or(0)
            .saturating_sub(self.prefix.get(position).copied().unwrap_or(0))
    }

    /// Top of one display position in visual lines.
    #[must_use]
    pub fn top(&self, position: usize) -> u64 {
        self.prefix.get(position).copied().unwrap_or(self.total())
    }

    /// The display position under an offset in visual lines.
    #[must_use]
    pub fn row_at(&self, offset: f64) -> usize {
        let target = offset.max(0.0);
        let mut low = 0usize;
        let mut high = self.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.prefix.get(middle + 1).copied().unwrap_or(u64::MAX) as f64 <= target {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low.min(self.len().saturating_sub(1))
    }

    /// The row positions covered by an offset and viewport.
    #[must_use]
    pub fn visible(&self, offset: f64, viewport: f64) -> Range<usize> {
        if self.is_empty() || viewport <= 0.0 {
            return 0..0;
        }
        let start = self.row_at(offset);
        let edge = (offset.max(0.0) + viewport).min(self.total() as f64);
        let end_row = self.row_at(edge);
        let end = if self.top(end_row) as f64 >= edge {
            end_row
        } else {
            end_row.saturating_add(1)
        };
        start..end.min(self.len())
    }

    /// True when no row is shown.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A vertical offset for fixed or variable-height Text Compare rows.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TextScroll {
    offset: f64,
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
impl TextScroll {
    /// The current offset in visual lines.
    #[must_use]
    pub const fn offset(self) -> f64 {
        self.offset
    }

    /// Clamp the offset to the content and viewport.
    pub fn clamp(&mut self, heights: &VisibleHeights, viewport: f64) {
        let furthest = (heights.total() as f64 - viewport.max(0.0)).max(0.0);
        if !self.offset.is_finite() {
            self.offset = 0.0;
        }
        self.offset = self.offset.clamp(0.0, furthest);
    }

    /// The first visible display row and the fraction through its height.
    #[must_use]
    pub fn position(self, heights: &VisibleHeights) -> (usize, f64) {
        if heights.is_empty() {
            return (0, 0.0);
        }
        let row = heights.row_at(self.offset);
        let top = heights.top(row) as f64;
        let height = heights.row_height(row).max(1) as f64;
        (row, ((self.offset - top) / height).clamp(0.0, 1.0))
    }

    /// Display rows that intersect the viewport.
    #[must_use]
    pub fn visible(self, heights: &VisibleHeights, viewport: f64) -> Range<usize> {
        heights.visible(self.offset, viewport)
    }

    /// The wrapped subrows of one comparison row intersecting the viewport.
    #[must_use]
    pub fn row_segments(
        self,
        heights: &VisibleHeights,
        position: usize,
        viewport: f64,
    ) -> Range<usize> {
        let height = heights.row_height(position);
        if height == 0 || viewport <= 0.0 {
            return 0..0;
        }
        #[allow(clippy::cast_precision_loss)]
        let top = heights.top(position) as f64;
        #[allow(clippy::cast_precision_loss)]
        let height_f64 = height as f64;
        let start = (self.offset - top).floor().max(0.0);
        let end = (self.offset + viewport - top).ceil().clamp(0.0, height_f64);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let start = start.min(height_f64) as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let end = end as usize;
        start..end.min(usize::try_from(height).unwrap_or(usize::MAX))
    }

    /// The vertical position of one display row in visual lines.
    #[must_use]
    pub fn row_y(self, heights: &VisibleHeights, position: usize) -> f64 {
        heights.top(position) as f64 - self.offset
    }

    /// The display row below a y coordinate measured in points.
    #[must_use]
    pub fn row_under(self, heights: &VisibleHeights, y: f32, row_height: f32) -> usize {
        if row_height <= 0.0 || heights.is_empty() {
            return 0;
        }
        heights.row_at(self.offset + f64::from(y.max(0.0) / row_height))
    }

    /// Scroll so one row is in view.
    pub fn reveal(&mut self, heights: &VisibleHeights, position: usize, viewport: f64) {
        if heights.is_empty() {
            self.offset = 0.0;
            return;
        }
        let top = heights.top(position) as f64;
        let bottom = top + heights.row_height(position) as f64;
        if top < self.offset {
            self.offset = top;
        } else if bottom > self.offset + viewport {
            self.offset = if bottom - top > viewport {
                top
            } else {
                bottom - viewport
            };
        }
        self.clamp(heights, viewport);
    }

    /// Reveal one visual line within a potentially much taller comparison row.
    pub fn reveal_visual_line(
        &mut self,
        heights: &VisibleHeights,
        visual_line: u64,
        viewport: f64,
    ) {
        if heights.total() == 0 {
            self.offset = 0.0;
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let top = visual_line.min(heights.total().saturating_sub(1)) as f64;
        let bottom = top + 1.0;
        if top < self.offset {
            self.offset = top;
        } else if bottom > self.offset + viewport {
            self.offset = if viewport < 1.0 {
                top
            } else {
                bottom - viewport
            };
        }
        self.clamp(heights, viewport);
    }

    /// Put the top of one row at the viewport top.
    pub fn go_to(&mut self, heights: &VisibleHeights, position: usize, viewport: f64) {
        self.offset = heights.top(position) as f64;
        self.clamp(heights, viewport);
    }

    /// Keep the same fraction of one row visible after a layout change.
    pub fn set_position(
        &mut self,
        heights: &VisibleHeights,
        position: usize,
        fraction: f64,
        viewport: f64,
    ) {
        let height = heights.row_height(position) as f64;
        self.offset = heights.top(position) as f64 + height * fraction.clamp(0.0, 1.0);
        self.clamp(heights, viewport);
    }

    /// Move the viewport by a pixel distance.
    pub fn scroll_by(
        &mut self,
        heights: &VisibleHeights,
        pixels: f32,
        row_height: f32,
        viewport: f64,
    ) {
        if row_height <= 0.0 {
            return;
        }
        self.offset += f64::from(pixels / row_height);
        self.clamp(heights, viewport);
    }

    /// Thumb top and bottom in track points.
    #[must_use]
    pub fn thumb(self, heights: &VisibleHeights, track: f32, viewport: f64) -> Range<f32> {
        if track <= 0.0 || heights.total() == 0 {
            return 0.0..track.max(0.0);
        }
        let total = heights.total() as f64;
        let viewport = viewport.max(0.0).min(total);
        let length = (f64::from(track) * viewport / total)
            .clamp(12.0_f64.min(f64::from(track)), f64::from(track));
        let furthest = (total - viewport).max(0.0);
        let progress = if furthest > 0.0 {
            self.offset / furthest
        } else {
            0.0
        };
        let top = (f64::from(track) - length) * progress.clamp(0.0, 1.0);
        top as f32..(top + length) as f32
    }

    /// Set the offset from a scrollbar pointer y coordinate.
    pub fn from_thumb(
        &mut self,
        heights: &VisibleHeights,
        pointer_top: f32,
        track: f32,
        viewport: f64,
    ) {
        if track <= 0.0 || heights.total() == 0 {
            self.offset = 0.0;
            return;
        }
        let thumb = self.thumb(heights, track, viewport);
        let available = (track - (thumb.end - thumb.start)).max(0.0);
        let progress = if available > 0.0 {
            f64::from(pointer_top.max(0.0) / available)
        } else {
            0.0
        };
        self.offset = progress.clamp(0.0, 1.0) * (heights.total() as f64 - viewport).max(0.0);
        self.clamp(heights, viewport);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        build, push_byte_ends, push_character_ends, push_chunked_byte_ends, SideWraps, TextScroll,
        VisibleHeights,
    };
    use crate::lines;
    use crate::model::Visible;
    use ca_text::TextBuffer;
    use ca_ui::worker::Cancel;
    use std::sync::Arc;

    #[test]
    fn wraps_at_the_last_word_boundary_that_fits() {
        let mut ends = Vec::new();
        push_character_ends(&mut ends, "one two three", 7, &Cancel::new()).expect("wrap");
        assert_eq!(ends, [7, 13]);
    }

    #[test]
    fn a_word_longer_than_the_pane_breaks_at_the_pane_edge() {
        let mut ends = Vec::new();
        push_character_ends(&mut ends, "abcdef", 3, &Cancel::new()).expect("wrap");
        assert_eq!(ends, [3, 6]);
    }

    #[test]
    fn byte_wrapping_keeps_utf8_boundaries_and_tabs_are_breaks() {
        let mut ends = Vec::new();
        push_byte_ends(&mut ends, "ab\täz", 3, &Cancel::new()).expect("wrap");
        assert_eq!(ends, [3, 6]);
    }

    #[test]
    fn chunked_wrapping_matches_contiguous_byte_wrapping_at_every_chunk_boundary() {
        for (text, width) in [("one two three", 7), ("ab\täz", 3), ("abcde", 2)] {
            let mut expected = Vec::new();
            push_byte_ends(&mut expected, text, width, &Cancel::new()).expect("contiguous");
            for split in text
                .char_indices()
                .map(|(offset, _)| offset)
                .chain(std::iter::once(text.len()))
            {
                let mut actual = Vec::new();
                let mut actual_chars = Vec::new();
                push_chunked_byte_ends(
                    &mut actual,
                    &mut actual_chars,
                    [
                        text.get(..split).expect("left chunk"),
                        text.get(split..).expect("right chunk"),
                    ],
                    width,
                    &Cancel::new(),
                )
                .expect("chunked");
                assert_eq!(actual, expected, "{text:?} split at {split}");
                let expected_chars = expected
                    .iter()
                    .map(|end| {
                        u32::try_from(
                            text.get(..usize::try_from(*end).expect("byte end"))
                                .expect("boundary")
                                .chars()
                                .count(),
                        )
                        .expect("character end")
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual_chars, expected_chars, "{text:?} split at {split}");
            }
        }
    }

    #[test]
    fn long_direct_rope_lines_match_the_byte_column_fallback() {
        for text in ["a".repeat(70_000), "é".repeat(40_000), "\t".repeat(70_000)] {
            let buffer = TextBuffer::from_text(&text);
            let mut expected = Vec::new();
            push_byte_ends(&mut expected, &text, 3, &Cancel::new()).expect("contiguous");
            let wraps = SideWraps::build(&buffer, 3, &Cancel::new()).expect("layout");
            assert_eq!(wraps.ends_for(0), expected);
            let expected_chars = expected
                .iter()
                .map(|end| {
                    u32::try_from(
                        text.get(..usize::try_from(*end).expect("byte end"))
                            .expect("boundary")
                            .chars()
                            .count(),
                    )
                    .expect("character end")
                })
                .collect::<Vec<_>>();
            let actual_chars = (0..wraps.line_count(0))
                .map(|segment| {
                    u32::try_from(wraps.character_segment(0, segment).expect("segment").end)
                        .expect("character end")
                })
                .collect::<Vec<_>>();
            assert_eq!(actual_chars, expected_chars);
        }
    }

    #[test]
    fn empty_lines_keep_one_visual_row() {
        let wraps =
            SideWraps::build(&TextBuffer::from_text("\n"), 20, &Cancel::new()).expect("wraps");
        assert_eq!(wraps.line_count(0), 1);
        assert_eq!(wraps.line_count(1), 1);
        assert_eq!(wraps.segment(0, 0), Some(0..0));
    }

    #[test]
    fn windows_expand_tabs_at_their_absolute_columns() {
        let text = "a\täbc";
        let metrics = lines::measure(text);
        let first = lines::window(text, metrics, 0, 8).into_owned();
        let second = lines::window(text, metrics, 8, 2).into_owned();
        let third = lines::window(text, metrics, 10, 1).into_owned();
        assert_eq!(
            [first.as_str(), second.as_str(), third.as_str()],
            ["a       ", "äb", "c"]
        );
    }

    #[test]
    fn wrapped_source_ranges_seek_before_each_non_direct_segment() {
        let text = "a\t日本語x";
        let buffer = TextBuffer::from_text(text);
        let wraps = SideWraps::build(&buffer, 4, &Cancel::new()).expect("wraps");
        let metrics = lines::measure(text);
        assert!(wraps.line_count(0) > 2);
        for segment in 0..wraps.line_count(0) {
            let columns = wraps.segment(0, segment).expect("columns");
            let bytes = wraps.source_byte_segment(0, segment).expect("source bytes");
            assert_eq!(
                lines::window_from_byte(
                    text,
                    metrics,
                    bytes.start,
                    columns.start,
                    columns.start,
                    columns.end - columns.start,
                ),
                lines::window(text, metrics, columns.start, columns.end - columns.start),
                "segment {segment}"
            );
        }
    }

    #[test]
    fn a_comparison_row_takes_the_taller_pane_height() {
        let left = TextBuffer::from_text("one two three\nshort").edit_snapshot();
        let right = TextBuffer::from_text("one\nshort").edit_snapshot();
        let layout = build(
            &left,
            &right,
            Arc::new(vec![(Some(0), Some(0)), (Some(1), Some(1))]),
            Arc::new(Visible::All(2)),
            7,
            &Cancel::new(),
        )
        .expect("layout");
        assert_eq!(layout.row_lines, [2, 1]);
        let heights = VisibleHeights::new(&layout, &Visible::All(2));
        assert!(Arc::ptr_eq(&heights.prefix, &layout.all_prefix));
    }

    #[test]
    fn worker_builds_the_initial_filtered_prefix() {
        let left = TextBuffer::from_text("a\nb c d e\nf").edit_snapshot();
        let right = TextBuffer::from_text("a\nb\nf").edit_snapshot();
        let visible = Arc::new(Visible::Subset(vec![1, 2]));
        let layout = build(
            &left,
            &right,
            Arc::new(vec![
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (Some(2), Some(2)),
            ]),
            Arc::clone(&visible),
            3,
            &Cancel::new(),
        )
        .expect("layout");

        assert_eq!(layout.row_lines, [1, 3, 1]);
        assert_eq!(layout.initial_prefix.as_slice(), [0, 3, 4]);
        let heights = VisibleHeights::new(&layout, visible.as_ref());
        assert!(Arc::ptr_eq(&heights.prefix, &layout.initial_prefix));
    }

    #[test]
    fn visible_prefixes_include_filter_order_and_heights() {
        let left = TextBuffer::from_text("a\nb\nc").edit_snapshot();
        let right = TextBuffer::from_text("a\nb\nc").edit_snapshot();
        let layout = build(
            &left,
            &right,
            Arc::new(vec![
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (Some(2), Some(2)),
            ]),
            Arc::new(Visible::All(3)),
            1,
            &Cancel::new(),
        )
        .expect("layout");
        let visible = Visible::Subset(vec![0, 2]);
        let heights = VisibleHeights::new(&layout, &visible);
        assert_eq!(heights.total(), 2);
        assert_eq!(heights.visible(0.0, 1.0), 0..1);
        assert_eq!(heights.visible(1.0, 1.0), 1..2);
    }

    #[test]
    fn variable_scroll_maps_rows_and_keeps_the_taller_line_in_view() {
        let left = TextBuffer::from_text("a\nb\nc").edit_snapshot();
        let right = TextBuffer::from_text("a\nb\nc").edit_snapshot();
        let mut layout = build(
            &left,
            &right,
            Arc::new(vec![
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (Some(2), Some(2)),
            ]),
            Arc::new(Visible::All(3)),
            1,
            &Cancel::new(),
        )
        .expect("layout");
        layout.row_lines[1] = 3;
        layout.all_prefix = Arc::new(vec![0, 1, 4, 5]);
        let heights = VisibleHeights::new(&layout, &Visible::All(3));
        let mut scroll = TextScroll::default();
        scroll.reveal(&heights, 1, 2.0);
        assert!((scroll.offset() - 1.0).abs() < f64::EPSILON);
        assert_eq!(scroll.visible(&heights, 2.0), 1..2);
        assert_eq!(scroll.row_segments(&heights, 1, 2.0), 0..2);
        assert_eq!(scroll.row_under(&heights, 1.5, 1.0), 1);
        assert!((scroll.row_y(&heights, 2) - 3.0).abs() < f64::EPSILON);
        scroll.set_position(&heights, 1, 0.5, 2.0);
        assert_eq!(scroll.row_segments(&heights, 1, 2.0), 1..3);
        scroll.reveal_visual_line(&heights, 4, 2.0);
        assert!((scroll.offset() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn variable_scrollbar_maps_the_thumb_to_visual_line_offsets() {
        let left = TextBuffer::from_text("a\nb\nc\nd").edit_snapshot();
        let right = TextBuffer::from_text("a\nb\nc\nd").edit_snapshot();
        let mut layout = build(
            &left,
            &right,
            Arc::new(vec![
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (Some(2), Some(2)),
                (Some(3), Some(3)),
            ]),
            Arc::new(Visible::All(4)),
            1,
            &Cancel::new(),
        )
        .expect("layout");
        layout.row_lines[1] = 4;
        layout.all_prefix = Arc::new(vec![0, 1, 5, 6, 7]);
        let heights = VisibleHeights::new(&layout, &Visible::All(4));
        let mut scroll = TextScroll::default();
        let thumb = scroll.thumb(&heights, 100.0, 3.0);
        let available = 100.0 - (thumb.end - thumb.start);
        scroll.from_thumb(&heights, available, 100.0, 3.0);
        assert!((scroll.offset() - 4.0).abs() < f64::EPSILON);
        assert_eq!(scroll.visible(&heights, 3.0), 1..4);
    }
}
