//! A small job system: one background thread per job, a typed message stream
//! back to the caller, and a cancellation flag the job body polls.
//!
//! Nothing here depends on a user interface, so the whole module is testable
//! headless. The only tie to a frame loop is [`Job::spawn_notifying`], which
//! takes a callback invoked after every message so the caller can ask for a
//! repaint.
//!
//! Two invariants hold for every job and are enforced here rather than in the
//! job bodies:
//!
//! - the queue is bounded, so a producer that outruns its reader is made to
//!   wait instead of growing the queue without limit;
//! - exactly one terminal message reaches the reader. A body that returns
//!   without sending one, and a body that unwinds, both produce a synthetic
//!   terminal message, so a view can never observe a job that neither finishes
//!   nor reports why.
//!
//! A job is finished for its reader in the drain that takes its terminal
//! message, not one wake-up later when the worker thread lets go of the
//! channel. A caller that needs the body itself to have returned asks
//! [`Job::has_returned`] or blocks in [`Job::wait_returned`].

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError,
};
use std::sync::Arc;
use std::time::Duration;

/// How many messages may wait in a job's queue before its body is made to wait.
const QUEUE_CAPACITY: usize = 256;

/// How long a full queue is left alone before the flag is checked again.
const BACKPRESSURE_PAUSE: Duration = Duration::from_millis(1);

/// A cancellation flag shared with a running job.
///
/// The engines take three unrelated flag types, so one flag object carries a
/// view for each of them; every view observes the same underlying bit.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    fs: ca_fs::Cancel,
}

impl Cancel {
    /// A flag that is not yet raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise the flag. Idempotent, and safe to call after the job has ended.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.fs.cancel();
    }

    /// True once this flag or any clone of it has been raised.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst) || self.fs.is_cancelled()
    }

    /// The same flag in the form the folder engine takes.
    #[must_use]
    pub fn as_fs(&self) -> &ca_fs::Cancel {
        &self.fs
    }
}

impl ca_diff::Cancel for Cancel {
    fn is_cancelled(&self) -> bool {
        Cancel::is_cancelled(self)
    }
}

/// What every job's message type must be able to say, so the worker layer can
/// close a stream the body left open.
pub trait Terminal: Send + 'static {
    /// True when this message ends the stream: nothing the reader needs can
    /// follow it.
    fn is_terminal(&self) -> bool;

    /// The message standing for a run that stopped without reaching a result.
    fn cancelled() -> Self;

    /// The message standing for a body that unwound, carrying what it said.
    fn panicked(detail: String) -> Self;

    /// Adjust a queued message when the reader has since cancelled the job.
    /// Side-effecting jobs retain their completion reports by default.
    #[must_use]
    fn after_cancel(self) -> Self
    where
        Self: Sized,
    {
        self
    }
}

/// The sending half handed to a job body.
pub struct Emitter<M> {
    sender: SyncSender<M>,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
    cancel: Cancel,
    /// Set once a terminal message has been accepted by the queue, which is
    /// what stops the worker wrapper adding a synthetic one.
    terminated: AtomicBool,
}

impl<M: Terminal> Emitter<M> {
    /// Post one message, waiting while the queue is full.
    ///
    /// Returns false once the receiving [`Job`] has been dropped or the flag has
    /// been raised, which is the job body's cue to stop.
    pub fn send(&self, message: M) -> bool {
        let terminal = message.is_terminal();
        let mut pending = message;
        loop {
            match self.sender.try_send(pending) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return false,
                Err(TrySendError::Full(returned)) => {
                    // A reader that has stopped draining must not be able to
                    // strand this thread, so the flag outranks the backlog.
                    if self.cancel.is_cancelled() && !terminal {
                        return false;
                    }
                    pending = returned;
                    std::thread::sleep(BACKPRESSURE_PAUSE);
                }
            }
        }
        if terminal {
            self.terminated.store(true, Ordering::SeqCst);
        }
        if let Some(notify) = &self.notify {
            notify();
        }
        true
    }
}

