//! Offline Claude transcript import and parity replay support.

pub mod import;
#[doc(hidden)]
pub mod parity;

pub use import::{
    ClaudeSessionImportPreviewRequest, claude_external_session_roots,
    import_selected_claude_sessions, preview_claude_external_sessions,
    scan_claude_external_sessions,
};
