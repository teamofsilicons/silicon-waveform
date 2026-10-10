//! Strict speech-operation header parsing.
//!
//! Credentials are exactly one `Authorization` header: `Bearer <access token>`
//! for the account itself, or `Proof sap_…` for another app acting for it. The
//! IAM-era headers are refused with an exact reason. An `X-Org-ID` sent by an old
//! client is ignored: the signed-in account is the only scope.

use std::str::FromStr as _;

use http::{HeaderMap, header::AUTHORIZATION};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{
    auth::{AccessToken, AuthFailure, InboundCredentials, ProofToken},
    idempotency::IdempotencyKey,
    identity::RequestId,
};

const IDEMPOTENCY_KEY: &str = "idempotency-key";
const REQUEST_ID: &str = "x-request-id";
const RETIRED_HEADERS: [&str; 3] = [
    "x-iam-obo-access-token",
    "x-iam-obo-access-proof",
    "x-app-id",
];
const MIN_IDEMPOTENCY_KEY_LENGTH: usize = 8;
const MAX_IDEMPOTENCY_KEY_LENGTH: usize = 255;

/// Validated common headers for a speech operation.
#[derive(Clone, Debug)]
pub struct SpeechHeaders {
    /// Transport correlation ID. A valid client UUID is preserved.
    pub request_id: RequestId,
    /// Exactly one inbound credential.
    pub credentials: InboundCredentials,
    /// Account-scoped, visible-ASCII idempotency key.
    pub idempotency_key: IdempotencyKey,
}

impl SpeechHeaders {
    /// Parses and validates all required speech-operation headers.
    ///
    /// # Errors
    ///
    /// Returns a redacted validation error when a required header is absent,
    /// repeated, malformed, or uses a retired credential.
    pub fn parse(headers: &HeaderMap) -> Result<Self, HeaderError> {
        let credentials = parse_credentials(headers).map_err(HeaderError::Credentials)?;
        let idempotency_key = parse_idempotency_key(required_header(headers, IDEMPOTENCY_KEY)?)?;
        Ok(Self {
            request_id: request_id(headers),
            credentials,
            idempotency_key,
        })
    }
}

/// Parses the one credential a request carries.
///
/// # Errors
///
/// The exact [`AuthFailure`] for a missing, repeated, malformed or retired credential.
pub fn parse_credentials(headers: &HeaderMap) -> Result<InboundCredentials, AuthFailure> {
    if RETIRED_HEADERS
        .iter()
        .any(|header| headers.contains_key(*header))
    {
        return Err(AuthFailure::RetiredCredentials);
    }
    let value = optional_header(headers, AUTHORIZATION.as_str())
        .map_err(|_| AuthFailure::MalformedAuthorization)?
        .ok_or(AuthFailure::MissingCredentials)?;
    let mut segments = value.split_ascii_whitespace();
    let scheme = segments.next().ok_or(AuthFailure::MalformedAuthorization)?;
    let token = segments.next().ok_or(AuthFailure::MalformedAuthorization)?;
    if segments.next().is_some() {
        return Err(AuthFailure::MalformedAuthorization);
    }
    if scheme.eq_ignore_ascii_case("bearer") {
        return AccessToken::new(token.to_owned())
            .map(InboundCredentials::Bearer)
            .map_err(|_| AuthFailure::MalformedAuthorization);
    }
    if scheme.eq_ignore_ascii_case("proof") {
        return ProofToken::new(token.to_owned())
            .map(InboundCredentials::Proof)
            .map_err(|_| AuthFailure::MalformedAuthorization);
    }
    Err(AuthFailure::MalformedAuthorization)
}

fn parse_idempotency_key(value: &str) -> Result<IdempotencyKey, HeaderError> {
    if !(MIN_IDEMPOTENCY_KEY_LENGTH..=MAX_IDEMPOTENCY_KEY_LENGTH).contains(&value.len())
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(HeaderError::InvalidIdempotencyKey);
    }
    value
        .parse()
        .map_err(|_| HeaderError::InvalidIdempotencyKey)
}

/// Whether a query string names an `account` (speech routes refuse it: speech
/// always runs as the caller).
pub(crate) fn names_an_account(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes()).any(|(name, _)| name == "account")
    })
}

pub(crate) fn request_id(headers: &HeaderMap) -> RequestId {
    optional_header(headers, REQUEST_ID)
        .ok()
        .flatten()
        .and_then(|value| RequestId::from_str(value).ok())
        .unwrap_or_else(generated_request_id)
}

