//! The application options: the document handle, the dialog and the resolved
//! values a frame runs under.
//!
//! The options of the program are typed data in `ca-session`. This module turns
//! them into pages a person edits, reads and writes the document off the frame
//! thread, and resolves the stored values into the color tables, the routing
//! table and the point sizes the views read.

pub mod dialog;
pub mod handle;
pub mod restore;
pub mod runtime;
pub mod schema;

pub use dialog::{OptionsDialog, OptionsOutcome};
pub use handle::{OptionsHandle, OptionsMessage, OptionsState};
pub use restore::RestoreDialog;
pub use runtime::{current, install, AppOptions};
pub use schema::{OptionField, OptionPage, PageKind, PAGES};
