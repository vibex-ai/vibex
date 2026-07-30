use serde::{Deserialize, Serialize};
use vibex_core::RequestId;

use crate::{BackendError, BackendResult};

pub const MAX_BACKEND_IDEMPOTENCY_KEY_LEN: usize = 256;
pub const MAX_BACKEND_REVISION_LEN: usize = 256;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationRequest<T> {
    pub request_id: RequestId,
    pub idempotency_key: Option<String>,
    pub expected_revision: Option<String>,
    pub payload: T,
}

impl<T> MutationRequest<T> {
    pub fn new(payload: T) -> Self {
        Self {
            request_id: RequestId::new(),
            idempotency_key: None,
            expected_revision: None,
            payload,
        }
    }

    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    pub fn with_expected_revision(mut self, revision: impl Into<String>) -> Self {
        self.expected_revision = Some(revision.into());
        self
    }

    pub fn validate(&self) -> BackendResult<()> {
        validate_optional_token(
            self.idempotency_key.as_deref(),
            MAX_BACKEND_IDEMPOTENCY_KEY_LEN,
            "backend_idempotency_key_invalid",
            "backend mutation idempotency key must be non-empty and bounded",
        )?;
        validate_optional_token(
            self.expected_revision.as_deref(),
            MAX_BACKEND_REVISION_LEN,
            "backend_revision_invalid",
            "backend mutation revision must be non-empty and bounded",
        )
    }
}

fn validate_optional_token(
    value: Option<&str>,
    max_len: usize,
    code: &'static str,
    message: &'static str,
) -> BackendResult<()> {
    if value.is_some_and(|value| {
        value.trim().is_empty() || value.len() > max_len || value.chars().any(char::is_control)
    }) {
        Err(BackendError::failed(code, message))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_context_requires_bounded_nonempty_tokens() {
        assert!(MutationRequest::new(1).validate().is_ok());
        assert!(
            MutationRequest::new(1)
                .with_idempotency_key("message-1")
                .with_expected_revision("rev-2")
                .validate()
                .is_ok()
        );
        assert_eq!(
            MutationRequest::new(1)
                .with_idempotency_key(" ")
                .validate()
                .unwrap_err()
                .code,
            "backend_idempotency_key_invalid"
        );
    }
}
