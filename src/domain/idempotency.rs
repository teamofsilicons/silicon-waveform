//! Actor- and operation-scoped idempotency policy.

use std::{fmt, str::FromStr, time::Duration};

use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    identity::{Actor, OrganizationId, RequestId},
    media::{BriefcaseFileUrl, MediaDuration, StoredAudio, TemporaryMediaUrl},
    provider::ProviderName,
    speech::{SttRequest, SttResult, TtsRequest, TtsResult},
};

/// Minimum public idempotency-key length.
pub const MIN_IDEMPOTENCY_KEY_LENGTH: usize = 8;

/// Maximum public idempotency-key length.
pub const MAX_IDEMPOTENCY_KEY_LENGTH: usize = 255;

/// Minimum key size for keyed request digests.
pub const MIN_REQUEST_DIGEST_KEY_BYTES: usize = 32;

/// Maximum key size accepted from configuration.
pub const MAX_REQUEST_DIGEST_KEY_BYTES: usize = 1_024;

/// Operation included in an idempotency scope.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechOperation {
    /// Text-to-speech synthesis.
    Tts,
    /// Speech-to-text transcription.
    Stt,
}

impl SpeechOperation {
    /// Stable storage label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tts => "tts",
            Self::Stt => "stt",
        }
    }
}

/// Validated caller idempotency key. Debug output is redacted.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Returns the key only for explicit storage or dependency translation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Stable derived key for the Briefcase upload side effect.
    #[must_use]
    pub fn for_briefcase_upload(request_id: RequestId) -> Self {
        Self(format!("waveform:briefcase:tts:{request_id}"))
    }
}

impl FromStr for IdempotencyKey {
    type Err = IdempotencyKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !(MIN_IDEMPOTENCY_KEY_LENGTH..=MAX_IDEMPOTENCY_KEY_LENGTH).contains(&value.len()) {
            return Err(IdempotencyKeyError::InvalidLength);
        }
        if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(IdempotencyKeyError::InvalidCharacters);
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Debug for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdempotencyKey([REDACTED])")
    }
}

/// Idempotency key validation failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum IdempotencyKeyError {
    /// Key length is outside the public contract bounds.
    #[error("idempotency key must contain between 8 and 255 bytes")]
    InvalidLength,
    /// Keys use a visible ASCII transport representation.
    #[error("idempotency key contains unsupported characters")]
    InvalidCharacters,
}

/// Complete tenant and operation boundary for one idempotency key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyScope {
    /// Represented actor, including actor kind.
    pub actor: Actor,
    /// Verified organization membership.
    pub organization_id: OrganizationId,
    /// Speech operation.
    pub operation: SpeechOperation,
    /// Caller key.
    pub key: IdempotencyKey,
}

/// Secret key used to make request digests resistant to offline guessing.
#[derive(Clone)]
pub struct RequestDigestKey(Zeroizing<Vec<u8>>);

impl RequestDigestKey {
    /// Copies and validates secret key material into zeroizing storage.
    ///
    /// # Errors
    ///
    /// Returns [`RequestDigestKeyError::InvalidLength`] unless the key contains
    /// between 32 and 1,024 bytes.
    pub fn new(value: &[u8]) -> Result<Self, RequestDigestKeyError> {
        if !(MIN_REQUEST_DIGEST_KEY_BYTES..=MAX_REQUEST_DIGEST_KEY_BYTES).contains(&value.len()) {
            return Err(RequestDigestKeyError::InvalidLength);
        }
        Ok(Self(Zeroizing::new(value.to_vec())))
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl fmt::Debug for RequestDigestKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestDigestKey([REDACTED])")
    }
}

impl PartialEq for RequestDigestKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && bool::from(self.0.ct_eq(&other.0))
    }
}

impl Eq for RequestDigestKey {}

/// Request-digest key validation failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RequestDigestKeyError {
    /// Key material must be large enough for SHA-256 and operationally bounded.
    #[error("request-digest key must contain between 32 and 1024 bytes")]
    InvalidLength,
}

/// Canonical HMAC-SHA-256 request-body digest. Debug output is redacted.
#[derive(Clone, Copy)]
pub struct RequestDigest([u8; 32]);

impl RequestDigest {
    /// Computes the canonical digest for a validated TTS request.
    #[must_use]
    pub fn for_tts(request: &TtsRequest, key: &RequestDigestKey) -> Self {
        digest_fields(
            SpeechOperation::Tts,
            request.text.as_str(),
            request
                .language
                .as_ref()
                .map(super::language::LanguageHint::as_str),
            key,
        )
    }

