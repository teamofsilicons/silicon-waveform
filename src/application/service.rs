//! Provider-independent synchronous TTS and STT orchestration.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use thiserror::Error;

use super::ports::{
    AudioNormalizationError, AudioNormalizer, BriefcaseError, BriefcaseFileAccessRequest,
    BriefcasePort, IamError, IamPort, IdempotencyStore, IdempotencyStoreError, LeaseIdGenerator,
    ReadSourceMediaRequest, SpeechToTextProvider, StoreGeneratedAudioRequest, TextToSpeechProvider,
};
use crate::domain::{
    auth::{
        AuthorizationRequest, AuthorizedActor, DelegationPurpose, DelegationRequest,
        InboundCredentials, WaveformAction,
    },
    error::{Dependency, WaveformError},
    idempotency::{
        CompletedSpeechOperation, CompletedTtsOperation, IdempotencyClaim, IdempotencyCompletion,
        IdempotencyDecision, IdempotencyKey, IdempotencyRelease, IdempotencyScope, RequestDigest,
        RequestDigestKey, SpeechOperation,
    },
    identity::{OrganizationId, RequestId},
    media::{BriefcaseFileUrl, BriefcaseOrigin, GeneratedAudioFileName, MediaSizeLimit},
    provider::{ProviderName, STT_PROVIDER_CHAIN, TTS_PROVIDER_CHAIN},
    speech::{
        MAX_TTS_TEXT_CHARACTERS, SttProviderRequest, SttRequest, SttResult, TtsProviderRequest,
        TtsRequest, TtsResult,
    },
};

/// Authenticated transport context shared by both speech operations.
#[derive(Clone, Debug)]
pub struct SpeechRequestContext {
    /// Candidate request ID. A reclaimed idempotency record may replace it with
    /// the canonical ID retained by the first attempt.
    pub request_id: RequestId,
    /// Caller-selected organization, verified online by IAM.
    pub organization_id: OrganizationId,
    /// Exactly one inbound credential mode.
    pub credentials: InboundCredentials,
    /// Caller-provided idempotency key.
    pub idempotency_key: IdempotencyKey,
}

/// Successful application result plus idempotency replay metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceResponse<T> {
    /// Normalized operation result.
    pub result: T,
    /// True only when the authoritative store supplied a completed response.
    pub replayed: bool,
}

impl<T> ServiceResponse<T> {
    fn fresh(result: T) -> Self {
        Self {
            result,
            replayed: false,
        }
    }

    fn replayed(result: T) -> Self {
        Self {
            result,
            replayed: true,
        }
    }

    /// Consumes the wrapper and returns the normalized domain result.
    #[must_use]
    pub fn into_result(self) -> T {
        self.result
    }
}

/// Validated application policy that affects deterministic orchestration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServicePolicy {
    lease_duration: Duration,
    source_media_size_limit: MediaSizeLimit,
    briefcase_origin: BriefcaseOrigin,
    max_text_chars: usize,
    request_digest_key: RequestDigestKey,
}

impl ServicePolicy {
    /// Constructs policy with a finite, representable lease period.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceConstructionError::InvalidDuration`] when the lease
    /// duration is zero or cannot be represented by the persistence time model,
    /// or [`ServiceConstructionError::InvalidTextLimit`] when the configured
    /// text limit is outside the domain ceiling.
    pub fn new(
        lease_duration: Duration,
        source_media_size_limit: MediaSizeLimit,
        briefcase_origin: BriefcaseOrigin,
        max_text_chars: usize,
        request_digest_key: RequestDigestKey,
    ) -> Result<Self, ServiceConstructionError> {
        if lease_duration.is_zero() {
            return Err(ServiceConstructionError::InvalidDuration);
        }
        time::Duration::try_from(lease_duration)
            .map_err(|_| ServiceConstructionError::InvalidDuration)?;
        if !(1..=MAX_TTS_TEXT_CHARACTERS).contains(&max_text_chars) {
            return Err(ServiceConstructionError::InvalidTextLimit);
        }
        Ok(Self {
            lease_duration,
            source_media_size_limit,
            briefcase_origin,
            max_text_chars,
            request_digest_key,
        })
    }

    /// Idempotency lease lifetime.
    #[must_use]
    pub const fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    /// Maximum source bytes accepted from Briefcase.
    #[must_use]
    pub const fn source_media_size_limit(&self) -> MediaSizeLimit {
        self.source_media_size_limit
    }

    /// Maximum Unicode scalars accepted by the deployed TTS service.
    #[must_use]
    pub const fn max_text_chars(&self) -> usize {
        self.max_text_chars
    }
}

/// Complete application service with dependencies supplied through object-safe ports.
pub struct WaveformService {
    iam: Arc<dyn IamPort>,
    briefcase: Arc<dyn BriefcasePort>,
    idempotency: Arc<dyn IdempotencyStore>,
    audio_normalizer: Arc<dyn AudioNormalizer>,
    lease_id_generator: Arc<dyn LeaseIdGenerator>,
    tts_providers: Vec<Arc<dyn TextToSpeechProvider>>,
    stt_providers: Vec<Arc<dyn SpeechToTextProvider>>,
    policy: ServicePolicy,
}

