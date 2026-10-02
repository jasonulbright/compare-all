//! Cancellation flag for long report loops.

use std::sync::atomic::{AtomicBool, Ordering};

/// A flag a report loop polls so a caller can stop a long render.
///
/// Every loop whose length grows with the input polls the flag. A report that
/// stops leaves a partial document on the writer; the caller discards it.
pub trait Cancel {
    /// True once the caller asked for the work to stop.
    fn is_cancelled(&self) -> bool;
}

/// A flag that never fires.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeverCancel;

impl Cancel for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// A flag backed by an atomic another thread sets.
#[derive(Debug, Clone, Copy)]
pub struct AtomicCancel<'a> {
    flag: &'a AtomicBool,
}

impl<'a> AtomicCancel<'a> {
    /// Read the flag through the supplied atomic.
    #[must_use]
    pub const fn new(flag: &'a AtomicBool) -> Self {
        Self { flag }
    }
}

impl Cancel for AtomicCancel<'_> {
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

impl<T: Cancel + ?Sized> Cancel for &T {
    fn is_cancelled(&self) -> bool {
        (**self).is_cancelled()
    }
}

/// Rows written between two polls of the cancellation flag.
///
/// A loop polls on this stride, so the bound on rows written after a caller
/// raises the flag is this value.
pub(crate) const CANCEL_STRIDE: u64 = 256;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{AtomicCancel, Cancel, NeverCancel};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn never_cancel_stays_clear() {
        assert!(!NeverCancel.is_cancelled());
    }

    #[test]
    fn an_atomic_flag_is_read_through() {
        let flag = AtomicBool::new(false);
        let cancel = AtomicCancel::new(&flag);
        assert!(!cancel.is_cancelled());
        flag.store(true, Ordering::Relaxed);
        assert!(cancel.is_cancelled());
    }
}
