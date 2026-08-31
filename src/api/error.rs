//! Stable, provider-independent HTTP error responses.

use std::time::Duration;

use axum::{
    Json,
    response::{IntoResponse, Response},
};
use http::{HeaderValue, StatusCode, header::RETRY_AFTER};
use serde::Serialize;

use crate::domain::{error::WaveformError, identity::RequestId};

const REQUEST_ID_HEADER: &str = "x-request-id";

/// Safe HTTP error with all transport metadata needed for one response.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    request_id: RequestId,
    retry_after: Option<Duration>,
}

impl ApiError {
    /// Maps an application failure to the stable public contract.
    #[must_use]
    pub fn from_waveform(error: WaveformError, request_id: RequestId) -> Self {
        let status = match error {
            WaveformError::InvalidRequest => StatusCode::BAD_REQUEST,
            WaveformError::Unauthenticated => StatusCode::UNAUTHORIZED,
            WaveformError::Forbidden => StatusCode::FORBIDDEN,
            WaveformError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            WaveformError::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            WaveformError::SourceNotFound => StatusCode::NOT_FOUND,
            WaveformError::IdempotencyKeyReused | WaveformError::RequestInProgress { .. } => {
                StatusCode::CONFLICT
            }
            WaveformError::ProvidersExhausted => StatusCode::BAD_GATEWAY,
            WaveformError::DependencyContractUnavailable { .. }
            | WaveformError::DependencyUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            WaveformError::DependencyTimeout { .. } => StatusCode::GATEWAY_TIMEOUT,
            WaveformError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = message_for_code(error.code().as_str());
        let retry_after = error.retry_after().or_else(|| {
            (status == StatusCode::SERVICE_UNAVAILABLE).then_some(Duration::from_secs(5))
        });
        Self {
            status,
            code: error.code().as_str(),
            message,
            request_id,
            retry_after,
        }
    }

    /// Constructs a malformed-request response without exposing parser detail.
    #[must_use]
    pub const fn invalid_request(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request is malformed or contains an invalid value.",
            request_id,
        )
    }

    /// Constructs a route-not-found response.
    #[must_use]
    pub const fn not_found(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested Waveform route does not exist.",
            request_id,
        )
    }

    /// Constructs a method-not-allowed response.
    #[must_use]
    pub const fn method_not_allowed(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "The HTTP method is not supported for this route.",
            request_id,
        )
    }

    /// Constructs an authentication failure.
    #[must_use]
    pub const fn unauthenticated(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Valid IAM authentication is required.",
            request_id,
        )
    }

    /// Constructs a bounded-body or domain-limit response.
    #[must_use]
    pub const fn payload_too_large(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "The request exceeds a synchronous service limit.",
            request_id,
        )
    }

    /// Constructs an admission-control response.
    #[must_use]
    pub const fn overloaded(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "Waveform is at its concurrent request limit.",
            request_id,
        )
        .with_retry_after(Duration::from_secs(1))
    }

    /// Constructs a total workflow deadline response.
    #[must_use]
    pub const fn request_timeout(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            "The synchronous Waveform operation exceeded its deadline.",
            request_id,
        )
    }

    /// Constructs a readiness response for an unavailable dependency contract.
    #[must_use]
    pub const fn not_ready(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "Waveform is not ready to serve speech operations.",
            request_id,
        )
        .with_retry_after(Duration::from_secs(5))
    }

    /// Constructs a panic-safe internal response.
    #[must_use]
    pub const fn internal(request_id: RequestId) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Waveform could not complete the request.",
            request_id,
        )
    }

    const fn new(
        status: StatusCode,
        code: &'static str,
        message: &'static str,
        request_id: RequestId,
    ) -> Self {
        Self {
            status,
            code,
            message,
            request_id,
            retry_after: None,
        }
    }

    const fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message,
                request_id: self.request_id.to_string(),
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        if let Ok(value) = HeaderValue::from_str(&self.request_id.to_string()) {
            response.headers_mut().insert(REQUEST_ID_HEADER, value);
        }
        if let Some(retry_after) = self.retry_after {
            let seconds = retry_after.as_secs().max(1).to_string();
            if let Ok(value) = HeaderValue::from_str(&seconds) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
        }
        response
    }
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

fn message_for_code(code: &str) -> &'static str {
    match code {
        "invalid_request" => "The request is malformed or contains an invalid value.",
        "unauthenticated" => "Valid IAM authentication is required.",
        "forbidden" => "The represented actor is not permitted to perform this action.",
        "payload_too_large" => "The request exceeds a synchronous service limit.",
        "unsupported_media_type" => "The Briefcase source media type is not supported.",
        "source_not_found" => "The referenced Briefcase file was not found.",
        "idempotency_key_reused" => "The idempotency key is bound to a different request.",
        "request_in_progress" => "An equivalent request is already in progress.",
        "providers_exhausted" => "No configured speech provider returned a usable result.",
        "dependency_contract_unavailable" => "A required dependency contract is unavailable.",
        "dependency_unavailable" => "A required dependency is temporarily unavailable.",
        "dependency_timeout" => "A required dependency exceeded its deadline.",
        _ => "Waveform could not complete the request.",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{body::to_bytes, response::IntoResponse as _};
    use http::{StatusCode, header::RETRY_AFTER};
    use uuid::Uuid;

    use super::ApiError;
    use crate::domain::{error::WaveformError, identity::RequestId};

    #[test]
    fn missing_briefcase_files_have_a_distinct_not_found_contract()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_id = RequestId::new(Uuid::from_u128(43))?;
        let response =
            ApiError::from_waveform(WaveformError::SourceNotFound, request_id).into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn in_progress_errors_include_safe_body_and_retry_metadata()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_id = RequestId::new(Uuid::from_u128(42))?;
        let response = ApiError::from_waveform(
            WaveformError::RequestInProgress {
                retry_after: Duration::from_secs(2),
            },
            request_id,
        )
        .into_response();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("2")
        );
        let body = to_bytes(response.into_body(), 4_096).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(json["error"]["code"], "request_in_progress");
        assert_eq!(json["error"]["request_id"], request_id.to_string());
        Ok(())
    }
}
