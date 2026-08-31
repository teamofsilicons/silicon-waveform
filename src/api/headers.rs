//! Strict speech-operation header parsing.

use std::str::FromStr as _;

use http::{HeaderMap, header::AUTHORIZATION};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{
    auth::{AccessToken, CredentialError, InboundCredentials, OboCredentials, OboProof},
    idempotency::IdempotencyKey,
    identity::{ApplicationId, IdentityError, OrganizationId, RequestId},
};

const ORG_ID: &str = "x-org-id";
const IDEMPOTENCY_KEY: &str = "idempotency-key";
const OBO_PROOF: &str = "x-iam-obo-access-proof";
const APP_ID: &str = "x-app-id";
const REQUEST_ID: &str = "x-request-id";
const MIN_IDEMPOTENCY_KEY_LENGTH: usize = 8;
const MAX_IDEMPOTENCY_KEY_LENGTH: usize = 255;

/// Validated common headers for a speech operation.
#[derive(Clone, Debug)]
pub struct SpeechHeaders {
    /// Transport correlation ID. A valid client UUID is preserved.
    pub request_id: RequestId,
    /// Caller-selected organization, subject to IAM verification.
    pub organization_id: OrganizationId,
    /// Exactly one inbound credential mode.
    pub credentials: InboundCredentials,
    /// Actor-scoped, visible-ASCII idempotency key.
    pub idempotency_key: IdempotencyKey,
}

impl SpeechHeaders {
    /// Parses and validates all required speech-operation headers.
    ///
    /// # Errors
    ///
    /// Returns a redacted validation error when a required header is absent,
    /// repeated, malformed, or combines authentication modes ambiguously.
    pub fn parse(headers: &HeaderMap) -> Result<Self, HeaderError> {
        let organization_id = required_header(headers, ORG_ID)?
            .parse()
            .map_err(HeaderError::InvalidIdentity)?;
        let idempotency_key = parse_idempotency_key(required_header(headers, IDEMPOTENCY_KEY)?)?;
        let credentials = parse_credentials(headers)?;
        let request_id = request_id(headers);

        Ok(Self {
            request_id,
            organization_id,
            credentials,
            idempotency_key,
        })
    }
}

fn parse_credentials(headers: &HeaderMap) -> Result<InboundCredentials, HeaderError> {
    let bearer = optional_header(headers, AUTHORIZATION.as_str())?
        .map(parse_bearer)
        .transpose()?;
    let proof = optional_header(headers, OBO_PROOF)?;
    let application = optional_header(headers, APP_ID)?;

    match (bearer, proof, application) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(HeaderError::AmbiguousCredentials),
        (Some(token), None, None) => Ok(InboundCredentials::Bearer(token)),
        (None, Some(proof), Some(application)) => {
            let application_id =
                ApplicationId::from_str(application).map_err(HeaderError::InvalidIdentity)?;
            let proof = OboProof::new(proof.to_owned()).map_err(HeaderError::InvalidCredential)?;
            Ok(InboundCredentials::OnBehalfOf(OboCredentials {
                application_id,
                proof,
            }))
        }
        (None, None, None) => Err(HeaderError::MissingCredentials),
        (None, _, _) => Err(HeaderError::IncompleteOboCredentials),
    }
}

fn parse_bearer(value: &str) -> Result<AccessToken, HeaderError> {
    let mut segments = value.split_ascii_whitespace();
    let scheme = segments.next().ok_or(HeaderError::InvalidAuthorization)?;
    let token = segments.next().ok_or(HeaderError::InvalidAuthorization)?;
    if !scheme.eq_ignore_ascii_case("bearer") || segments.next().is_some() {
        return Err(HeaderError::InvalidAuthorization);
    }
    AccessToken::new(token.to_owned()).map_err(HeaderError::InvalidCredential)
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

fn optional_header<'a>(
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
    /// No supported IAM credential mode was supplied.
    #[error("IAM authentication credentials are required")]
    MissingCredentials,
    /// OBO proof and application headers must be supplied together.
    #[error("X-IAM-OBO-Access-Proof and X-App-ID must be supplied together")]
    IncompleteOboCredentials,
    /// Bearer and OBO credentials cannot be combined.
    #[error("Bearer and OBO credentials cannot be combined")]
    AmbiguousCredentials,
    /// The Authorization value is not exactly one Bearer credential.
    #[error("Authorization must contain exactly one Bearer credential")]
    InvalidAuthorization,
    /// A secret-bearing credential failed bounded validation.
    #[error(transparent)]
    InvalidCredential(CredentialError),
    /// An organization or application identifier is invalid.
    #[error(transparent)]
    InvalidIdentity(IdentityError),
    /// Idempotency keys are bounded visible ASCII.
    #[error("Idempotency-Key must contain 8 to 255 visible ASCII characters")]
    InvalidIdempotencyKey,
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::{APP_ID, HeaderError, IDEMPOTENCY_KEY, OBO_PROOF, ORG_ID, SpeechHeaders};
    use crate::domain::auth::InboundCredentials;

    fn base_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ORG_ID, HeaderValue::from_static("tos"));
        headers.insert(
            IDEMPOTENCY_KEY,
            HeaderValue::from_static("operation-key-001"),
        );
        headers
    }

    #[test]
    fn accepts_exactly_one_bearer_mode() {
        let mut headers = base_headers();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque-token"),
        );

        let parsed = SpeechHeaders::parse(&headers);

        assert!(
            matches!(parsed, Ok(value) if matches!(value.credentials, InboundCredentials::Bearer(_)))
        );
    }

    #[test]
    fn requires_the_complete_obo_pair() {
        let mut headers = base_headers();
        headers.insert(OBO_PROOF, HeaderValue::from_static("proof-token"));

        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Err(HeaderError::IncompleteOboCredentials)
        ));

        headers.insert(APP_ID, HeaderValue::from_static("silicon-dm"));
        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Ok(value) if matches!(value.credentials, InboundCredentials::OnBehalfOf(_))
        ));
    }

    #[test]
    fn rejects_ambiguous_credentials() {
        let mut headers = base_headers();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque-token"),
        );
        headers.insert(OBO_PROOF, HeaderValue::from_static("proof-token"));
        headers.insert(APP_ID, HeaderValue::from_static("silicon-dm"));

        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Err(HeaderError::AmbiguousCredentials)
        ));
    }

    #[test]
    fn rejects_repeated_security_headers() {
        let mut headers = base_headers();
        headers.append(ORG_ID, HeaderValue::from_static("other"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque-token"),
        );

        assert!(matches!(
            SpeechHeaders::parse(&headers),
            Err(HeaderError::DuplicateHeader(ORG_ID))
        ));
    }
}
