//! The undo history of registry edits.
//!
//! Each step is the list of plan operations one command produced, so one undo
//! reverses one command. The plan in force is every operation of the steps
//! before the cursor. Undo and redo move the cursor; a new step drops the
//! steps after it.
//!
//! Each side has a save point: the cursor position its source last received.
//! A save point that sits in the steps a new step drops can never be reached
//! again, so it is cleared and the side stays modified until the next write.

use crate::model::Side;
use ca_records::registry::plan::{EditOp, Side as PlanSide};

/// The plan side that names a model side.
#[must_use]
pub const fn plan_side(side: Side) -> PlanSide {
    match side {
        Side::Left => PlanSide::Left,
        Side::Right => PlanSide::Right,
    }
}

const fn slot(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
    }
}

/// Steps of edits with an undo cursor and a save point per side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    steps: Vec<Vec<EditOp>>,
    applied: usize,
    saved: [Option<usize>; 2],
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// No steps, both sides saved.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            steps: Vec::new(),
            applied: 0,
            saved: [Some(0), Some(0)],
        }
    }

    /// Add a step after the cursor, dropping any step that was undone.
    pub fn push(&mut self, step: Vec<EditOp>) {
        if step.is_empty() {
            return;
        }
        self.steps.truncate(self.applied);
        self.drop_unreachable_save_points();
        self.steps.push(step);
        self.applied = self.steps.len();
    }

    /// Take back the last step that [`History::push`] added, without keeping
    /// it for redo. Used when the step could not be applied.
    pub fn discard_last(&mut self) {
        if self.applied == 0 || self.applied != self.steps.len() {
            return;
        }
        self.steps.pop();
        self.applied = self.steps.len();
        self.drop_unreachable_save_points();
    }

    fn drop_unreachable_save_points(&mut self) {
        for saved in &mut self.saved {
            if saved.is_some_and(|depth| depth > self.applied) {
                *saved = None;
            }
        }
    }

    /// Step back one command. Returns false when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        if self.applied == 0 {
            return false;
        }
        self.applied -= 1;
        true
    }

    /// Step forward one command. Returns false when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        if self.applied >= self.steps.len() {
            return false;
        }
        self.applied += 1;
        true
    }

    /// True when a step can be undone.
    #[must_use]
    pub const fn can_undo(&self) -> bool {
        self.applied > 0
    }

    /// True when an undone step can be redone.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.applied < self.steps.len()
    }

    /// Every operation of the steps before the cursor, in order.
    #[must_use]
    pub fn ops(&self) -> Vec<EditOp> {
        self.steps
            .iter()
            .take(self.applied)
            .flatten()
            .cloned()
            .collect()
    }

    /// Record that `side`'s source now holds the plan in force.
    pub fn mark_saved(&mut self, side: Side) {
        if let Some(saved) = self.saved.get_mut(slot(side)) {
            *saved = Some(self.applied);
        }
    }

    /// True when the plan in force changes `side` beyond what its source
    /// last received.
    #[must_use]
    pub fn is_modified(&self, side: Side) -> bool {
        let Some(Some(saved)) = self.saved.get(slot(side)).copied() else {
            return true;
        };
        let (from, to) = if saved <= self.applied {
            (saved, self.applied)
        } else {
            (self.applied, saved)
        };
        let target = plan_side(side);
        self.steps
            .get(from..to)
            .unwrap_or_default()
            .iter()
            .flatten()
            .any(|op| op.target() == Some(target))
    }

    /// True when either side is modified.
    #[must_use]
    pub fn is_any_modified(&self) -> bool {
        self.is_modified(Side::Left) || self.is_modified(Side::Right)
    }
}

#[cfg(test)]
mod tests {
    use super::History;
    use crate::model::Side;
    use ca_records::registry::plan::{EditOp, Side as PlanSide};

    fn on(side: PlanSide, key: &str) -> Vec<EditOp> {
        vec![EditOp::create_key(side, key)]
    }

    #[test]
    fn undo_and_redo_move_through_whole_steps() {
        let mut history = History::new();
        history.push(vec![
            EditOp::create_key(PlanSide::Left, "A"),
            EditOp::create_key(PlanSide::Left, "B"),
        ]);
        history.push(on(PlanSide::Right, "C"));
        assert_eq!(history.ops().len(), 3);
        assert!(history.undo());
        assert_eq!(history.ops().len(), 2);
        assert!(history.undo());
        assert!(!history.undo());
        assert!(history.ops().is_empty());
        assert!(history.redo());
        assert!(history.redo());
        assert!(!history.redo());
        assert_eq!(history.ops().len(), 3);
    }

    #[test]
    fn a_side_is_modified_only_by_steps_that_change_it() {
        let mut history = History::new();
        history.push(on(PlanSide::Left, "A"));
        assert!(history.is_modified(Side::Left));
        assert!(!history.is_modified(Side::Right));
        history.push(vec![EditOp::copy_key(PlanSide::Left, "A")]);
        assert!(history.is_modified(Side::Right));
        history.mark_saved(Side::Left);
        history.mark_saved(Side::Right);
        assert!(!history.is_any_modified());
        history.undo();
        assert!(history.is_modified(Side::Right));
        assert!(!history.is_modified(Side::Left));
        history.redo();
        assert!(!history.is_any_modified());
    }

    #[test]
    fn a_save_point_in_a_dropped_branch_is_never_reached_again() {
        let mut history = History::new();
        history.push(on(PlanSide::Left, "A"));
        history.mark_saved(Side::Left);
        history.undo();
        history.push(on(PlanSide::Left, "B"));
        assert!(history.is_modified(Side::Left));
        history.undo();
        // The content now equals what was read, not what was written.
        assert!(history.is_modified(Side::Left));
        assert!(!history.is_modified(Side::Right));
    }

    #[test]
    fn a_step_that_failed_is_discarded_without_a_redo() {
        let mut history = History::new();
        history.push(on(PlanSide::Left, "A"));
        history.push(on(PlanSide::Left, "B"));
        history.discard_last();
        assert_eq!(history.ops().len(), 1);
        assert!(!history.can_redo());
        history.push(Vec::new());
        assert_eq!(history.ops().len(), 1);
    }
}
