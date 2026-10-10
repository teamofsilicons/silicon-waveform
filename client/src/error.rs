//! Typed errors. Every failure says what went wrong and why; failures Waveform answered keep
//! its `{"error":{"code","message","hint","reason",…}}` body. Nothing here ever contains a
//! token, a proof, an app secret or a provider key.

use std::fmt;

use thiserror::Error;

/// Everything that can go wrong calling Waveform.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A value was rejected before any request was sent (bad URL, missing idempotency key,
    /// speech requested for another account…). The text says what and why.
    #[error("{0}")]
    Invalid(String),
    /// The request never got an answer: connection refused, DNS, TLS or timeout.
    #[error("Could not reach Waveform: {0}")]
    Transport(#[from] reqwest::Error),
    /// Waveform answered with an error body.
    #[error("{0}")]
    Api(Box<ApiError>),
    /// Waveform answered with a success status but a body this client does not understand.
    #[error("Waveform answered with a body this client does not understand: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Waveform's error body, bounded and stripped of control characters.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ApiError {
    /// HTTP status.
    pub status: u16,
    /// Stable machine-readable code, e.g. `not_custodian`, `storage_authorization_required`.
    pub code: String,
    /// What failed and why.
    pub message: String,
    /// What to do next, when Waveform said.
    pub hint: Option<String>,
    /// Exact reason: the authentication reason (`token_expired`, `sign_in_ended`,
    /// `proof_scope_missing`…), the dependency that failed, or a provider reason.
    pub reason: Option<String>,
    /// Waveform's request id; quote it in bug reports.
    pub request_id: Option<String>,
    /// The speech provider that failed, for provider errors.
    pub provider: Option<String>,
    /// The provider's HTTP status, for provider errors.
    pub provider_status: Option<u16>,
    /// Seconds Waveform asked to wait before retrying (`Retry-After`).
    pub retry_after_seconds: Option<u64>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Waveform answered HTTP {} {}: {}",
            self.status, self.code, self.message
        )?;
        if let Some(hint) = &self.hint {
            write!(f, " Hint: {hint}")?;
        }
        Ok(())
    }
}

impl Error {
    /// Stable machine-readable code: Waveform's error code, or `invalid_input`,
    /// `connection_failed`, `request_timeout`, `unexpected_response`.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Invalid(_) => "invalid_input",
            Self::Transport(error) if error.is_timeout() => "request_timeout",
            Self::Transport(_) => "connection_failed",
            Self::Api(error) => &error.code,
            Self::Decode(_) => "unexpected_response",
        }
    }

    /// What went wrong and why (without the hint).
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Api(error) => error.message.clone(),
            other => other.to_string(),
        }
    }

    /// What to do next, when known.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Api(error) => error.hint.as_deref(),
            Self::Transport(_) => Some(
                "Check the network and the Waveform URL (--url or WAVEFORM_URL), then retry. A speech request may or may not have run: retry it with the same idempotency key.",
            ),
            Self::Decode(_) => Some(
                "Check that the URL points at a Waveform API (it answers GET /api/v1/accounts), then retry.",
            ),
            Self::Invalid(_) => None,
        }
    }

    /// HTTP status of Waveform's answer, if there was one.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api(error) => Some(error.status),
            _ => None,
        }
    }

    /// The exact reason Waveform gave (authentication reason, dependency, provider reason).
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.as_api().and_then(|error| error.reason.as_deref())
    }

    /// Waveform's error body, if Waveform answered with one.
    #[must_use]
    pub fn as_api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(error) => Some(error),
            _ => None,
        }
    }

    /// True when Waveform answered with this error code.
    #[must_use]
    pub fn is_code(&self, code: &str) -> bool {
        self.code() == code
    }

    /// True when Waveform refused the credential itself (HTTP 401).
    #[must_use]
    pub fn is_unauthenticated(&self) -> bool {
        self.status() == Some(401)
    }

    /// True for a refusal that a fresh access token can cure: the token expired, its sign-in
    /// was revoked after it was issued (a newer token may still be valid), or it is signed with
    /// a key Waveform no longer knows. Callers holding a refresh token refresh once and retry.
    #[must_use]
    pub fn is_stale_credential(&self) -> bool {
        self.is_unauthenticated()
            && matches!(
                self.reason(),
                Some("token_expired" | "sign_in_ended" | "token_inactive" | "unknown_signing_key")
            )
    }
}

