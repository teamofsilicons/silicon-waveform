//! Object-safe contracts implemented by infrastructure adapters.

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::{
    auth::{AccessToken, AuthFailure, AuthorizationRequest, AuthorizedActor, BriefcaseProof},
    idempotency::{
        IdempotencyClaim, IdempotencyCompletion, IdempotencyDecision, IdempotencyLeaseId,
        IdempotencyRelease, SpeechOperation,
    },
    identity::{ActorId, RequestId},
    media::{
        AudioArtifact, BriefcaseFileUrl, GeneratedAudioFileName, HeldReason, MediaSizeLimit,
        NormalizedAudio, SourceMedia,
    },
    provider::{ProviderError, ProviderName},
    speech::{SttProviderRequest, SttProviderResult, TtsProviderRequest},
};

/// Text-to-speech provider boundary.
#[async_trait]
pub trait TextToSpeechProvider: Send + Sync {
    /// Canonical identity used for ordering, telemetry, and normalized output.
    fn name(&self) -> ProviderName;

    /// Clones this adapter with a request-local key, preserving shared limits.
    /// An adapter that cannot take a key returns `None` (the default).
    fn with_api_key(
        &self,
        _key: secrecy::SecretString,
    ) -> Option<std::sync::Arc<dyn TextToSpeechProvider>> {
        None
    }

    /// Executes one bounded logical attempt. An adapter may perform the single
    /// documented transient retry within the same deadline and concurrency permit.
    async fn synthesize(
        &self,
        request: TtsProviderRequest,
        request_id: RequestId,
    ) -> Result<AudioArtifact, ProviderError>;
}

/// Speech-to-text provider boundary.
#[async_trait]
pub trait SpeechToTextProvider: Send + Sync {
    /// Canonical identity used for ordering, telemetry, and normalized output.
    fn name(&self) -> ProviderName;

    /// Clones this adapter with a request-local key, preserving shared limits.
    /// An adapter that cannot take a key returns `None` (the default).
    fn with_api_key(
        &self,
        _key: secrecy::SecretString,
    ) -> Option<std::sync::Arc<dyn SpeechToTextProvider>> {
        None
    }

    /// Executes one bounded logical attempt. An adapter may perform the single
    /// documented transient retry within the same deadline and concurrency permit.
    async fn transcribe(
        &self,
        request: SttProviderRequest,
        request_id: RequestId,
    ) -> Result<SttProviderResult, ProviderError>;
}

/// MP3 validation, conversion, and exact-duration inspection boundary.
#[async_trait]
pub trait AudioNormalizer: Send + Sync {
    /// Produces a validated MP3, passing through valid MP3 input when possible.
    async fn normalize_to_mp3(
        &self,
        artifact: AudioArtifact,
        request_id: RequestId,
    ) -> Result<NormalizedAudio, AudioNormalizationError>;

    /// Measures decoded source audio before transcription; the bytes sent to providers stay unchanged.
    async fn source_duration(
        &self,
        media: &SourceMedia,
        request_id: RequestId,
    ) -> Result<crate::domain::media::MediaDuration, AudioNormalizationError>;

    /// Verifies the local codec/inspector runtime without processing user data.
    async fn check_ready(&self) -> Result<(), AudioNormalizationError>;
}

/// Audio-normalization failure category without process stderr or media content.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AudioNormalizationError {
    /// Local concurrency budget is exhausted.
    #[error("audio normalizer is saturated")]
    Saturated,
    /// Normalization exceeded its deadline.
    #[error("audio normalization timed out")]
    Timeout,
    /// Input or output was malformed, empty, or unsupported.
    #[error("audio artifact is invalid")]
    InvalidAudio,
    /// Required codec runtime is absent or misconfigured.
    #[error("audio normalizer is unavailable")]
    Unavailable,
    /// Codec process failed without a safe client-visible detail.
    #[error("audio normalization failed")]
    Failed,
}

/// Silicon Accounts authentication: bearer tokens verified locally against the
/// cached key set, proofs verified online.
#[async_trait]
pub trait AccountsPort: Send + Sync {
    /// Verifies exactly one credential for one action and resolves the account
    /// to its private storage key.
    async fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizedActor, AccountsError>;
}

/// Safe authentication failure category.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AccountsError {
    /// The credential was refused for an exact, caller-visible reason.
    #[error("{}", .0.message())]
    Rejected(AuthFailure),
    /// Silicon Accounts (or Waveform's account store) could not be reached.
    #[error("Silicon Accounts is unavailable")]
    Unavailable,
    /// Silicon Accounts exceeded the configured deadline.
    #[error("Silicon Accounts timed out")]
    Timeout,
}

