//! Shared cancellation flag for long running operations.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A flag shared between a worker and whoever asked it to stop.
///
/// Cloning shares the flag rather than copying its state, so a clone handed to
/// a worker thread observes a cancel raised by the caller.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
}

impl Cancel {
    /// A flag that has not been raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Share an existing flag, so a caller holding a flag of its own stops
    /// this work with the same call that stops its own.
    #[must_use]
    pub const fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self { flag }
    }

    /// The flag itself, for a caller that shares it with other work.
    #[must_use]
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// Raise the flag. Every clone observes the change.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// True once [`Cancel::cancel`] has been called on any clone.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Fail with [`crate::VfsError::Cancelled`] when the flag is raised.
    ///
    /// # Errors
    /// Returns [`crate::VfsError::Cancelled`] if the flag is set.
    pub fn check(&self) -> Result<(), crate::VfsError> {
        if self.is_cancelled() {
            return Err(crate::VfsError::Cancelled);
        }
        Ok(())
    }
}
