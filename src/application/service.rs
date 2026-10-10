//! Provider-independent synchronous TTS and STT orchestration.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use thiserror::Error;
use url::Url;

use super::ports::{
    AccountsError, AccountsPort, AudioNormalizationError, AudioNormalizer, BriefcaseError,
    BriefcaseFileAccessRequest, BriefcasePort, HeldAudio, HeldAudioStore, IdempotencyStore,
    IdempotencyStoreError, LeaseIdGenerator, ReadSourceMediaRequest, SpeechToTextProvider,
    StorageAuthorityError, StorageAuthorityPort, StoreGeneratedAudioRequest, TextToSpeechProvider,
};
use crate::domain::{
    auth::{
        AccessToken, AuthorizationRequest, AuthorizedActor, BriefcaseProof, InboundCredentials,
        WaveformAction,
    },
    error::{Dependency, WaveformError},
    idempotency::{
        CompletedSpeechOperation, CompletedTtsOperation, IdempotencyClaim, IdempotencyCompletion,
        IdempotencyDecision, IdempotencyKey, IdempotencyRelease, IdempotencyScope, RequestDigest,
        RequestDigestKey, SpeechOperation,
    },
    identity::{Actor, ApplicationId, RequestId},
    media::{
        AudioStorage, BriefcaseOrigin, GeneratedAudioFileName, HeldReason, MediaSizeLimit,
        NormalizedAudio,
    },
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
    /// Exactly one inbound credential.
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
    /// Account a fresh operation ran for.
    pub authorized_actor: Option<Actor>,
}

impl<T> ServiceResponse<T> {
    fn replayed(result: T) -> Self {
        Self {
            result,
            replayed: true,
            authorized_actor: None,
        }
    }

