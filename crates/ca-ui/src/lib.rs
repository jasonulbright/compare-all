//! Everything the comparison views share, and nothing specific to one of them.
//!
//! The crate holds the worker job system, the [`view::SessionView`] trait a tab
//! is reached through, the command vocabulary and its keyboard routing, the one
//! color table, the shared widgets, the scroll model, the editing model of one
//! pane, the find and go to strips, the display filter result, the re-diff
//! scheduler and the window icon.
//!
//! A view crate depends on this crate. No view crate depends on another view
//! crate: a view that wants a different comparison opened returns a
//! [`view::OpenRequest`] and the shell constructs it.

pub mod clipboard;
pub mod command;
pub mod dialog;
pub mod editor;
pub mod filter;
pub mod find;
pub mod font;
pub mod format;
pub mod icon;
pub mod icons;
pub mod launch;
pub mod options;
pub mod paths;
pub mod rediff;
pub mod report;
pub mod save;
pub mod schedule;
pub mod scroll;
pub mod sessions;
pub mod settings;
pub mod share;
pub mod testing;
pub mod theme;
pub mod thumbnail;
pub mod toolbar;
pub mod view;
pub mod widgets;
pub mod worker;
pub mod workspace;

pub use command::{Command, Keystroke, Menu};
pub use view::{CommandState, OpenRequest, SessionView, ViewAction, ViewContext};