pub(crate) fn generated_request_id() -> RequestId {
    // UUIDv7 generation cannot produce nil. Retaining a defensive fallback
    // keeps this boundary total without panicking if the UUID crate changes.
    RequestId::new(Uuid::now_v7()).unwrap_or_else(|_| {
        RequestId::new(Uuid::from_u128(1)).unwrap_or_else(|_| unreachable_request_id())
    })
}

fn unreachable_request_id() -> RequestId {
    // `Uuid::from_u128(1)` is statically non-nil. This branch exists only to
    // avoid panic-based construction in production code.
    loop {
        if let Ok(request_id) = RequestId::new(Uuid::now_v7()) {
            return request_id;
        }
    }
}

fn required_header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, HeaderError> {
    optional_header(headers, name)?.ok_or(HeaderError::MissingHeader(name))
}

pub(crate) fn optional_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, HeaderError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(HeaderError::DuplicateHeader(name));
    }
    first
        .map(|value| value.to_str().map_err(|_| HeaderError::InvalidHeader(name)))
        .transpose()
}

/// Public-header validation failure.
#[derive(Debug, Error)]
pub enum HeaderError {
    /// A required header is absent.
    #[error("required header {0} is missing")]
    MissingHeader(&'static str),
    /// A header cannot be represented as visible text.
    #[error("header {0} is malformed")]
    InvalidHeader(&'static str),
    /// Security-sensitive request headers must not be repeated.
    #[error("header {0} must be supplied at most once")]
    DuplicateHeader(&'static str),
    /// The credential is missing, malformed or retired.
    #[error("{}", .0.message())]
    Credentials(AuthFailure),
    /// Idempotency keys are bounded visible ASCII.
    #[error("Idempotency-Key must contain 8 to 255 visible ASCII characters")]
    InvalidIdempotencyKey,
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::{HeaderError, IDEMPOTENCY_KEY, SpeechHeaders, names_an_account};
    use crate::domain::auth::{AuthFailure, InboundCredentials};

    #[test]
    fn an_account_selector_is_found_in_any_spelling_of_the_query() {
        for query in [
            "account=si:scout",
            "a=1&account=",
            "account",
            "x=1&%61ccount=si:scout",
        ] {
            assert!(names_an_account(Some(query)), "{query}");
        }
        for query in [
            None,
            Some(""),
            Some("accounts=1"),
            Some("my_account=si:scout"),
        ] {
            assert!(!names_an_account(query), "{query:?}");
        }
    }

    fn base_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            IDEMPOTENCY_KEY,
            HeaderValue::from_static("operation-key-001"),
        );
        headers
    }

    #[test]
    fn accepts_bearer_and_proof_and_ignores_the_retired_org_header() {
        let mut headers = base_headers();
        headers.insert("x-org-id", HeaderValue::from_static("tos"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer eyJ.opaque.token"),
        );
        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Ok(value) if matches!(value.credentials, InboundCredentials::Bearer(_))
        ));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Proof sap_opaque"));
        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Ok(value) if matches!(value.credentials, InboundCredentials::Proof(_))
        ));
    }

    #[test]
    fn refuses_missing_malformed_and_retired_credentials_exactly() {
        let mut headers = base_headers();
        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Err(HeaderError::Credentials(AuthFailure::MissingCredentials))
        ));
        for value in ["Proof sapr_refresh", "Basic abc", "Bearer", "Bearer a b"] {
            headers.insert(AUTHORIZATION, HeaderValue::from_static(value));
            assert!(
                matches!(
                    SpeechHeaders::parse(&headers),
                    Err(HeaderError::Credentials(
                        AuthFailure::MalformedAuthorization
                    ))
                ),
                "{value}"
            );
        }
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token"));
        headers.insert("x-iam-obo-access-token", HeaderValue::from_static("oba_x"));
        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Err(HeaderError::Credentials(AuthFailure::RetiredCredentials))
        ));
    }

    #[test]
    fn rejects_repeated_security_headers() {
        let mut headers = base_headers();
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer one"));
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer two"));
        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Err(HeaderError::Credentials(
                AuthFailure::MalformedAuthorization
            ))
        ));
        let mut headers = base_headers();
        headers.append(
            IDEMPOTENCY_KEY,
            HeaderValue::from_static("operation-key-002"),
        );
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer one"));
        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Err(HeaderError::DuplicateHeader(IDEMPOTENCY_KEY))
        ));
    }
}
