//! Object-safe contracts implemented by infrastructure adapters.

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::{
    auth::{AuthorizationRequest, AuthorizedActor, DelegatedAuthorization, DelegationRequest},
    idempotency::{
        IdempotencyClaim, IdempotencyCompletion, IdempotencyDecision, IdempotencyLeaseId,
        IdempotencyRelease,
    },
    identity::RequestId,
    media::{
        AudioArtifact, BriefcaseFileUrl, GeneratedAudioFileName, MediaSizeLimit, NormalizedAudio,
        SourceMedia, StoredAudio, TemporaryMediaUrl,
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
    /// Unsupported fixture adapters return `None`.
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
    /// Unsupported fixture adapters return `None`.
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

/// Online IAM authorization and proof-exchange boundary.
#[async_trait]
pub trait IamPort: Send + Sync {
    /// Introspects or consumes exactly one credential mode and verifies current
    /// actor membership, organization, audience, and requested action.
    async fn authorize(&self, request: AuthorizationRequest) -> Result<AuthorizedActor, IamError>;

    /// Mints a new short-lived proof for Briefcase. Implementations must never
    /// forward an inbound Waveform proof to the different audience.
    async fn delegate(
        &self,
        request: DelegationRequest,
    ) -> Result<DelegatedAuthorization, IamError>;
}

/// Safe IAM failure category.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum IamError {
    /// Credential is absent, inactive, expired, malformed, or already consumed.
    #[error("IAM rejected the credential")]
    InvalidCredential,
    /// Verified actor or application lacks the requested action.
    #[error("IAM denied the action")]
    Forbidden,
    /// Verified membership does not match the caller-selected organization.
    #[error("IAM organization does not match")]
    OrganizationMismatch,
    /// IAM has not published the required safe proof-exchange operation.
    #[error("required IAM contract is unavailable")]
    ContractUnavailable,
    /// IAM could not be reached or reported temporary unavailability.
    #[error("IAM is unavailable")]
    Unavailable,
    /// IAM exceeded the configured operation deadline.
    #[error("IAM timed out")]
    Timeout,
    /// IAM returned a response that could not establish authorization safely.
    #[error("IAM returned an invalid response")]
    InvalidResponse,
}

/// Input for an authorized, bounded Briefcase source read.
#[derive(Clone, Debug)]
pub struct ReadSourceMediaRequest {
    /// Actor and organization already verified by IAM.
    pub authorization: AuthorizedActor,
    /// Stable permanent URL to resolve inside Briefcase.
    pub source_url: BriefcaseFileUrl,
    /// Original request-local subject token used for fresh per-operation proofs.
    pub subject_token: Option<crate::domain::auth::AccessToken>,
    /// Maximum response size enforced before and during streaming.
    pub size_limit: MediaSizeLimit,
    /// Correlation ID propagated to Briefcase.
    pub request_id: RequestId,
}

/// Input for a non-content Briefcase read check or delivery-URL issuance.
#[derive(Clone, Debug)]
pub struct BriefcaseFileAccessRequest {
    /// Actor and organization already verified by IAM.
    pub authorization: AuthorizedActor,
    /// Original request-local subject token used for fresh per-operation proofs.
    pub subject_token: Option<crate::domain::auth::AccessToken>,
    /// Stable permanent file reference whose current access is checked.
    pub file_url: BriefcaseFileUrl,
    /// Current attempt correlation ID propagated to Briefcase.
    pub request_id: RequestId,
}

/// Input for an idempotent generated-audio upload.
#[derive(Clone, Debug)]
pub struct StoreGeneratedAudioRequest {
    /// Actor and organization already verified by IAM.
    pub authorization: AuthorizedActor,
    /// Newly minted Briefcase-audience credential.
    pub delegated_authorization: DelegatedAuthorization,
    /// Collision-resistant UTC filename.
    pub filename: GeneratedAudioFileName,
    /// Validated MP3 artifact.
    pub audio: NormalizedAudio,
    /// Stable request-derived key that prevents duplicate files.
    pub idempotency_key: crate::domain::idempotency::IdempotencyKey,
    /// Correlation ID propagated to Briefcase.
    pub request_id: RequestId,
}

/// Authorized Briefcase content and generated-audio boundary.
#[async_trait]
pub trait BriefcasePort: Send + Sync {
    /// Verifies current read access without transferring the file content.
    async fn verify_file_read_access(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<(), BriefcaseError>;

    /// Verifies current read access and atomically issues a fresh delivery URL.
    async fn issue_temporary_url(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<TemporaryMediaUrl, BriefcaseError>;

    /// Resolves and reads a permanent URL through Briefcase authorization.
    async fn read_source_media(
        &self,
        request: ReadSourceMediaRequest,
    ) -> Result<SourceMedia, BriefcaseError>;

    /// Stores MP3 in the actor's Waveform app folder and issues a temporary URL.
    async fn store_generated_audio(
        &self,
        request: StoreGeneratedAudioRequest,
    ) -> Result<StoredAudio, BriefcaseError>;
}

/// Safe Briefcase failure category.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BriefcaseError {
    /// Delegated proof was rejected.
    #[error("Briefcase rejected delegated authorization")]
    Unauthorized,
    /// Actor lacks current access to the source or destination.
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
    /// Safe app-folder or authenticated-content operation is not published.
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

/// Account and environment scoped, decrypted provider credentials.
pub type ProviderKeys = std::collections::HashMap<ProviderName, secrecy::SecretString>;

/// Retrieves personal keys only after the caller has been authorized.
#[async_trait]
pub trait ProviderKeyStore: Send + Sync {
    /// Returns keys for this exact principal, organization, and Waveform plane.
    async fn load(
        &self,
        plane_id: uuid::Uuid,
        actor: &AuthorizedActor,
    ) -> Result<ProviderKeys, ProviderKeyStoreError>;
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
        plane_id: uuid::Uuid,
        actor: &AuthorizedActor,
        requested: Option<&str>,
    ) -> Result<crate::domain::voice::VoiceProfile, crate::domain::error::WaveformError>;
}
