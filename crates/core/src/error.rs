use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::CorrelationId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Validation,
    Capability,
    Permission,
    Provider,
    Process,
    Storage,
    Remote,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactedDiagnostic {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct VibexError {
    pub category: ErrorCategory,
    pub code: String,
    pub message: String,
    pub recovery_hint: Option<String>,
    pub correlation_id: Option<CorrelationId>,
    pub diagnostics: Vec<RedactedDiagnostic>,
}

pub type VibexResult<T> = Result<T, VibexError>;

impl VibexError {
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
            correlation_id: None,
            diagnostics: Vec::new(),
        }
    }

    pub fn validation(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Validation, code, message)
    }

    pub fn storage(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Storage, code, message)
    }

    pub fn process(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Process, code, message)
    }

    pub fn provider(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Provider, code, message)
    }

    pub fn capability(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Capability, code, message)
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Conflict, code, message)
    }

    pub fn with_recovery_hint(mut self, hint: impl Into<String>) -> Self {
        self.recovery_hint = Some(hint.into());
        self
    }

    pub fn with_correlation_id(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.diagnostics.push(RedactedDiagnostic {
            key: key.into(),
            value: value.into(),
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_shape_is_structured() {
        let err = VibexError::process("binary_missing", "codex was not found")
            .with_recovery_hint("Install Codex CLI and retry")
            .with_diagnostic("binary", "codex");

        let json = serde_json::to_value(err).unwrap();
        assert_eq!(json["category"], "process");
        assert_eq!(json["code"], "binary_missing");
        assert_eq!(json["recoveryHint"], "Install Codex CLI and retry");
    }
}