    /// Computes the canonical digest for a validated STT request.
    #[must_use]
    pub fn for_stt(request: &SttRequest, key: &RequestDigestKey) -> Self {
        digest_fields(
            SpeechOperation::Stt,
            request.source_url.as_url().as_str(),
            request
                .language
                .as_ref()
                .map(super::language::LanguageHint::as_str),
            key,
        )
    }

    /// Reconstructs a digest loaded from trusted storage.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns bytes for constant-time comparison or storage binding.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for RequestDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestDigest([REDACTED])")
    }
}

impl PartialEq for RequestDigest {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for RequestDigest {}

/// Opaque ownership token for a bounded idempotency lease.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyLeaseId(Uuid);

impl IdempotencyLeaseId {
    /// Constructs a non-nil lease identifier.
    ///
    /// # Errors
    ///
    /// Returns [`IdempotencyLeaseError::Nil`] for the nil UUID.
    pub fn new(value: Uuid) -> Result<Self, IdempotencyLeaseError> {
        if value.is_nil() {
            Err(IdempotencyLeaseError::Nil)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

/// Lease ID construction failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum IdempotencyLeaseError {
    /// Nil cannot uniquely own a lease.
    #[error("idempotency lease ID must not be nil")]
    Nil,
}

/// Exclusive right to execute work for an idempotency scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdempotencyLease {
    /// Opaque compare-and-set token.
    pub id: IdempotencyLeaseId,
    /// Time after which another caller may reclaim abandoned work.
    pub expires_at: OffsetDateTime,
}

/// Atomic store acquisition input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyClaim {
    /// Fully isolated key scope.
    pub scope: IdempotencyScope,
    /// Canonical validated request digest.
    pub digest: RequestDigest,
    /// Request ID that must be replayed after completion.
    pub request_id: RequestId,
    /// New ownership token supplied by the injected ID source.
    pub lease_id: IdempotencyLeaseId,
    /// Bounded lease lifetime.
    pub lease_duration: Duration,
}

/// Durable TTS fields retained without an expiring delivery capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedTtsOperation {
    /// Original operation correlation ID.
    pub request_id: RequestId,
    /// Stable authenticated Briefcase reference.
    pub permanent_url: BriefcaseFileUrl,
    /// Provider that produced the stored audio.
    pub provider: ProviderName,
    /// Measured normalized-audio duration.
    pub duration: MediaDuration,
}

impl CompletedTtsOperation {
    /// Captures only durable replay fields from a fresh result.
    #[must_use]
    pub fn from_result(result: &TtsResult) -> Self {
        Self {
            request_id: result.request_id,
            permanent_url: result.audio.permanent_url.clone(),
            provider: result.provider,
            duration: result.duration,
        }
    }

    /// Materializes the public response with a newly authorized temporary URL.
    #[must_use]
    pub fn with_temporary_url(self, temporary_url: TemporaryMediaUrl) -> TtsResult {
        TtsResult::new(
            self.request_id,
            StoredAudio {
                permanent_url: self.permanent_url,
                temporary_url,
            },
            self.provider,
            self.duration,
        )
    }
}

/// Durable success state retained for provider-free replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletedSpeechOperation {
    /// Completed text-to-speech fields excluding the temporary URL.
    Tts(CompletedTtsOperation),
    /// Completed speech-to-text response.
    Stt(SttResult),
}

impl CompletedSpeechOperation {
    /// Returns the response operation.
    #[must_use]
    pub const fn operation(&self) -> SpeechOperation {
        match self {
            Self::Tts(_) => SpeechOperation::Tts,
            Self::Stt(_) => SpeechOperation::Stt,
        }
    }
}

/// Result of atomically claiming an idempotency scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdempotencyDecision {
    /// Caller owns the returned lease and may execute work.
    Acquired {
        /// Current lease ownership.
        lease: IdempotencyLease,
        /// Canonical ID retained from the first claim, including on reclaim.
        request_id: RequestId,
        /// Canonical start time retained from the first claim for stable side effects.
        operation_started_at: OffsetDateTime,
    },
    /// A completed result may be returned after current resource authorization,
    /// without invoking speech providers or mutable content side effects.
    Replay(CompletedSpeechOperation),
    /// The key was already bound to a different canonical request.
    KeyReused,
    /// Another caller owns a live lease.
    InProgress {
        /// Suggested delay before the caller retries.
        retry_after: Duration,
    },
}

