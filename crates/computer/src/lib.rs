//! Computer use: let an Agent read and drive the desktop of the machine the
//! runtime runs on.
//!
//! The runtime owns everything with authority: the engine helper process, the
//! accessibility observations and their reference lifecycle, the risk policy,
//! the approval flow, the action ledger and the emergency stop. Clients
//! subscribe to frames and ask for state; they never hold an engine connection
//! and never speak to a desktop API themselves.
//!
//! Two channels read the same desktop:
//!
//! * **humans** watch encoded frames rendered as GPUI textures, with the
//!   emergency stop beside them;
//! * **agents** read the accessibility tree through the eight MCP tools.
//!
//! They are separate on purpose — and for this feature the separation is load
//! bearing, because the runtime may not hand a screenshot to an Agent whose
//! adapter has never been observed to forward image content to its model. The
//! human is often the only party that can see the pixels.
//!
//! ## What is explicit rather than silent
//!
//! * A missing engine, desktop session, accessibility bridge or OS permission
//!   is a named [`vibex_core::ComputerUnavailableReason`], never an empty
//!   element list.
//! * A stopped or disconnected session answers with its own error code, not a
//!   timeout.
//! * An action the runtime did not verify is reported as
//!   [`vibex_core::ComputerVerification::Unverified`], never as success.
//! * The remote desktop hop is a separate contract with its own degradation;
//!   the loopback MCP endpoint is a localhost surface and is not it.

pub mod cli;
pub mod doctor;
pub mod driver;
pub mod engine;
pub mod error;
pub mod fixture;
pub mod helper;
pub mod http;
pub mod loopguard;
pub mod mcp;
pub mod policy;
pub mod screenshot;
pub mod selfguard;
pub mod service;
pub mod stdio;
pub mod tools;

pub use driver::{CuaDriverCli, DRIVER_COMMAND, DRIVER_PATH_ENV};
pub use engine::{
    ComputerEngine, EngineActionResult, EngineAppState, EngineClick, EngineDelivery, EngineElement,
    EnginePressKey, EngineProbe, EngineScroll, EngineSetValue, EngineStateRequest, EngineTypeText,
};
pub use error::{ComputerError, ComputerResult};
pub use helper::{
    HELPER_DRIVER_ENV, HELPER_OWNER_FILE_ENV, HELPER_PARENT_PID_ENV, HELPER_TOKEN_ENV,
    HelperEngine, HelperOwnerLock, HelperState, run_helper_with_engine,
};
pub use http::{ComputerMcpEndpoint, forward_to_endpoint};
pub use loopguard::{LoopGuard, LoopWarning, fingerprint_bytes};
pub use mcp::{
    COMPUTER_MCP_SERVER_ID, COMPUTER_TOKEN_PREFIX, ComputerMcpHandler, ComputerMcpHost,
    ComputerMcpSession, ComputerPermissionDecision, issue_session_token, verify_session_token,
};
pub use policy::{GrantStore, PolicyOutcome, PolicyRequest};
pub use screenshot::{
    BudgetVerdict, WrittenScreenshot, plan as plan_screenshot, write_screenshot_file,
};
pub use selfguard::SelfTargetGuard;
pub use service::{
    ApprovedAction, ComputerApprovalRequest, ComputerImageContent, ComputerService,
    ComputerServiceConfig, ComputerServiceEvent, ComputerSessionKey, ComputerToolContext,
    ComputerToolOutcome,
};
pub use tools::{initialize_instructions, tools_list_payload};
