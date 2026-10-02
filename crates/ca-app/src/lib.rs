//! The desktop shell: the window, the tab strip, the menus, the launcher and
//! the registry that decides which view crate answers for a session kind.
//!
//! Everything a view needs and every view share lives in `ca-ui`. Each
//! comparison type is its own crate, reached only through
//! [`ca_ui::view::SessionView`] and [`registry`].

pub mod cli;
pub mod explorer;
pub mod home;
pub mod icon;
pub mod profiles;
pub mod registry;
mod session_save;
pub mod shell;
pub mod tree;
pub mod update;

pub use shell::{App, VERSION};