impl WaveformService {
    /// Creates a service only when both provider lists exactly match product order.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceConstructionError`] when either provider chain differs
    /// from the documented fallback order.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        iam: Arc<dyn IamPort>,
        briefcase: Arc<dyn BriefcasePort>,
        idempotency: Arc<dyn IdempotencyStore>,
        audio_normalizer: Arc<dyn AudioNormalizer>,
        lease_id_generator: Arc<dyn LeaseIdGenerator>,
        tts_providers: Vec<Arc<dyn TextToSpeechProvider>>,
        stt_providers: Vec<Arc<dyn SpeechToTextProvider>>,
        policy: ServicePolicy,
    ) -> Result<Self, ServiceConstructionError> {
        let actual_tts = tts_providers
            .iter()
            .map(|provider| provider.name())
            .collect::<Vec<_>>();
        if actual_tts.as_slice() != TTS_PROVIDER_CHAIN {
            return Err(ServiceConstructionError::InvalidTtsProviderChain { actual: actual_tts });
        }

        let actual_stt = stt_providers
            .iter()
            .map(|provider| provider.name())
            .collect::<Vec<_>>();
        if actual_stt.as_slice() != STT_PROVIDER_CHAIN {
            return Err(ServiceConstructionError::InvalidSttProviderChain { actual: actual_stt });
        }

        Ok(Self {
            iam,
            briefcase,
            idempotency,
            audio_normalizer,
            lease_id_generator,
            tts_providers,
            stt_providers,
            policy,
        })
    }

    /// Runs the synchronous text-to-speech workflow.
    ///
    /// # Errors
    ///
    /// Returns a stable [`WaveformError`] after authorization, idempotency,
    /// provider, normalization, or Briefcase work fails terminally.
    pub async fn synthesize(
        &self,
        context: SpeechRequestContext,
        request: TtsRequest,
    ) -> Result<ServiceResponse<TtsResult>, WaveformError> {
        if request.text.character_count() > self.policy.max_text_chars {
            return Err(WaveformError::PayloadTooLarge);
        }
        let authorization = self
            .authorize(&context, WaveformAction::SynthesizeSpeech)
            .await?;
        let scope = IdempotencyScope {
            actor: authorization.actor,
            organization_id: authorization.organization_id.clone(),
            operation: SpeechOperation::Tts,
            key: context.idempotency_key,
        };
        let decision = self
            .claim(
                scope.clone(),
                RequestDigest::for_tts(&request, &self.policy.request_digest_key),
                context.request_id,
            )
            .await?;

        let (lease, canonical_request_id, operation_started_at) = match decision {
            IdempotencyDecision::Acquired {
                lease,
                request_id,
                operation_started_at,
            } => (lease, request_id, operation_started_at),
            IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(completed)) => {
                let result = self
                    .replay_tts(context.request_id, authorization, completed)
                    .await?;
                return Ok(ServiceResponse::replayed(result));
            }
            IdempotencyDecision::Replay(CompletedSpeechOperation::Stt(_)) => {
                return Err(WaveformError::Internal);
            }
            IdempotencyDecision::KeyReused => return Err(WaveformError::IdempotencyKeyReused),
            IdempotencyDecision::InProgress { retry_after } => {
                return Err(WaveformError::RequestInProgress { retry_after });
            }
        };

        let result = self
            .execute_tts(
                canonical_request_id,
                operation_started_at,
                authorization,
                request,
            )
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.release_lease(scope, lease.id).await;
                return Err(error);
            }
        };

        let completion = Self::completion(
            scope.clone(),
            lease.id,
            CompletedSpeechOperation::Tts(CompletedTtsOperation::from_result(&result)),
        );
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => {
                self.release_lease(scope, lease.id).await;
                return Err(error);
            }
        };
        if let Err(error) = self.idempotency.complete(completion).await {
            self.release_lease(scope, lease.id).await;
            return Err(map_idempotency_error(error));
        }

        Ok(ServiceResponse::fresh(result))
    }

    /// Runs the synchronous speech-to-text workflow.
    ///
    /// # Errors
    ///
    /// Returns a stable [`WaveformError`] after authorization, idempotency,
    /// Briefcase, or provider work fails terminally.
    pub async fn transcribe(
        &self,
        context: SpeechRequestContext,
        request: SttRequest,
    ) -> Result<ServiceResponse<SttResult>, WaveformError> {
        let authorization = self
            .authorize(&context, WaveformAction::TranscribeSpeech)
            .await?;
        if !self.policy.briefcase_origin.contains(&request.source_url) {
            return Err(WaveformError::InvalidRequest);
        }

        let scope = IdempotencyScope {
            actor: authorization.actor,
            organization_id: authorization.organization_id.clone(),
            operation: SpeechOperation::Stt,
            key: context.idempotency_key,
        };
        let decision = self
            .claim(
                scope.clone(),
                RequestDigest::for_stt(&request, &self.policy.request_digest_key),
                context.request_id,
            )
            .await?;

        let (lease, canonical_request_id) = match decision {
            IdempotencyDecision::Acquired {
                lease, request_id, ..
            } => (lease, request_id),
            IdempotencyDecision::Replay(CompletedSpeechOperation::Stt(result)) => {
                self.reauthorize_stt_replay(
                    context.request_id,
                    authorization,
                    request.source_url.clone(),
                )
                .await?;
                return Ok(ServiceResponse::replayed(result));
            }
            IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(_)) => {
                return Err(WaveformError::Internal);
            }
            IdempotencyDecision::KeyReused => return Err(WaveformError::IdempotencyKeyReused),
            IdempotencyDecision::InProgress { retry_after } => {
                return Err(WaveformError::RequestInProgress { retry_after });
            }
        };

        let result = self
            .execute_stt(canonical_request_id, authorization, request)
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.release_lease(scope, lease.id).await;
                return Err(error);
            }
        };

        let completion = Self::completion(
            scope.clone(),
            lease.id,
            CompletedSpeechOperation::Stt(result.clone()),
        );
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => {
                self.release_lease(scope, lease.id).await;
                return Err(error);
            }
        };
        if let Err(error) = self.idempotency.complete(completion).await {
            self.release_lease(scope, lease.id).await;
            return Err(map_idempotency_error(error));
        }

        Ok(ServiceResponse::fresh(result))
    }

    async fn authorize(
        &self,
        context: &SpeechRequestContext,
        action: WaveformAction,
    ) -> Result<AuthorizedActor, WaveformError> {
        let expected_organization = context.organization_id.clone();
        let authorization = self
            .iam
            .authorize(AuthorizationRequest {
                credentials: context.credentials.clone(),
                organization_id: expected_organization.clone(),
                action,
                request_id: context.request_id,
            })
            .await
            .map_err(map_iam_error)?;

        if authorization.organization_id != expected_organization {
            return Err(WaveformError::Forbidden);
        }
        Ok(authorization)
    }

    async fn claim(
        &self,
        scope: IdempotencyScope,
        digest: RequestDigest,
        candidate_request_id: RequestId,
    ) -> Result<IdempotencyDecision, WaveformError> {
        self.idempotency
            .claim(IdempotencyClaim {
                scope,
                digest,
                request_id: candidate_request_id,
                lease_id: self.lease_id_generator.new_idempotency_lease_id(),
                lease_duration: self.policy.lease_duration,
            })
            .await
            .map_err(map_idempotency_error)
    }

    async fn replay_tts(
        &self,
        attempt_request_id: RequestId,
        authorization: AuthorizedActor,
        completed: CompletedTtsOperation,
    ) -> Result<TtsResult, WaveformError> {
        let delegated_authorization = self
            .iam
            .delegate(DelegationRequest {
                authorization: authorization.clone(),
                purpose: DelegationPurpose::ReadBriefcaseFile,
                request_id: attempt_request_id,
            })
            .await
            .map_err(map_iam_error)?;
        let temporary_url = self
            .briefcase
            .issue_temporary_url(BriefcaseFileAccessRequest {
                authorization,
                delegated_authorization,
                file_url: completed.permanent_url.clone(),
                request_id: attempt_request_id,
            })
            .await
            .map_err(map_briefcase_error)?;

        Ok(completed.with_temporary_url(temporary_url))
    }

    async fn reauthorize_stt_replay(
        &self,
        attempt_request_id: RequestId,
        authorization: AuthorizedActor,
        source_url: BriefcaseFileUrl,
    ) -> Result<(), WaveformError> {
        let delegated_authorization = self
            .iam
            .delegate(DelegationRequest {
                authorization: authorization.clone(),
                purpose: DelegationPurpose::ReadBriefcaseFile,
                request_id: attempt_request_id,
            })
            .await
            .map_err(map_iam_error)?;
        self.briefcase
            .verify_file_read_access(BriefcaseFileAccessRequest {
                authorization,
                delegated_authorization,
                file_url: source_url,
                request_id: attempt_request_id,
            })
            .await
            .map_err(map_briefcase_error)
    }

    async fn execute_tts(
        &self,
        request_id: RequestId,
        operation_started_at: time::OffsetDateTime,
        authorization: AuthorizedActor,
        request: TtsRequest,
    ) -> Result<TtsResult, WaveformError> {
        // Establish downstream authority before invoking a billable provider.
        // The current production IAM adapter fails closed here until proof
        // exchange is published, avoiding charges for audio that cannot be stored.
        let delegated_authorization = self
            .iam
            .delegate(DelegationRequest {
                authorization: authorization.clone(),
                purpose: DelegationPurpose::StoreGeneratedAudio,
                request_id,
            })
            .await
            .map_err(map_iam_error)?;
        let mut successful_audio = None;
        for provider in &self.tts_providers {
            let provider_name = provider.name();
            let started = Instant::now();
            let artifact = match provider
                .synthesize(
                    TtsProviderRequest::for_provider(&request, provider_name),
                    request_id,
                )
                .await
            {
                Ok(artifact) => artifact,
                Err(error) => {
                    trace_provider_attempt(
                        request_id,
                        "tts",
                        provider_name,
                        error.kind.as_str(),
                        started,
                    );
                    continue;
                }
            };

            match self
                .audio_normalizer
                .normalize_to_mp3(artifact, request_id)
                .await
            {
                Ok(audio) => {
                    trace_provider_attempt(request_id, "tts", provider_name, "success", started);
                    successful_audio = Some((provider_name, audio));
                    break;
                }
                Err(AudioNormalizationError::InvalidAudio) => {
                    trace_provider_attempt(
                        request_id,
                        "tts",
                        provider_name,
                        "invalid_audio",
                        started,
                    );
                }
                Err(AudioNormalizationError::Failed) => {
                    trace_provider_attempt(
                        request_id,
                        "tts",
                        provider_name,
                        "normalization_failed",
                        started,
                    );
                }
                Err(error) => {
                    trace_provider_attempt(
                        request_id,
                        "tts",
                        provider_name,
                        audio_failure_outcome(error),
                        started,
                    );
                    return Err(map_audio_error(error));
                }
            }
        }

        let (provider, audio) = successful_audio.ok_or(WaveformError::ProvidersExhausted)?;
        let duration = audio.duration();
        let filename = GeneratedAudioFileName::for_request(operation_started_at, request_id);
        let stored = self
            .briefcase
            .store_generated_audio(StoreGeneratedAudioRequest {
                authorization,
                delegated_authorization,
                filename,
                audio,
                idempotency_key: IdempotencyKey::for_briefcase_upload(request_id),
                request_id,
            })
            .await
            .map_err(map_briefcase_error)?;

        Ok(TtsResult::new(request_id, stored, provider, duration))
    }

    async fn execute_stt(
        &self,
        request_id: RequestId,
        authorization: AuthorizedActor,
        request: SttRequest,
    ) -> Result<SttResult, WaveformError> {
        let delegated_authorization = self
            .iam
            .delegate(DelegationRequest {
                authorization: authorization.clone(),
                purpose: DelegationPurpose::ReadBriefcaseFile,
                request_id,
            })
            .await
            .map_err(map_iam_error)?;
        let media = self
            .briefcase
            .read_source_media(ReadSourceMediaRequest {
                authorization,
                source_url: request.source_url,
                delegated_authorization,
                size_limit: self.policy.source_media_size_limit,
                request_id,
            })
            .await
            .map_err(map_briefcase_error)?;

        for provider in &self.stt_providers {
            let provider_name = provider.name();
            let started = Instant::now();
            match provider
                .transcribe(
                    SttProviderRequest {
                        media: media.clone(),
                        language: request.language.clone(),
                    },
                    request_id,
                )
                .await
            {
                Ok(result) => {
                    trace_provider_attempt(request_id, "stt", provider_name, "success", started);
                    return Ok(SttResult::from_provider(request_id, provider_name, result));
                }
                Err(error) => {
                    trace_provider_attempt(
                        request_id,
                        "stt",
                        provider_name,
                        error.kind.as_str(),
                        started,
                    );
                }
            }
        }

        Err(WaveformError::ProvidersExhausted)
    }

    fn completion(
        scope: IdempotencyScope,
        lease_id: crate::domain::idempotency::IdempotencyLeaseId,
        response: CompletedSpeechOperation,
    ) -> Result<IdempotencyCompletion, WaveformError> {
        if response.operation() != scope.operation {
            return Err(WaveformError::Internal);
        }
        Ok(IdempotencyCompletion {
            scope,
            lease_id,
            response,
        })
    }

    async fn release_lease(
        &self,
        scope: IdempotencyScope,
        lease_id: crate::domain::idempotency::IdempotencyLeaseId,
    ) {
        let _release_result = self
            .idempotency
            .release(IdempotencyRelease { scope, lease_id })
            .await;
    }
}

