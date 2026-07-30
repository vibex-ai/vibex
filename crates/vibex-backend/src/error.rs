use serde::{Deserialize, Serialize};
use thiserror::Error;
use vibex_core::{CorrelationId, ErrorCategory, RedactedDiagnostic, VibexError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendErrorKind {
    Loading,
    Offline,
    Conflict,
    Permission,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct BackendError {
    pub kind: BackendErrorKind,
    pub code: String,
    pub message: String,
    pub recovery_hint: Option<String>,
    pub correlation_id: Option<CorrelationId>,
    pub diagnostics: Vec<RedactedDiagnostic>,
}

pub type BackendResult<T> = Result<T, BackendError>;

impl BackendError {
    pub fn new(
        kind: BackendErrorKind,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            recovery_hint: None,
            correlation_id: None,
            diagnostics: Vec::new(),
        }
    }

    pub fn loading(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(BackendErrorKind::Loading, code, message)
    }

    pub fn offline(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(BackendErrorKind::Offline, code, message)
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(BackendErrorKind::Conflict, code, message)
    }

    pub fn permission(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(BackendErrorKind::Permission, code, message)
    }

    pub fn unsupported(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(BackendErrorKind::Unsupported, code, message)
    }

    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(BackendErrorKind::Failed, code, message)
    }

    pub fn with_recovery_hint(mut self, hint: impl Into<String>) -> Self {
        self.recovery_hint = Some(hint.into());
        self
    }
}

impl From<VibexError> for BackendError {
    fn from(error: VibexError) -> Self {
        let kind = match error.category {
            ErrorCategory::Capability => BackendErrorKind::Unsupported,
            ErrorCategory::Permission => BackendErrorKind::Permission,
            ErrorCategory::Conflict => BackendErrorKind::Conflict,
            ErrorCategory::Remote => BackendErrorKind::Offline,
            ErrorCategory::Validation
            | ErrorCategory::Provider
            | ErrorCategory::Process
            | ErrorCategory::Storage => BackendErrorKind::Failed,
        };
        Self {
            kind,
            code: error.code,
            message: error.message,
            recovery_hint: error.recovery_hint,
            correlation_id: error.correlation_id,
            diagnostics: error.diagnostics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_domain_errors_to_shared_ui_states_without_losing_codes() {
        let cases = [
            (
                VibexError::capability("missing", "not supported"),
                BackendErrorKind::Unsupported,
            ),
            (
                VibexError::conflict("stale", "refresh required"),
                BackendErrorKind::Conflict,
            ),
            (
                VibexError::new(ErrorCategory::Permission, "denied", "permission denied"),
                BackendErrorKind::Permission,
            ),
            (
                VibexError::new(ErrorCategory::Remote, "offline", "host unavailable"),
                BackendErrorKind::Offline,
            ),
        ];

        for (source, expected) in cases {
            let code = source.code.clone();
            let mapped = BackendError::from(source);
            assert_eq!(mapped.kind, expected);
            assert_eq!(mapped.code, code);
        }
    }
}
