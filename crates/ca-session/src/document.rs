//! Finite input and serialization budgets for the owned settings documents.

use std::io::{self, Read, Write};
use std::path::Path;

/// Sessions and options use the same byte budget as an exported settings
/// package. Refusal preserves the original document rather than quarantining it.
pub const MAX_DOCUMENT_BYTES: u64 = crate::share::MAX_PACKAGE_BYTES;

fn too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the settings document is larger than {MAX_DOCUMENT_BYTES} bytes; it was left untouched"),
    )
}

/// Check the opened file, then bound the actual transfer too: a writer can
/// extend the file after metadata was read.
pub(crate) fn read(path: &Path) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_DOCUMENT_BYTES {
        return Err(too_large());
    }
    read_bounded(file, MAX_DOCUMENT_BYTES)
}

fn read_bounded(mut reader: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let remaining = limit - bytes.len() as u64;
        let wanted = usize::try_from(remaining.min(chunk.len() as u64)).unwrap_or(chunk.len());
        if wanted == 0 {
            // One byte distinguishes an exact-budget document from a larger
            // source without retaining an over-budget buffer.
            return match reader.read(&mut chunk[..1]) {
                Ok(0) => Ok(bytes),
                Ok(_) => Err(too_large()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => Err(error),
            };
        }
        match reader.read(&mut chunk[..wanted]) {
            Ok(0) => return Ok(bytes),
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Refuse serialization before appending an over-budget fragment. The
/// enclosing atomic writer never receives a successful partial document.
pub(crate) struct Writer<'a> {
    buffer: &'a mut Vec<u8>,
    limit: u64,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(buffer: &'a mut Vec<u8>) -> Self {
        Self {
            buffer,
            limit: MAX_DOCUMENT_BYTES,
        }
    }
}

impl Write for Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.buffer.len() as u64) {
            return Err(too_large());
        }
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A source without a length hint is checked at the boundary, including
    /// the exact-limit success case and the one-byte refusal case.
    #[test]
    fn a_growing_document_reads_only_one_byte_beyond_the_budget() {
        struct Metered {
            remaining: usize,
            read: usize,
        }
        impl Read for Metered {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let count = self.remaining.min(output.len());
                output[..count].fill(b' ');
                self.remaining -= count;
                self.read += count;
                Ok(count)
            }
        }
        let mut source = Metered {
            remaining: 1_000_000,
            read: 0,
        };
        assert!(read_bounded(&mut source, 20).is_err());
        assert_eq!(source.read, 21);
        assert_eq!(read_bounded(&b"1234567890"[..], 10).unwrap(), b"1234567890");
    }

    /// Serialization rejects a whole over-budget fragment without appending
    /// part of it or reporting success for a truncated JSON document.
    #[test]
    fn serialization_never_retains_an_over_budget_fragment() {
        let mut bytes = Vec::new();
        let mut writer = Writer {
            buffer: &mut bytes,
            limit: 10,
        };
        writer.write_all(b"1234567890").unwrap();
        assert!(writer.write_all(b"x").is_err());
        assert_eq!(bytes, b"1234567890");
    }
}
