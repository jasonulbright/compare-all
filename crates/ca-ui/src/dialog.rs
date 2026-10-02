//! File and folder pickers, run as jobs rather than inside a frame.
//!
//! A native picker does not return until the user is finished with it, so
//! calling one from `ui()` stops the frame thread for as long as the panel is
//! open. Every picker here goes through the job system instead and the view
//! applies the answer when it polls.
//!
//! Platform handling differs and is the reason the asynchronous interface is
//! used rather than a plain worker thread around the blocking one:
//!
//! - macOS requires the panel to be raised on the main thread. The asynchronous
//!   dialog dispatches it there and resolves its future afterwards, so the
//!   panel is legal and the frame thread still never waits.
//! - Windows and Linux raise the panel on whichever thread asks, so it runs on
//!   the job's own thread.
//!
//! The future is driven by [`block_on`] on the job's thread, which parks on a
//! condition variable between wake-ups and so costs nothing while the panel is
//! open.

use crate::worker::{Job, Terminal};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};

/// Which kind of picker to raise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// One existing file.
    File,
    /// One existing folder.
    Folder,
    /// A name to write to, which need not exist.
    SaveFile,
}

/// Which field a picker was raised for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The left path field.
    Left,
    /// The right path field.
    Right,
}

/// What a picker posts back.
#[derive(Debug)]
pub enum DialogMessage {
    /// The user chose a path.
    Chosen(PathBuf),
    /// The user closed the panel without choosing.
    Dismissed,
    /// The panel could not be raised.
    Failed(String),
}

impl Terminal for DialogMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        DialogMessage::Dismissed
    }

    fn panicked(detail: String) -> Self {
        DialogMessage::Failed(detail)
    }
}

/// Raise a picker on a worker and post what the user chose.
#[must_use]
pub fn spawn(pick: Pick, notify: Arc<dyn Fn() + Send + Sync>) -> Job<DialogMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let chosen = match pick {
                Pick::File => block_on(rfd::AsyncFileDialog::new().pick_file()),
                Pick::Folder => block_on(rfd::AsyncFileDialog::new().pick_folder()),
                Pick::SaveFile => block_on(rfd::AsyncFileDialog::new().save_file()),
            };
            emitter.send(match chosen {
                Some(handle) => DialogMessage::Chosen(handle.path().to_path_buf()),
                None => DialogMessage::Dismissed,
            });
        },
        notify,
    )
}

/// A parked thread waiting for a future to make progress.
#[derive(Default)]
struct Signal {
    woken: Mutex<bool>,
    changed: Condvar,
}

impl Signal {
    fn wait(&self) {
        let Ok(mut woken) = self.woken.lock() else {
            return;
        };
        while !*woken {
            match self.changed.wait(woken) {
                Ok(next) => woken = next,
                Err(_) => return,
            }
        }
        *woken = false;
    }

    fn signal(&self) {
        if let Ok(mut woken) = self.woken.lock() {
            *woken = true;
        }
        self.changed.notify_all();
    }
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.signal();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.signal();
    }
}

/// Drive `future` to its answer on the calling thread.
///
/// The thread parks between wake-ups, so a panel that stays open for a minute
/// costs one parked thread and no processor time.
fn block_on<F: Future>(future: F) -> F::Output {
    // Pinning on the heap keeps the whole driver inside safe code.
    let mut future = Box::pin(future);
    let signal = Arc::new(Signal::default());
    let waker = Waker::from(Arc::clone(&signal));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => signal.wait(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{block_on, DialogMessage, Signal};
    use crate::worker::Terminal;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    #[test]
    fn a_ready_future_returns_without_parking() {
        assert_eq!(block_on(std::future::ready(7)), 7);
    }

    #[test]
    fn a_future_that_answers_from_another_thread_is_waited_for() {
        struct Later(Arc<AtomicBool>);
        impl std::future::Future for Later {
            type Output = u32;
            fn poll(self: std::pin::Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u32> {
                if self.0.load(Ordering::SeqCst) {
                    return Poll::Ready(9);
                }
                let waker = context.waker().clone();
                let flag = Arc::clone(&self.0);
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    flag.store(true, Ordering::SeqCst);
                    waker.wake();
                });
                Poll::Pending
            }
        }
        assert_eq!(block_on(Later(Arc::new(AtomicBool::new(false)))), 9);
    }

    #[test]
    fn a_signal_raised_before_the_wait_does_not_lose_the_wake_up() {
        let signal = Arc::new(Signal::default());
        let waker = Waker::from(Arc::clone(&signal));
        waker.wake_by_ref();
        // Returns rather than parking, because the flag was already set.
        signal.wait();
    }

    #[test]
    fn every_dialog_message_ends_the_stream() {
        for message in [
            DialogMessage::Dismissed,
            DialogMessage::Failed("no panel".into()),
            DialogMessage::Chosen(std::path::PathBuf::from("x")),
        ] {
            assert!(message.is_terminal());
        }
    }
}
