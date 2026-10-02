//! Scheduling of the comparison that follows an edit.
//!
//! An edit does not compare anything. It marks the comparison stale and starts
//! a timer; the comparison runs on a worker once the timer expires, so a run of
//! keystrokes costs one comparison rather than one per character. A newer edit
//! while a comparison runs supersedes it: the older job is cancelled and its
//! result, if it still arrives, is refused.
//!
//! The alignment on screen is never cleared while a comparison runs, so the
//! panes stay readable and editable throughout.
//!
//! The quiet period and the generation counter are [`crate::schedule`]. What is
//! added here is the running job, which the shared policy does not hold.
//!
//! The type is generic over the job handle so the whole policy is testable
//! without spawning anything.

use crate::schedule::{Debounce, Generations};
use std::time::{Duration, Instant};

/// How long the last edit is waited out before a comparison starts.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// A running job the scheduler can stop.
pub trait Stoppable {
    /// Ask the job to stop.
    fn stop(&self);
}

impl<M: crate::worker::Terminal> Stoppable for crate::worker::Job<M> {
    fn stop(&self) {
        self.cancel();
    }
}

struct Run<J> {
    generation: u64,
    job: J,
}

/// The re-diff state of one comparison.
pub struct Rediff<J> {
    debounce_period: Duration,
    debounce: Debounce,
    generations: Generations,
    running: Option<Run<J>>,
}

impl<J: Stoppable> Default for Rediff<J> {
    fn default() -> Self {
        Self::new(DEBOUNCE)
    }
}

impl<J: Stoppable> Rediff<J> {
    /// A scheduler with the given quiet period.
    #[must_use]
    pub const fn new(debounce_period: Duration) -> Self {
        Self {
            debounce_period,
            debounce: Debounce::new(),
            generations: Generations::new(),
            running: None,
        }
    }

    /// Record that an edit made the comparison stale.
    ///
    /// Each call pushes the start time out, so a continuous run of keystrokes
    /// never starts a comparison in the middle of it.
    pub fn mark_stale(&mut self, now: Instant) {
        self.debounce.mark(now, self.debounce_period);
    }

    /// True when an edit is waiting for its comparison.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.debounce.is_pending()
    }

    /// True when a comparison is running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// How long until the comparison is due, or `None` when nothing is waiting.
    #[must_use]
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.debounce.remaining(now)
    }

    /// True when the quiet period has expired and a comparison should start.
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        self.debounce.is_due(now)
    }

    /// Hand a freshly spawned job to the scheduler.
    ///
    /// Any job already running is stopped and its generation is left behind, so
    /// a result from it is refused by [`Rediff::accepts`].
    pub fn start(&mut self, job: J) -> u64 {
        if let Some(previous) = self.running.take() {
            previous.job.stop();
        }
        let generation = self.generations.start();
        self.debounce.clear();
        self.running = Some(Run { generation, job });
        generation
    }

    /// The generation of the running job, if there is one.
    #[must_use]
    pub fn generation(&self) -> Option<u64> {
        self.running.as_ref().map(|run| run.generation)
    }

    /// True when a result tagged `generation` is the one being waited for.
    #[must_use]
    pub fn accepts(&self, generation: u64) -> bool {
        self.generation() == Some(generation)
    }

    /// The running job, for draining.
    pub fn job_mut(&mut self) -> Option<&mut J> {
        self.running.as_mut().map(|run| &mut run.job)
    }

    /// Forget the running job once it has reached a terminal state.
    pub fn finish(&mut self) {
        self.running = None;
    }

    /// Stop whatever is running and forget any pending edit.
    pub fn clear(&mut self) {
        if let Some(previous) = self.running.take() {
            previous.job.stop();
        }
        self.debounce.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{Rediff, Stoppable};
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    struct Fake(Rc<Cell<bool>>);

    impl Stoppable for Fake {
        fn stop(&self) {
            self.0.set(true);
        }
    }

    fn fake() -> (Fake, Rc<Cell<bool>>) {
        let flag = Rc::new(Cell::new(false));
        (Fake(Rc::clone(&flag)), flag)
    }

    #[test]
    fn an_edit_does_not_start_a_comparison_at_once() {
        let mut rediff: Rediff<Fake> = Rediff::new(Duration::from_millis(100));
        let now = Instant::now();
        rediff.mark_stale(now);
        assert!(rediff.is_stale());
        assert!(!rediff.is_due(now));
        assert!(rediff.is_due(now + Duration::from_millis(100)));
    }

    #[test]
    fn starting_clears_the_pending_edit() {
        let mut rediff: Rediff<Fake> = Rediff::new(Duration::from_millis(10));
        rediff.mark_stale(Instant::now());
        let (job, _) = fake();
        rediff.start(job);
        assert!(!rediff.is_stale());
        assert!(rediff.is_running());
    }

    #[test]
    fn a_newer_job_stops_the_older_one_and_refuses_its_result() {
        let mut rediff: Rediff<Fake> = Rediff::default();
        let (first, stopped) = fake();
        let old = rediff.start(first);
        let (second, _) = fake();
        let new = rediff.start(second);
        assert!(stopped.get());
        assert_ne!(old, new);
        assert!(!rediff.accepts(old));
        assert!(rediff.accepts(new));
    }

    #[test]
    fn a_result_arriving_after_the_job_is_forgotten_is_refused() {
        let mut rediff: Rediff<Fake> = Rediff::default();
        let (job, _) = fake();
        let generation = rediff.start(job);
        rediff.finish();
        assert!(!rediff.accepts(generation));
        assert!(!rediff.is_running());
    }

    #[test]
    fn clearing_stops_the_job_and_forgets_the_edit() {
        let mut rediff: Rediff<Fake> = Rediff::default();
        rediff.mark_stale(Instant::now());
        let (job, stopped) = fake();
        rediff.start(job);
        rediff.clear();
        assert!(stopped.get());
        assert!(!rediff.is_running());
        assert!(!rediff.is_stale());
    }

    #[test]
    fn generations_never_repeat() {
        let mut rediff: Rediff<Fake> = Rediff::default();
        let mut seen = Vec::new();
        for _ in 0..8 {
            let (job, _) = fake();
            seen.push(rediff.start(job));
        }
        for (index, generation) in seen.iter().enumerate() {
            assert!(!seen[..index].contains(generation));
        }
    }
}
