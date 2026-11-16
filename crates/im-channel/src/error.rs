#[derive(Debug, thiserror::Error)]
pub enum ImError {
    #[error("configuration invalid: {0}")]
    ConfigurationInvalid(String),
    #[error("authentication failed")]
    AuthFailed,
    #[error("transport error")]
    Transport,
    #[error("rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },
    #[error("provider rejected: {code}: {message}")]
    ProviderRejected { code: String, message: String },
    #[error("unsupported by this channel")]
    Unsupported,
    #[error("internal error")]
    Internal,
}

impl ImError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfigurationInvalid(_) => "im_configuration_invalid",
            Self::AuthFailed => "im_auth_failed",
            Self::Transport => "im_transport_failed",
            Self::RateLimited { .. } => "im_rate_limited",
            Self::ProviderRejected { .. } => "im_provider_rejected",
            Self::Unsupported => "im_unsupported",
            Self::Internal => "im_internal_error",
        }
    }
}

pub type ImResult<T> = Result<T, ImError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_rejection_keeps_the_message_it_was_given() {
        let error = ImError::ProviderRejected {
            code: "21001".to_string(),
            message: "Access denied due to platform policy".to_string(),
        };

        assert_eq!(
            error.to_string(),
            "provider rejected: 21001: Access denied due to platform policy"
        );
        assert_eq!(error.code(), "im_provider_rejected");
    }
}