/// Builds the typed error for a non-success answer.
pub(crate) fn api_error(status: u16, retry_after: Option<u64>, bytes: &[u8]) -> Error {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).unwrap_or_default();
    let error = &value["error"];
    // Waveform's messages are safe public explanations; bound them again at this edge.
    let field = |name: &str, limit: usize| {
        error[name]
            .as_str()
            .map(|text| {
                text.chars()
                    .filter(|ch| !ch.is_control())
                    .take(limit)
                    .collect::<String>()
            })
            .filter(|text| !text.is_empty())
    };
    let fallback_message = || match status {
        401 => "Waveform refused the credential.".to_owned(),
        403 => "Waveform refused the request for this account.".to_owned(),
        404 => "Waveform has no such resource for this account.".to_owned(),
        429 => "Waveform is limiting requests; wait and retry.".to_owned(),
        500..=599 => format!("Waveform failed with HTTP {status}."),
        _ => format!("Waveform answered HTTP {status} without an error this client understands."),
    };
    Error::Api(Box::new(ApiError {
        status,
        code: field("code", 128).unwrap_or_else(|| "unstructured_response".into()),
        message: field("message", 1024).unwrap_or_else(fallback_message),
        hint: field("hint", 1024),
        reason: field("reason", 128),
        request_id: field("request_id", 128),
        provider: field("provider", 32),
        provider_status: error["provider_status"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok()),
        retry_after_seconds: retry_after,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_keep_code_message_hint_and_reason_without_raw_fields() {
        let error = api_error(
            502,
            None,
            br#"{"error":{"code":"provider_failed","message":"elevenlabs request failed: invalid credentials. Try another provider.","hint":"Choose another provider.","request_id":"fixture","provider":"elevenlabs","reason":"invalid_credentials","provider_status":401,"raw_body":"secret-provider-response"}}"#,
        );
        assert!(error.to_string().contains("invalid credentials"));
        assert!(error.to_string().contains("Hint: Choose another provider."));
        assert!(!format!("{error:?}").contains("secret-provider-response"));
        assert_eq!(error.code(), "provider_failed");
        assert_eq!(error.hint(), Some("Choose another provider."));
        assert_eq!(error.reason(), Some("invalid_credentials"));
        let api = error.as_api().unwrap();
        assert_eq!(api.provider.as_deref(), Some("elevenlabs"));
        assert_eq!(api.provider_status, Some(401));
        assert!(!error.is_unauthenticated());
    }

    #[test]
    fn stale_credentials_are_told_apart_from_configuration_errors() {
        let stale = api_error(
            401,
            None,
            br#"{"error":{"code":"unauthenticated","message":"The access token expired.","reason":"token_expired"}}"#,
        );
        assert!(stale.is_stale_credential());
        let wrong = api_error(
            401,
            None,
            br#"{"error":{"code":"unauthenticated","message":"x","reason":"wrong_issuer"}}"#,
        );
        assert!(wrong.is_unauthenticated());
        assert!(!wrong.is_stale_credential());
        let forbidden = api_error(
            403,
            None,
            br#"{"error":{"code":"forbidden","message":"x","reason":"proof_scope_missing"}}"#,
        );
        assert!(!forbidden.is_stale_credential());
    }

    #[test]
    fn unstructured_bodies_still_say_what_happened() {
        let error = api_error(503, Some(5), b"<html>bad gateway</html>");
        assert_eq!(error.code(), "unstructured_response");
        assert_eq!(error.message(), "Waveform failed with HTTP 503.");
        assert_eq!(error.as_api().unwrap().retry_after_seconds, Some(5));
    }
}