fn trace_provider_attempt(
    request_id: RequestId,
    operation: &'static str,
    provider: ProviderName,
    outcome: &'static str,
    started: Instant,
) {
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        request_id = %request_id,
        operation,
        provider = %provider,
        outcome,
        elapsed_ms,
        "speech provider attempt completed"
    );
}

const fn audio_failure_outcome(error: AudioNormalizationError) -> &'static str {
    match error {
        AudioNormalizationError::Saturated => "normalizer_saturated",
        AudioNormalizationError::Timeout => "normalizer_timeout",
        AudioNormalizationError::Unavailable => "normalizer_unavailable",
        AudioNormalizationError::InvalidAudio => "invalid_audio",
        AudioNormalizationError::Failed => "normalization_failed",
    }
}

/// Service wiring or policy validation failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ServiceConstructionError {
    /// Lease and retention periods must be positive and representable by `time`.
    #[error("lease and retention durations must be finite and non-zero")]
    InvalidDuration,
    /// Configured TTS text limit is zero or exceeds the domain ceiling.
    #[error("invalid TTS text-character limit")]
    InvalidTextLimit,
    /// TTS adapters do not exactly match the documented order.
    #[error("invalid TTS provider chain: {actual:?}")]
    InvalidTtsProviderChain {
        /// Actual configured order.
        actual: Vec<ProviderName>,
    },
    /// STT adapters do not exactly match the documented order.
    #[error("invalid STT provider chain: {actual:?}")]
    InvalidSttProviderChain {
        /// Actual configured order.
        actual: Vec<ProviderName>,
    },
}

