//! Embedded browser domain service.
//!
//! The embedded browser is a **tool panel** backed by the system
//! Chrome/Chromium/Edge through the Chrome DevTools Protocol — not an
//! application shell and not a WebUI product.
//!
//! Ownership follows the runtime-is-authority invariant: this crate owns the
//! browser process, the single CDP connection, tab/ref/generation state, the
//! screencast frame slots, the policy decisions and the audit ledger. Clients
//! subscribe through `BrowserBackend`; they never hold a CDP connection.
//!
//! Two independent channels read the same target:
//!
//! * humans watch `Page.startScreencast` frames rendered as textures;
//! * agents read `Accessibility.getFullAXTree` snapshots.
//!
//! A third, always-on channel records console output and failed network
//! requests, because for a coding agent the diagnostics are often more useful
//! than the page structure.

pub mod ax;
pub mod cdp;
pub mod devserver;
pub mod discovery;
pub mod element_source;
pub mod error;
pub mod execute;
pub mod http;
pub mod mcp;
pub mod policy;
pub mod process;
pub mod recording;
pub mod service;
pub mod stdio;
pub mod tools;
pub mod visual;

pub use error::{BrowserError, BrowserResult};
pub use http::BrowserMcpEndpoint;
pub use mcp::{
    BROWSER_MCP_SERVER_ID, BrowserMcpHandler, BrowserMcpHost, BrowserMcpSession,
    BrowserPermissionDecision, issue_session_token, verify_session_token,
};
pub use service::{
    BrowserElementInspection, BrowserFrameSubscription, BrowserInput, BrowserSelectMenu,
    BrowserSelectOption, BrowserService, BrowserServiceConfig, BrowserServiceEvent,
    BrowserSessionKey, BrowserToolContext, BrowserToolOutcome,
};
