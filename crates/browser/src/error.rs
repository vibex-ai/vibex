//! Error type for the embedded browser subsystem.
//!
//! `Debug` output never carries page content, frame bytes, form values or full
//! URLs; diagnostics are key/value pairs chosen by the caller.

use vibex_core::{ErrorCategory, VibexError};

pub type BrowserResult<T> = Result<T, BrowserError>;

/// A browser-subsystem failure.
///
/// This mirrors [`VibexError`] so it converts losslessly at the runtime
/// boundary while letting the crate stay free of the runtime's error plumbing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct BrowserError {
    pub category: ErrorCategory,
    pub code: String,
    pub message: String,
    pub recovery_hint: Option<Box<str>>,
    pub diagnostics: Vec<(String, String)>,
}

impl BrowserError {
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

    pub fn cdp(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Process, code, message)
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

    pub fn timeout(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Process, code, message)
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

    /// True when the failure says this machine has no usable browser.
    ///
    /// This is the only condition a caller may report as "the embedded browser
    /// is unavailable here". Every other failure — a spawn that failed, a CDP
    /// command that timed out, a transport that broke — is a defect and has to
    /// surface: treating them all as a missing browser is how a broken Windows
    /// transport stayed hidden behind tests that skipped themselves.
    pub fn is_browser_missing(&self) -> bool {
        self.code == "browser_unavailable" || self.code == "browser_feature_disabled"
    }
}

/// Raised when an agent action is stopped by a human taking over.
///
/// Already-issued DevTools commands cannot be revoked; this error only promises
/// that no further page action will be dispatched. The wording is deliberately
/// explicit so the UI never implies that "stop" rolled anything back.
pub fn operation_aborted_error() -> BrowserError {
    BrowserError::process(
        "browser_operation_aborted",
        "the browser operation was stopped. Page instructions that were already sent may have run; \
         re-observe the page state to confirm what happened.",
    )
    .with_recovery_hint("Call browser_observe again before continuing.")
}

pub fn browser_unavailable_error(code: &str, message: impl Into<String>) -> BrowserError {
    BrowserError::capability(code, message)
}

impl From<BrowserError> for VibexError {
    fn from(error: BrowserError) -> Self {
        let mut mapped = VibexError::new(error.category, error.code, error.message);
        if let Some(hint) = error.recovery_hint {
            mapped = mapped.with_recovery_hint(hint.to_string());
        }
        for (key, value) in error.diagnostics {
            mapped = mapped.with_diagnostic(key, value);
        }
        mapped
    }
}

impl From<std::io::Error> for BrowserError {
    fn from(error: std::io::Error) -> Self {
        BrowserError::process("browser_io_error", error.to_string())
    }
}

impl From<serde_json::Error> for BrowserError {
    fn from(error: serde_json::Error) -> Self {
        BrowserError::validation("browser_json_error", error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_error_states_that_sent_commands_are_not_revoked() {
        let error = operation_aborted_error();
        assert!(error.message.contains("already sent may have run"));
        assert_eq!(error.code, "browser_operation_aborted");
        assert!(error.recovery_hint.is_some());
    }

    #[test]
    fn error_converts_into_vibex_error_without_losing_shape() {
        let error =
            BrowserError::validation("browser_test", "bad input").with_diagnostic("field", "ref");
        let mapped: VibexError = error.into();
        assert_eq!(mapped.code, "browser_test");
        assert_eq!(mapped.category, ErrorCategory::Validation);
        assert_eq!(mapped.diagnostics.len(), 1);
    }

    #[test]
    fn only_a_missing_browser_reads_as_an_environment_without_one() {
        assert!(BrowserError::capability("browser_unavailable", "none").is_browser_missing());
        assert!(
            BrowserError::capability("browser_feature_disabled", "off").is_browser_missing(),
            "a disabled feature is still not a broken browser"
        );
        assert!(
            !BrowserError::timeout("cdp_command_timeout", "no answer").is_browser_missing(),
            "a transport failure must never be reported as a machine without a browser"
        );
        assert!(!BrowserError::process("browser_spawn_failed", "no").is_browser_missing());
    }
}
