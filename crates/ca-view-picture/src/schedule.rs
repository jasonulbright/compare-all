//! How much of the pipeline a scheduled run covers.
//!
//! The debounce and supersede policy itself is [`ca_ui::schedule`]; this module
//! names what a run of this view does.

use std::time::Duration;

/// How long a change waits before it becomes a run.
pub const DEBOUNCE: Duration = Duration::from_millis(120);

/// How much of the pipeline a request asks for.
///
/// The order matters: a merge of two requests takes the wider of the two, so a
/// reload asked for during a compare still re-reads the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Compare the images already decoded, under the current settings.
    Compare,
    /// Read both files again, decode them, then compare.
    Load,
}

/// The queue of at most one waiting run of this view's pipeline.
pub type Scheduler = ca_ui::schedule::Scheduler<Stage>;