/// A running or finished background job.
///
/// Dropping the handle raises the cancellation flag, so a view that closes
/// never leaves work running against state nobody reads.
pub struct Job<M> {
    receiver: Receiver<M>,
    cancel: Cancel,
    /// Set by the drain that takes the terminal message, or by the disconnect.
    finished: bool,
    /// Stored by the worker before it drops the sender, so a reader that has
    /// seen the disconnect also sees this flag raised.
    returned: Arc<AtomicBool>,
}

impl<M: Terminal> Job<M> {
    /// Run `body` on a new thread.
    pub fn spawn<F>(body: F) -> Self
    where
        F: FnOnce(&Emitter<M>, &Cancel) + Send + 'static,
    {
        Self::spawn_inner(body, None)
    }

    /// Run `body` on a new thread, calling `notify` after each message reaches
    /// the queue.
    pub fn spawn_notifying<F>(body: F, notify: Arc<dyn Fn() + Send + Sync>) -> Self
    where
        F: FnOnce(&Emitter<M>, &Cancel) + Send + 'static,
    {
        Self::spawn_inner(body, Some(notify))
    }

    fn spawn_inner<F>(body: F, notify: Option<Arc<dyn Fn() + Send + Sync>>) -> Self
    where
        F: FnOnce(&Emitter<M>, &Cancel) + Send + 'static,
    {
        let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
        let cancel = Cancel::new();
        let worker_cancel = cancel.clone();
        let finish_notify = notify.clone();
        let returned = Arc::new(AtomicBool::new(false));
        let worker_returned = Arc::clone(&returned);
        std::thread::spawn(move || {
            let emitter = Emitter {
                sender,
                notify,
                cancel: worker_cancel.clone(),
                terminated: AtomicBool::new(false),
            };
            // An unwinding body must still close the stream, otherwise the
            // reader waits on a result that can no longer arrive. Release
            // builds abort instead of unwinding, so this path only carries the
            // panic message where one exists to carry.
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                body(&emitter, &worker_cancel);
            }));
            worker_returned.store(true, Ordering::SeqCst);
            if !emitter.terminated.load(Ordering::SeqCst) {
                let closing = match outcome {
                    Ok(()) => M::cancelled(),
                    Err(payload) => M::panicked(panic_detail(payload.as_ref())),
                };
                emitter.send(closing);
            }
            drop(emitter);
            // The disconnect is silent, so a frame loop that waits for the
            // body to return needs one more wake-up to observe it.
            if let Some(notify) = finish_notify {
                notify();
            }
        });
        Self {
            receiver,
            cancel,
            finished: false,
            returned,
        }
    }

    /// Hand one received message to the reader, finishing the job when it is
    /// terminal.
    fn accept(&mut self, message: M, out: &mut Vec<M>) {
        if message.is_terminal() {
            self.finished = true;
        }
        out.push(if self.cancel.is_cancelled() {
            message.after_cancel()
        } else {
            message
        });
    }

    /// Take every message that has arrived since the last call.
    ///
    /// Never blocks, so it is safe on the frame thread. Messages queued behind
    /// the terminal one are taken in the same call, in the order sent.
    pub fn drain(&mut self) -> Vec<M> {
        let mut out = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(message) => self.accept(message, &mut out),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        out
    }

    /// Take every message until the terminal one, or until `limit` passes.
    ///
    /// Blocks the calling thread, so it is for a caller that must not go on
    /// before the job lands, such as the exit path. A frame uses
    /// [`Job::drain`]. Work a body does after its terminal message may still
    /// be running on return; [`Job::wait_returned`] waits for that too.
    pub fn wait(&mut self, limit: Duration) -> Vec<M> {
        let deadline = std::time::Instant::now() + limit;
        let mut out = Vec::new();
        while !self.finished {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match self.receiver.recv_timeout(left) {
                Ok(message) => self.accept(message, &mut out),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => self.finished = true,
            }
        }
        out.extend(self.drain());
        out
    }

    /// Take every message until the body has returned and every message has
    /// been taken, or until `limit` passes.
    ///
    /// Blocks the calling thread. [`Job::has_returned`] tells the two
    /// outcomes apart.
    pub fn wait_returned(&mut self, limit: Duration) -> Vec<M> {
        let deadline = std::time::Instant::now() + limit;
        let mut out = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match self.receiver.recv_timeout(left) {
                Ok(message) => self.accept(message, &mut out),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        out
    }

    /// True once the reader holds everything the job will say: a terminal
    /// message has been drained, or the body has returned and every message
    /// has been drained.
    ///
    /// It does not say that the worker thread has ended; see
    /// [`Job::has_returned`].
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// True once the body has returned: every side effect it makes has
    /// happened and every value it captured has been dropped. Messages, the
    /// synthetic terminal one included, may still be queued.
    #[must_use]
    pub fn has_returned(&self) -> bool {
        self.returned.load(Ordering::SeqCst)
    }

    /// Ask the job to stop.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// True once cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The job's flag, for a caller that wants to share it further.
    #[must_use]
    pub fn cancel_handle(&self) -> Cancel {
        self.cancel.clone()
    }
}

