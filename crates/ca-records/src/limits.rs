//! Allocation ceilings applied before any buffer is reserved.
//!
//! A hostile source states its own sizes. Every parser in this crate checks a
//! stated size against these ceilings before it allocates, so a four byte
//! length field cannot force a four gigabyte reservation.

use crate::error::{RecordError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Fields written by another build, preserved verbatim.
pub type Unknown = BTreeMap<String, serde_json::Value>;

/// Ceilings a parser checks before it allocates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    /// Largest source the parser accepts, in bytes.
    pub max_input_bytes: u64,
    /// Largest single value payload, in bytes.
    pub max_value_bytes: u64,
    /// Largest number of records one source may produce.
    pub max_records: u64,
    /// Largest number of tree levels one source may produce.
    pub max_depth: u32,
    /// Largest name or key path, in bytes.
    pub max_name_bytes: u64,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 << 20,
            max_value_bytes: 16 << 20,
            max_records: 1_000_000,
            max_depth: 64,
            max_name_bytes: 64 << 10,
            unknown: Unknown::new(),
        }
    }
}

impl Limits {
    /// Refuse a source larger than [`Limits::max_input_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::LimitExceeded`] when `len` is over the ceiling.
    pub fn check_input(&self, len: u64) -> Result<()> {
        check("maxInputBytes", self.max_input_bytes, len)
    }

    /// Refuse a value payload larger than [`Limits::max_value_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::LimitExceeded`] when `len` is over the ceiling.
    pub fn check_value(&self, len: u64) -> Result<()> {
        check("maxValueBytes", self.max_value_bytes, len)
    }

    /// Refuse a name longer than [`Limits::max_name_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::LimitExceeded`] when `len` is over the ceiling.
    pub fn check_name(&self, len: u64) -> Result<()> {
        check("maxNameBytes", self.max_name_bytes, len)
    }

    /// Refuse a record count over [`Limits::max_records`].
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::LimitExceeded`] when `count` is over the ceiling.
    pub fn check_records(&self, count: u64) -> Result<()> {
        check("maxRecords", self.max_records, count)
    }

    /// Refuse a nesting level over [`Limits::max_depth`].
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::LimitExceeded`] when `depth` is over the ceiling.
    pub fn check_depth(&self, depth: u32) -> Result<()> {
        check("maxDepth", u64::from(self.max_depth), u64::from(depth))
    }
}

fn check(limit: &'static str, allowed: u64, requested: u64) -> Result<()> {
    if requested > allowed {
        return Err(RecordError::LimitExceeded {
            limit,
            allowed,
            requested,
        });
    }
    Ok(())
}

/// A counter that fails the parse once it passes [`Limits::max_records`].
#[derive(Debug)]
pub(crate) struct RecordBudget {
    used: u64,
    allowed: u64,
}

impl RecordBudget {
    pub(crate) const fn new(limits: &Limits) -> Self {
        Self {
            used: 0,
            allowed: limits.max_records,
        }
    }

    /// Charge one record against the budget.
    pub(crate) fn spend(&mut self) -> Result<()> {
        self.spend_many(1)
    }

    /// Charge `count` records against the budget at once, before anything
    /// is reserved for them.
    pub(crate) fn spend_many(&mut self, count: u64) -> Result<()> {
        self.used = self.used.saturating_add(count);
        check("maxRecords", self.allowed, self.used)
    }

    /// Records charged so far.
    #[cfg(test)]
    pub(crate) const fn used(&self) -> u64 {
        self.used
    }
}
