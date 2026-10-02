//! Deciding when a change becomes a background run.
//!
//! Two rules hold wherever a view turns user input into worker requests. A
//! newer request replaces an older one that has not started, so dragging a
//! slider or typing a line leaves one run to make and not one run per value.
//! And a request that does start supersedes whatever is running, so the result
//! a view shows always answers the state as it stands.
//!
//! Time is a parameter rather than a call to the clock, so the whole policy is
//! testable without waiting.

use std::time::{Duration, Instant};

/// A quiet period that restarts on every change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Debounce {
    due_at: Option<Instant>,
}

impl Debounce {
    /// A debounce with nothing waiting.
    #[must_use]
    pub const fn new() -> Self {
        Self { due_at: None }
    }

    /// Record a change, pushing the start time out by `delay`.
    pub fn mark(&mut self, now: Instant, delay: Duration) {
        self.due_at = Some(now + delay);
    }

    /// True when a change is waiting for its run.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.due_at.is_some()
    }

    /// True when the quiet period has expired.
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        self.due_at.is_some_and(|due| now >= due)
    }

    /// How long until the run is due, or `None` when nothing is waiting.
    #[must_use]
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.due_at.map(|due| due.saturating_duration_since(now))
    }

    /// Forget the waiting change.
    pub fn clear(&mut self) {
        self.due_at = None;
    }
}

/// Identifiers for runs, so a superseded run's result can be refused.
///
/// Zero names no run, which is what a cleared counter reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Generations {
    current: u64,
    next: u64,
}

impl Generations {
    /// A counter that has started nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: 0,
            next: 0,
        }
    }

    /// Take the next identifier and make it the current one.
    pub fn start(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        if self.next == 0 {
            self.next = 1;
        }
        self.current = self.next;
        self.current
    }

    /// The identifier of the run started last, or zero when none has started.
    #[must_use]
    pub const fn current(&self) -> u64 {
        self.current
    }

    /// True when `id` names the run started last, so its result is still
    /// wanted.
    #[must_use]
    pub const fn is_current(&self, id: u64) -> bool {
        id == self.current && id != 0
    }

    /// Stop accepting the running run's result.
    pub fn clear(&mut self) {
        self.next = self.next.wrapping_add(1);
        self.current = 0;
    }
}

/// The queue of at most one waiting run, carrying what the run should do.
///
/// Two waiting requests merge into the wider of the two, which is why the
/// payload is ordered: a reload asked for during a compare still re-reads.
#[derive(Debug)]
pub struct Scheduler<P> {
    debounce: Debounce,
    pending: Option<P>,
    generations: Generations,
}

impl<P> Default for Scheduler<P> {
    fn default() -> Self {
        Self {
            debounce: Debounce::new(),
            pending: None,
            generations: Generations::new(),
        }
    }
}

impl<P: Copy + Ord> Scheduler<P> {
    /// An empty scheduler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for a run, replacing whatever was waiting.
    ///
    /// The delay restarts on every request, so a continuous drag produces one
    /// run once the drag settles rather than one run per frame.
    pub fn request(&mut self, payload: P, now: Instant, delay: Duration) {
        self.pending = Some(match self.pending {
            Some(waiting) => waiting.max(payload),
            None => payload,
        });
        self.debounce.mark(now, delay);
    }

    /// Ask for a run that starts at the next poll.
    pub fn request_now(&mut self, payload: P, now: Instant) {
        self.request(payload, now, Duration::ZERO);
    }

    /// Take the waiting run once its delay has passed.
    ///
    /// The identifier returned names the run. A result carrying an older
    /// identifier belongs to a run that has been superseded and is dropped.
    pub fn take_due(&mut self, now: Instant) -> Option<(u64, P)> {
        let payload = self.pending?;
        if !self.debounce.is_due(now) {
            return None;
        }
        self.pending = None;
        self.debounce.clear();
        Some((self.generations.start(), payload))
    }

    /// True when a run is waiting to start.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// What the waiting run would do.
    #[must_use]
    pub const fn pending_payload(&self) -> Option<P> {
        self.pending
    }

    /// The identifier of the run started last.
    #[must_use]
    pub const fn current(&self) -> u64 {
        self.generations.current()
    }

    /// True when `id` names the run started last.
    #[must_use]
    pub const fn is_current(&self, id: u64) -> bool {
        self.generations.is_current(id)
    }

