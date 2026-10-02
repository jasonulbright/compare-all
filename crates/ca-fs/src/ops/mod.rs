//! Destructive folder operations, in two phases.
//!
//! [`plan`] turns a selection and a set of options into an
//! [`OperationPlan`](plan::OperationPlan): every concrete step, its source and
//! destination, the bytes it moves and the conflicts it implies. Planning
//! reads nothing and writes nothing.
//!
//! [`exec`] runs a plan through [`fsops::FileOps`], cancellably and with a
//! caller-supplied error policy, recording each step to a journal before and
//! after it runs so an interrupted batch can be reported and cleaned up.

pub mod exec;
pub mod fsops;
pub mod plan;
pub mod vfsops;

#[cfg(test)]
mod tests;
