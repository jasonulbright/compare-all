//! Cooperative cancellation shared between the scanner and the comparison
//! workers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cloneable cancellation flag.
///
/// Every clone observes the same flag, so a single [`Cancel::cancel`] stops
/// every worker holding a clone. Cancellation is cooperative: workers observe
/// it between units of work, never mid-write.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// Create a flag that is not yet cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The flag itself, so work driven through another crate's cancellation
    /// type stops on the same call.
    #[must_use]
    pub fn as_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// True once [`Cancel::cancel`] has been called on this flag or any clone.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::Cancel;

    #[test]
    fn clones_share_one_flag() {
        let a = Cancel::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
    }
}