fn map_iam_error(error: IamError) -> WaveformError {
    match error {
        IamError::InvalidCredential => WaveformError::Unauthenticated,
        IamError::Forbidden | IamError::OrganizationMismatch => WaveformError::Forbidden,
        IamError::ContractUnavailable => WaveformError::DependencyContractUnavailable {
            dependency: Dependency::Iam,
        },
        IamError::Unavailable => WaveformError::DependencyUnavailable {
            dependency: Dependency::Iam,
        },
        IamError::Timeout => WaveformError::DependencyTimeout {
            dependency: Dependency::Iam,
        },
        IamError::InvalidResponse => WaveformError::Internal,
    }
}

fn map_briefcase_error(error: BriefcaseError) -> WaveformError {
    match error {
        BriefcaseError::InvalidResponse => WaveformError::Internal,
        BriefcaseError::Forbidden => WaveformError::Forbidden,
        BriefcaseError::NotFound => WaveformError::SourceNotFound,
        BriefcaseError::MediaTooLarge => WaveformError::PayloadTooLarge,
        BriefcaseError::UnsupportedMediaType => WaveformError::UnsupportedMediaType,
        BriefcaseError::ContractUnavailable => WaveformError::DependencyContractUnavailable {
            dependency: Dependency::Briefcase,
        },
        BriefcaseError::Unauthorized | BriefcaseError::Unavailable => {
            WaveformError::DependencyUnavailable {
                dependency: Dependency::Briefcase,
            }
        }
        BriefcaseError::Timeout => WaveformError::DependencyTimeout {
            dependency: Dependency::Briefcase,
        },
    }
}

fn map_idempotency_error(error: IdempotencyStoreError) -> WaveformError {
    match error {
        IdempotencyStoreError::Unavailable => WaveformError::DependencyUnavailable {
            dependency: Dependency::IdempotencyStore,
        },
        IdempotencyStoreError::Timeout => WaveformError::DependencyTimeout {
            dependency: Dependency::IdempotencyStore,
        },
        IdempotencyStoreError::InvalidRecord | IdempotencyStoreError::LeaseLost => {
            WaveformError::Internal
        }
    }
}