/// Atomic completion input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyCompletion {
    /// Scope being completed.
    pub scope: IdempotencyScope,
    /// Lease ownership token.
    pub lease_id: IdempotencyLeaseId,
    /// Durable normalized response fields to replay.
    pub response: CompletedSpeechOperation,
}

/// Atomic failed-work release input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyRelease {
    /// Scope whose failed lease should be released.
    pub scope: IdempotencyScope,
    /// Lease ownership token.
    pub lease_id: IdempotencyLeaseId,
}

fn digest_fields(
    operation: SpeechOperation,
    primary: &str,
    language: Option<&str>,
    key: &RequestDigestKey,
) -> RequestDigest {
    let mut hasher = Hmac::<Sha256>::new_from_slice(key.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC accepts every validated key length"));
    hasher.update(b"silicon-waveform:request:v1\0");
    update_length_prefixed(&mut hasher, operation.as_str().as_bytes());
    update_length_prefixed(&mut hasher, primary.as_bytes());
    match language {
        Some(value) => {
            hasher.update(&[1]);
            update_length_prefixed(&mut hasher, value.as_bytes());
        }
        None => hasher.update(&[0]),
    }
    RequestDigest(hasher.finalize().into_bytes().into())
}

fn update_length_prefixed(hasher: &mut Hmac<Sha256>, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(&length.to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::{
        IdempotencyKey, IdempotencyKeyError, RequestDigest, RequestDigestKey, RequestDigestKeyError,
    };
    use crate::domain::{
        language::LanguageHint,
        speech::{SpeechText, TtsRequest},
    };

    #[test]
    fn idempotency_key_is_bounded_and_redacted() {
        assert_eq!(
            IdempotencyKey::from_str("short"),
            Err(IdempotencyKeyError::InvalidLength)
        );
        assert_eq!(
            IdempotencyKey::from_str("contains a space"),
            Err(IdempotencyKeyError::InvalidCharacters)
        );
        let key = IdempotencyKey::from_str("safe-key-123");
        assert_eq!(
            key.map(|value| format!("{value:?}")),
            Ok("IdempotencyKey([REDACTED])".to_owned())
        );
    }

    #[test]
    fn canonical_digest_is_stable_and_binds_language_presence() {
        let key = RequestDigestKey::new(b"a-test-only-request-digest-key!!")
            .unwrap_or_else(|error| panic!("valid digest key: {error}"));
        let text = SpeechText::new("hello".to_owned());
        let english = LanguageHint::from_str("en");
        let without_language = text.clone().and_then(|text| TtsRequest::new(text, None));
        let with_language = text.and_then(|text| {
            english
                .map_err(|_| crate::domain::speech::SpeechValidationError::UnsupportedTtsLanguage)
                .and_then(|language| TtsRequest::new(text, Some(language)))
        });

        assert!(matches!(
            (&without_language, &with_language),
            (Ok(left), Ok(right))
                if RequestDigest::for_tts(left, &key) != RequestDigest::for_tts(right, &key)
        ));
        assert!(matches!(
            &without_language,
            Ok(request) if RequestDigest::for_tts(request, &key) == RequestDigest::for_tts(request, &key)
        ));
    }

    #[test]
    fn digest_key_is_bounded_redacted_and_changes_the_digest() {
        assert_eq!(
            RequestDigestKey::new(b"too-short"),
            Err(RequestDigestKeyError::InvalidLength)
        );
        let first = RequestDigestKey::new(b"first-test-only-request-digest-key")
            .unwrap_or_else(|error| panic!("valid key: {error}"));
        let second = RequestDigestKey::new(b"second-test-only-request-digest-key")
            .unwrap_or_else(|error| panic!("valid key: {error}"));
        let request = TtsRequest::new(
            SpeechText::new("predictable text".to_owned())
                .unwrap_or_else(|error| panic!("valid text: {error}")),
            None,
        )
        .unwrap_or_else(|error| panic!("valid request: {error}"));

        assert_eq!(format!("{first:?}"), "RequestDigestKey([REDACTED])");
        assert_ne!(
            RequestDigest::for_tts(&request, &first),
            RequestDigest::for_tts(&request, &second)
        );
    }
}
