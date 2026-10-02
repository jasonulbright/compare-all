//! The session settings dialog and the field model behind it.
//!
//! The settings of a session are typed data in `ca-session`. This module turns
//! them into pages a person edits: one description of the fields, one dialog
//! that draws any kind, and the layer arithmetic that decides which values a
//! session states for itself and which it inherits.

pub mod dialog;
pub mod field;
pub mod schema;

pub use dialog::{Scope, SettingsDialog, SettingsOutcome};
pub use field::{Choice, Field, FieldShape, FieldValue};
pub use schema::{fields_for, is_inherited, override_against, reset_to, tabs_for, Tab};