    fn fresh_authorized(result: T, actor: Actor) -> Self {
        Self {
            result,
            replayed: false,
            authorized_actor: Some(actor),
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
    public_base_url: Url,
}

impl ServicePolicy {
    /// Constructs policy with a finite, representable lease period.
    ///
    /// `public_base_url` is Waveform's own public origin; held audio is served at
    /// `{public_base_url}/api/v1/audio/{request_id}`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceConstructionError`] when the lease duration is zero or
    /// unrepresentable, the text limit is outside the domain ceiling, or the
    /// public URL is not an absolute http(s) origin.
    pub fn new(
        lease_duration: Duration,
        source_media_size_limit: MediaSizeLimit,
        briefcase_origin: BriefcaseOrigin,
        max_text_chars: usize,
        request_digest_key: RequestDigestKey,
        public_base_url: Url,
    ) -> Result<Self, ServiceConstructionError> {
        if lease_duration.is_zero() {
            return Err(ServiceConstructionError::InvalidDuration);
        }
        time::Duration::try_from(lease_duration)
            .map_err(|_| ServiceConstructionError::InvalidDuration)?;
        if !(1..=MAX_TTS_TEXT_CHARACTERS).contains(&max_text_chars) {
            return Err(ServiceConstructionError::InvalidTextLimit);
        }
        if !matches!(public_base_url.scheme(), "http" | "https")
            || public_base_url.host_str().is_none()
            || public_base_url.query().is_some()
        {
            return Err(ServiceConstructionError::InvalidPublicUrl);
        }
        Ok(Self {
            lease_duration,
            source_media_size_limit,
            briefcase_origin,
            max_text_chars,
            request_digest_key,
            public_base_url,
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

    /// Absolute URL serving audio Waveform holds for one request.
    #[must_use]
    pub fn held_audio_url(&self, request_id: RequestId) -> Url {
        let mut url = self.public_base_url.clone();
        let base = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base}/api/v1/audio/{request_id}"));
        url
    }
}

/// Where generated speech can go: Briefcase (through a proof), or Waveform's own store.
#[derive(Clone)]
pub struct StorageBoundary {
    /// Briefcase proofs Waveform holds for accounts.
    pub authority: Arc<dyn StorageAuthorityPort>,
    /// Briefcase itself.
    pub briefcase: Arc<dyn BriefcasePort>,
    /// Retention-limited store used when Briefcase cannot take the audio.
    pub held_audio: Arc<dyn HeldAudioStore>,
}

/// Complete application service with dependencies supplied through object-safe ports.
#[derive(Clone)]
pub struct WaveformService {
    accounts: Arc<dyn AccountsPort>,
    storage: StorageBoundary,
    idempotency: Arc<dyn IdempotencyStore>,
    audio_normalizer: Arc<dyn AudioNormalizer>,
    lease_id_generator: Arc<dyn LeaseIdGenerator>,
    tts_providers: Vec<Arc<dyn TextToSpeechProvider>>,
    stt_providers: Vec<Arc<dyn SpeechToTextProvider>>,
    policy: ServicePolicy,
    provider_keys: Option<Arc<dyn crate::application::ports::ProviderKeyStore>>,
    voice_profiles: Option<Arc<dyn crate::application::ports::VoiceProfileStore>>,
    provider_orders: Option<Arc<dyn crate::application::ports::ProviderOrderStore>>,
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
        accounts: Arc<dyn AccountsPort>,
        storage: StorageBoundary,
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
            accounts,
            storage,
            idempotency,
            audio_normalizer,
            lease_id_generator,
            tts_providers,
            stt_providers,
            policy,
            provider_keys: None,
            voice_profiles: None,
            provider_orders: None,
        })
    }

    /// Enables personal provider keys.
    #[must_use]
    pub fn with_provider_keys(
        mut self,
        store: Arc<dyn crate::application::ports::ProviderKeyStore>,
    ) -> Self {
        self.provider_keys = Some(store);
        self
    }

    /// Enables account voice profiles for bearer and proof callers.
    #[must_use]
    pub fn with_voice_profiles(
        mut self,
        store: Arc<dyn crate::application::ports::VoiceProfileStore>,
    ) -> Self {
        self.voice_profiles = Some(store);
        self
    }

    /// Enables saved per-account provider orders (and the database defaults).
    #[must_use]
    pub fn with_provider_orders(
        mut self,
        store: Arc<dyn crate::application::ports::ProviderOrderStore>,
    ) -> Self {
        self.provider_orders = Some(store);
        self
    }

    async fn for_actor(
        &self,
        actor: &AuthorizedActor,
        request_keys: &crate::domain::provider_keys::ProviderApiKeys,
    ) -> Result<Self, WaveformError> {
        let mut service = self.clone();
        let mut keys = if let Some(store) = &self.provider_keys {
            store
                .load(actor)
                .await
                .map_err(|_| WaveformError::DependencyUnavailable {
                    dependency: Dependency::ProviderKeys,
                })?
        } else {
            crate::application::ports::ProviderKeys::new()
        };
        for (provider, key) in request_keys.iter() {
            keys.insert(provider, key.clone());
        }
        for provider in &mut service.tts_providers {
            if let Some(key) = keys.get(&provider.name()) {
                *provider = provider
                    .with_api_key(key.clone())
                    .ok_or(WaveformError::Internal)?;
            }
        }
        for provider in &mut service.stt_providers {
            if let Some(key) = keys.get(&provider.name()) {
                *provider = provider
                    .with_api_key(key.clone())
                    .ok_or(WaveformError::Internal)?;
            }
        }
        Ok(service)
    }

    /// Completes the caller's optional provider prefix against the account's
    /// saved order (or the database defaults). An empty explicit list is invalid.
    async fn complete_provider_order(
        &self,
        actor: &AuthorizedActor,
        operation: SpeechOperation,
        requested: Option<Vec<ProviderName>>,
    ) -> Result<Vec<ProviderName>, WaveformError> {
        let chain = match operation {
            SpeechOperation::Tts => TTS_PROVIDER_CHAIN.as_slice(),
            SpeechOperation::Stt => STT_PROVIDER_CHAIN.as_slice(),
        };
        let base = match &self.provider_orders {
            Some(store) => store.provider_order(actor, operation).await?,
            None => chain.to_vec(),
        };
        let requested = match requested {
            Some(order) if order.is_empty() => return Err(WaveformError::InvalidRequest),
            Some(order) => order,
            None => Vec::new(),
        };
        crate::domain::provider::resolve_order(&requested, &base)
            .filter(|order| order.len() == chain.len())
            .ok_or(WaveformError::InvalidRequest)
    }

    /// Runs the synchronous text-to-speech workflow.
    ///
    /// # Errors
    ///
    /// Returns a stable [`WaveformError`] after authentication, idempotency,
    /// provider or normalization work fails terminally. Briefcase failures do
    /// not fail the request: the audio is held by Waveform instead.
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
        self.synthesize_authorized(context, request, authorization)
            .await
    }