/// Briefcase access (a User verification proof) Waveform holds for an account.
#[async_trait]
pub trait StorageAuthorityPort: Send + Sync {
    /// Returns a Briefcase proof that is valid for at least a minute. With the
    /// account's own Waveform access token, a missing proof, or one that ended
    /// with the sign-in it stood on, is replaced by a newly issued one; a proof
    /// the account or its custodian revoked is not (that needs an explicit
    /// approval). Without the token only a stored proof can be used.
    async fn briefcase_proof(
        &self,
        actor: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
    ) -> Result<BriefcaseProof, StorageAuthorityError>;

    /// Called when Briefcase refused `refused` (HTTP 401): a User verification
    /// proof ends with the sign-in it was issued under (another machine's
    /// `waveform logout`, the website's sign-out), and Briefcase then refuses it
    /// before it expires. Answers the proof another request already put in its
    /// place, or asks Silicon Accounts to refresh it: a live proof gets a new
    /// token; an ended one is replaced as in [`Self::briefcase_proof`]; one the
    /// account revoked at Silicon Accounts is marked revoked and not replaced.
    async fn renew_refused_proof(
        &self,
        actor: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        refused: &BriefcaseProof,
    ) -> Result<BriefcaseProof, StorageAuthorityError>;
}

/// Why no Briefcase proof is available.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum StorageAuthorityError {
    /// Waveform holds no usable Briefcase access for the account and cannot create one now.
    #[error("Waveform holds no Briefcase access for this account")]
    NotAuthorized,
    /// Silicon Accounts refused to issue or refresh the proof.
    #[error("Silicon Accounts refused the Briefcase proof")]
    Refused,
    /// Silicon Accounts or the proof store is unavailable.
    #[error("Briefcase access could not be checked")]
    Unavailable,
    /// Silicon Accounts timed out.
    #[error("Briefcase access check timed out")]
    Timeout,
}

/// Input for an authorized, bounded Briefcase source read.
#[derive(Clone, Debug)]
pub struct ReadSourceMediaRequest {
    /// Briefcase proof for the account.
    pub proof: BriefcaseProof,
    /// Stable permanent URL to resolve inside Briefcase.
    pub source_url: BriefcaseFileUrl,
    /// Maximum response size enforced before and during streaming.
    pub size_limit: MediaSizeLimit,
    /// Correlation ID propagated to Briefcase.
    pub request_id: RequestId,
}

/// Input for a non-content Briefcase read check.
#[derive(Clone, Debug)]
pub struct BriefcaseFileAccessRequest {
    /// Briefcase proof for the account.
    pub proof: BriefcaseProof,
    /// Stable permanent file reference whose current access is checked.
    pub file_url: BriefcaseFileUrl,
    /// Current attempt correlation ID propagated to Briefcase.
    pub request_id: RequestId,
}

/// Input for an idempotent generated-audio upload.
#[derive(Clone, Debug)]
pub struct StoreGeneratedAudioRequest {
    /// Briefcase proof for the account.
    pub proof: BriefcaseProof,
    /// Collision-resistant UTC filename.
    pub filename: GeneratedAudioFileName,
    /// Validated MP3 artifact.
    pub audio: NormalizedAudio,
    /// Correlation ID; also the stable upload operation id, so retries never duplicate files.
    pub request_id: RequestId,
}

/// Briefcase content boundary, authorized by Briefcase proofs.
#[async_trait]
pub trait BriefcasePort: Send + Sync {
    /// Verifies current read access without transferring the file content.
    async fn verify_file_read_access(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<(), BriefcaseError>;

    /// Resolves and reads a permanent URL through Briefcase authorization.
    async fn read_source_media(
        &self,
        request: ReadSourceMediaRequest,
    ) -> Result<SourceMedia, BriefcaseError>;

    /// Stores the MP3 in the account's Waveform app folder and returns its permanent URL.
    async fn store_generated_audio(
        &self,
        request: StoreGeneratedAudioRequest,
    ) -> Result<BriefcaseFileUrl, BriefcaseError>;
}

/// Safe Briefcase failure category.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BriefcaseError {
    /// Briefcase rejected the proof.
    #[error("Briefcase rejected Waveform's proof")]
    Unauthorized,
    /// Account lacks current access to the source or destination.
    #[error("Briefcase denied access")]
    Forbidden,
    /// Permanent source reference does not resolve to a visible file.
    #[error("Briefcase source was not found")]
    NotFound,
    /// Downloaded media exceeded the bounded request.
    #[error("Briefcase source is too large")]
    MediaTooLarge,
    /// Declared or detected source media is unsupported.
    #[error("Briefcase source media type is unsupported")]
    UnsupportedMediaType,
    /// Briefcase does not offer the delegated operation Waveform needs.
    #[error("required Briefcase contract is unavailable")]
    ContractUnavailable,
    /// Briefcase could not be reached or reported temporary unavailability.
    #[error("Briefcase is unavailable")]
    Unavailable,
    /// Briefcase exceeded the configured operation deadline.
    #[error("Briefcase timed out")]
    Timeout,
    /// Briefcase returned a malformed or untrusted response.
    #[error("Briefcase returned an invalid response")]
    InvalidResponse,
}