    /// Forget the waiting run and stop accepting the running one's result.
    pub fn clear(&mut self) {
        self.pending = None;
        self.debounce.clear();
        self.generations.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{Debounce, Generations, Scheduler};
    use std::time::{Duration, Instant};

    const DEBOUNCE: Duration = Duration::from_millis(120);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Stage {
        Compare,
        Load,
    }

    #[test]
    fn a_debounce_waits_out_its_delay() {
        let mut debounce = Debounce::new();
        let start = Instant::now();
        debounce.mark(start, DEBOUNCE);
        assert!(debounce.is_pending());
        assert!(!debounce.is_due(start));
        assert!(debounce.is_due(start + DEBOUNCE));
    }

    #[test]
    fn a_further_change_pushes_the_start_out() {
        let mut debounce = Debounce::new();
        let start = Instant::now();
        debounce.mark(start, Duration::from_millis(100));
        debounce.mark(
            start + Duration::from_millis(80),
            Duration::from_millis(100),
        );
        assert!(!debounce.is_due(start + Duration::from_millis(120)));
        assert!(debounce.is_due(start + Duration::from_millis(180)));
    }

    #[test]
    fn the_remaining_time_falls_to_zero() {
        let mut debounce = Debounce::new();
        let now = Instant::now();
        assert_eq!(debounce.remaining(now), None);
        debounce.mark(now, Duration::from_millis(100));
        assert_eq!(debounce.remaining(now), Some(Duration::from_millis(100)));
        assert_eq!(
            debounce.remaining(now + Duration::from_millis(500)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn no_run_is_current_before_one_starts() {
        let generations = Generations::new();
        assert!(!generations.is_current(0));
        assert_eq!(generations.current(), 0);
    }

    #[test]
    fn generations_never_repeat_and_supersede_one_another() {
        let mut generations = Generations::new();
        let mut seen = Vec::new();
        for _ in 0..8 {
            let id = generations.start();
            assert!(!seen.contains(&id));
            assert!(generations.is_current(id));
            seen.push(id);
        }
        for id in &seen[..seen.len() - 1] {
            assert!(!generations.is_current(*id));
        }
    }

    #[test]
    fn a_request_waits_out_its_delay() {
        let mut scheduler = Scheduler::new();
        let start = Instant::now();
        scheduler.request(Stage::Compare, start, DEBOUNCE);
        assert!(scheduler.take_due(start).is_none());
        assert!(scheduler.take_due(start + DEBOUNCE / 2).is_none());
        assert_eq!(
            scheduler.take_due(start + DEBOUNCE),
            Some((1, Stage::Compare))
        );
        assert!(!scheduler.has_pending());
    }

    #[test]
    fn a_continuous_drag_produces_one_run() {
        let mut scheduler = Scheduler::new();
        let mut now = Instant::now();
        for _ in 0..20 {
            scheduler.request(Stage::Compare, now, DEBOUNCE);
            now += Duration::from_millis(16);
            assert!(scheduler.take_due(now).is_none(), "a run started mid drag");
        }
        assert_eq!(
            scheduler.take_due(now + DEBOUNCE),
            Some((1, Stage::Compare))
        );
        assert!(scheduler.take_due(now + DEBOUNCE * 4).is_none());
    }

    #[test]
    fn a_wider_request_absorbs_a_narrower_one() {
        let mut scheduler = Scheduler::new();
        let now = Instant::now();
        scheduler.request(Stage::Compare, now, Duration::ZERO);
        scheduler.request(Stage::Load, now, Duration::ZERO);
        assert_eq!(scheduler.pending_payload(), Some(Stage::Load));
        scheduler.request(Stage::Compare, now, Duration::ZERO);
        assert_eq!(scheduler.take_due(now), Some((1, Stage::Load)));
    }

    #[test]
    fn a_newer_run_supersedes_the_one_before_it() {
        let mut scheduler = Scheduler::new();
        let now = Instant::now();
        scheduler.request_now(Stage::Compare, now);
        let (first, _) = scheduler.take_due(now).unwrap_or((0, Stage::Compare));
        assert!(scheduler.is_current(first));
        scheduler.request_now(Stage::Compare, now);
        let (second, _) = scheduler.take_due(now).unwrap_or((0, Stage::Compare));
        assert_ne!(first, second);
        assert!(!scheduler.is_current(first), "a stale result was accepted");
        assert!(scheduler.is_current(second));
    }

    #[test]
    fn clearing_rejects_the_running_result_and_the_waiting_one() {
        let mut scheduler = Scheduler::new();
        let now = Instant::now();
        scheduler.request_now(Stage::Load, now);
        let (id, _) = scheduler.take_due(now).unwrap_or((0, Stage::Load));
        scheduler.request(Stage::Compare, now, DEBOUNCE);
        scheduler.clear();
        assert!(!scheduler.is_current(id));
        assert!(!scheduler.has_pending());
        assert!(scheduler.take_due(now + DEBOUNCE * 10).is_none());
    }

    #[test]
    fn nothing_is_current_before_a_run_starts() {
        let scheduler: Scheduler<Stage> = Scheduler::new();
        assert!(!scheduler.is_current(0));
        assert_eq!(scheduler.current(), 0);
    }
}
