//! Error type for the computer-use subsystem.
//!
//! `Debug` output never carries typed text, clipboard contents, accessibility
//! tree bodies or screenshot bytes; diagnostics are key/value pairs chosen by
//! the caller.

use vibex_core::{ErrorCategory, VibexError};

pub type ComputerResult<T> = Result<T, ComputerError>;

/// Codes callers branch on. They are part of the tool contract: a model sees
/// the code, and the runtime's approval path keys off it.
pub mod codes {
    /// The user denied the approval card.
    pub const DENIED: &str = "computer_permission_denied";
    /// The approval policy refuses every approval-requiring action.
    pub const APPROVAL_POLICY_DENIED: &str = "computer_approval_policy_denied";
    /// The action class is refused outright and no approval can override it.
    pub const HARD_DENIED: &str = "computer_action_blocked";
    /// The emergency stop is in force.
    pub const STOPPED_BY_USER: &str = "computer_stopped_by_user";
    /// The human paused Agent operations.
    pub const PAUSED_BY_USER: &str = "computer_paused_by_user";
    /// The control channel went silent.
    pub const PAUSED_OFFLINE: &str = "computer_paused_offline";
    /// The engine asked for the foreground and the user did not authorize it.
    pub const FOREGROUND_REQUIRED: &str = "computer_foreground_required";
    /// A credential-bearing target.
    pub const CREDENTIAL_TARGET: &str = "computer_credential_target_blocked";
    /// The target resolves to the Vibex window itself.
    pub const SELF_TARGET: &str = "computer_self_target_blocked";
    /// The human appears to be using the target window.
    pub const CONCURRENT_USER: &str = "computer_concurrent_user_activity";
    /// An element reference outlived its observation.
    pub const STALE_REFERENCE: &str = "computer_element_reference_stale";
    /// The engine will not act on an element, and observing again will not
    /// change that.
    ///
    /// Distinct from [`STALE_REFERENCE`] on purpose: a stale reference is fixed
    /// by observing again, while a window whose accessibility identity the
    /// engine cannot prove has nothing to observe — it issues no handles at
    /// all, or keeps no snapshot for the ones it does issue. Reporting both as
    /// "stale" is how a model is sent into an observe/act/fail loop.
    pub const ELEMENT_NOT_ADDRESSABLE: &str = "computer_element_not_addressable";
    /// The observation digest no longer matches the live tree.
    pub const STALE_TREE: &str = "computer_tree_changed";
    /// The tier does not include screenshots.
    pub const SCREENSHOT_TIER: &str = "computer_screenshot_tier_unavailable";
    /// The app identity is ambiguous or unknown.
    pub const UNKNOWN_APP: &str = "computer_application_not_found";
    /// The engine is not installed.
    pub const ENGINE_MISSING: &str = "computer_engine_missing";
    /// No operable desktop session.
    pub const NO_DESKTOP: &str = "computer_no_desktop_session";
    /// The accessibility bridge is missing.
    pub const A11Y_MISSING: &str = "computer_accessibility_bridge_missing";
    /// The platform is unsupported.
    pub const PLATFORM_UNSUPPORTED: &str = "computer_platform_unsupported";
    /// The helper refused or dropped the request.
    pub const HELPER_FAILED: &str = "computer_helper_failed";
    /// The engine answered, and its answer was a refusal.
    ///
    /// Distinct from [`HELPER_FAILED`] on purpose: a refused call carries the
    /// engine's own reason, while a broken channel has none. Collapsing the two
    /// is how a precise "unknown argument `app`" becomes an unreadable
    /// "the helper failed".
    pub const DRIVER_REFUSED: &str = "computer_driver_refused";
    /// The engine reported the background delivery unavailable.
    pub const BACKGROUND_UNAVAILABLE: &str = "computer_background_unavailable";
    /// The feature is switched off.
    pub const FEATURE_DISABLED: &str = "computer_feature_disabled";
    /// A remote runtime host, where the live desktop hop is not implemented.
    pub const REMOTE_UNSUPPORTED: &str = "computer_remote_runtime_unsupported";
    /// The runtime refused to start the helper because it would run as root.
    pub const RUNNING_AS_ROOT: &str = "computer_running_as_root";
}

/// A computer-use subsystem failure.
///
/// Mirrors [`VibexError`] so it converts losslessly at the runtime boundary
/// while letting the crate stay free of the runtime's error plumbing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ComputerError {
    pub category: ErrorCategory,
    pub code: String,
    pub message: String,
    pub recovery_hint: Option<Box<str>>,
    pub diagnostics: Vec<(String, String)>,
}

