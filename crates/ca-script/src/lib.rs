//! Script language parser and executor.
//!
//! A script is a plain text file with one command per line. [`parse`] turns the
//! text into a typed [`ast::Script`]; [`exec`] runs one against a session state
//! that holds the base folders, the comparison, the selection, the criteria,
//! the filters and the log.
//!
//! # Shape
//!
//! - [`lex`] cuts the text into logical lines of tokens, joining a line that
//!   ends with `&` to the next one and dropping everything after an unquoted
//!   `#`.
//! - [`subst`] replaces `%1` through `%9`, environment variables and the
//!   `date`, `time` and `fn_time` values.
//! - [`parse`] reads the tokens into commands with source positions.
//! - [`text`] writes a command back out as script text.
//! - [`state`] holds what the commands read and change.
//! - [`exec`] runs the commands.
//!
//! # Destructive commands
//!
//! Every command that writes to disk builds a plan with `ca_fs` and runs it
//! through the journalling executor, so an interrupted run leaves a record that
//! names the step that was in flight. A run in dry run mode builds the same
//! plans and executes none of them.

pub mod ast;
pub mod clock;
pub mod error;
pub mod exec;
pub mod lex;
pub mod log;
pub mod parse;
pub mod paths;
pub mod profiles;
pub mod report;
pub mod rules;
pub mod state;
pub mod subst;
pub mod text;

pub use ast::{Command, Script, Span, Statement};
pub use error::{EncodeError, ExecError, ParseError, RunError};
pub use exec::{run, run_script, Outcome, StepReport};
pub use parse::{parse, parse_with};
pub use profiles::{is_remote, NoProfiles, NoStore, ProfileLookup};
pub use state::{Session, SessionOptions};
pub use subst::Substitution;
pub use text::{encode, encode_script};
