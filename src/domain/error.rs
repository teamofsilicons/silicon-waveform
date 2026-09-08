//! Stable application-facing failure taxonomy.

use std::time::Duration;

use thiserror::Error;

/// Stable machine-readable error code independent of HTTP status mapping.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ErrorCode {
    /// Request syntax or a validated value was invalid.
    InvalidRequest,
    /// Caller did not provide a single valid credential mode.
    Unauthenticated,
    /// Verified caller lacks permission.
    Forbidden,
    /// Input exceeds a synchronous service limit.
    PayloadTooLarge,
    /// Source media type is outside the stable allowlist.
    UnsupportedMediaType,
    /// Briefcase reported the exact source or generated replay file as missing.
    SourceNotFound,
    /// Idempotency key was rebound to different input.
    IdempotencyKeyReused,
    /// Another caller owns a live idempotency lease.
    RequestInProgress,
    /// Every configured speech provider failed.
    ProvidersExhausted,
    /// Required dependency contract has not yet been published.
    DependencyContractUnavailable,
    /// A required dependency is temporarily unavailable.
    DependencyUnavailable,
    /// A required dependency exceeded its deadline.
    DependencyTimeout,
    /// An invariant failed without a safe client-visible explanation.
    Internal,
}

impl ErrorCode {
    /// Returns the public error-code spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::SourceNotFound => "source_not_found",
            Self::IdempotencyKeyReused => "idempotency_key_reused",
            Self::RequestInProgress => "request_in_progress",
            Self::ProvidersExhausted => "providers_exhausted",
            Self::DependencyContractUnavailable => "dependency_contract_unavailable",
            Self::DependencyUnavailable => "dependency_unavailable",
            Self::DependencyTimeout => "dependency_timeout",
            Self::Internal => "internal_error",
        }
    }
}

/// External dependency named in safe operational errors.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Dependency {
    /// Account voice profiles and provider mappings.
    VoiceProfiles,
    /// Encrypted account provider credentials.
    ProviderKeys,
    /// Silicon IAM.
    Iam,
    /// Silicon Briefcase.
    Briefcase,
    /// Shared idempotency store.
    IdempotencyStore,
    /// Audio inspection and normalization runtime.
    AudioNormalizer,
}

/// Cohesive service failure consumed by the delivery layer.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WaveformError {
    /// Validated request could not be constructed.
    #[error("request is invalid")]
    InvalidRequest,
    /// No valid authentication was established.
    #[error("authentication is required")]
    Unauthenticated,
    /// Identity was verified but the action was not authorized.
    #[error("action is not permitted")]
    Forbidden,
    /// Bounded synchronous limit was exceeded.
    #[error("request payload is too large")]
    PayloadTooLarge,
    /// Source media is not supported.
    #[error("source media type is not supported")]
    UnsupportedMediaType,
    /// Briefcase reported the exact source or generated replay file as missing.
    #[error("Briefcase file was not found")]
    SourceNotFound,
    /// Same key was used with a different body digest.
    #[error("idempotency key was already used for a different request")]
    IdempotencyKeyReused,
    /// A live request already owns the key.
    #[error("an equivalent request is already in progress")]
    RequestInProgress {
        /// Suggested retry delay.
        retry_after: Duration,
    },
    /// No provider returned a usable normalized result.
    #[error("all speech providers failed")]
    ProvidersExhausted,
    /// Product flow is blocked on a dependency contract gap.
    #[error("required {dependency:?} contract is unavailable")]
    DependencyContractUnavailable {
        /// Dependency with the missing safe operation.
        dependency: Dependency,
    },
    /// Dependency is temporarily unavailable.
    #[error("required {dependency:?} dependency is unavailable")]
    DependencyUnavailable {
        /// Failed dependency.
        dependency: Dependency,
    },
    /// Dependency exceeded its deadline.
    #[error("required {dependency:?} dependency timed out")]
    DependencyTimeout {
        /// Timed-out dependency.
        dependency: Dependency,
    },
    /// Internal invariant or invalid dependency response.
    #[error("internal service error")]
    Internal,
}

impl WaveformError {
    /// Stable public code for this failure.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidRequest => ErrorCode::InvalidRequest,
            Self::Unauthenticated => ErrorCode::Unauthenticated,
            Self::Forbidden => ErrorCode::Forbidden,
            Self::PayloadTooLarge => ErrorCode::PayloadTooLarge,
            Self::UnsupportedMediaType => ErrorCode::UnsupportedMediaType,
            Self::SourceNotFound => ErrorCode::SourceNotFound,
            Self::IdempotencyKeyReused => ErrorCode::IdempotencyKeyReused,
            Self::RequestInProgress { .. } => ErrorCode::RequestInProgress,
            Self::ProvidersExhausted => ErrorCode::ProvidersExhausted,
            Self::DependencyContractUnavailable { .. } => ErrorCode::DependencyContractUnavailable,
            Self::DependencyUnavailable { .. } => ErrorCode::DependencyUnavailable,
            Self::DependencyTimeout { .. } => ErrorCode::DependencyTimeout,
            Self::Internal => ErrorCode::Internal,
        }
    }

    /// Suggested retry delay for an in-progress duplicate.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RequestInProgress { retry_after } => Some(*retry_after),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ErrorCode, WaveformError};

    #[test]
    fn codes_and_retry_metadata_are_stable() {
        let error = WaveformError::RequestInProgress {
            retry_after: Duration::from_secs(3),
        };

        assert_eq!(error.code(), ErrorCode::RequestInProgress);
        assert_eq!(error.code().as_str(), "request_in_progress");
        assert_eq!(error.retry_after(), Some(Duration::from_secs(3)));
    }
}