impl<M> Drop for Job<M> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The text of a panic payload, for the two shapes the standard library uses.
fn panic_detail(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "background work stopped unexpectedly".to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{Cancel, Emitter, Job, Terminal, QUEUE_CAPACITY};
    use crate::testing::{job_held_after, wait_until as wait_for};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    #[derive(Debug, PartialEq, Eq)]
    enum Note {
        Value(u32),
        Done,
        Cancelled,
        Panicked(String),
    }

    impl Terminal for Note {
        fn is_terminal(&self) -> bool {
            matches!(self, Note::Done | Note::Cancelled | Note::Panicked(_))
        }

        fn cancelled() -> Self {
            Note::Cancelled
        }

        fn panicked(detail: String) -> Self {
            Note::Panicked(detail)
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    /// Drain until the terminal message has been taken.
    fn collect(job: &mut Job<Note>) -> Vec<Note> {
        let mut seen = Vec::new();
        assert!(wait_until(|| {
            seen.extend(job.drain());
            job.is_finished()
        }));
        seen
    }

    #[test]
    fn messages_arrive_in_order() {
        let mut job: Job<Note> = Job::spawn(|emitter: &Emitter<Note>, _| {
            for value in 0..5 {
                emitter.send(Note::Value(value));
            }
            emitter.send(Note::Done);
        });
        let seen = collect(&mut job);
        assert_eq!(
            seen,
            vec![
                Note::Value(0),
                Note::Value(1),
                Note::Value(2),
                Note::Value(3),
                Note::Value(4),
                Note::Done
            ]
        );
    }

    #[test]
    fn waiting_takes_every_message_up_to_the_end_and_stops_at_the_limit() {
        let mut job: Job<Note> = Job::spawn(|emitter: &Emitter<Note>, _| {
            emitter.send(Note::Value(1));
            emitter.send(Note::Done);
        });
        assert_eq!(
            job.wait(Duration::from_secs(10)),
            vec![Note::Value(1), Note::Done]
        );
        assert!(job.is_finished());

        let mut running: Job<Note> = Job::spawn(|_: &Emitter<Note>, cancel| {
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        assert!(running.wait(Duration::from_millis(20)).is_empty());
        assert!(!running.is_finished());
        running.cancel();
        assert_eq!(running.wait(Duration::from_secs(10)), vec![Note::Cancelled]);
        assert!(running.is_finished());
    }

    #[test]
    fn a_cancelled_job_stops_early() {
        let mut job: Job<Note> = Job::spawn(|emitter: &Emitter<Note>, cancel| {
            let mut value = 0;
            while !cancel.is_cancelled() {
                if !emitter.send(Note::Value(value)) {
                    return;
                }
                value += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        assert!(wait_until(|| !job.drain().is_empty()));
        job.cancel();
        assert!(job.is_cancelled());
        assert!(wait_until(|| {
            job.drain();
            job.is_finished()
        }));
    }

    #[test]
    fn a_body_that_returns_without_a_result_still_closes_the_stream() {
        let mut job: Job<Note> = Job::spawn(|emitter: &Emitter<Note>, _| {
            emitter.send(Note::Value(1));
        });
        let seen = collect(&mut job);
        assert_eq!(seen.last(), Some(&Note::Cancelled));
        assert_eq!(seen.iter().filter(|note| note.is_terminal()).count(), 1);
    }

    #[test]
    fn a_body_that_unwinds_reports_what_it_said() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut job: Job<Note> = Job::spawn(|_: &Emitter<Note>, _| {
            #[allow(clippy::panic)]
            {
                panic!("the engine gave up");
            }
        });
        let seen = collect(&mut job);
        std::panic::set_hook(previous);
        assert_eq!(
            seen.last(),
            Some(&Note::Panicked("the engine gave up".to_string()))
        );
    }

    #[test]
    fn a_body_that_reports_a_result_gets_no_second_terminal_message() {
        let mut job: Job<Note> = Job::spawn(|emitter: &Emitter<Note>, _| {
            emitter.send(Note::Done);
        });
        let seen = collect(&mut job);
        assert_eq!(seen, vec![Note::Done]);
    }

    #[test]
    fn the_queue_stays_bounded_while_the_reader_is_asleep() {
        let produced = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&produced);
        let mut job: Job<Note> = Job::spawn(move |emitter: &Emitter<Note>, _| {
            for value in 0..100_000 {
                if !emitter.send(Note::Value(value)) {
                    return;
                }
                counter.fetch_add(1, Ordering::SeqCst);
            }
            emitter.send(Note::Done);
        });
        // Nothing is drained for a while: a bounded queue holds the producer at
        // its capacity rather than letting it run to the end.
        std::thread::sleep(Duration::from_millis(150));
        let ahead = produced.load(Ordering::SeqCst);
        assert!(
            ahead <= QUEUE_CAPACITY + 2,
            "the producer ran {ahead} ahead of a reader that took nothing"
        );
        let seen = collect(&mut job);
        assert_eq!(seen.last(), Some(&Note::Done));
    }

    #[test]
    fn dropping_the_handle_stops_the_body() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let observer = Arc::clone(&stopped);
        let job: Job<Note> = Job::spawn(move |emitter: &Emitter<Note>, _| {
            let mut value = 0;
            while emitter.send(Note::Value(value)) {
                value += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            observer.store(1, Ordering::SeqCst);
        });
        drop(job);
        assert!(wait_until(|| stopped.load(Ordering::SeqCst) == 1));
    }

    #[test]
    fn notify_fires_for_every_message_and_for_completion() {
        let count = Arc::new(AtomicUsize::new(0));
        let observer = Arc::clone(&count);
        let mut job: Job<Note> = Job::spawn_notifying(
            |emitter: &Emitter<Note>, _| {
                emitter.send(Note::Value(1));
                emitter.send(Note::Done);
            },
            Arc::new(move || {
                observer.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert!(wait_until(|| {
            job.drain();
            job.is_finished() && count.load(Ordering::SeqCst) >= 3
        }));
    }

    #[test]
    fn one_flag_is_visible_through_every_engine_view() {
        let cancel = Cancel::new();
        let fs = cancel.as_fs().clone();
        assert!(!fs.is_cancelled());
        cancel.cancel();
        assert!(fs.is_cancelled());
        assert!(ca_diff::Cancel::is_cancelled(&cancel));
    }

    const LIMIT: Duration = Duration::from_secs(30);

    #[test]
    fn a_drained_terminal_message_finishes_the_job_while_the_body_still_runs() {
        let (mut job, held) = job_held_after(vec![Note::Value(1), Note::Done]);
        let seen = job.drain();
        let finished = job.is_finished();
        drop(held);
        assert_eq!(seen, vec![Note::Value(1), Note::Done]);
        assert!(finished, "the drain that takes Done must finish the job");
    }

    #[test]
    fn a_finished_job_tells_whether_its_body_has_returned() {
        let (mut job, held) = job_held_after(vec![Note::Done]);
        job.drain();
        let finished = job.is_finished();
        let returned = job.has_returned();
        drop(held);
        assert!(finished);
        assert!(!returned, "the body was still held");
        assert!(wait_for(LIMIT, || job.has_returned()));
    }

    #[test]
    fn messages_queued_behind_the_terminal_one_arrive_in_order_in_the_same_drain() {
        let (mut job, held) = job_held_after(vec![
            Note::Value(1),
            Note::Done,
            Note::Value(2),
            Note::Value(3),
        ]);
        let seen = job.drain();
        let finished = job.is_finished();
        drop(held);
        assert_eq!(
            seen,
            vec![Note::Value(1), Note::Done, Note::Value(2), Note::Value(3)]
        );
        assert!(finished);
        assert!(job.drain().is_empty());
        assert!(job.is_finished());
    }

    #[test]
    fn dropping_a_finished_job_cuts_off_no_work_the_reader_needs() {
        let written = Arc::new(AtomicUsize::new(0));
        let cleaned = Arc::new(AtomicUsize::new(0));
        let (release, held) = mpsc::channel::<()>();
        let mut job: Job<Note> = {
            let written = Arc::clone(&written);
            let cleaned = Arc::clone(&cleaned);
            Job::spawn(move |emitter: &Emitter<Note>, _| {
                written.store(1, Ordering::SeqCst);
                emitter.send(Note::Done);
                let _ = held.recv_timeout(LIMIT);
                cleaned.store(1, Ordering::SeqCst);
            })
        };
        let flag = job.cancel_handle();
        assert!(wait_for(LIMIT, || {
            job.drain();
            job.is_finished()
        }));
        assert_eq!(
            written.load(Ordering::SeqCst),
            1,
            "work before the terminal message is complete when the job finishes"
        );
        drop(job);
        assert!(flag.is_cancelled());
        release.send(()).unwrap();
        assert!(wait_for(LIMIT, || cleaned.load(Ordering::SeqCst) == 1));
    }

    #[test]
    fn waiting_ends_at_the_terminal_message_while_the_body_still_runs() {
        let (mut job, held) = job_held_after(vec![Note::Value(1), Note::Done]);
        let started = Instant::now();
        let seen = job.wait(LIMIT);
        let waited = started.elapsed();
        let finished = job.is_finished();
        drop(held);
        assert_eq!(seen, vec![Note::Value(1), Note::Done]);
        assert!(finished);
        assert!(waited < LIMIT, "the wait ran to its limit");
    }

    #[test]
    fn waiting_for_the_return_ends_at_the_return_or_at_the_limit() {
        let (mut job, held) = job_held_after(vec![Note::Value(1), Note::Done]);
        let early = job.wait_returned(Duration::from_millis(20));
        let returned_at_limit = job.has_returned();
        drop(held);
        let rest = job.wait_returned(LIMIT);
        assert_eq!(early, vec![Note::Value(1), Note::Done]);
        assert!(!returned_at_limit);
        assert!(rest.is_empty());
        assert!(job.has_returned());
        assert!(job.is_finished());
    }

    #[test]
    fn waiting_for_the_return_takes_the_synthetic_terminal_message() {
        let mut job: Job<Note> = Job::spawn(|emitter: &Emitter<Note>, _| {
            emitter.send(Note::Value(7));
        });
        assert_eq!(
            job.wait_returned(LIMIT),
            vec![Note::Value(7), Note::Cancelled]
        );
        assert!(job.has_returned());
        assert!(job.is_finished());
    }
    #[test]
    fn draining_a_finished_job_twice_is_stable() {
        let mut job: Job<Note> = Job::spawn(|emitter: &Emitter<Note>, _| {
            emitter.send(Note::Done);
        });
        let seen = collect(&mut job);
        assert_eq!(seen, vec![Note::Done]);
        assert!(job.drain().is_empty());
        assert!(job.is_finished());
    }
}
