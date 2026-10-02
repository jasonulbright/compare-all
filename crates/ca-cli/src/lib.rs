//! The `ca` command line.
//!
//! [`args::parse`] reads a command line into an [`args::Invocation`]. The
//! console program runs a script or a quick comparison itself; a command line
//! that asks for a view becomes a [`request::DesktopRequest`], which the
//! desktop program reads when it adopts this parser.
//!
//! Exit codes are in [`exit`], and every one of them is listed in the usage
//! summary [`help::text`] writes.

pub mod args;
pub mod exit;
pub mod help;
pub mod profiles;
pub mod quick;
pub mod request;
pub mod script;

pub use args::{parse, ArgError, Invocation, QuickKind, QuickRun, ScriptRun};
pub use profiles::StoredProfiles;
pub use request::{AutoMerge, DesktopRequest, Pane, ReadOnly};
