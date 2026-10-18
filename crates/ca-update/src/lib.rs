//! Update check, download, verification, staging and apply.
//!
//! Nothing here runs unless the application calls it, and nothing here
//! registers a task, service or startup entry with the system.

pub mod apply;
pub mod flow;
pub mod install;
pub mod manifest;
pub mod mode;
pub mod net;
pub mod signature;
pub mod staging;
pub mod system;
pub mod verify;
mod version;

pub use version::{InstallerVersion, Version};
