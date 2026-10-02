//! Cooperative cancellation for long comparisons.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A signal a long running comparison polls between row bands.
///
/// Implementations must be cheap to poll and safe to poll from several worker
/// threads at once.
pub trait Cancel: Sync {
    /// True once the caller wants the operation to stop. The operation then
    /// returns [`crate::Error::Cancelled`] and leaves no partial result.
    fn is_cancelled(&self) -> bool;
}

/// A signal that never fires. Use it when the caller cannot cancel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NeverCancel;

impl Cancel for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// A shared flag one thread sets and the workers poll.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag {
    flag: Arc<AtomicBool>,
}

impl CancelFlag {
    /// A flag that has not fired.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fires the flag. Every clone of it observes the change.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Clears the flag so the same handle can drive another comparison.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

impl Cancel for CancelFlag {
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

impl<T: Cancel + ?Sized> Cancel for &T {
    fn is_cancelled(&self) -> bool {
        (**self).is_cancelled()
    }
}

impl<T: Cancel + Send + ?Sized> Cancel for Arc<T> {
    fn is_cancelled(&self) -> bool {
        (**self).is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_is_shared_by_its_clones() {
        let flag = CancelFlag::new();
        let clone = flag.clone();
        assert!(!clone.is_cancelled());
        flag.cancel();
        assert!(clone.is_cancelled());
        clone.reset();
        assert!(!flag.is_cancelled());
    }

    #[test]
    fn the_never_signal_stays_clear() {
        assert!(!NeverCancel.is_cancelled());
    }
}