fn map_audio_error(error: AudioNormalizationError) -> WaveformError {
    match error {
        AudioNormalizationError::Saturated | AudioNormalizationError::Unavailable => {
            WaveformError::DependencyUnavailable {
                dependency: Dependency::AudioNormalizer,
            }
        }
        AudioNormalizationError::Timeout => WaveformError::DependencyTimeout {
            dependency: Dependency::AudioNormalizer,
        },
        AudioNormalizationError::InvalidAudio | AudioNormalizationError::Failed => {
            WaveformError::Internal
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        str::FromStr as _,
        sync::{Arc, Mutex, MutexGuard},
        time::Duration,
    };

    use async_trait::async_trait;
    use bytes::Bytes;
    use time::macros::datetime;
    use url::Url;
    use uuid::Uuid;

    use super::{ServicePolicy, ServiceResponse, SpeechRequestContext, WaveformService};
    use crate::{
        application::ports::{
            AudioNormalizationError, AudioNormalizer, BriefcaseError, BriefcaseFileAccessRequest,
            BriefcasePort, IamError, IamPort, IdempotencyStore, IdempotencyStoreError,
            LeaseIdGenerator, ReadSourceMediaRequest, SpeechToTextProvider,
            StoreGeneratedAudioRequest, TextToSpeechProvider,
        },
        domain::{
            auth::{
                AccessToken, AuthorizationRequest, AuthorizedActor, DelegatedAuthorization,
                DelegationRequest, InboundCredentials, OboCredentials, OboProof,
            },
            error::WaveformError,
            idempotency::{
                CompletedSpeechOperation, CompletedTtsOperation, IdempotencyClaim,
                IdempotencyCompletion, IdempotencyDecision, IdempotencyKey, IdempotencyLease,
                IdempotencyLeaseId, IdempotencyRelease, RequestDigestKey,
            },
            identity::{Actor, ActorId, ActorKind, ApplicationId, OrganizationId, RequestId},
            media::{
                AudioArtifact, BriefcaseFileUrl, BriefcaseOrigin, MediaDuration, MediaSizeLimit,
                NormalizedAudio, ProviderAudioFormat, SourceMedia, SourceMediaType, StoredAudio,
                TemporaryMediaUrl,
            },
            provider::{ProviderError, ProviderFailureKind, ProviderName},
            speech::{
                MAX_TTS_TEXT_CHARACTERS, SpeechText, SttProviderRequest, SttProviderResult,
                SttRequest, SttResult, Transcript, TtsProviderRequest, TtsRequest,
            },
        },
    };

    struct FakeLeaseIds {
        lease_id: IdempotencyLeaseId,
    }

    impl LeaseIdGenerator for FakeLeaseIds {
        fn new_idempotency_lease_id(&self) -> IdempotencyLeaseId {
            self.lease_id
        }
    }

    struct FakeIam {
        authorized: AuthorizedActor,
        authorize_count: Mutex<usize>,
        delegated_request_ids: Mutex<Vec<RequestId>>,
    }

    #[async_trait]
    impl IamPort for FakeIam {
        async fn authorize(
            &self,
            request: AuthorizationRequest,
        ) -> Result<AuthorizedActor, IamError> {
            *lock(&self.authorize_count) += 1;
            let mut authorized = self.authorized.clone();
            authorized.originating_application = match request.credentials {
                InboundCredentials::Bearer(_) => None,
                InboundCredentials::OnBehalfOf(credentials) => Some(credentials.application_id),
            };
            Ok(authorized)
        }

        async fn delegate(
            &self,
            request: DelegationRequest,
        ) -> Result<DelegatedAuthorization, IamError> {
            lock(&self.delegated_request_ids).push(request.request_id);
            Ok(DelegatedAuthorization {
                application_id: application_id("silicon-waveform"),
                proof: secret_proof(),
                purpose: request.purpose,
                expires_at: datetime!(2026-08-31 07:05:56 UTC),
            })
        }
    }

    struct FakeIdempotency {
        decisions: Mutex<VecDeque<Result<IdempotencyDecision, IdempotencyStoreError>>>,
        claims: Mutex<Vec<IdempotencyClaim>>,
        completions: Mutex<Vec<IdempotencyCompletion>>,
        completion_result: Mutex<Result<(), IdempotencyStoreError>>,
        releases: Mutex<Vec<IdempotencyRelease>>,
    }

    #[async_trait]
    impl IdempotencyStore for FakeIdempotency {
        async fn claim(
            &self,
            claim: IdempotencyClaim,
        ) -> Result<IdempotencyDecision, IdempotencyStoreError> {
            lock(&self.claims).push(claim);
            lock(&self.decisions)
                .pop_front()
                .unwrap_or(Err(IdempotencyStoreError::InvalidRecord))
        }

        async fn complete(
            &self,
            completion: IdempotencyCompletion,
        ) -> Result<(), IdempotencyStoreError> {
            lock(&self.completions).push(completion);
            *lock(&self.completion_result)
        }

        async fn release(&self, release: IdempotencyRelease) -> Result<(), IdempotencyStoreError> {
            lock(&self.releases).push(release);
            Ok(())
        }

        async fn check_ready(&self) -> Result<(), IdempotencyStoreError> {
            Ok(())
        }
    }

    struct FakeBriefcase {
        source: Result<SourceMedia, BriefcaseError>,
        stored: Result<StoredAudio, BriefcaseError>,
        access_result: Mutex<Result<(), BriefcaseError>>,
        temporary_url_result: Mutex<Result<TemporaryMediaUrl, BriefcaseError>>,
        verified_accesses: Mutex<Vec<(RequestId, String)>>,
        temporary_url_requests: Mutex<Vec<(RequestId, String)>>,
        read_request_ids: Mutex<Vec<RequestId>>,
        store_requests: Mutex<Vec<(RequestId, String, String)>>,
    }

    #[async_trait]
    impl BriefcasePort for FakeBriefcase {
        async fn verify_file_read_access(
            &self,
            request: BriefcaseFileAccessRequest,
        ) -> Result<(), BriefcaseError> {
            lock(&self.verified_accesses).push((request.request_id, request.file_url.to_string()));
            *lock(&self.access_result)
        }

        async fn issue_temporary_url(
            &self,
            request: BriefcaseFileAccessRequest,
        ) -> Result<TemporaryMediaUrl, BriefcaseError> {
            lock(&self.temporary_url_requests)
                .push((request.request_id, request.file_url.to_string()));
            lock(&self.temporary_url_result).clone()
        }

        async fn read_source_media(
            &self,
            request: ReadSourceMediaRequest,
        ) -> Result<SourceMedia, BriefcaseError> {
            lock(&self.read_request_ids).push(request.request_id);
            self.source.clone()
        }

        async fn store_generated_audio(
            &self,
            request: StoreGeneratedAudioRequest,
        ) -> Result<StoredAudio, BriefcaseError> {
            lock(&self.store_requests).push((
                request.request_id,
                request.filename.to_string(),
                request.idempotency_key.as_str().to_owned(),
            ));
            self.stored.clone()
        }
    }

    struct FakeNormalizer {
        result: Result<NormalizedAudio, AudioNormalizationError>,
    }

    #[async_trait]
    impl AudioNormalizer for FakeNormalizer {
        async fn normalize_to_mp3(
            &self,
            _artifact: AudioArtifact,
            _request_id: RequestId,
        ) -> Result<NormalizedAudio, AudioNormalizationError> {
            self.result.clone()
        }

        async fn check_ready(&self) -> Result<(), AudioNormalizationError> {
            Ok(())
        }
    }

    struct FakeTtsProvider {
        name: ProviderName,
        result: Result<AudioArtifact, ProviderError>,
        request_ids: Mutex<Vec<RequestId>>,
    }

    #[async_trait]
    impl TextToSpeechProvider for FakeTtsProvider {
        fn name(&self) -> ProviderName {
            self.name
        }

        async fn synthesize(
            &self,
            _request: TtsProviderRequest,
            request_id: RequestId,
        ) -> Result<AudioArtifact, ProviderError> {
            lock(&self.request_ids).push(request_id);
            self.result.clone()
        }
    }

    struct FakeSttProvider {
        name: ProviderName,
        result: Result<SttProviderResult, ProviderError>,
        request_ids: Mutex<Vec<RequestId>>,
    }

    #[async_trait]
    impl SpeechToTextProvider for FakeSttProvider {
        fn name(&self) -> ProviderName {
            self.name
        }

        async fn transcribe(
            &self,
            _request: SttProviderRequest,
            request_id: RequestId,
        ) -> Result<SttProviderResult, ProviderError> {
            lock(&self.request_ids).push(request_id);
            self.result.clone()
        }
    }

    struct Fixture {
        service: WaveformService,
        iam: Arc<FakeIam>,
        idempotency: Arc<FakeIdempotency>,
        briefcase: Arc<FakeBriefcase>,
        tts: Vec<Arc<FakeTtsProvider>>,
        stt: Vec<Arc<FakeSttProvider>>,
    }

    #[tokio::test]
    async fn reclaimed_tts_uses_the_original_request_id_for_every_side_effect() {
        let candidate_id = request_id(100);
        let canonical_id = request_id(7);
        let fixture = fixture(
            IdempotencyDecision::Acquired {
                lease: lease(),
                request_id: canonical_id,
                operation_started_at: original_operation_start(),
            },
            [
                provider_failure(ProviderName::Gemini),
                Ok(audio_artifact()),
                provider_failure(ProviderName::OpenAi),
            ],
            [
                provider_failure(ProviderName::Gemini),
                provider_failure(ProviderName::OpenAi),
                provider_failure(ProviderName::Deepgram),
            ],
        );

        let result = fixture
            .service
            .synthesize(context(candidate_id, "reclaim-key"), tts_request())
            .await;

        assert!(matches!(
            &result,
            Ok(response) if response.result.request_id == canonical_id && !response.replayed
        ));
        assert_eq!(&*lock(&fixture.tts[0].request_ids), &[canonical_id]);
        assert_eq!(&*lock(&fixture.tts[1].request_ids), &[canonical_id]);
        assert!(lock(&fixture.tts[2].request_ids).is_empty());
        let stores = lock(&fixture.briefcase.store_requests);
        assert!(matches!(
            stores.as_slice(),
            [(request_id, filename, key)]
                if *request_id == canonical_id
                    && filename.starts_with("tts_20260830_010203_")
                    && filename.contains(&canonical_id.to_string())
                    && key == &format!("waveform:briefcase:tts:{canonical_id}")
        ));
        assert!(lock(&fixture.idempotency.releases).is_empty());
        assert!(matches!(
            lock(&fixture.idempotency.completions).as_slice(),
            [completion]
                if matches!(
                    &completion.response,
                    CompletedSpeechOperation::Tts(result) if result.request_id == canonical_id
                )
        ));
    }

    #[tokio::test]
    async fn configured_text_limit_rejects_before_authorization_and_claim() {
        let mut fixture = fixture(
            IdempotencyDecision::InProgress {
                retry_after: Duration::from_secs(2),
            },
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );
        fixture.service.policy.max_text_chars = 3;

        let result = fixture
            .service
            .synthesize(context(request_id(8), "text-limit"), tts_request())
            .await;

        assert_eq!(result, Err(WaveformError::PayloadTooLarge));
        assert_eq!(*lock(&fixture.iam.authorize_count), 0);
        assert!(lock(&fixture.idempotency.claims).is_empty());
    }

    #[tokio::test]
    async fn tts_replay_reauthorizes_the_file_and_issues_a_fresh_temporary_url() {
        let completed = replayed_tts_result(request_id(11));
        let expected = completed.clone().with_temporary_url(fresh_temporary_url());
        let fixture = fixture(
            IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(completed.clone())),
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );
        let attempt_request_id = request_id(99);

        let result = fixture
            .service
            .synthesize(context(attempt_request_id, "replay-key"), tts_request())
            .await;

        assert_eq!(result, Ok(ServiceResponse::replayed(expected)));
        assert_eq!(*lock(&fixture.iam.authorize_count), 1);
        assert_eq!(
            &*lock(&fixture.iam.delegated_request_ids),
            &[attempt_request_id]
        );
        assert_eq!(
            &*lock(&fixture.briefcase.temporary_url_requests),
            &[(attempt_request_id, completed.permanent_url.to_string())]
        );
        assert!(lock(&fixture.briefcase.verified_accesses).is_empty());
        assert!(lock(&fixture.briefcase.store_requests).is_empty());
        assert!(
            fixture
                .tts
                .iter()
                .all(|provider| lock(&provider.request_ids).is_empty())
        );
        assert!(lock(&fixture.idempotency.completions).is_empty());
        assert!(lock(&fixture.idempotency.releases).is_empty());
    }

    #[tokio::test]
    async fn tts_replay_denial_never_reruns_a_provider_or_changes_the_record() {
        let completed = replayed_tts_result(request_id(12));
        let fixture = fixture(
            IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(completed)),
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );
        *lock(&fixture.briefcase.temporary_url_result) = Err(BriefcaseError::Forbidden);

        let result = fixture
            .service
            .synthesize(context(request_id(98), "replay-denied"), tts_request())
            .await;

        assert_eq!(result, Err(WaveformError::Forbidden));
        assert!(
            fixture
                .tts
                .iter()
                .all(|provider| lock(&provider.request_ids).is_empty())
        );
        assert!(lock(&fixture.briefcase.store_requests).is_empty());
        assert!(lock(&fixture.idempotency.completions).is_empty());
        assert!(lock(&fixture.idempotency.releases).is_empty());
    }

    #[tokio::test]
    async fn stt_replay_verifies_current_source_access_without_downloading_media() {
        let replayed = replayed_stt_result(request_id(13));
        let fixture = fixture(
            IdempotencyDecision::Replay(CompletedSpeechOperation::Stt(replayed.clone())),
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );
        let attempt_request_id = request_id(97);

        let result = fixture
            .service
            .transcribe(context(attempt_request_id, "stt-replay"), stt_request())
            .await;

        assert_eq!(result, Ok(ServiceResponse::replayed(replayed)));
        assert_eq!(
            &*lock(&fixture.briefcase.verified_accesses),
            &[(attempt_request_id, briefcase_file_url().to_string())]
        );
        assert!(lock(&fixture.briefcase.read_request_ids).is_empty());
        assert!(
            fixture
                .stt
                .iter()
                .all(|provider| lock(&provider.request_ids).is_empty())
        );
        assert!(lock(&fixture.idempotency.completions).is_empty());
        assert!(lock(&fixture.idempotency.releases).is_empty());
    }

    #[tokio::test]
    async fn stt_replay_denial_never_exposes_the_cached_transcript() {
        let fixture = fixture(
            IdempotencyDecision::Replay(CompletedSpeechOperation::Stt(replayed_stt_result(
                request_id(14),
            ))),
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );
        *lock(&fixture.briefcase.access_result) = Err(BriefcaseError::NotFound);

        let result = fixture
            .service
            .transcribe(context(request_id(96), "stt-denied"), stt_request())
            .await;

        assert_eq!(result, Err(WaveformError::SourceNotFound));
        assert!(lock(&fixture.briefcase.read_request_ids).is_empty());
        assert!(
            fixture
                .stt
                .iter()
                .all(|provider| lock(&provider.request_ids).is_empty())
        );
        assert!(lock(&fixture.idempotency.completions).is_empty());
        assert!(lock(&fixture.idempotency.releases).is_empty());
    }

    #[tokio::test]
    async fn exhausted_stt_chain_releases_the_owned_lease() {
        let canonical_id = request_id(21);
        let fixture = fixture(
            IdempotencyDecision::Acquired {
                lease: lease(),
                request_id: canonical_id,
                operation_started_at: original_operation_start(),
            },
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );

        let result = fixture
            .service
            .transcribe(context(request_id(22), "failure-key"), stt_request())
            .await;

        assert_eq!(result, Err(WaveformError::ProvidersExhausted));
        assert_eq!(&*lock(&fixture.briefcase.read_request_ids), &[canonical_id]);
        assert!(
            fixture
                .stt
                .iter()
                .all(|provider| lock(&provider.request_ids).as_slice() == [canonical_id])
        );
        assert_eq!(lock(&fixture.idempotency.releases).len(), 1);
        assert!(lock(&fixture.idempotency.completions).is_empty());
    }

    #[tokio::test]
    async fn in_progress_duplicate_never_releases_someone_elses_lease() {
        let fixture = fixture(
            IdempotencyDecision::InProgress {
                retry_after: Duration::from_secs(2),
            },
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );

        let result = fixture
            .service
            .synthesize(context(request_id(31), "progress-key"), tts_request())
            .await;

        assert_eq!(
            result,
            Err(WaveformError::RequestInProgress {
                retry_after: Duration::from_secs(2)
            })
        );
        assert!(lock(&fixture.idempotency.releases).is_empty());
        assert!(
            fixture
                .tts
                .iter()
                .all(|provider| lock(&provider.request_ids).is_empty())
        );
    }

    #[tokio::test]
    async fn reused_key_never_touches_briefcase_or_a_speech_provider() {
        let fixture = fixture(
            IdempotencyDecision::KeyReused,
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );

        let result = fixture
            .service
            .synthesize(context(request_id(32), "reused-key"), tts_request())
            .await;

        assert_eq!(result, Err(WaveformError::IdempotencyKeyReused));
        assert_eq!(*lock(&fixture.iam.authorize_count), 1);
        assert!(lock(&fixture.iam.delegated_request_ids).is_empty());
        assert!(lock(&fixture.briefcase.temporary_url_requests).is_empty());
        assert!(lock(&fixture.briefcase.store_requests).is_empty());
        assert!(
            fixture
                .tts
                .iter()
                .all(|provider| lock(&provider.request_ids).is_empty())
        );
        assert!(lock(&fixture.idempotency.completions).is_empty());
        assert!(lock(&fixture.idempotency.releases).is_empty());
    }

    #[tokio::test]
    async fn bearer_and_obo_for_the_same_actor_share_idempotency_scope() {
        let fixture = fixture(
            IdempotencyDecision::InProgress {
                retry_after: Duration::from_secs(2),
            },
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );
        lock(&fixture.idempotency.decisions).push_back(Ok(IdempotencyDecision::InProgress {
            retry_after: Duration::from_secs(2),
        }));

        let bearer_result = fixture
            .service
            .synthesize(context(request_id(33), "shared-app-key"), tts_request())
            .await;
        let obo_result = fixture
            .service
            .synthesize(
                obo_context(request_id(34), "shared-app-key", "silicon-dm"),
                tts_request(),
            )
            .await;

        assert!(matches!(
            bearer_result,
            Err(WaveformError::RequestInProgress { .. })
        ));
        assert!(matches!(
            obo_result,
            Err(WaveformError::RequestInProgress { .. })
        ));
        let claims = lock(&fixture.idempotency.claims);
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0].scope, claims[1].scope);
        assert!(lock(&fixture.iam.delegated_request_ids).is_empty());
    }

    #[tokio::test]
    async fn completion_failure_releases_the_lease_without_masking_the_store_error() {
        let canonical_id = request_id(41);
        let fixture = fixture(
            IdempotencyDecision::Acquired {
                lease: lease(),
                request_id: canonical_id,
                operation_started_at: original_operation_start(),
            },
            [
                Ok(audio_artifact()),
                provider_failure(ProviderName::ElevenLabs),
                provider_failure(ProviderName::OpenAi),
            ],
            provider_failures_for_stt(),
        );
        *lock(&fixture.idempotency.completion_result) = Err(IdempotencyStoreError::Unavailable);

        let result = fixture
            .service
            .synthesize(context(request_id(42), "complete-failure"), tts_request())
            .await;

        assert_eq!(
            result,
            Err(WaveformError::DependencyUnavailable {
                dependency: crate::domain::error::Dependency::IdempotencyStore
            })
        );
        assert_eq!(lock(&fixture.idempotency.completions).len(), 1);
        assert_eq!(lock(&fixture.idempotency.releases).len(), 1);
    }

    fn fixture(
        decision: IdempotencyDecision,
        tts_results: [Result<AudioArtifact, ProviderError>; 3],
        stt_results: [Result<SttProviderResult, ProviderError>; 3],
    ) -> Fixture {
        let authorized = AuthorizedActor {
            actor: Actor::new(ActorKind::Carbon, actor_id(1)),
            organization_id: organization_id(),
            originating_application: None,
            expires_at: None,
        };
        let iam = Arc::new(FakeIam {
            authorized,
            authorize_count: Mutex::new(0),
            delegated_request_ids: Mutex::new(Vec::new()),
        });
        let idempotency = Arc::new(FakeIdempotency {
            decisions: Mutex::new(VecDeque::from([Ok(decision)])),
            claims: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
            completion_result: Mutex::new(Ok(())),
            releases: Mutex::new(Vec::new()),
        });
        let briefcase = Arc::new(FakeBriefcase {
            source: Ok(source_media()),
            stored: Ok(stored_audio()),
            access_result: Mutex::new(Ok(())),
            temporary_url_result: Mutex::new(Ok(fresh_temporary_url())),
            verified_accesses: Mutex::new(Vec::new()),
            temporary_url_requests: Mutex::new(Vec::new()),
            read_request_ids: Mutex::new(Vec::new()),
            store_requests: Mutex::new(Vec::new()),
        });
        let normalizer = Arc::new(FakeNormalizer {
            result: Ok(normalized_audio()),
        });
        let tts_names = [
            ProviderName::Gemini,
            ProviderName::ElevenLabs,
            ProviderName::OpenAi,
        ];
        let tts = tts_names
            .into_iter()
            .zip(tts_results)
            .map(|(name, result)| {
                Arc::new(FakeTtsProvider {
                    name,
                    result,
                    request_ids: Mutex::new(Vec::new()),
                })
            })
            .collect::<Vec<_>>();
        let stt_names = [
            ProviderName::Gemini,
            ProviderName::OpenAi,
            ProviderName::Deepgram,
        ];
        let stt = stt_names
            .into_iter()
            .zip(stt_results)
            .map(|(name, result)| {
                Arc::new(FakeSttProvider {
                    name,
                    result,
                    request_ids: Mutex::new(Vec::new()),
                })
            })
            .collect::<Vec<_>>();
        let tts_ports = tts
            .iter()
            .cloned()
            .map(|provider| provider as Arc<dyn TextToSpeechProvider>)
            .collect();
        let stt_ports = stt
            .iter()
            .cloned()
            .map(|provider| provider as Arc<dyn SpeechToTextProvider>)
            .collect();
        let service = WaveformService::new(
            iam.clone(),
            briefcase.clone(),
            idempotency.clone(),
            normalizer,
            Arc::new(FakeLeaseIds {
                lease_id: lease_id(),
            }),
            tts_ports,
            stt_ports,
            policy(),
        )
        .unwrap_or_else(|error| panic!("valid fixture: {error}"));

        Fixture {
            service,
            iam,
            idempotency,
            briefcase,
            tts,
            stt,
        }
    }

    fn provider_failures_for_tts() -> [Result<AudioArtifact, ProviderError>; 3] {
        [
            provider_failure(ProviderName::Gemini),
            provider_failure(ProviderName::ElevenLabs),
            provider_failure(ProviderName::OpenAi),
        ]
    }

    fn original_operation_start() -> time::OffsetDateTime {
        datetime!(2026-08-30 01:02:03 UTC)
    }

    fn provider_failures_for_stt() -> [Result<SttProviderResult, ProviderError>; 3] {
        [
            provider_failure(ProviderName::Gemini),
            provider_failure(ProviderName::OpenAi),
            provider_failure(ProviderName::Deepgram),
        ]
    }

    fn provider_failure<T>(provider: ProviderName) -> Result<T, ProviderError> {
        Err(ProviderError::new(
            provider,
            ProviderFailureKind::Unavailable,
        ))
    }

    fn context(request_id: RequestId, key: &str) -> SpeechRequestContext {
        SpeechRequestContext {
            request_id,
            organization_id: organization_id(),
            credentials: InboundCredentials::Bearer(
                AccessToken::new("opaque-token".to_owned())
                    .unwrap_or_else(|error| panic!("valid token: {error}")),
            ),
            idempotency_key: IdempotencyKey::from_str(key)
                .unwrap_or_else(|error| panic!("valid idempotency key: {error}")),
        }
    }

    fn obo_context(request_id: RequestId, key: &str, app_id: &str) -> SpeechRequestContext {
        SpeechRequestContext {
            request_id,
            organization_id: organization_id(),
            credentials: InboundCredentials::OnBehalfOf(OboCredentials {
                application_id: application_id(app_id),
                proof: secret_proof(),
            }),
            idempotency_key: IdempotencyKey::from_str(key)
                .unwrap_or_else(|error| panic!("valid idempotency key: {error}")),
        }
    }

    fn tts_request() -> TtsRequest {
        let text = SpeechText::new("hello world".to_owned())
            .unwrap_or_else(|error| panic!("valid text: {error}"));
        TtsRequest::new(text, None).unwrap_or_else(|error| panic!("valid request: {error}"))
    }

    fn stt_request() -> SttRequest {
        SttRequest::new(briefcase_file_url(), None)
            .unwrap_or_else(|error| panic!("valid request: {error}"))
    }

    fn replayed_tts_result(request_id: RequestId) -> CompletedTtsOperation {
        CompletedTtsOperation {
            request_id,
            permanent_url: briefcase_file_url(),
            provider: ProviderName::Gemini,
            duration: MediaDuration::from_millis(125),
        }
    }

    fn replayed_stt_result(request_id: RequestId) -> SttResult {
        SttResult {
            request_id,
            transcript: Transcript::new("authorized replay".to_owned())
                .unwrap_or_else(|error| panic!("valid transcript: {error}")),
            detected_language: None,
            provider: ProviderName::Gemini,
            duration: Some(MediaDuration::from_millis(125)),
        }
    }

    fn audio_artifact() -> AudioArtifact {
        AudioArtifact::new(
            ProviderAudioFormat::Mp3,
            Bytes::from_static(b"provider-audio"),
        )
        .unwrap_or_else(|error| panic!("valid artifact: {error}"))
    }

    fn normalized_audio() -> NormalizedAudio {
        NormalizedAudio::new(
            Bytes::from_static(b"normalized-mp3"),
            MediaDuration::from_millis(125),
        )
        .unwrap_or_else(|error| panic!("valid normalized audio: {error}"))
    }

    fn source_media() -> SourceMedia {
        SourceMedia::new(
            SourceMediaType::Mpeg,
            Bytes::from_static(b"source-media"),
            MediaSizeLimit::default(),
        )
        .unwrap_or_else(|error| panic!("valid source media: {error}"))
    }

    fn stored_audio() -> StoredAudio {
        StoredAudio {
            permanent_url: briefcase_file_url(),
            temporary_url: TemporaryMediaUrl::new(
                Url::parse("https://cdn.briefcase.test/audio.mp3?signature=opaque")
                    .unwrap_or_else(|error| panic!("valid URL: {error}")),
            )
            .unwrap_or_else(|error| panic!("valid temporary URL: {error}")),
        }
    }

    fn fresh_temporary_url() -> TemporaryMediaUrl {
        TemporaryMediaUrl::new(
            Url::parse("https://cdn.briefcase.test/audio.mp3?signature=fresh")
                .unwrap_or_else(|error| panic!("valid URL: {error}")),
        )
        .unwrap_or_else(|error| panic!("valid temporary URL: {error}"))
    }

    fn policy() -> ServicePolicy {
        ServicePolicy::new(
            Duration::from_secs(30),
            MediaSizeLimit::default(),
            BriefcaseOrigin::new(
                Url::parse("https://briefcase.test")
                    .unwrap_or_else(|error| panic!("valid URL: {error}")),
            )
            .unwrap_or_else(|error| panic!("valid origin: {error}")),
            MAX_TTS_TEXT_CHARACTERS,
            RequestDigestKey::new(b"application-service-test-digest-key")
                .unwrap_or_else(|error| panic!("valid digest key: {error}")),
        )
        .unwrap_or_else(|error| panic!("valid policy: {error}"))
    }

    fn briefcase_file_url() -> BriefcaseFileUrl {
        BriefcaseFileUrl::new(
            Url::parse("https://briefcase.test/files/source")
                .unwrap_or_else(|error| panic!("valid URL: {error}")),
        )
        .unwrap_or_else(|error| panic!("valid Briefcase URL: {error}"))
    }

    fn organization_id() -> OrganizationId {
        OrganizationId::from_str("tos").unwrap_or_else(|error| panic!("valid org: {error}"))
    }

    fn application_id(value: &str) -> ApplicationId {
        ApplicationId::from_str(value).unwrap_or_else(|error| panic!("valid app: {error}"))
    }

    fn actor_id(value: u128) -> ActorId {
        ActorId::new(Uuid::from_u128(value))
            .unwrap_or_else(|error| panic!("valid actor ID: {error}"))
    }

    fn request_id(value: u128) -> RequestId {
        RequestId::new(Uuid::from_u128(value))
            .unwrap_or_else(|error| panic!("valid request ID: {error}"))
    }

    fn lease_id() -> IdempotencyLeaseId {
        IdempotencyLeaseId::new(Uuid::from_u128(900))
            .unwrap_or_else(|error| panic!("valid lease ID: {error}"))
    }

    fn lease() -> IdempotencyLease {
        IdempotencyLease {
            id: lease_id(),
            expires_at: datetime!(2026-08-31 07:05:26 UTC),
        }
    }

    fn secret_proof() -> OboProof {
        OboProof::new("delegated-proof".to_owned())
            .unwrap_or_else(|error| panic!("valid proof: {error}"))
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
