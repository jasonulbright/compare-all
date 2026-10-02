//! Planned registry edits.
//!
//! An edit is described, not performed. The plan is a value: it can be built,
//! inspected, serialised and shown before anything changes. Applying a plan to
//! a `.reg` file model changes only the model in memory.
//!
//! The live registry is never written from a plan directly. The model of a
//! live side is an export of its keys; [`live_steps`] states the writes that
//! turn one such model into another, and `registry::live::apply_confirmed`
//! carries them out behind a stated consent. [`EditPlan::apply_to_live`] has no
//! consent to act on and refuses.

use crate::error::{RecordError, Result};
use crate::limits::{Limits, Unknown};
use crate::record::ByteRange;
use crate::registry::reg_file::{RegEntry, RegFile, RegKeyBlock};
use crate::registry::value::{ValueData, ValueKind, ValueName};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Which side of a comparison an operation names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    /// The left side.
    Left,
    /// The right side.
    Right,
}

impl Side {
    /// The opposite side.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// How a deletion changes a `.reg` file model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeleteForm {
    /// A deletion writes the export deletion form, `[-key]` or `"name"=-`, so
    /// the file carries the deletion to whatever imports it.
    #[default]
    Mark,
    /// A deletion takes the key blocks or the value entry out of the model, so
    /// the file no longer holds what was deleted.
    Remove,
}

