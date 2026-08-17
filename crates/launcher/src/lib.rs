//! Launcher application services shared by the GUI and narrow lifecycle host.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

mod preferences;

pub mod remote_lifecycle;

pub use preferences::{LauncherPreferences, MAX_RECENT_PROJECTS};