/// Generated speech Waveform keeps when it cannot or must not store it in Briefcase.
#[derive(Clone, Debug)]
pub struct HeldAudio {
    /// Canonical request id; the download path is `/api/v1/audio/{request_id}`.
    pub request_id: RequestId,
    /// Owner's private storage key.
    pub owner: ActorId,
    /// Generated filename, used as the download name.
    pub filename: GeneratedAudioFileName,
    /// Validated MP3.
    pub audio: NormalizedAudio,
    /// Why it is not in Briefcase.
    pub reason: HeldReason,
}

/// Retention-limited store for held audio.
#[async_trait]
pub trait HeldAudioStore: Send + Sync {
    /// Keeps the audio (idempotently per request id) and returns when it will be deleted.
    async fn hold(&self, audio: HeldAudio) -> Result<time::OffsetDateTime, HeldAudioError>;

    /// True when audio for this request is still held for this owner.
    async fn is_held(&self, request_id: RequestId, owner: ActorId) -> Result<bool, HeldAudioError>;
}

/// Held-audio store failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("Waveform's audio store is unavailable")]
pub struct HeldAudioError;

/// Shared idempotency state boundary.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Atomically creates, reclaims, conflicts, or replays a scoped record.
    async fn claim(
        &self,
        claim: IdempotencyClaim,
    ) -> Result<IdempotencyDecision, IdempotencyStoreError>;

    /// Persists running history under the canonical operation ID before work.
    /// In-memory implementations may omit durable history.
    async fn start_job(
        &self,
        _job: crate::domain::idempotency::SpeechJobStart,
    ) -> Result<(), IdempotencyStoreError> {
        Ok(())
    }

    /// Atomically replaces a caller-owned lease with a retained success and
    /// completes the history row created by `start_job`.
    async fn complete(
        &self,
        completion: IdempotencyCompletion,
    ) -> Result<(), IdempotencyStoreError>;

    /// Releases failed work only when the supplied lease still owns the record.
    async fn release(&self, release: IdempotencyRelease) -> Result<(), IdempotencyStoreError>;

    /// Verifies authoritative storage connectivity for readiness.
    async fn check_ready(&self) -> Result<(), IdempotencyStoreError>;
}

/// Safe authoritative-store failure category.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum IdempotencyStoreError {
    /// Store could not be reached or reported temporary unavailability.
    #[error("idempotency store is unavailable")]
    Unavailable,
    /// Store exceeded its configured operation deadline.
    #[error("idempotency store timed out")]
    Timeout,
    /// Persisted state violates a domain invariant or cannot be decoded safely.
    #[error("idempotency store contains an invalid record")]
    InvalidRecord,
    /// Compare-and-set lease ownership changed during completion or release.
    #[error("idempotency lease is no longer owned by this request")]
    LeaseLost,
}

/// Injected source of unpredictable idempotency-lease ownership tokens.
pub trait LeaseIdGenerator: Send + Sync {
    /// Creates an unpredictable non-nil lease ownership ID.
    fn new_idempotency_lease_id(&self) -> IdempotencyLeaseId;
}

/// Account-scoped, decrypted provider credentials.
pub type ProviderKeys = std::collections::HashMap<ProviderName, secrecy::SecretString>;

/// Retrieves personal keys only after the caller has been authorized.
#[async_trait]
pub trait ProviderKeyStore: Send + Sync {
    /// Returns the saved keys of this exact account.
    async fn load(&self, actor: &AuthorizedActor) -> Result<ProviderKeys, ProviderKeyStoreError>;
}

/// The account's saved provider order (or Waveform's database defaults).
#[async_trait]
pub trait ProviderOrderStore: Send + Sync {
    /// Complete provider order the account's requests start from.
    async fn provider_order(
        &self,
        actor: &AuthorizedActor,
        operation: SpeechOperation,
    ) -> Result<Vec<ProviderName>, crate::domain::error::WaveformError>;
}

/// Redacted credential lookup failure; never falls back to deployment billing.
#[derive(Debug, Error)]
#[error("personal provider credentials are unavailable")]
pub struct ProviderKeyStoreError;

/// Resolves an authorized account's profile independently of provider order.
#[async_trait]
pub trait VoiceProfileStore: Send + Sync {
    /// Reads one complete mapping; no provider calls or API secrets are involved.
    async fn resolve(
        &self,
        actor: &AuthorizedActor,
        requested: Option<&str>,
    ) -> Result<crate::domain::voice::VoiceProfile, crate::domain::error::WaveformError>;
}