/// One planned change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum EditOp {
    /// Copy a key and everything under it to the other side.
    CopyKey {
        /// Side the key is read from.
        from: Side,
        /// Full key path.
        key_path: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// Copy one value to the other side.
    CopyValue {
        /// Side the value is read from.
        from: Side,
        /// Full key path that holds the value.
        key_path: String,
        /// Name of the value.
        value_name: ValueName,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// Delete a key and everything under it.
    DeleteKey {
        /// Side the key is deleted from.
        side: Side,
        /// Full key path.
        key_path: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// Delete one value.
    DeleteValue {
        /// Side the value is deleted from.
        side: Side,
        /// Full key path that holds the value.
        key_path: String,
        /// Name of the value.
        value_name: ValueName,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// Create a key with no values.
    CreateKey {
        /// Side the key is created on.
        side: Side,
        /// Full key path.
        key_path: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// Write one value, creating its key when the key is missing.
    SetValue {
        /// Side the value is written on.
        side: Side,
        /// Full key path that holds the value.
        key_path: String,
        /// Name of the value.
        value_name: ValueName,
        /// Numeric type code of the value.
        kind: u32,
        /// Raw registry bytes of the value.
        data: Vec<u8>,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// Give a key a new name under the same parent.
    RenameKey {
        /// Side the key is renamed on.
        side: Side,
        /// Full key path.
        key_path: String,
        /// The new last segment of the path.
        new_name: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// Give a value a new name in the same key.
    RenameValue {
        /// Side the value is renamed on.
        side: Side,
        /// Full key path that holds the value.
        key_path: String,
        /// Name of the value.
        value_name: ValueName,
        /// The new name.
        new_name: ValueName,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: Unknown,
    },
    /// An operation a later build named, preserved verbatim.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl EditOp {
    /// Copy a key to the other side.
    #[must_use]
    pub fn copy_key(from: Side, key_path: impl Into<String>) -> Self {
        Self::CopyKey {
            from,
            key_path: key_path.into(),
            unknown: Unknown::new(),
        }
    }

    /// Copy one value to the other side.
    #[must_use]
    pub fn copy_value(from: Side, key_path: impl Into<String>, value_name: ValueName) -> Self {
        Self::CopyValue {
            from,
            key_path: key_path.into(),
            value_name,
            unknown: Unknown::new(),
        }
    }

    /// Delete a key.
    #[must_use]
    pub fn delete_key(side: Side, key_path: impl Into<String>) -> Self {
        Self::DeleteKey {
            side,
            key_path: key_path.into(),
            unknown: Unknown::new(),
        }
    }

    /// Delete one value.
    #[must_use]
    pub fn delete_value(side: Side, key_path: impl Into<String>, value_name: ValueName) -> Self {
        Self::DeleteValue {
            side,
            key_path: key_path.into(),
            value_name,
            unknown: Unknown::new(),
        }
    }

    /// Create a key.
    #[must_use]
    pub fn create_key(side: Side, key_path: impl Into<String>) -> Self {
        Self::CreateKey {
            side,
            key_path: key_path.into(),
            unknown: Unknown::new(),
        }
    }

    /// Write one value.
    #[must_use]
    pub fn set_value(
        side: Side,
        key_path: impl Into<String>,
        value_name: ValueName,
        data: &ValueData,
    ) -> Self {
        Self::SetValue {
            side,
            key_path: key_path.into(),
            value_name,
            kind: data.kind().code(),
            data: data.to_raw(),
            unknown: Unknown::new(),
        }
    }

    /// Rename a key.
    #[must_use]
    pub fn rename_key(
        side: Side,
        key_path: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Self {
        Self::RenameKey {
            side,
            key_path: key_path.into(),
            new_name: new_name.into(),
            unknown: Unknown::new(),
        }
    }

    /// Rename a value.
    #[must_use]
    pub fn rename_value(
        side: Side,
        key_path: impl Into<String>,
        value_name: ValueName,
        new_name: ValueName,
    ) -> Self {
        Self::RenameValue {
            side,
            key_path: key_path.into(),
            value_name,
            new_name,
            unknown: Unknown::new(),
        }
    }

    /// The key path the operation acts on, when the operation names one.
    #[must_use]
    pub fn key_path(&self) -> Option<&str> {
        match self {
            Self::CopyKey { key_path, .. }
            | Self::CopyValue { key_path, .. }
            | Self::DeleteKey { key_path, .. }
            | Self::DeleteValue { key_path, .. }
            | Self::CreateKey { key_path, .. }
            | Self::SetValue { key_path, .. }
            | Self::RenameKey { key_path, .. }
            | Self::RenameValue { key_path, .. } => Some(key_path),
            Self::Unknown(_) => None,
        }
    }

    /// The side the operation changes, when the operation names one.
    #[must_use]
    pub const fn target(&self) -> Option<Side> {
        match self {
            Self::CopyKey { from, .. } | Self::CopyValue { from, .. } => Some(from.other()),
            Self::DeleteKey { side, .. }
            | Self::DeleteValue { side, .. }
            | Self::CreateKey { side, .. }
            | Self::SetValue { side, .. }
            | Self::RenameKey { side, .. }
            | Self::RenameValue { side, .. } => Some(*side),
            Self::Unknown(_) => None,
        }
    }
}

/// A list of planned changes, in the order the caller added them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EditPlan {
    /// The planned changes.
    pub ops: Vec<EditOp>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl EditPlan {
    /// An empty plan.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one operation.
    #[must_use]
    pub fn with(mut self, op: EditOp) -> Self {
        self.ops.push(op);
        self
    }

    /// Add one operation in place.
    pub fn push(&mut self, op: EditOp) {
        self.ops.push(op);
    }

    /// True when the plan changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Apply the plan to two `.reg` file models, writing deletions in the
    /// export deletion form.
    ///
    /// Both models change in place. A copy writes the source entry into the
    /// other model, creating the key block when it is missing. A delete writes
    /// the deletion form, so the result exports the same intent the plan holds.
    ///
    /// # Errors
    ///
    /// Same as [`EditPlan::apply_to_files_with`].
    pub fn apply_to_files(&self, left: &mut RegFile, right: &mut RegFile) -> Result<()> {
        self.apply_to_files_with(left, right, DeleteForm::Mark)
    }

    /// Apply the plan to two `.reg` file models with the chosen deletion form.
    ///
    /// The operations run in order and each one sees the result of the ones
    /// before it. On an error the models hold the operations before the one
    /// that failed.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::NotFound`] when an operation names a key or value
    /// that the model does not hold, [`RecordError::AlreadyExists`] when a
    /// create or a rename meets a name that is taken, [`RecordError::InvalidSpec`]
    /// for a name that cannot be a key or value name, and
    /// [`RecordError::Unsupported`] for an operation this build does not name.
    pub fn apply_to_files_with(
        &self,
        left: &mut RegFile,
        right: &mut RegFile,
        form: DeleteForm,
    ) -> Result<()> {
        for op in &self.ops {
            apply_one(op, left, right, form)?;
        }
        Ok(())
    }

    /// Apply the plan to the live registry without a stated consent.
    ///
    /// # Errors
    ///
    /// Always returns [`RecordError::Unsupported`]. A live write goes through
    /// `registry::live::apply_confirmed`, which takes the consent this call
    /// does not have.
    pub fn apply_to_live(&self) -> Result<()> {
        Err(RecordError::unsupported(
            "writing a plan to the live registry needs an explicit confirmation step that this call does not have",
        ))
    }
}

fn apply_one(op: &EditOp, left: &mut RegFile, right: &mut RegFile, form: DeleteForm) -> Result<()> {
    match op {
        EditOp::Unknown(_) => Err(RecordError::unsupported(
            "the plan holds an operation this build does not name",
        )),
        EditOp::CopyKey { from, key_path, .. } => {
            let (source, target) = pick(*from, left, right);
            copy_key(source, target, key_path)
        }
        EditOp::CopyValue {
            from,
            key_path,
            value_name,
            ..
        } => {
            let (source, target) = pick(*from, left, right);
            copy_value(source, target, key_path, value_name)
        }
        EditOp::DeleteKey { side, key_path, .. } => {
            let (target, _) = pick(*side, left, right);
            match form {
                DeleteForm::Mark => {
                    delete_key(target, key_path);
                    Ok(())
                }
                DeleteForm::Remove => remove_key(target, key_path),
            }
        }
        EditOp::DeleteValue {
            side,
            key_path,
            value_name,
            ..
        } => {
            let (target, _) = pick(*side, left, right);
            match form {
                DeleteForm::Mark => {
                    delete_value(target, key_path, value_name);
                    Ok(())
                }
                DeleteForm::Remove => remove_value(target, key_path, value_name),
            }
        }
        EditOp::CreateKey { side, key_path, .. } => {
            let (target, _) = pick(*side, left, right);
            create_key(target, key_path)
        }
        EditOp::SetValue {
            side,
            key_path,
            value_name,
            kind,
            data,
            ..
        } => {
            let (target, _) = pick(*side, left, right);
            let data = ValueData::from_raw(ValueKind::from_code(*kind), data, &Limits::default())?;
            let index = writable_block(target, key_path);
            if let Some(block) = target.keys.get_mut(index) {
                put_entry(
                    block,
                    RegEntry {
                        name: value_name.clone(),
                        data: Some(data),
                        source: ByteRange::default(),
                    },
                );
            }
            Ok(())
        }
        EditOp::RenameKey {
            side,
            key_path,
            new_name,
            ..
        } => {
            let (target, _) = pick(*side, left, right);
            rename_key(target, key_path, new_name, form)
        }
        EditOp::RenameValue {
            side,
            key_path,
            value_name,
            new_name,
            ..
        } => {
            let (target, _) = pick(*side, left, right);
            rename_value(target, key_path, value_name, new_name, form)
        }
    }
}

fn pick<'a>(
    side: Side,
    left: &'a mut RegFile,
    right: &'a mut RegFile,
) -> (&'a mut RegFile, &'a mut RegFile) {
    match side {
        Side::Left => (left, right),
        Side::Right => (right, left),
    }
}

/// True when `path` is `key` or a key under it, ignoring ASCII case.
#[must_use]
pub fn is_within(path: &str, key: &str) -> bool {
    if path.len() < key.len() {
        return false;
    }
    let (head, tail) = path.split_at_checked(key.len()).unwrap_or((path, ""));
    head.eq_ignore_ascii_case(key) && (tail.is_empty() || tail.starts_with('\\'))
}

fn copy_key(source: &mut RegFile, target: &mut RegFile, key_path: &str) -> Result<()> {
    let blocks: Vec<RegKeyBlock> = source
        .keys
        .iter()
        .filter(|block| is_within(&block.path, key_path))
        .cloned()
        .collect();
    if blocks.is_empty() {
        return Err(RecordError::NotFound {
            path: key_path.to_owned(),
        });
    }
    for block in blocks {
        let index = ensure_block(target, &block.path);
        if let Some(target_block) = target.keys.get_mut(index) {
            target_block.delete = block.delete;
            for entry in block.entries {
                put_entry(target_block, entry);
            }
        }
    }
    Ok(())
}

fn copy_value(
    source: &mut RegFile,
    target: &mut RegFile,
    key_path: &str,
    name: &ValueName,
) -> Result<()> {
    let entry = source
        .key(key_path)
        .and_then(|block| {
            block
                .entries
                .iter()
                .find(|entry| names_equal(&entry.name, name))
        })
        .cloned();
    let Some(entry) = entry else {
        return Err(RecordError::NotFound {
            path: format!("{key_path}\\{}", name.display()),
        });
    };
    let index = writable_block(target, key_path);
    if let Some(block) = target.keys.get_mut(index) {
        put_entry(block, entry);
    }
    Ok(())
}

fn delete_key(target: &mut RegFile, key_path: &str) {
    // An importer creates a child block listed after the deletion line again.
    target.keys.retain(|block| {
        block.path.eq_ignore_ascii_case(key_path) || !is_within(&block.path, key_path)
    });
    let index = ensure_block(target, key_path);
    if let Some(block) = target.keys.get_mut(index) {
        block.delete = true;
        block.entries.clear();
    }
}

fn delete_value(target: &mut RegFile, key_path: &str, name: &ValueName) {
    let index = ensure_block(target, key_path);
    if let Some(block) = target.keys.get_mut(index) {
        put_entry(
            block,
            RegEntry {
                name: name.clone(),
                data: None,
                source: ByteRange::default(),
            },
        );
    }
}

fn remove_key(target: &mut RegFile, key_path: &str) -> Result<()> {
    let before = target.keys.len();
    target
        .keys
        .retain(|block| !is_within(&block.path, key_path));
    if target.keys.len() == before {
        return Err(RecordError::NotFound {
            path: key_path.to_owned(),
        });
    }
    Ok(())
}

fn remove_value(target: &mut RegFile, key_path: &str, name: &ValueName) -> Result<()> {
    let mut removed = false;
    for block in target
        .keys
        .iter_mut()
        .filter(|block| block.path.eq_ignore_ascii_case(key_path))
    {
        let before = block.entries.len();
        block
            .entries
            .retain(|entry| !names_equal(&entry.name, name));
        removed |= block.entries.len() != before;
    }
    if removed {
        Ok(())
    } else {
        Err(RecordError::NotFound {
            path: format!("{key_path}\\{}", name.display()),
        })
    }
}

/// True when the model holds `key_path` as a key block or as the parent of
/// one, not counting deletion blocks.
fn holds_key(file: &RegFile, key_path: &str) -> bool {
    file.keys
        .iter()
        .any(|block| !block.delete && is_within(&block.path, key_path))
}

fn create_key(target: &mut RegFile, key_path: &str) -> Result<()> {
    check_key_path(key_path)?;
    if holds_key(target, key_path) {
        return Err(RecordError::AlreadyExists {
            path: key_path.to_owned(),
        });
    }
    target.keys.push(RegKeyBlock {
        path: key_path.to_owned(),
        delete: false,
        entries: Vec::new(),
        source: ByteRange::default(),
    });
    Ok(())
}

/// Refuse a key path with an empty segment, which no registry holds.
fn check_key_path(key_path: &str) -> Result<()> {
    if key_path.is_empty() || key_path.split('\\').any(str::is_empty) {
        return Err(RecordError::InvalidSpec(key_path.to_owned()));
    }
    Ok(())
}

fn rename_key(
    target: &mut RegFile,
    key_path: &str,
    new_name: &str,
    form: DeleteForm,
) -> Result<()> {
    if new_name.is_empty() || new_name.contains('\\') {
        return Err(RecordError::InvalidSpec(new_name.to_owned()));
    }
    let Some((parent, _)) = key_path.rsplit_once('\\') else {
        return Err(RecordError::refused(format!(
            "{key_path} is a hive and cannot be renamed"
        )));
    };
    let new_path = format!("{parent}\\{new_name}");
    if holds_key(target, &new_path) {
        return Err(RecordError::AlreadyExists { path: new_path });
    }
    if !holds_key(target, key_path) {
        return Err(RecordError::NotFound {
            path: key_path.to_owned(),
        });
    }
    let moved = |path: &str| {
        let tail = path.get(key_path.len()..).unwrap_or_default();
        format!("{new_path}{tail}")
    };
    match form {
        DeleteForm::Remove => {
            for block in target
                .keys
                .iter_mut()
                .filter(|block| is_within(&block.path, key_path))
            {
                block.path = moved(&block.path);
            }
        }
        DeleteForm::Mark => {
            let copies: Vec<RegKeyBlock> = target
                .keys
                .iter()
                .filter(|block| !block.delete && is_within(&block.path, key_path))
                .map(|block| RegKeyBlock {
                    path: moved(&block.path),
                    ..block.clone()
                })
                .collect();
            delete_key(target, key_path);
            target.keys.extend(copies);
        }
    }
    Ok(())
}

fn rename_value(
    target: &mut RegFile,
    key_path: &str,
    name: &ValueName,
    new_name: &ValueName,
    form: DeleteForm,
) -> Result<()> {
    let Some(block) = target
        .keys
        .iter_mut()
        .rev()
        .find(|block| !block.delete && block.path.eq_ignore_ascii_case(key_path))
    else {
        return Err(RecordError::NotFound {
            path: key_path.to_owned(),
        });
    };
    if !names_equal(name, new_name)
        && block
            .entries
            .iter()
            .any(|entry| entry.data.is_some() && names_equal(&entry.name, new_name))
    {
        return Err(RecordError::AlreadyExists {
            path: format!("{key_path}\\{}", new_name.display()),
        });
    }
    let Some(position) = block
        .entries
        .iter()
        .position(|entry| entry.data.is_some() && names_equal(&entry.name, name))
    else {
        return Err(RecordError::NotFound {
            path: format!("{key_path}\\{}", name.display()),
        });
    };
    match form {
        DeleteForm::Remove => {
            if let Some(entry) = block.entries.get_mut(position) {
                entry.name = new_name.clone();
            }
        }
        DeleteForm::Mark => {
            let Some(entry) = block.entries.get(position).cloned() else {
                return Ok(());
            };
            if let Some(slot) = block.entries.get_mut(position) {
                slot.data = None;
            }
            put_entry(
                block,
                RegEntry {
                    name: new_name.clone(),
                    ..entry
                },
            );
        }
    }
    Ok(())
}

/// Index of the key block for `key_path`, appending an empty block when the
/// file does not hold one yet.
fn ensure_block(file: &mut RegFile, key_path: &str) -> usize {
    if let Some(index) = file
        .keys
        .iter()
        .position(|block| block.path.eq_ignore_ascii_case(key_path))
    {
        return index;
    }
    file.keys.push(RegKeyBlock {
        path: key_path.to_owned(),
        delete: false,
        entries: Vec::new(),
        source: ByteRange::default(),
    });
    file.keys.len().saturating_sub(1)
}

/// Index of the last block for `key_path` that sets rather than deletes,
/// appending one when there is none.
///
/// An importer skips the entries under a deletion line, so a value written
/// into a deletion block would be lost.
fn writable_block(file: &mut RegFile, key_path: &str) -> usize {
    if let Some(index) = file
        .keys
        .iter()
        .rposition(|block| !block.delete && block.path.eq_ignore_ascii_case(key_path))
    {
        return index;
    }
    file.keys.push(RegKeyBlock {
        path: key_path.to_owned(),
        delete: false,
        entries: Vec::new(),
        source: ByteRange::default(),
    });
    file.keys.len().saturating_sub(1)
}

fn put_entry(block: &mut RegKeyBlock, entry: RegEntry) {
    if let Some(slot) = block
        .entries
        .iter_mut()
        .find(|existing| names_equal(&existing.name, &entry.name))
    {
        *slot = entry;
        return;
    }
    block.entries.push(entry);
}

fn names_equal(left: &ValueName, right: &ValueName) -> bool {
    left.raw().eq_ignore_ascii_case(right.raw())
}

/// Build a plan that makes the right side match the left side for one key.
#[must_use]
pub fn copy_key_plan(from: Side, key_path: &str) -> EditPlan {
    EditPlan::new().with(EditOp::copy_key(from, key_path))
}

/// Build a plan that copies one value to the other side.
#[must_use]
pub fn copy_value_plan(from: Side, key_path: &str, name: ValueName) -> EditPlan {
    EditPlan::new().with(EditOp::copy_value(from, key_path, name))
}

/// The data a copy would place on the other side, for a preview.
#[must_use]
pub fn preview_value(file: &RegFile, key_path: &str, name: &ValueName) -> Option<ValueData> {
    file.key(key_path)
        .and_then(|block| {
            block
                .entries
                .iter()
                .find(|entry| names_equal(&entry.name, name))
        })
        .and_then(|entry| entry.data.clone())
}

/// The blocks of `file` at and under `key_path`, as a file of their own.
///
/// This is what an export of one key writes.
#[must_use]
pub fn subtree(file: &RegFile, key_path: &str) -> RegFile {
    RegFile {
        version: file.version,
        keys: file
            .keys
            .iter()
            .filter(|block| is_within(&block.path, key_path))
            .cloned()
            .collect(),
    }
}

/// One write to the live registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveStep {
    /// Delete a key and everything under it.
    DeleteKey {
        /// Full key path.
        key_path: String,
    },
    /// Create a key, and any missing key above it.
    CreateKey {
        /// Full key path.
        key_path: String,
    },
    /// Write one value.
    SetValue {
        /// Full key path that holds the value.
        key_path: String,
        /// Name of the value.
        value_name: ValueName,
        /// The data written.
        data: ValueData,
    },
    /// Delete one value.
    DeleteValue {
        /// Full key path that holds the value.
        key_path: String,
        /// Name of the value.
        value_name: ValueName,
    },
}

impl LiveStep {
    /// The key the step writes to.
    #[must_use]
    pub fn key_path(&self) -> &str {
        match self {
            Self::DeleteKey { key_path }
            | Self::CreateKey { key_path }
            | Self::SetValue { key_path, .. }
            | Self::DeleteValue { key_path, .. } => key_path,
        }
    }
}

/// How many keys and values a list of steps changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StepCounts {
    /// Keys created.
    pub keys_created: usize,
    /// Keys deleted, each with everything under it.
    pub keys_deleted: usize,
    /// Values written.
    pub values_set: usize,
    /// Values deleted.
    pub values_deleted: usize,
}

impl StepCounts {
    /// Count the steps of each kind.
    #[must_use]
    pub fn of(steps: &[LiveStep]) -> Self {
        let mut counts = Self::default();
        for step in steps {
            let slot = match step {
                LiveStep::CreateKey { .. } => &mut counts.keys_created,
                LiveStep::DeleteKey { .. } => &mut counts.keys_deleted,
                LiveStep::SetValue { .. } => &mut counts.values_set,
                LiveStep::DeleteValue { .. } => &mut counts.values_deleted,
            };
            *slot = slot.saturating_add(1);
        }
        counts
    }

    /// Every step counted.
    #[must_use]
    pub const fn total(self) -> usize {
        self.keys_created
            .saturating_add(self.keys_deleted)
            .saturating_add(self.values_set)
            .saturating_add(self.values_deleted)
    }
}

/// The keys a model holds, by lower case path, and the values of each.
type KeyStates = BTreeMap<String, (String, BTreeMap<String, (ValueName, ValueData)>)>;

/// Read a model as the state it leaves behind: a deletion block takes the key
/// and everything under it away, a deletion entry takes the value away.
fn key_states(file: &RegFile) -> KeyStates {
    let mut states = KeyStates::new();
    for block in &file.keys {
        if block.delete {
            states.retain(|_, (path, _)| !is_within(path, &block.path));
            continue;
        }
        let (_, values) = states
            .entry(block.path.to_ascii_lowercase())
            .or_insert_with(|| (block.path.clone(), BTreeMap::new()));
        for entry in &block.entries {
            let name = entry.name.raw().to_ascii_lowercase();
            match &entry.data {
                Some(data) => {
                    values.insert(name, (entry.name.clone(), data.clone()));
                }
                None => {
                    values.remove(&name);
                }
            }
        }
    }
    states
}

/// The writes that turn the registry state `before` describes into the one
/// `after` describes.
///
/// Key deletions come first, then key creations parent first, then value
/// writes, then value deletions. A deleted key is named once, at the top of
/// what goes. A key that `after` holds under a deleted key is created again
/// with all its values, so the order never loses it.
#[must_use]
pub fn live_steps(before: &RegFile, after: &RegFile) -> Vec<LiveStep> {
    let old = key_states(before);
    let new = key_states(after);
    let deleted: BTreeSet<&String> = old.keys().filter(|key| !new.contains_key(*key)).collect();
    let under_deleted = |key: &str| {
        let mut current = key;
        while let Some((parent, _)) = current.rsplit_once('\\') {
            if deleted.contains(&parent.to_owned()) {
                return true;
            }
            current = parent;
        }
        false
    };
    let mut steps = Vec::new();
    for key in &deleted {
        if under_deleted(key) {
            continue;
        }
        if let Some((path, _)) = old.get(*key) {
            steps.push(LiveStep::DeleteKey {
                key_path: path.clone(),
            });
        }
    }
    let fresh = |key: &str| !old.contains_key(key) || under_deleted(key);
    for (key, (path, _)) in &new {
        if fresh(key) {
            steps.push(LiveStep::CreateKey {
                key_path: path.clone(),
            });
        }
    }
    for (key, (path, values)) in &new {
        let previous = if fresh(key) {
            None
        } else {
            old.get(key).map(|(_, values)| values)
        };
        for (name, (value_name, data)) in values {
            let same = previous
                .and_then(|values| values.get(name))
                .is_some_and(|(_, held)| held == data);
            if !same {
                steps.push(LiveStep::SetValue {
                    key_path: path.clone(),
                    value_name: value_name.clone(),
                    data: data.clone(),
                });
            }
        }
    }
    for (key, (path, values)) in &new {
        if fresh(key) {
            continue;
        }
        let Some((_, previous)) = old.get(key) else {
            continue;
        };
        for (name, (value_name, _)) in previous {
            if !values.contains_key(name) {
                steps.push(LiveStep::DeleteValue {
                    key_path: path.clone(),
                    value_name: value_name.clone(),
                });
            }
        }
    }
    steps
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        is_within, live_steps, subtree, DeleteForm, EditOp, EditPlan, LiveStep, Side, StepCounts,
    };
    use crate::limits::Limits;
    use crate::registry::reg_file::RegFile;
    use crate::registry::value::{ValueData, ValueName};
    use crate::RecordError;

    const HEADER: &str = "Windows Registry Editor Version 5.00\r\n";

    fn file(body: &str) -> RegFile {
        RegFile::parse_text(&format!("{HEADER}{body}"), &Limits::default()).unwrap()
    }

    fn apply(plan: &EditPlan, left: &mut RegFile, right: &mut RegFile) -> Result<(), RecordError> {
        plan.apply_to_files_with(left, right, DeleteForm::Remove)
    }

    fn name(text: &str) -> ValueName {
        ValueName::from_raw(text)
    }

    #[test]
    fn a_path_is_within_itself_and_its_parent_only_at_a_segment_boundary() {
        assert!(is_within(r"HKCU\A", r"hkcu\a"));
        assert!(is_within(r"HKCU\A\B", r"HKCU\A"));
        assert!(!is_within(r"HKCU\AB", r"HKCU\A"));
        assert!(!is_within(r"HKCU", r"HKCU\A"));
    }

    #[test]
    fn a_removed_key_takes_every_block_under_it_away() {
        let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n\r\n[HKEY_CURRENT_USER\\A\\B]\r\n\r\n[HKEY_CURRENT_USER\\AB]\r\n");
        let mut right = RegFile::empty(left.version);
        let plan = EditPlan::new().with(EditOp::delete_key(Side::Left, "HKEY_CURRENT_USER\\A"));
        apply(&plan, &mut left, &mut right).unwrap();
        assert_eq!(left.keys.len(), 1);
        assert_eq!(left.keys[0].path, "HKEY_CURRENT_USER\\AB");
        assert!(!left.to_text().contains("[-"));
        let again = apply(&plan, &mut left, &mut right).unwrap_err();
        assert!(matches!(again, RecordError::NotFound { .. }));
    }

    #[test]
    fn a_removed_value_leaves_no_deletion_entry() {
        let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n\"M\"=\"w\"\r\n");
        let mut right = RegFile::empty(left.version);
        let plan = EditPlan::new().with(EditOp::delete_value(
            Side::Left,
            "HKEY_CURRENT_USER\\A",
            name("n"),
        ));
        apply(&plan, &mut left, &mut right).unwrap();
        assert_eq!(left.keys[0].entries.len(), 1);
        assert!(!left.to_text().contains("=-"));
    }

    #[test]
    fn a_new_key_and_a_new_value_appear_and_a_taken_name_is_refused() {
        let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n");
        let mut right = RegFile::empty(left.version);
        let plan = EditPlan::new()
            .with(EditOp::create_key(Side::Left, "HKEY_CURRENT_USER\\A\\New"))
            .with(EditOp::set_value(
                Side::Left,
                "HKEY_CURRENT_USER\\A\\New",
                name("Count"),
                &ValueData::Dword(7),
            ));
        apply(&plan, &mut left, &mut right).unwrap();
        assert!(left.to_text().contains("\"Count\"=dword:00000007"));
        let twice = EditPlan::new().with(EditOp::create_key(Side::Left, "HKEY_CURRENT_USER\\A"));
        assert!(matches!(
            apply(&twice, &mut left, &mut right),
            Err(RecordError::AlreadyExists { .. })
        ));
        let empty_segment =
            EditPlan::new().with(EditOp::create_key(Side::Left, "HKEY_CURRENT_USER\\\\X"));
        assert!(matches!(
            apply(&empty_segment, &mut left, &mut right),
            Err(RecordError::InvalidSpec(_))
        ));
    }

    #[test]
    fn a_renamed_key_moves_its_subtree_and_a_renamed_value_keeps_its_data() {
        let mut left = file(
            "\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n\r\n[HKEY_CURRENT_USER\\A\\B]\r\n\"C\"=\"c\"\r\n",
        );
        let mut right = RegFile::empty(left.version);
        let plan = EditPlan::new()
            .with(EditOp::rename_key(Side::Left, "HKEY_CURRENT_USER\\A", "Z"))
            .with(EditOp::rename_value(
                Side::Left,
                "HKEY_CURRENT_USER\\Z",
                name("N"),
                name("Renamed"),
            ));
        apply(&plan, &mut left, &mut right).unwrap();
        assert!(left.key("HKEY_CURRENT_USER\\Z\\B").is_some());
        assert!(left.key("HKEY_CURRENT_USER\\A").is_none());
        let text = left.to_text();
        assert!(text.contains("\"Renamed\"=\"v\""), "{text}");
        let clash = EditPlan::new().with(EditOp::rename_key(
            Side::Left,
            "HKEY_CURRENT_USER\\Z\\B",
            "B",
        ));
        assert!(apply(&clash, &mut left, &mut right).is_err());
        let hive = EditPlan::new().with(EditOp::rename_key(Side::Left, "HKEY_CURRENT_USER", "X"));
        assert!(matches!(
            apply(&hive, &mut left, &mut right),
            Err(RecordError::Refused(_))
        ));
    }

    #[test]
    fn the_mark_form_of_a_rename_exports_a_deletion_and_a_new_key() {
        let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n");
        let mut right = RegFile::empty(left.version);
        let plan =
            EditPlan::new().with(EditOp::rename_key(Side::Left, "HKEY_CURRENT_USER\\A", "Z"));
        plan.apply_to_files(&mut left, &mut right).unwrap();
        let text = left.to_text();
        assert!(text.contains("[-HKEY_CURRENT_USER\\A]"));
        assert!(text.contains("[HKEY_CURRENT_USER\\Z]\r\n\"N\"=\"v\""));
    }

    #[test]
    fn the_mark_form_of_a_key_deletion_takes_the_child_blocks_away() {
        let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n\r\n[HKEY_CURRENT_USER\\A\\B]\r\n\"M\"=\"w\"\r\n\r\n[HKEY_CURRENT_USER\\AB]\r\n");
        let mut right = RegFile::empty(left.version);
        let plan = EditPlan::new().with(EditOp::delete_key(Side::Left, "HKEY_CURRENT_USER\\A"));
        plan.apply_to_files(&mut left, &mut right).unwrap();
        let text = left.to_text();
        assert!(text.contains("[-HKEY_CURRENT_USER\\A]"));
        assert!(!text.contains("[HKEY_CURRENT_USER\\A\\B]"));
        assert!(text.contains("[HKEY_CURRENT_USER\\AB]"));
    }

    #[test]
    fn a_copied_value_lands_in_a_block_that_sets_rather_than_deletes() {
        let mut left = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\"\r\n");
        let mut right = file("\r\n[-HKEY_CURRENT_USER\\A]\r\n");
        let plan = EditPlan::new().with(EditOp::copy_value(
            Side::Left,
            "HKEY_CURRENT_USER\\A",
            name("N"),
        ));
        plan.apply_to_files(&mut left, &mut right).unwrap();
        let text = right.to_text();
        assert!(text.contains("[-HKEY_CURRENT_USER\\A]"));
        assert!(text.contains("[HKEY_CURRENT_USER\\A]\r\n\"N\"=\"v\""));
    }

    #[test]
    fn a_plan_of_every_new_operation_survives_a_round_trip() {
        let plan = EditPlan::new()
            .with(EditOp::create_key(Side::Right, "HKEY_CURRENT_USER\\K"))
            .with(EditOp::set_value(
                Side::Right,
                "HKEY_CURRENT_USER\\K",
                ValueName::Default,
                &ValueData::Binary(vec![1, 2]),
            ))
            .with(EditOp::rename_key(Side::Left, "HKEY_CURRENT_USER\\K", "L"))
            .with(EditOp::rename_value(
                Side::Left,
                "HKEY_CURRENT_USER\\K",
                name("a"),
                name("b"),
            ));
        let text = serde_json::to_string(&plan).unwrap();
        let back: EditPlan = serde_json::from_str(&text).unwrap();
        assert_eq!(back, plan);
        assert_eq!(plan.ops[0].target(), Some(Side::Right));
        assert_eq!(
            EditOp::copy_key(Side::Left, "K").target(),
            Some(Side::Right)
        );
    }

    #[test]
    fn the_steps_between_two_states_name_only_what_changed() {
        let before = file(
            "\r\n[HKEY_CURRENT_USER\\T]\r\n\"Keep\"=\"k\"\r\n\"Change\"=\"old\"\r\n\"Drop\"=\"d\"\r\n\r\n[HKEY_CURRENT_USER\\T\\Gone]\r\n\r\n[HKEY_CURRENT_USER\\T\\Gone\\Deep]\r\n",
        );
        let after = file(
            "\r\n[HKEY_CURRENT_USER\\T]\r\n\"Keep\"=\"k\"\r\n\"Change\"=\"new\"\r\n\r\n[HKEY_CURRENT_USER\\T\\Fresh]\r\n\"V\"=dword:00000001\r\n",
        );
        let steps = live_steps(&before, &after);
        assert_eq!(
            steps,
            vec![
                LiveStep::DeleteKey {
                    key_path: "HKEY_CURRENT_USER\\T\\Gone".to_owned()
                },
                LiveStep::CreateKey {
                    key_path: "HKEY_CURRENT_USER\\T\\Fresh".to_owned()
                },
                LiveStep::SetValue {
                    key_path: "HKEY_CURRENT_USER\\T".to_owned(),
                    value_name: name("Change"),
                    data: ValueData::Sz("new".to_owned()),
                },
                LiveStep::SetValue {
                    key_path: "HKEY_CURRENT_USER\\T\\Fresh".to_owned(),
                    value_name: name("V"),
                    data: ValueData::Dword(1),
                },
                LiveStep::DeleteValue {
                    key_path: "HKEY_CURRENT_USER\\T".to_owned(),
                    value_name: name("Drop"),
                },
            ]
        );
        let counts = StepCounts::of(&steps);
        assert_eq!(counts.total(), 5);
        assert_eq!(counts.keys_deleted, 1);
        assert!(live_steps(&after, &after).is_empty());
    }

    #[test]
    fn a_key_kept_under_a_deleted_key_is_created_again_with_its_values() {
        let before =
            file("\r\n[HKEY_CURRENT_USER\\T]\r\n\r\n[HKEY_CURRENT_USER\\T\\In]\r\n\"V\"=\"v\"\r\n");
        let after = file("\r\n[HKEY_CURRENT_USER\\T\\In]\r\n\"V\"=\"v\"\r\n");
        let steps = live_steps(&before, &after);
        assert_eq!(steps.len(), 3);
        assert!(
            matches!(&steps[0], LiveStep::DeleteKey { key_path } if key_path == "HKEY_CURRENT_USER\\T")
        );
        assert!(
            matches!(&steps[1], LiveStep::CreateKey { key_path } if key_path == "HKEY_CURRENT_USER\\T\\In")
        );
        assert!(matches!(&steps[2], LiveStep::SetValue { .. }));
    }

    #[test]
    fn an_export_of_one_key_holds_that_key_and_the_keys_under_it() {
        let whole = file("\r\n[HKEY_CURRENT_USER\\A]\r\n\r\n[HKEY_CURRENT_USER\\A\\B]\r\n\r\n[HKEY_CURRENT_USER\\C]\r\n");
        let part = subtree(&whole, "HKEY_CURRENT_USER\\A");
        assert_eq!(part.keys.len(), 2);
        assert_eq!(part.version, whole.version);
    }
}
