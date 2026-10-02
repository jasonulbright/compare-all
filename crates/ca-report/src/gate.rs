//! Display filtering with a bounded window.
//!
//! The context filter needs the rows around a difference, so it holds back at
//! most the configured count of matching rows. Nothing else is buffered, so a
//! filtered report over a very large input still costs a fixed amount of
//! memory.

use std::collections::VecDeque;

/// What the gate lets through.
pub(crate) enum Emit<T> {
    /// A run of rows the filter dropped.
    Gap(u64),
    /// A row the filter kept.
    Row(T),
}

/// Which rows a gate keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    /// Every row.
    All,
    /// Differences only.
    Mismatches,
    /// Differences and the rows around them.
    Context,
    /// Matches only.
    Matches,
}

/// A display filter over a row stream.
pub(crate) struct Gate<T> {
    keep: Keep,
    context: u32,
    before: VecDeque<T>,
    after_remaining: u32,
    dropped: u64,
    seen_difference: bool,
}

impl<T> Gate<T> {
    /// Build a gate. `context` is used by [`Keep::Context`] alone.
    pub(crate) fn new(keep: Keep, context: u32) -> Self {
        Self {
            keep,
            context,
            before: VecDeque::new(),
            after_remaining: 0,
            dropped: 0,
            seen_difference: false,
        }
    }

    /// Offer one row. `is_difference` decides which side of the filter it falls.
    pub(crate) fn push<E>(
        &mut self,
        row: T,
        is_difference: bool,
        emit: &mut dyn FnMut(Emit<T>) -> Result<(), E>,
    ) -> Result<(), E> {
        match self.keep {
            Keep::All => emit(Emit::Row(row)),
            Keep::Mismatches => {
                if is_difference {
                    self.flush_gap(emit)?;
                    emit(Emit::Row(row))
                } else {
                    self.dropped += 1;
                    Ok(())
                }
            }
            Keep::Matches => {
                if is_difference {
                    self.dropped += 1;
                    Ok(())
                } else {
                    self.flush_gap(emit)?;
                    emit(Emit::Row(row))
                }
            }
            Keep::Context => self.push_context(row, is_difference, emit),
        }
    }

    fn push_context<E>(
        &mut self,
        row: T,
        is_difference: bool,
        emit: &mut dyn FnMut(Emit<T>) -> Result<(), E>,
    ) -> Result<(), E> {
        if is_difference {
            let held = std::mem::take(&mut self.before);
            self.flush_gap(emit)?;
            for kept in held {
                emit(Emit::Row(kept))?;
            }
            self.after_remaining = self.context;
            self.seen_difference = true;
            return emit(Emit::Row(row));
        }
        if self.after_remaining > 0 {
            self.after_remaining -= 1;
            return emit(Emit::Row(row));
        }
        if self.context == 0 {
            self.dropped += 1;
            return Ok(());
        }
        self.before.push_back(row);
        while self.before.len() > self.context as usize {
            self.before.pop_front();
            self.dropped += 1;
        }
        Ok(())
    }

    fn flush_gap<E>(&mut self, emit: &mut dyn FnMut(Emit<T>) -> Result<(), E>) -> Result<(), E> {
        if self.dropped > 0 {
            let dropped = self.dropped;
            self.dropped = 0;
            emit(Emit::Gap(dropped))?;
        }
        Ok(())
    }

    /// Close the stream, reporting any trailing run the filter dropped.
    pub(crate) fn finish<E>(
        &mut self,
        emit: &mut dyn FnMut(Emit<T>) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.keep == Keep::Context {
            self.dropped += self.before.len() as u64;
            self.before.clear();
        }
        if self.seen_difference || self.keep != Keep::Context {
            self.flush_gap(emit)?;
        } else {
            self.dropped = 0;
        }
        Ok(())
    }

    /// Rows the filter is holding back, for a memory assertion in a test.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.before.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{Emit, Gate, Keep};

    fn run(keep: Keep, context: u32, rows: &[(u32, bool)]) -> Vec<String> {
        let mut out = Vec::new();
        let mut gate = Gate::new(keep, context);
        for (value, is_difference) in rows.iter().copied() {
            gate.push::<()>(value, is_difference, &mut |emit| {
                match emit {
                    Emit::Gap(count) => out.push(format!("gap {count}")),
                    Emit::Row(value) => out.push(value.to_string()),
                }
                Ok(())
            })
            .expect("no error");
        }
        gate.finish::<()>(&mut |emit| {
            if let Emit::Gap(count) = emit {
                out.push(format!("gap {count}"));
            }
            Ok(())
        })
        .expect("no error");
        out
    }

    #[test]
    fn every_row_passes_the_open_gate() {
        let rows = [(1, false), (2, true), (3, false)];
        assert_eq!(run(Keep::All, 0, &rows), vec!["1", "2", "3"]);
    }

    #[test]
    fn the_mismatch_gate_reports_the_run_it_dropped() {
        let rows = [(1, false), (2, false), (3, true), (4, false)];
        assert_eq!(run(Keep::Mismatches, 0, &rows), vec!["gap 2", "3", "gap 1"]);
    }

    #[test]
    fn the_match_gate_keeps_the_other_side() {
        let rows = [(1, false), (2, true), (3, false)];
        assert_eq!(run(Keep::Matches, 0, &rows), vec!["1", "gap 1", "3"]);
    }

    #[test]
    fn the_context_gate_keeps_the_rows_around_a_difference() {
        let rows = [
            (1, false),
            (2, false),
            (3, false),
            (4, true),
            (5, false),
            (6, false),
            (7, false),
        ];
        assert_eq!(
            run(Keep::Context, 1, &rows),
            vec!["gap 2", "3", "4", "5", "gap 2"]
        );
    }

    #[test]
    fn the_context_gate_holds_back_no_more_than_the_context() {
        let mut gate: Gate<u32> = Gate::new(Keep::Context, 2);
        for value in 0..10_000u32 {
            gate.push::<()>(value, false, &mut |_| Ok(()))
                .expect("push");
            assert!(gate.held() <= 2, "the window stays bounded");
        }
    }

    #[test]
    fn a_context_report_with_no_difference_writes_nothing() {
        let rows = [(1, false), (2, false)];
        assert!(run(Keep::Context, 3, &rows).is_empty());
    }
}