    /// Runs TTS for an already authenticated account.
    ///
    /// # Errors
    ///
    /// Returns a stable [`WaveformError`] when idempotency, provider or
    /// normalization work fails.
    #[allow(clippy::too_many_lines)]
    pub async fn synthesize_authorized(
        &self,
        context: SpeechRequestContext,
        mut request: TtsRequest,
        authorization: AuthorizedActor,
    ) -> Result<ServiceResponse<TtsResult>, WaveformError> {
        if request.text.character_count() > self.policy.max_text_chars {
            return Err(WaveformError::PayloadTooLarge);
        }
        let requested = request.provider_order.take();
        let provider_order = self
            .complete_provider_order(&authorization, SpeechOperation::Tts, requested)
            .await?;
        request = request.with_provider_order(provider_order.clone());
        if request.auto_fallback && !request.provider_options.is_empty() {
            return Err(WaveformError::InvalidProviderOptions(
                "Provider-specific controls require auto_fallback=false.",
            ));
        }
        request
            .provider_options
            .validate_for(provider_order[0])
            .map_err(WaveformError::InvalidProviderOptions)?;
        if !request.provider_keys.supports_only(&TTS_PROVIDER_CHAIN) {
            return Err(WaveformError::InvalidRequest);
        }
        let service = self
            .for_actor(&authorization, &request.provider_keys)
            .await?;
        let scope = IdempotencyScope {
            plane_id: uuid::Uuid::nil(),
            actor: authorization.actor,
            operation: SpeechOperation::Tts,
            key: context.idempotency_key.clone(),
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
                    .replay_tts(
                        context.request_id,
                        &authorization,
                        context.credentials.subject_token(),
                        completed,
                    )
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

        let mut guard = OperationLeaseGuard::new(self.idempotency.clone(), scope.clone(), lease.id);
        if let Some(store) = &self.voice_profiles {
            match store
                .resolve(&authorization, request.voice_profile.as_deref())
                .await
            {
                Ok(profile) => request.resolved_voice = Some(profile),
                Err(error) => {
                    guard.fail(Some(error.code())).await;
                    return Err(error);
                }
            }
        } else if request.voice_profile.is_some() {
            guard
                .fail(Some(crate::domain::error::ErrorCode::InvalidRequest))
                .await;
            return Err(WaveformError::InvalidRequest);
        }
        self.idempotency
            .start_job(crate::domain::idempotency::SpeechJobStart {
                voice_profile: request.resolved_voice.as_ref().map(Into::into),
                scope: scope.clone(),
                issuing_app: authorization.originating_application.clone(),
                lease_id: lease.id,
                first_line: request
                    .text
                    .as_str()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(512)
                    .collect(),
            })
            .await
            .map_err(map_idempotency_error)?;
        let authorized_actor = authorization.actor;
        let result = service
            .execute_tts(
                canonical_request_id,
                operation_started_at,
                &authorization,
                context.credentials.subject_token(),
                request,
            )
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                guard.fail(Some(error.code())).await;
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
                guard.fail(Some(error.code())).await;
                return Err(error);
            }
        };
        if let Err(error) = self.idempotency.complete(completion).await {
            guard.fail(Some(map_idempotency_error(error).code())).await;
            return Err(map_idempotency_error(error));
        }

