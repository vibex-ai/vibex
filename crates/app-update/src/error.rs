use thiserror::Error;

pub type AppUpdateResult<T> = Result<T, AppUpdateError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {message}")]
pub struct AppUpdateError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl AppUpdateError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: true,
        }
    }
}
