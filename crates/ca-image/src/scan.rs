//! Structural frame counting.
//!
//! Counting walks the container structure only: it reads the block headers and
//! skips every payload, so it never decodes, composites or allocates a frame.
//! The cost is therefore bounded by the file size, not by the logical screen
//! size times the frame count.
//!
//! Two further bounds apply. A byte budget stops a scan that walks further than
//! the caller allows, and a cancellation signal stops it between blocks. Both
//! stop the walk and report the frames found so far, marked as a lower bound.

use crate::cancel::Cancel;
use crate::error::Result;
use crate::settings::provisional;
use std::collections::BTreeSet;
use std::io::{BufRead, Seek, SeekFrom};

/// Blocks walked between two cancellation polls.
const POLL_INTERVAL: u32 = 64;

/// The container formats a structural scan understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    /// Frames are image descriptors in the block stream.
    Gif,
    /// Frames are `ANMF` chunks in the RIFF stream.
    WebP,
    /// Frames are `fcTL` chunks in the chunk stream.
    Png,
    /// Pages are chained image file directories.
    Tiff,
    /// Entries are the directory records in an icon header.
    Ico,
}

/// Reader state shared by the three scanners.
struct Walk<'a, R: BufRead + Seek, C: Cancel> {
    reader: R,
    budget: u64,
    used: u64,
    blocks: u32,
    cancel: &'a C,
    /// Set when a budget, the frame ceiling or cancellation stopped the walk
    /// before the structure ended.
    partial: bool,
}

/// Why a walk stopped. Every reason yields the frames counted so far.
enum Stop {
    /// The structure ended, or ended in a way the scanner does not follow.
    End,
}

impl<R: BufRead + Seek, C: Cancel> Walk<'_, R, C> {
    /// Charges `count` bytes against the budget. Reports the end once the
    /// budget is spent.
    fn spend(&mut self, count: u64) -> std::result::Result<(), Stop> {
        self.used = self.used.saturating_add(count);
        if self.used > self.budget {
            self.partial = true;
            return Err(Stop::End);
        }
        Ok(())
    }

    /// Reads exactly `N` bytes, or reports the end.
    fn read<const N: usize>(&mut self) -> std::result::Result<[u8; N], Stop> {
        self.spend(u64::try_from(N).unwrap_or(u64::MAX))?;
        let mut buffer = [0u8; N];
        self.reader.read_exact(&mut buffer).map_err(|_| Stop::End)?;
        Ok(buffer)
    }

    /// Skips `count` bytes without reading them into memory.
    fn skip(&mut self, count: u64) -> std::result::Result<(), Stop> {
        self.spend(count)?;
        let step = i64::try_from(count).map_err(|_| Stop::End)?;
        self.reader
            .seek(SeekFrom::Current(step))
            .map_err(|_| Stop::End)?;
        Ok(())
    }

    /// Polls the cancellation signal every [`POLL_INTERVAL`] blocks.
    fn poll(&mut self) -> std::result::Result<(), Stop> {
        self.blocks = self.blocks.wrapping_add(1);
        if self.blocks.is_multiple_of(POLL_INTERVAL) && self.cancel.is_cancelled() {
            self.partial = true;
            return Err(Stop::End);
        }
        Ok(())
    }
}

/// Counts the frames a container declares, without decoding any of them.
///
/// The count stops at [`provisional::MAX_COUNTED_FRAMES`], at `budget` bytes,
/// or when `cancel` fires, and reports the frames found up to that point. A
/// container that carries no animation structure reports one frame, because the
/// still image it holds is that frame.
///
/// # Errors
/// Returns [`crate::Error::Io`] only when the reader cannot be rewound.
#[cfg(test)]
pub(crate) fn count_frames<R: BufRead + Seek, C: Cancel>(
    reader: R,
    container: Container,
    budget: u64,
    cancel: &C,
) -> Result<u32> {
    count_frames_in_part(reader, container, budget, cancel).map(|(count, _)| count)
}

/// Like [`count_frames`], also stating whether the walk stopped before the
/// structure ended, so the count is a lower bound.
///
/// # Errors
/// Returns [`crate::Error::Io`] only when the reader cannot be rewound.
pub(crate) fn count_frames_in_part<R: BufRead + Seek, C: Cancel>(
    mut reader: R,
    container: Container,
    budget: u64,
    cancel: &C,
) -> Result<(u32, bool)> {
    reader.seek(SeekFrom::Start(0))?;
    let mut walk = Walk {
        reader,
        budget,
        used: 0,
        blocks: 0,
        cancel,
        partial: false,
    };
    let count = match container {
        Container::Gif => count_gif(&mut walk),
        Container::WebP => count_webp(&mut walk),
        Container::Png => count_png(&mut walk),
        Container::Tiff => count_tiff(&mut walk),
        Container::Ico => count_ico(&mut walk),
    };
    let partial = walk.partial || count > provisional::MAX_COUNTED_FRAMES;
    Ok((count.min(provisional::MAX_COUNTED_FRAMES), partial))
}

