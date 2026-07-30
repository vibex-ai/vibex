//! Framework-neutral desktop view models and persistence contracts.
//!
//! This crate deliberately has no GPUI, Tauri, Tokio, database, or native
//! surface dependency. Callers inject time and identifiers at reducer edges.

mod agent_workbench;
mod composer;
mod content_preview;
mod diff;
mod editor;
mod file_tree;
mod git_workbench;
mod management;
mod navigation;
mod polling;
mod preview;
mod projection;
mod query;
mod runtime;
mod timeline;
mod ui_state;

pub use agent_workbench::*;
pub use composer::*;
pub use content_preview::*;
pub use diff::*;
pub use editor::*;
pub use file_tree::*;
pub use git_workbench::*;
pub use management::*;
pub use navigation::*;
pub use polling::*;
pub use preview::*;
pub use projection::*;
pub use query::*;
pub use runtime::*;
pub use timeline::*;
pub use ui_state::*;