impl ComputerError {
    pub fn new(
        category: ErrorCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            code: code.into(),
            message: message.into(),
            recovery_hint: None,
            diagnostics: Vec::new(),
        }
    }

    pub fn process(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Process, code, message)
    }

    pub fn validation(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Validation, code, message)
    }

    pub fn capability(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Capability, code, message)
    }

    pub fn permission(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Permission, code, message)
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Conflict, code, message)
    }

    pub fn storage(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Storage, code, message)
    }

    pub fn with_recovery_hint(mut self, hint: impl Into<String>) -> Self {
        self.recovery_hint = Some(hint.into().into_boxed_str());
        self
    }

    pub fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.diagnostics.push((key.into(), value.into()));
        self
    }

    pub fn is_code(&self, code: &str) -> bool {
        self.code == code
    }

    /// True when the failure says this machine cannot run computer use at all.
    ///
    /// Only these codes may be reported as an unavailable capability. A helper
    /// that crashed or an engine call that timed out is a defect and must
    /// surface: treating every failure as "the platform does not support it" is
    /// how a broken engine path ships looking like a missing feature.
    pub fn is_unavailable_here(&self) -> bool {
        matches!(
            self.code.as_str(),
            codes::ENGINE_MISSING
                | codes::NO_DESKTOP
                | codes::A11Y_MISSING
                | codes::PLATFORM_UNSUPPORTED
                | codes::FEATURE_DISABLED
                | codes::RUNNING_AS_ROOT
                | codes::REMOTE_UNSUPPORTED
        )
    }

    /// True when the failure means "ask the human and retry with the answer".
    pub fn is_approval_required(&self) -> bool {
        matches!(
            self.code.as_str(),
            codes::FOREGROUND_REQUIRED
                | codes::HARD_DENIED
                | codes::DENIED
                | codes::CONCURRENT_USER
        )
    }

    /// True when the model should observe again rather than retry the action.
    pub fn is_stale_state(&self) -> bool {
        matches!(
            self.code.as_str(),
            codes::STALE_REFERENCE | codes::STALE_TREE
        )
    }

    /// The delivery mode an approval-required failure was asking for, when the
    /// failure came from a background attempt that needs the foreground.
    pub fn requested_foreground(&self) -> bool {
        self.code == codes::FOREGROUND_REQUIRED
    }
}

impl From<ComputerError> for VibexError {
    fn from(error: ComputerError) -> Self {
        let mut converted = VibexError::new(error.category, error.code, error.message);
        converted.recovery_hint = error.recovery_hint;
        converted.diagnostics = error
            .diagnostics
            .into_iter()
            .map(|(key, value)| vibex_core::RedactedDiagnostic { key, value })
            .collect();
        converted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_capability_failures_count_as_unavailable_here() {
        assert!(
            ComputerError::capability(codes::ENGINE_MISSING, "no engine").is_unavailable_here()
        );
        assert!(
            ComputerError::capability(codes::PLATFORM_UNSUPPORTED, "no platform")
                .is_unavailable_here()
        );
        assert!(!ComputerError::process(codes::HELPER_FAILED, "died").is_unavailable_here());
        assert!(
            !ComputerError::process("computer_engine_timeout", "timed out").is_unavailable_here()
        );
    }

    #[test]
    fn approval_and_staleness_codes_are_classified() {
        assert!(ComputerError::permission(codes::FOREGROUND_REQUIRED, "fg").is_approval_required());
        assert!(ComputerError::permission(codes::CONCURRENT_USER, "busy").is_approval_required());
        assert!(ComputerError::validation(codes::STALE_REFERENCE, "stale").is_stale_state());
        assert!(!ComputerError::validation(codes::UNKNOWN_APP, "nope").is_stale_state());
        assert!(ComputerError::permission(codes::FOREGROUND_REQUIRED, "fg").requested_foreground());
    }

    #[test]
    fn conversion_keeps_the_code_and_hint() {
        let error = ComputerError::capability(codes::ENGINE_MISSING, "missing")
            .with_recovery_hint("install it")
            .with_diagnostic("path", "/usr/bin/cua-driver");
        let converted: VibexError = error.into();
        assert_eq!(converted.code, codes::ENGINE_MISSING);
        assert_eq!(converted.recovery_hint.as_deref(), Some("install it"));
        assert_eq!(converted.diagnostics.len(), 1);
    }
}
