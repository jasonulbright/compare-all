//! Bounded reads over untrusted bytes.
//!
//! Every read states the structure it belongs to, so a refusal names the field
//! that lied about its size. No function here can panic on any input.

use crate::error::{RecordError, Result};
use std::fmt::Write as _;

/// A forward reader that refuses every out of range read.
#[derive(Debug, Clone)]
pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    context: &'static str,
}

impl<'a> Cursor<'a> {
    pub(crate) const fn new(data: &'a [u8], context: &'static str) -> Self {
        Self {
            data,
            pos: 0,
            context,
        }
    }

    pub(crate) const fn position(&self) -> usize {
        self.pos
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub(crate) fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(self.truncate_error(pos, 0));
        }
        self.pos = pos;
        Ok(())
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or_else(|| {
            RecordError::malformed(self.context, u64::try_from(self.pos).unwrap_or(u64::MAX))
        })?;
        if end > self.data.len() {
            return Err(self.truncate_error(self.pos, len));
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn skip(&mut self, len: usize) -> Result<()> {
        self.take(len).map(|_| ())
    }

    pub(crate) fn u16_le(&mut self) -> Result<u16> {
        let raw = self.take(2)?;
        Ok(u16::from_le_bytes([raw[0], raw[1]]))
    }

    pub(crate) fn u32_le(&mut self) -> Result<u32> {
        let raw = self.take(4)?;
        Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    fn truncate_error(&self, offset: usize, needed: usize) -> RecordError {
        RecordError::truncated(
            self.context,
            u64::try_from(offset).unwrap_or(u64::MAX),
            u64::try_from(needed).unwrap_or(u64::MAX),
            u64::try_from(self.remaining()).unwrap_or(u64::MAX),
        )
    }
}

/// Read `len` bytes at `off`, or report the shortfall.
pub(crate) fn slice<'a>(
    data: &'a [u8],
    off: usize,
    len: usize,
    context: &'static str,
) -> Result<&'a [u8]> {
    let end = off
        .checked_add(len)
        .ok_or_else(|| RecordError::malformed(context, u64::try_from(off).unwrap_or(u64::MAX)))?;
    if end > data.len() {
        return Err(RecordError::truncated(
            context,
            u64::try_from(off).unwrap_or(u64::MAX),
            u64::try_from(len).unwrap_or(u64::MAX),
            u64::try_from(data.len().saturating_sub(off.min(data.len()))).unwrap_or(u64::MAX),
        ));
    }
    Ok(&data[off..end])
}

/// Decode a little endian UTF-16 run, stopping at the first null unit.
///
/// Unpaired surrogates become the replacement character; the function never
/// fails and never allocates more than `units.len()` characters.
pub(crate) fn utf16_le_string(units: &[u8]) -> (String, usize) {
    let mut out = String::new();
    let taken;
    let mut buffer = Vec::new();
    let mut index = 0usize;
    while index + 1 < units.len() {
        let unit = u16::from_le_bytes([units[index], units[index + 1]]);
        index += 2;
        if unit == 0 {
            taken = index;
            let decoded = String::from_utf16_lossy(&buffer);
            out.push_str(&decoded);
            return (out, taken);
        }
        buffer.push(unit);
    }
    taken = index;
    out.push_str(&String::from_utf16_lossy(&buffer));
    (out, taken)
}

/// Decode a big endian UTF-16 run with the same rules as [`utf16_le_string`].
pub(crate) fn utf16_be_string(units: &[u8]) -> String {
    let mut buffer = Vec::with_capacity(units.len() / 2);
    let mut index = 0usize;
    while index + 1 < units.len() {
        let unit = u16::from_be_bytes([units[index], units[index + 1]]);
        index += 2;
        if unit == 0 {
            break;
        }
        buffer.push(unit);
    }
    String::from_utf16_lossy(&buffer)
}

/// Format bytes as upper case hexadecimal pairs separated by spaces.
pub(crate) fn hex_display(bytes: &[u8], max: usize) -> String {
    let shown = bytes.len().min(max);
    let mut out = String::with_capacity(shown * 3);
    for (index, byte) in bytes.iter().take(shown).enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{byte:02X}");
    }
    if bytes.len() > shown {
        let _ = write!(out, " ... ({} bytes)", bytes.len());
    }
    out
}

/// Cast a `usize` to `u64` without loss on every supported target.
pub(crate) fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
