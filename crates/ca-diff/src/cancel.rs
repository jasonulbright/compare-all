//! Cooperative cancellation for long running comparisons.
//!
//! Every loop in this crate whose iteration count grows with the input size
//! polls a [`Cancel`] and abandons the work with [`DiffError::Cancelled`]
//! rather than running to completion. Polling is cheap enough to sit in an
//! inner loop only when it is amortised, so checks are placed once per outer
//! iteration of the loop that dominates the cost.
//!
//! [`DiffError::Cancelled`]: crate::DiffError::Cancelled

use std::sync::atomic::{AtomicBool, Ordering};

/// A flag a caller can raise to abandon a comparison in progress.
pub trait Cancel {
    /// Whether the comparison should stop.
    fn is_cancelled(&self) -> bool;
}

impl Cancel for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Relaxed)
    }
}

impl<T: Cancel + ?Sized> Cancel for &T {
    fn is_cancelled(&self) -> bool {
        (**self).is_cancelled()
    }
}

/// A flag that is never raised, for callers that do not cancel.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeverCancel;

impl Cancel for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Return [`DiffError::Cancelled`] when `cancel` has been raised.
///
/// [`DiffError::Cancelled`]: crate::DiffError::Cancelled
pub(crate) fn check(cancel: &dyn Cancel) -> Result<(), crate::DiffError> {
    if cancel.is_cancelled() {
        Err(crate::DiffError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn never_cancel_stays_clear() {
        assert!(!NeverCancel.is_cancelled());
        assert!(check(&NeverCancel).is_ok());
    }

    #[test]
    fn atomic_flag_is_observed() {
        let flag = AtomicBool::new(false);
        assert!(check(&flag).is_ok());
        flag.store(true, Ordering::Relaxed);
        assert!(flag.is_cancelled());
        assert!(check(&flag).is_err());
    }
}