/// Counts entries in an ICO/CUR directory without opening any image payload.
fn count_ico<R: BufRead + Seek, C: Cancel>(walk: &mut Walk<'_, R, C>) -> u32 {
    let Ok(header) = walk.read::<6>() else {
        return 0;
    };
    let reserved = u16::from_le_bytes([header[0], header[1]]);
    let kind = u16::from_le_bytes([header[2], header[3]]);
    let count = u16::from_le_bytes([header[4], header[5]]);
    if reserved == 0 && matches!(kind, 1 | 2) {
        u32::from(count)
    } else {
        0
    }
}

/// Counts classic TIFF and `BigTIFF` directories, stopping on a bad offset,
/// repeated directory, cancellation, or the existing scan budgets.
fn count_tiff<R: BufRead + Seek, C: Cancel>(walk: &mut Walk<'_, R, C>) -> u32 {
    let Ok(header) = walk.read::<8>() else {
        return 0;
    };
    let little = match &header[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return 0,
    };
    let u16_at = |bytes: [u8; 2]| {
        if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    let u32_at = |bytes: [u8; 4]| {
        if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    let u64_at = |bytes: [u8; 8]| {
        if little {
            u64::from_le_bytes(bytes)
        } else {
            u64::from_be_bytes(bytes)
        }
    };
    let magic = u16_at([header[2], header[3]]);
    let (mut offset, count_width, entry_width, next_width) = match magic {
        42 => (
            u64::from(u32_at([header[4], header[5], header[6], header[7]])),
            2u64,
            12u64,
            4u64,
        ),
        43 => {
            let Ok(big_header) = walk.read::<8>() else {
                return 0;
            };
            if u16_at([big_header[0], big_header[1]]) != 8
                || u16_at([big_header[2], big_header[3]]) != 0
            {
                return 0;
            }
            (u64_at(big_header), 8, 20, 8)
        }
        _ => return 0,
    };
    let mut seen = BTreeSet::new();
    let mut pages = 0u32;
    while offset != 0 {
        if pages >= provisional::MAX_COUNTED_FRAMES || walk.cancel.is_cancelled() {
            walk.partial = true;
            break;
        }
        if !seen.insert(offset) {
            break;
        }
        if walk.reader.seek(SeekFrom::Start(offset)).is_err() {
            break;
        }
        let entries = if count_width == 2 {
            let Ok(bytes) = walk.read::<2>() else {
                break;
            };
            u64::from(u16_at(bytes))
        } else {
            let Ok(bytes) = walk.read::<8>() else {
                break;
            };
            u64_at(bytes)
        };
        let Some(table_size) = entries.checked_mul(entry_width) else {
            break;
        };
        if walk.skip(table_size).is_err() {
            break;
        }
        offset = if next_width == 4 {
            let Ok(bytes) = walk.read::<4>() else {
                break;
            };
            u64::from(u32_at(bytes))
        } else {
            let Ok(bytes) = walk.read::<8>() else {
                break;
            };
            u64_at(bytes)
        };
        pages = pages.saturating_add(1);
    }
    pages
}

/// Walks the GIF block stream and counts image descriptors.
fn count_gif<R: BufRead + Seek, C: Cancel>(walk: &mut Walk<'_, R, C>) -> u32 {
    let mut count = 0u32;
    let _ = gif_blocks(walk, &mut count);
    count
}

fn gif_blocks<R: BufRead + Seek, C: Cancel>(
    walk: &mut Walk<'_, R, C>,
    count: &mut u32,
) -> std::result::Result<(), Stop> {
    let header: [u8; 6] = walk.read()?;
    if &header[..3] != b"GIF" {
        return Err(Stop::End);
    }
    let screen: [u8; 7] = walk.read()?;
    skip_gif_color_table(walk, screen[4])?;
    loop {
        walk.poll()?;
        let [block] = walk.read::<1>()?;
        match block {
            // Image descriptor: position, size, flags, then the pixel data.
            0x2C => {
                if *count >= provisional::MAX_COUNTED_FRAMES {
                    walk.partial = true;
                    return Err(Stop::End);
                }
                let descriptor: [u8; 9] = walk.read()?;
                skip_gif_color_table(walk, descriptor[8])?;
                // The minimum code size byte precedes the data sub-blocks.
                let _ = walk.read::<1>()?;
                skip_gif_sub_blocks(walk)?;
                *count = count.saturating_add(1);
            }
            // Extension: a label byte, then data sub-blocks.
            0x21 => {
                let _ = walk.read::<1>()?;
                skip_gif_sub_blocks(walk)?;
            }
            // Trailer, or a byte no block starts with.
            _ => return Err(Stop::End),
        }
    }
}

/// Skips a color table when the packed field says one follows.
fn skip_gif_color_table<R: BufRead + Seek, C: Cancel>(
    walk: &mut Walk<'_, R, C>,
    packed: u8,
) -> std::result::Result<(), Stop> {
    if packed & 0x80 == 0 {
        return Ok(());
    }
    let entries = 1u64 << ((u64::from(packed) & 0x07) + 1);
    walk.skip(entries * 3)
}

/// Skips a chain of data sub-blocks, each a length byte and its bytes.
fn skip_gif_sub_blocks<R: BufRead + Seek, C: Cancel>(
    walk: &mut Walk<'_, R, C>,
) -> std::result::Result<(), Stop> {
    loop {
        let [length] = walk.read::<1>()?;
        if length == 0 {
            return Ok(());
        }
        walk.skip(u64::from(length))?;
        walk.poll()?;
    }
}

/// Walks the RIFF chunk stream and counts animation frame chunks.
fn count_webp<R: BufRead + Seek, C: Cancel>(walk: &mut Walk<'_, R, C>) -> u32 {
    let mut count = 0u32;
    let _ = webp_chunks(walk, &mut count);
    // A still WebP holds one image and no frame chunk.
    count.max(1)
}

fn webp_chunks<R: BufRead + Seek, C: Cancel>(
    walk: &mut Walk<'_, R, C>,
    count: &mut u32,
) -> std::result::Result<(), Stop> {
    let header: [u8; 12] = walk.read()?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WEBP" {
        return Err(Stop::End);
    }
    loop {
        walk.poll()?;
        let chunk: [u8; 8] = walk.read()?;
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        if &chunk[..4] == b"ANMF" {
            if *count >= provisional::MAX_COUNTED_FRAMES {
                walk.partial = true;
                return Err(Stop::End);
            }
            *count = count.saturating_add(1);
        }
        // Every chunk payload is padded to an even length.
        walk.skip(size + (size & 1))?;
    }
}

/// Walks the PNG chunk stream and counts frame control chunks.
fn count_png<R: BufRead + Seek, C: Cancel>(walk: &mut Walk<'_, R, C>) -> u32 {
    let mut count = 0u32;
    let _ = png_chunks(walk, &mut count);
    // A PNG without animation chunks holds one image.
    count.max(1)
}

fn png_chunks<R: BufRead + Seek, C: Cancel>(
    walk: &mut Walk<'_, R, C>,
    count: &mut u32,
) -> std::result::Result<(), Stop> {
    let signature: [u8; 8] = walk.read()?;
    if signature != [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Err(Stop::End);
    }
    loop {
        walk.poll()?;
        let header: [u8; 8] = walk.read()?;
        let size = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let kind = &header[4..8];
        if kind == b"fcTL" {
            if *count >= provisional::MAX_COUNTED_FRAMES {
                walk.partial = true;
                return Err(Stop::End);
            }
            *count = count.saturating_add(1);
        }
        if kind == b"IEND" {
            return Err(Stop::End);
        }
        // The payload is followed by a four byte checksum.
        walk.skip(size + 4)?;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cancel::NeverCancel;
    use std::io::Cursor;

    /// A GIF with a global color table and `frames` one-pixel image
    /// descriptors on a large logical screen.
    fn gif_with(frames: u32, screen: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GIF89a");
        bytes.extend_from_slice(&screen.to_le_bytes());
        bytes.extend_from_slice(&screen.to_le_bytes());
        // A global color table of two entries follows.
        bytes.extend_from_slice(&[0x80, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        for _ in 0..frames {
            bytes.push(0x2C);
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.push(0x00);
            // One pixel of index zero: a clear code, the index and an end code.
            bytes.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
        }
        bytes.push(0x3B);
        bytes
    }

    #[test]
    fn a_gif_reports_its_image_descriptors() {
        let bytes = gif_with(7, 64);
        let count = count_frames(Cursor::new(&bytes), Container::Gif, u64::MAX, &NeverCancel);
        assert_eq!(count.ok(), Some(7));
    }

    #[test]
    fn a_gif_with_no_frames_reports_none() {
        let bytes = gif_with(0, 16);
        let count = count_frames(Cursor::new(&bytes), Container::Gif, u64::MAX, &NeverCancel);
        assert_eq!(count.ok(), Some(0));
    }

    #[test]
    fn a_spent_budget_stops_the_walk() {
        let bytes = gif_with(100, 16);
        let count =
            count_frames(Cursor::new(&bytes), Container::Gif, 64, &NeverCancel).unwrap_or(u32::MAX);
        assert!(count < 100, "the budget did not stop the walk");
    }

    #[test]
    fn a_still_png_reports_one_frame() {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"IEND");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let count = count_frames(Cursor::new(&bytes), Container::Png, u64::MAX, &NeverCancel);
        assert_eq!(count.ok(), Some(1));
    }

    #[test]
    fn an_icon_reports_all_directory_entries() {
        let mut bytes = vec![0, 0, 1, 0];
        bytes.extend_from_slice(&3u16.to_le_bytes());
        let count = count_frames(Cursor::new(bytes), Container::Ico, u64::MAX, &NeverCancel);
        assert_eq!(count.ok(), Some(3));
    }

    #[test]
    fn tiff_page_scan_follows_ifd_offsets_and_stops_on_a_cycle() {
        let mut bytes = b"II".to_vec();
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&14u32.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        let count = count_frames(Cursor::new(bytes), Container::Tiff, u64::MAX, &NeverCancel);
        assert_eq!(count.ok(), Some(2));
    }
}