        guard.disarm();
        Ok(ServiceResponse::fresh_authorized(result, authorized_actor))
    }

    /// Runs the synchronous speech-to-text workflow.
    ///
    /// # Errors
    ///
    /// Returns a stable [`WaveformError`] after authentication, idempotency,
    /// Briefcase, or provider work fails terminally.
    pub async fn transcribe(
        &self,
        context: SpeechRequestContext,
        request: SttRequest,
    ) -> Result<ServiceResponse<SttResult>, WaveformError> {
        let authorization = self
            .authorize(&context, WaveformAction::TranscribeSpeech)
            .await?;
        self.transcribe_authorized(context, request, authorization)
            .await
    }

    /// Runs STT for an already authenticated account.
    ///
    /// # Errors
    ///
    /// Returns a stable [`WaveformError`] when idempotency, Briefcase, or
    /// provider work fails.
    #[allow(clippy::too_many_lines)]
    pub async fn transcribe_authorized(
        &self,
        context: SpeechRequestContext,
        mut request: SttRequest,
        authorization: AuthorizedActor,
    ) -> Result<ServiceResponse<SttResult>, WaveformError> {
        if !self.policy.briefcase_origin.contains(&request.source_url) {
            return Err(WaveformError::InvalidRequest);
        }
        if !request.provider_keys.supports_only(&STT_PROVIDER_CHAIN) {
            return Err(WaveformError::InvalidRequest);
        }
        let requested = request.provider_order.take();
        let provider_order = self
            .complete_provider_order(&authorization, SpeechOperation::Stt, requested)
            .await?;
        request = request.with_provider_order(provider_order);
        let service = self
            .for_actor(&authorization, &request.provider_keys)
            .await?;
        let scope = IdempotencyScope {
            plane_id: uuid::Uuid::nil(),
            actor: authorization.actor,
            operation: SpeechOperation::Stt,
            key: context.idempotency_key.clone(),
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
                // A cached transcript is content: re-check current read access first.
                let proof = self
                    .briefcase_authority(&authorization, context.credentials.subject_token())
                    .await?;
                self.storage
                    .briefcase
                    .verify_file_read_access(BriefcaseFileAccessRequest {
                        proof,
                        file_url: request.source_url.clone(),
                        request_id: context.request_id,
                    })
                    .await
                    .map_err(map_briefcase_error)?;
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

        let mut guard = OperationLeaseGuard::new(self.idempotency.clone(), scope.clone(), lease.id);
        self.idempotency
            .start_job(crate::domain::idempotency::SpeechJobStart {
                voice_profile: None,
                scope: scope.clone(),
                issuing_app: authorization.originating_application.clone(),
                lease_id: lease.id,
                first_line: String::new(),
            })
            .await
            .map_err(map_idempotency_error)?;
        let authorized_actor = authorization.actor;
        let result = service
            .execute_stt(
                canonical_request_id,
                &authorization,
                context.credentials.subject_token(),
                request,
            )
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                guard.fail(Some(error.code())).await;
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
                guard.fail(Some(error.code())).await;
                return Err(error);
            }
        };
        if let Err(error) = self.idempotency.complete(completion).await {
            guard.fail(Some(map_idempotency_error(error).code())).await;
            return Err(map_idempotency_error(error));
        }

        guard.disarm();
        Ok(ServiceResponse::fresh_authorized(result, authorized_actor))
    }

    async fn authorize(
        &self,
        context: &SpeechRequestContext,
        action: WaveformAction,
    ) -> Result<AuthorizedActor, WaveformError> {
        let actor = self
            .accounts
            .authorize(AuthorizationRequest {
                credentials: context.credentials.clone(),
                action,
                request_id: context.request_id,
            })
            .await
            .map_err(map_accounts_error)?;
        tracing::info!(
            request_id = %context.request_id,
            account = actor.account.uuid.as_str(),
            issuing_app = actor.originating_application.as_ref().map(ApplicationId::as_str),
            token_family = actor.token_family.as_deref(),
            "speech request authenticated"
        );
        Ok(actor)
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

    /// Briefcase access for reading on the account's behalf. A direct caller's
    /// own token can create it; a delegated call can only use a stored proof.
    async fn briefcase_authority(
        &self,
        authorization: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
    ) -> Result<BriefcaseProof, WaveformError> {
        let subject_token = if authorization.is_delegated() {
            None
        } else {
            subject_token
        };
        self.storage
            .authority
            .briefcase_proof(authorization, subject_token)
            .await
            .map_err(|error| match error {
                StorageAuthorityError::NotAuthorized | StorageAuthorityError::Refused => {
                    WaveformError::StorageAuthorizationRequired
                }
                StorageAuthorityError::Unavailable => WaveformError::DependencyUnavailable {
                    dependency: Dependency::Accounts,
                },
                StorageAuthorityError::Timeout => WaveformError::DependencyTimeout {
                    dependency: Dependency::Accounts,
                },
            })
    }

    /// Runs one Briefcase operation with `proof`. When Briefcase refuses the
    /// proof (401: it ended with the sign-in it stood on, or was revoked), asks
    /// for it to be renewed and runs the operation once more with the new one.
    async fn with_renewal<T, F, Fut>(
        &self,
        authorization: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        proof: BriefcaseProof,
        operation: F,
    ) -> Result<T, Renewal>
    where
        F: Fn(BriefcaseProof) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, BriefcaseError>> + Send,
    {
        match operation(proof.clone()).await {
            Err(BriefcaseError::Unauthorized) => {
                let subject_token = if authorization.is_delegated() {
                    None
                } else {
                    subject_token
                };
                let renewed = self
                    .storage
                    .authority
                    .renew_refused_proof(authorization, subject_token, &proof)
                    .await
                    .map_err(Renewal::Authority)?;
                tracing::info!(
                    "Briefcase refused a stored proof; retrying once with a renewed one"
                );
                operation(renewed).await.map_err(Renewal::Briefcase)
            }
            other => other.map_err(Renewal::Briefcase),
        }
    }

    async fn replay_tts(
        &self,
        attempt_request_id: RequestId,
        authorization: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        completed: CompletedTtsOperation,
    ) -> Result<TtsResult, WaveformError> {
        match &completed.storage {
            AudioStorage::Briefcase { permanent_url } => {
                let proof = self
                    .briefcase_authority(authorization, subject_token)
                    .await?;
                self.with_renewal(authorization, subject_token, proof, |proof| {
                    self.storage
                        .briefcase
                        .verify_file_read_access(BriefcaseFileAccessRequest {
                            proof,
                            file_url: permanent_url.clone(),
                            request_id: attempt_request_id,
                        })
                })
                .await
                .map_err(map_renewal_error)?;
            }
            AudioStorage::Held { .. } => {
                let held = self
                    .storage
                    .held_audio
                    .is_held(completed.request_id, authorization.actor.id)
                    .await
                    .map_err(|_| WaveformError::DependencyUnavailable {
                        dependency: Dependency::AudioStore,
                    })?;
                if !held {
                    return Err(WaveformError::SourceNotFound);
                }
            }
        }
        Ok(completed.into_result())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "ordered synthesis, normalization, and storage form one bounded pipeline"
    )]
    async fn execute_tts(
        &self,
        request_id: RequestId,
        operation_started_at: time::OffsetDateTime,
        authorization: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        request: TtsRequest,
    ) -> Result<TtsResult, WaveformError> {
        let requested_order = request.provider_order.as_deref().unwrap_or(&[]);
        let mut provider_order =
            crate::domain::provider::resolve_order(requested_order, &TTS_PROVIDER_CHAIN)
                .ok_or(WaveformError::InvalidRequest)?;
        if !request.auto_fallback {
            provider_order.truncate(1);
        }
        let mut successful_audio = None;
        for provider_name in provider_order {
            let Some(provider) = self
                .tts_providers
                .iter()
                .find(|provider| provider.name() == provider_name)
            else {
                return Err(WaveformError::Internal);
            };
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
                    if !request.auto_fallback {
                        return Err(WaveformError::ProviderFailed { failure: error });
                    }
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
                    if !request.auto_fallback {
                        return Err(WaveformError::ProviderFailed {
                            failure: crate::domain::provider::ProviderError::new(
                                provider_name,
                                crate::domain::provider::ProviderFailureKind::InvalidResponse,
                            ),
                        });
                    }
                }
                Err(AudioNormalizationError::Failed) => {
                    trace_provider_attempt(
                        request_id,
                        "tts",
                        provider_name,
                        "normalization_failed",
                        started,
                    );
                    if !request.auto_fallback {
                        return Err(WaveformError::DependencyUnavailable {
                            dependency: Dependency::AudioNormalizer,
                        });
                    }
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
        let storage = self
            .store_audio(authorization, subject_token, filename, audio, request_id)
            .await?;

        let mut result = TtsResult::new(request_id, storage, provider, duration);
        result.voice_profile = request.resolved_voice.as_ref().map(Into::into);
        Ok(result)
    }

    /// Stores the MP3 in the account's Briefcase files when Waveform can act
    /// there for the account; otherwise holds it and says why. Only a failure
    /// of Waveform's own audio store fails the request.
    async fn store_audio(
        &self,
        authorization: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        filename: GeneratedAudioFileName,
        audio: NormalizedAudio,
        request_id: RequestId,
    ) -> Result<AudioStorage, WaveformError> {
        if authorization.is_delegated() {
            return self
                .hold(
                    authorization,
                    filename,
                    audio,
                    request_id,
                    HeldReason::DelegatedCall,
                )
                .await;
        }
        let proof = match self
            .storage
            .authority
            .briefcase_proof(authorization, subject_token)
            .await
        {
            Ok(proof) => proof,
            Err(error) => {
                tracing::warn!(request_id = %request_id, reason = %error, "Briefcase access unavailable; holding audio");
                return self
                    .hold(
                        authorization,
                        filename,
                        audio,
                        request_id,
                        HeldReason::StorageNotAuthorized,
                    )
                    .await;
            }
        };
        match self
            .with_renewal(authorization, subject_token, proof, |proof| {
                self.storage
                    .briefcase
                    .store_generated_audio(StoreGeneratedAudioRequest {
                        proof,
                        filename: filename.clone(),
                        audio: audio.clone(),
                        request_id,
                    })
            })
            .await
        {
            Ok(permanent_url) => Ok(AudioStorage::Briefcase { permanent_url }),
            Err(Renewal::Authority(error)) => {
                let reason = match error {
                    StorageAuthorityError::NotAuthorized | StorageAuthorityError::Refused => {
                        HeldReason::StorageNotAuthorized
                    }
                    // The refusal stands when the proof could not be renewed now.
                    StorageAuthorityError::Unavailable | StorageAuthorityError::Timeout => {
                        HeldReason::BriefcaseRefused
                    }
                };
                tracing::warn!(request_id = %request_id, reason = reason.as_str(), "Briefcase refused the proof and it could not be renewed; holding audio");
                self.hold(authorization, filename, audio, request_id, reason)
                    .await
            }
            Err(Renewal::Briefcase(error)) => {
                let reason = match error {
                    BriefcaseError::Unauthorized | BriefcaseError::Forbidden => {
                        HeldReason::BriefcaseRefused
                    }
                    BriefcaseError::ContractUnavailable => HeldReason::BriefcaseContractUnavailable,
                    BriefcaseError::Timeout => HeldReason::BriefcaseTimeout,
                    BriefcaseError::NotFound
                    | BriefcaseError::MediaTooLarge
                    | BriefcaseError::UnsupportedMediaType
                    | BriefcaseError::Unavailable
                    | BriefcaseError::InvalidResponse => HeldReason::BriefcaseUnavailable,
                };
                tracing::warn!(request_id = %request_id, reason = reason.as_str(), "Briefcase upload failed; holding audio");
                self.hold(authorization, filename, audio, request_id, reason)
                    .await
            }
        }
    }

    async fn hold(
        &self,
        authorization: &AuthorizedActor,
        filename: GeneratedAudioFileName,
        audio: NormalizedAudio,
        request_id: RequestId,
        reason: HeldReason,
    ) -> Result<AudioStorage, WaveformError> {
        let expires_at = self
            .storage
            .held_audio
            .hold(HeldAudio {
                request_id,
                owner: authorization.actor.id,
                filename,
                audio,
                reason,
            })
            .await
            .map_err(|_| WaveformError::DependencyUnavailable {
                dependency: Dependency::AudioStore,
            })?;
        Ok(AudioStorage::Held {
            reason,
            audio_url: self.policy.held_audio_url(request_id),
            expires_at,
        })
    }

    async fn execute_stt(
        &self,
        request_id: RequestId,
        authorization: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        request: SttRequest,
    ) -> Result<SttResult, WaveformError> {
        let proof = self
            .briefcase_authority(authorization, subject_token)
            .await?;
        let media = self
            .with_renewal(authorization, subject_token, proof, |proof| {
                self.storage
                    .briefcase
                    .read_source_media(ReadSourceMediaRequest {
                        proof,
                        source_url: request.source_url.clone(),
                        size_limit: self.policy.source_media_size_limit,
                        request_id,
                    })
            })
            .await
            .map_err(map_renewal_error)?;

        let source_duration = self
            .audio_normalizer
            .source_duration(&media, request_id)
            .await
            .map_err(|error| match error {
                AudioNormalizationError::InvalidAudio => WaveformError::UnsupportedMediaType,
                other => map_audio_error(other),
            })?;

        let requested_order = request.provider_order.as_deref().unwrap_or(&[]);
        let provider_order =
            crate::domain::provider::resolve_order(requested_order, &STT_PROVIDER_CHAIN)
                .ok_or(WaveformError::InvalidRequest)?;
        for provider_name in provider_order {
            let Some(provider) = self
                .stt_providers
                .iter()
                .find(|provider| provider.name() == provider_name)
            else {
                return Err(WaveformError::Internal);
            };
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
                Ok(mut result) => {
                    trace_provider_attempt(request_id, "stt", provider_name, "success", started);
                    result.duration = Some(source_duration);
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
}

/// Releasing on cancellation is best effort; persisted expiry covers process death.
struct OperationLeaseGuard {
    store: Arc<dyn IdempotencyStore>,
    release: Option<IdempotencyRelease>,
}

impl OperationLeaseGuard {
    fn new(
        store: Arc<dyn IdempotencyStore>,
        scope: IdempotencyScope,
        lease_id: crate::domain::idempotency::IdempotencyLeaseId,
    ) -> Self {
        Self {
            store,
            release: Some(IdempotencyRelease {
                scope,
                lease_id,
                failure_code: None,
            }),
        }
    }

    fn disarm(&mut self) {
        self.release = None;
    }

    async fn fail(&mut self, code: Option<crate::domain::error::ErrorCode>) {
        if let Some(release) = &mut self.release {
            release.failure_code = code;
            if self.store.release(release.clone()).await.is_err() {
                tracing::warn!(
                    "operation lease release failed; expiry recovery will reconcile history"
                );
            }
        }
        self.disarm();
    }
}

impl Drop for OperationLeaseGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let store = self.store.clone();
            runtime.spawn(async move {
                if store.release(release).await.is_err() {
                    tracing::warn!("interrupted operation release failed; expiry recovery will reconcile history");
                }
            });
        }
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
    /// Waveform's public URL is not an absolute http(s) origin.
    #[error("invalid Waveform public URL")]
    InvalidPublicUrl,
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

const fn map_accounts_error(error: AccountsError) -> WaveformError {
    match error {
        AccountsError::Rejected(failure) => WaveformError::AuthenticationFailed(failure),
        AccountsError::Unavailable => WaveformError::DependencyUnavailable {
            dependency: Dependency::Accounts,
        },
        AccountsError::Timeout => WaveformError::DependencyTimeout {
            dependency: Dependency::Accounts,
        },
    }
}

/// A Briefcase operation that failed at Briefcase, or whose refused proof could
/// not be renewed.
#[derive(Clone, Copy, Debug)]
enum Renewal {
    Briefcase(BriefcaseError),
    Authority(StorageAuthorityError),
}

const fn map_renewal_error(error: Renewal) -> WaveformError {
    match error {
        Renewal::Briefcase(error) => map_briefcase_error(error),
        Renewal::Authority(
            StorageAuthorityError::NotAuthorized | StorageAuthorityError::Refused,
        ) => WaveformError::StorageAuthorizationRequired,
        Renewal::Authority(StorageAuthorityError::Unavailable) => {
            WaveformError::DependencyUnavailable {
                dependency: Dependency::Accounts,
            }
        }
        Renewal::Authority(StorageAuthorityError::Timeout) => WaveformError::DependencyTimeout {
            dependency: Dependency::Accounts,
        },
    }
}

const fn map_briefcase_error(error: BriefcaseError) -> WaveformError {
    match error {
        BriefcaseError::InvalidResponse => WaveformError::Internal,
        BriefcaseError::Forbidden => WaveformError::Forbidden,
        BriefcaseError::NotFound => WaveformError::SourceNotFound,
        BriefcaseError::MediaTooLarge => WaveformError::PayloadTooLarge,
        BriefcaseError::UnsupportedMediaType => WaveformError::UnsupportedMediaType,
        BriefcaseError::ContractUnavailable => WaveformError::DependencyContractUnavailable {
            dependency: Dependency::Briefcase,
        },
        BriefcaseError::Unauthorized => WaveformError::StorageAuthorizationRequired,
        BriefcaseError::Unavailable => WaveformError::DependencyUnavailable {
            dependency: Dependency::Briefcase,
        },
        BriefcaseError::Timeout => WaveformError::DependencyTimeout {
            dependency: Dependency::Briefcase,
        },
    }
}

const fn map_idempotency_error(error: IdempotencyStoreError) -> WaveformError {
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

const fn map_audio_error(error: AudioNormalizationError) -> WaveformError {
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
#[path = "service_tests.rs"]
mod tests;
