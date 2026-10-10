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

use super::{
    ServicePolicy, ServiceResponse, SpeechRequestContext, StorageBoundary, WaveformService,
};
use crate::{
    application::ports::{
        AccountsError, AccountsPort, AudioNormalizationError, AudioNormalizer, BriefcaseError,
        BriefcaseFileAccessRequest, BriefcasePort, HeldAudio, HeldAudioError, HeldAudioStore,
        IdempotencyStore, IdempotencyStoreError, LeaseIdGenerator, ReadSourceMediaRequest,
        SpeechToTextProvider, StorageAuthorityError, StorageAuthorityPort,
        StoreGeneratedAudioRequest, TextToSpeechProvider,
    },
    domain::{
        auth::{
            AccessToken, AuthFailure, AuthorizationRequest, AuthorizedActor, BriefcaseProof,
            InboundCredentials, ProofToken,
        },
        error::{Dependency, WaveformError},
        idempotency::{
            CompletedSpeechOperation, CompletedTtsOperation, IdempotencyClaim,
            IdempotencyCompletion, IdempotencyDecision, IdempotencyKey, IdempotencyLease,
            IdempotencyLeaseId, IdempotencyRelease, RequestDigestKey, SpeechJobStart,
            SpeechOperation,
        },
        identity::{AccountRef, AccountUuid, Actor, ActorId, ActorKind, ApplicationId, RequestId},
        media::{
            AudioArtifact, AudioStorage, BriefcaseFileUrl, BriefcaseOrigin, HeldReason,
            MediaDuration, MediaSizeLimit, NormalizedAudio, ProviderAudioFormat, SourceMedia,
            SourceMediaType,
        },
        provider::{ProviderError, ProviderFailureKind, ProviderName},
        speech::{
            MAX_TTS_TEXT_CHARACTERS, SpeechText, SttProviderRequest, SttProviderResult, SttRequest,
            SttResult, Transcript, TtsProviderRequest, TtsRequest,
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

/// Bearer credentials act as `authorized`; proof credentials act for the same
/// account on behalf of `interface`.
struct FakeAccounts {
    authorized: AuthorizedActor,
    authorize_count: Mutex<usize>,
    rejection: Mutex<Option<AccountsError>>,
}

#[async_trait]
impl AccountsPort for FakeAccounts {
    async fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizedActor, AccountsError> {
        *lock(&self.authorize_count) += 1;
        if let Some(error) = *lock(&self.rejection) {
            return Err(error);
        }
        let mut authorized = self.authorized.clone();
        authorized.originating_application = match request.credentials {
            InboundCredentials::Bearer(_) => None,
            InboundCredentials::Proof(_) => Some(application_id("interface")),
        };
        Ok(authorized)
    }
}

/// Records whether a subject token was offered; answers with `result`.
struct FakeAuthority {
    result: Mutex<Result<(), StorageAuthorityError>>,
    calls: Mutex<Vec<bool>>,
    /// `renew_refused_proof`: its answer, and whether each call carried a subject token.
    renew_result: Mutex<Result<(), StorageAuthorityError>>,
    renewals: Mutex<Vec<bool>>,
}

#[async_trait]
impl StorageAuthorityPort for FakeAuthority {
    async fn briefcase_proof(
        &self,
        _actor: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        lock(&self.calls).push(subject_token.is_some());
        (*lock(&self.result))?;
        Ok(BriefcaseProof {
            token: ProofToken::new("sap_briefcase-proof".to_owned())
                .unwrap_or_else(|error| panic!("valid proof: {error}")),
            expires_at: datetime!(2099-01-01 00:00:00 UTC),
        })
    }

    async fn renew_refused_proof(
        &self,
        _actor: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        refused: &BriefcaseProof,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        assert_eq!(refused.token.expose_secret(), "sap_briefcase-proof");
        lock(&self.renewals).push(subject_token.is_some());
        (*lock(&self.renew_result))?;
        Ok(BriefcaseProof {
            token: ProofToken::new("sap_briefcase-renewed".to_owned())
                .unwrap_or_else(|error| panic!("valid proof: {error}")),
            expires_at: datetime!(2099-01-01 00:00:00 UTC),
        })
    }
}

struct FakeBriefcase {
    /// How many of the next calls Briefcase refuses with 401 (a proof that ended).
    refusals: Mutex<u32>,
    /// The proof token each call presented.
    tokens: Mutex<Vec<String>>,
    source: Result<SourceMedia, BriefcaseError>,
    stored: Mutex<Result<BriefcaseFileUrl, BriefcaseError>>,
    access_result: Mutex<Result<(), BriefcaseError>>,
    verified_accesses: Mutex<Vec<(RequestId, String)>>,
    read_request_ids: Mutex<Vec<RequestId>>,
    store_requests: Mutex<Vec<(RequestId, String)>>,
}

#[async_trait]
impl BriefcasePort for FakeBriefcase {
    async fn verify_file_read_access(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<(), BriefcaseError> {
        self.refuse(&request.proof)?;
        lock(&self.verified_accesses).push((request.request_id, request.file_url.to_string()));
        *lock(&self.access_result)
    }

    async fn read_source_media(
        &self,
        request: ReadSourceMediaRequest,
    ) -> Result<SourceMedia, BriefcaseError> {
        self.refuse(&request.proof)?;
        lock(&self.read_request_ids).push(request.request_id);
        self.source.clone()
    }

    async fn store_generated_audio(
        &self,
        request: StoreGeneratedAudioRequest,
    ) -> Result<BriefcaseFileUrl, BriefcaseError> {
        self.refuse(&request.proof)?;
        lock(&self.store_requests).push((request.request_id, request.filename.to_string()));
        lock(&self.stored).clone()
    }
}

impl FakeBriefcase {
    fn refuse(&self, proof: &BriefcaseProof) -> Result<(), BriefcaseError> {
        lock(&self.tokens).push(proof.token.expose_secret().to_owned());
        let mut refusals = lock(&self.refusals);
        if *refusals > 0 {
            *refusals -= 1;
            return Err(BriefcaseError::Unauthorized);
        }
        Ok(())
    }
}

struct FakeHeldAudio {
    holds: Mutex<Vec<(RequestId, ActorId, HeldReason)>>,
    hold_result: Mutex<Result<(), HeldAudioError>>,
    still_held: Mutex<bool>,
}

#[async_trait]
impl HeldAudioStore for FakeHeldAudio {
    async fn hold(&self, audio: HeldAudio) -> Result<time::OffsetDateTime, HeldAudioError> {
        (*lock(&self.hold_result))?;
        lock(&self.holds).push((audio.request_id, audio.owner, audio.reason));
        Ok(datetime!(2026-08-31 01:02:03 UTC))
    }

    async fn is_held(
        &self,
        _request_id: RequestId,
        _owner: ActorId,
    ) -> Result<bool, HeldAudioError> {
        Ok(*lock(&self.still_held))
    }
}

struct FakeIdempotency {
    decisions: Mutex<VecDeque<Result<IdempotencyDecision, IdempotencyStoreError>>>,
    claims: Mutex<Vec<IdempotencyClaim>>,
    job_starts: Mutex<Vec<Option<ApplicationId>>>,
    completions: Mutex<Vec<IdempotencyCompletion>>,
    completion_result: Mutex<Result<(), IdempotencyStoreError>>,
    job_start_result: Mutex<Result<(), IdempotencyStoreError>>,
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

    async fn start_job(&self, job: SpeechJobStart) -> Result<(), IdempotencyStoreError> {
        lock(&self.job_starts).push(job.issuing_app);
        *lock(&self.job_start_result)
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

struct FakeNormalizer {
    result: Result<NormalizedAudio, AudioNormalizationError>,
}

#[async_trait]
impl AudioNormalizer for FakeNormalizer {
    async fn source_duration(
        &self,
        _media: &SourceMedia,
        _request_id: RequestId,
    ) -> Result<MediaDuration, AudioNormalizationError> {
        Ok(MediaDuration::from_millis(125))
    }

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
    requests: Mutex<Vec<TtsProviderRequest>>,
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
        request: TtsProviderRequest,
        request_id: RequestId,
    ) -> Result<AudioArtifact, ProviderError> {
        lock(&self.requests).push(request);
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

struct Profiles {
    calls: Mutex<Vec<(Actor, Option<String>)>>,
}

#[async_trait]
impl crate::application::ports::VoiceProfileStore for Profiles {
    async fn resolve(
        &self,
        actor: &AuthorizedActor,
        requested: Option<&str>,
    ) -> Result<crate::domain::voice::VoiceProfile, WaveformError> {
        lock(&self.calls).push((actor.actor, requested.map(str::to_owned)));
        let profiles: Vec<crate::domain::voice::VoiceProfile> =
            serde_json::from_str(include_str!("../domain/voice_profiles.json"))
                .map_err(|_| WaveformError::Internal)?;
        profiles
            .into_iter()
            .find(|p| p.id == requested.unwrap_or("kore"))
            .ok_or(WaveformError::InvalidRequest)
    }
}

struct SavedOrder(Vec<ProviderName>);

#[async_trait]
impl crate::application::ports::ProviderOrderStore for SavedOrder {
    async fn provider_order(
        &self,
        _actor: &AuthorizedActor,
        operation: SpeechOperation,
    ) -> Result<Vec<ProviderName>, WaveformError> {
        assert_eq!(operation, SpeechOperation::Tts);
        Ok(self.0.clone())
    }
}

struct Fixture {
    service: WaveformService,
    accounts: Arc<FakeAccounts>,
    authority: Arc<FakeAuthority>,
    idempotency: Arc<FakeIdempotency>,
    briefcase: Arc<FakeBriefcase>,
    held: Arc<FakeHeldAudio>,
    tts: Vec<Arc<FakeTtsProvider>>,
    stt: Vec<Arc<FakeSttProvider>>,
}

#[allow(clippy::too_many_lines)]
fn fixture(
    decision: IdempotencyDecision,
    tts_results: [Result<AudioArtifact, ProviderError>; 3],
    stt_results: [Result<SttProviderResult, ProviderError>; 3],
) -> Fixture {
    let accounts = Arc::new(FakeAccounts {
        authorized: AuthorizedActor {
            actor: Actor::new(ActorKind::Carbon, actor_id(1)),
            account: AccountRef {
                uuid: AccountUuid::from_str("zQo").unwrap_or_else(|_| unreachable!()),
                id: "c:ada".to_owned(),
            },
            originating_application: None,
            expires_at: None,
            scopes: vec!["profile".to_owned()],
            token_family: Some("fid-zQo".to_owned()),
        },
        authorize_count: Mutex::new(0),
        rejection: Mutex::new(None),
    });
    let authority = Arc::new(FakeAuthority {
        result: Mutex::new(Ok(())),
        calls: Mutex::new(Vec::new()),
        renew_result: Mutex::new(Ok(())),
        renewals: Mutex::new(Vec::new()),
    });
    let idempotency = Arc::new(FakeIdempotency {
        decisions: Mutex::new(VecDeque::from([Ok(decision)])),
        claims: Mutex::new(Vec::new()),
        job_starts: Mutex::new(Vec::new()),
        completions: Mutex::new(Vec::new()),
        completion_result: Mutex::new(Ok(())),
        job_start_result: Mutex::new(Ok(())),
        releases: Mutex::new(Vec::new()),
    });
    let briefcase = Arc::new(FakeBriefcase {
        refusals: Mutex::new(0),
        tokens: Mutex::new(Vec::new()),
        source: Ok(source_media()),
        stored: Mutex::new(Ok(briefcase_file_url())),
        access_result: Mutex::new(Ok(())),
        verified_accesses: Mutex::new(Vec::new()),
        read_request_ids: Mutex::new(Vec::new()),
        store_requests: Mutex::new(Vec::new()),
    });
    let held = Arc::new(FakeHeldAudio {
        holds: Mutex::new(Vec::new()),
        hold_result: Mutex::new(Ok(())),
        still_held: Mutex::new(true),
    });
    let tts = [
        ProviderName::Gemini,
        ProviderName::ElevenLabs,
        ProviderName::OpenAi,
    ]
    .into_iter()
    .zip(tts_results)
    .map(|(name, result)| {
        Arc::new(FakeTtsProvider {
            requests: Mutex::new(Vec::new()),
            name,
            result,
            request_ids: Mutex::new(Vec::new()),
        })
    })
    .collect::<Vec<_>>();
    let stt = [
        ProviderName::Gemini,
        ProviderName::OpenAi,
        ProviderName::Deepgram,
    ]
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
    let service = WaveformService::new(
        accounts.clone(),
        StorageBoundary {
            authority: authority.clone(),
            briefcase: briefcase.clone(),
            held_audio: held.clone(),
        },
        idempotency.clone(),
        Arc::new(FakeNormalizer {
            result: Ok(normalized_audio()),
        }),
        Arc::new(FakeLeaseIds {
            lease_id: lease_id(),
        }),
        tts.iter()
            .cloned()
            .map(|provider| provider as Arc<dyn TextToSpeechProvider>)
            .collect(),
        stt.iter()
            .cloned()
            .map(|provider| provider as Arc<dyn SpeechToTextProvider>)
            .collect(),
        policy(),
    )
    .unwrap_or_else(|error| panic!("valid fixture: {error}"));
    Fixture {
        service,
        accounts,
        authority,
        idempotency,
        briefcase,
        held,
        tts,
        stt,
    }
}

fn acquired(request: u128) -> IdempotencyDecision {
    IdempotencyDecision::Acquired {
        lease: lease(),
        request_id: request_id(request),
        operation_started_at: original_operation_start(),
    }
}

fn provider_failures_for_tts() -> [Result<AudioArtifact, ProviderError>; 3] {
    [
        provider_failure(ProviderName::Gemini),
        provider_failure(ProviderName::ElevenLabs),
        provider_failure(ProviderName::OpenAi),
    ]
}

fn first_tts_succeeds() -> [Result<AudioArtifact, ProviderError>; 3] {
    [
        Ok(audio_artifact()),
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
        credentials: InboundCredentials::Bearer(
            AccessToken::new("eyJ.test.token".to_owned())
                .unwrap_or_else(|error| panic!("valid token: {error}")),
        ),
        idempotency_key: IdempotencyKey::from_str(key)
            .unwrap_or_else(|error| panic!("valid idempotency key: {error}")),
    }
}

fn proof_context(request_id: RequestId, key: &str) -> SpeechRequestContext {
    SpeechRequestContext {
        request_id,
        credentials: InboundCredentials::Proof(
            ProofToken::new("sap_delegated-proof".to_owned())
                .unwrap_or_else(|error| panic!("valid proof: {error}")),
        ),
        idempotency_key: IdempotencyKey::from_str(key)
            .unwrap_or_else(|error| panic!("valid idempotency key: {error}")),
    }
}

fn tts_request() -> TtsRequest {
    let text = SpeechText::new("hello world".to_owned())
        .unwrap_or_else(|error| panic!("valid text: {error}"));
    let mut request =
        TtsRequest::new(text, None).unwrap_or_else(|error| panic!("valid request: {error}"));
    // Fallback regression scenarios explicitly opt in.
    request.auto_fallback = true;
    request
}

fn stt_request() -> SttRequest {
    SttRequest::new(briefcase_file_url(), None)
        .unwrap_or_else(|error| panic!("valid request: {error}"))
}

fn replayed_tts_result(request_id: RequestId) -> CompletedTtsOperation {
    CompletedTtsOperation {
        voice_profile: None,
        request_id,
        storage: AudioStorage::Briefcase {
            permanent_url: briefcase_file_url(),
        },
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
        Url::parse("https://backend.waveform.test")
            .unwrap_or_else(|error| panic!("valid URL: {error}")),
    )
    .unwrap_or_else(|error| panic!("valid policy: {error}"))
}

fn briefcase_file_url() -> BriefcaseFileUrl {
    BriefcaseFileUrl::new(
        Url::parse("https://briefcase.test/zQo/apps/waveform/source.mp3")
            .unwrap_or_else(|error| panic!("valid URL: {error}")),
    )
    .unwrap_or_else(|error| panic!("valid Briefcase URL: {error}"))
}

fn application_id(value: &str) -> ApplicationId {
    ApplicationId::from_str(value).unwrap_or_else(|error| panic!("valid app: {error}"))
}

fn actor_id(value: u128) -> ActorId {
    ActorId::new(Uuid::from_u128(value)).unwrap_or_else(|error| panic!("valid actor ID: {error}"))
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[tokio::test]
async fn profile_follows_default_override_fallback_and_proof_actor()
-> Result<(), Box<dyn std::error::Error>> {
    for (selected, winner, delegated) in [
        (None, 0, false),
        (Some("puck"), 1, false),
        (Some("sulafat"), 2, true),
    ] {
        let mut outcomes = provider_failures_for_tts();
        outcomes[winner] = Ok(audio_artifact());
        let fixture = fixture(acquired(77), outcomes, provider_failures_for_stt());
        let store = Arc::new(Profiles {
            calls: Mutex::new(Vec::new()),
        });
        let service = fixture.service.with_voice_profiles(store.clone());
        let mut request = tts_request();
        request.voice_profile = selected.map(str::to_owned);
        let context = if delegated {
            proof_context(request_id(78), "voice-profile")
        } else {
            context(request_id(78), "voice-profile")
        };
        let result = service.synthesize(context, request).await?.result;
        let profiles: Vec<crate::domain::voice::VoiceProfile> =
            serde_json::from_str(include_str!("../domain/voice_profiles.json"))?;
        let expected = profiles
            .iter()
            .find(|p| p.id == selected.unwrap_or("kore"))
            .ok_or("profile")?;
        assert_eq!(result.voice_profile, Some(expected.into()));
        let calls = lock(&store.calls);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, fixture.accounts.authorized.actor);
        for (index, provider) in fixture.tts.iter().enumerate() {
            let requests = lock(&provider.requests);
            if index <= winner {
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].voice, expected.for_provider(provider.name));
            } else {
                assert!(requests.is_empty());
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn completed_profile_replay_ignores_new_defaults_and_invalid_profiles_never_call_providers()
-> Result<(), Box<dyn std::error::Error>> {
    let mut completed = replayed_tts_result(request_id(77));
    completed.voice_profile = Some(crate::domain::voice::VoiceProfileRef {
        id: "puck".into(),
        revision: 1,
    });
    let fixture = fixture(
        IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(completed.clone())),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    let store = Arc::new(Profiles {
        calls: Mutex::new(Vec::new()),
    });
    let service = fixture.service.with_voice_profiles(store.clone());
    let result = service
        .synthesize(context(request_id(78), "profile-replay"), tts_request())
        .await?
        .result;
    assert_eq!(result.voice_profile, completed.voice_profile);
    assert!(lock(&store.calls).is_empty());
    assert!(fixture.tts.iter().all(|p| lock(&p.requests).is_empty()));
    let fixture = self::fixture(
        acquired(77),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    let service = fixture.service.with_voice_profiles(store);
    let mut request = tts_request();
    request.voice_profile = Some("missing".into());
    assert_eq!(
        service
            .synthesize(context(request_id(78), "bad-profile"), request)
            .await,
        Err(WaveformError::InvalidRequest)
    );
    assert!(fixture.tts.iter().all(|p| lock(&p.requests).is_empty()));
    assert_eq!(lock(&fixture.idempotency.releases).len(), 1);
    Ok(())
}

#[tokio::test]
async fn request_provider_order_overrides_default_fallback_chain() {
    let fixture = fixture(
        acquired(77),
        [
            provider_failure(ProviderName::Gemini),
            provider_failure(ProviderName::ElevenLabs),
            Ok(audio_artifact()),
        ],
        provider_failures_for_stt(),
    );
    let request = tts_request().with_provider_order(vec![
        ProviderName::OpenAi,
        ProviderName::Gemini,
        ProviderName::ElevenLabs,
    ]);
    let response = fixture
        .service
        .synthesize(context(request_id(78), "order-override-key"), request)
        .await
        .unwrap_or_else(|error| panic!("request should complete: {error}"));
    assert_eq!(response.result.provider, ProviderName::OpenAi);
    assert!(lock(&fixture.tts[0].request_ids).is_empty());
    assert!(lock(&fixture.tts[1].request_ids).is_empty());
    assert_eq!(lock(&fixture.tts[2].request_ids).len(), 1);
}

#[tokio::test]
async fn saved_account_order_is_the_base_for_request_prefixes() {
    let fixture = fixture(
        acquired(77),
        [
            provider_failure(ProviderName::Gemini),
            Ok(audio_artifact()),
            provider_failure(ProviderName::OpenAi),
        ],
        provider_failures_for_stt(),
    );
    let service = fixture
        .service
        .with_provider_orders(Arc::new(SavedOrder(vec![
            ProviderName::ElevenLabs,
            ProviderName::OpenAi,
            ProviderName::Gemini,
        ])));
    let mut request = tts_request();
    request.auto_fallback = false;
    let response = service
        .synthesize(context(request_id(78), "saved-order-key"), request)
        .await
        .unwrap_or_else(|error| panic!("request should complete: {error}"));
    assert_eq!(response.result.provider, ProviderName::ElevenLabs);
    assert!(lock(&fixture.tts[0].request_ids).is_empty());
    let claims = lock(&fixture.idempotency.claims);
    assert_eq!(claims.len(), 1);
}

#[tokio::test]
async fn tts_default_failure_never_attempts_another_provider() {
    let fixture = fixture(
        acquired(77),
        [
            provider_failure(ProviderName::Gemini),
            Ok(audio_artifact()),
            Ok(audio_artifact()),
        ],
        provider_failures_for_stt(),
    );
    let request = TtsRequest::new(
        SpeechText::new("hello".to_owned()).unwrap_or_else(|_| unreachable!()),
        None,
    )
    .unwrap_or_else(|_| unreachable!());
    assert!(!request.auto_fallback);
    let result = fixture
        .service
        .synthesize(context(request_id(78), "no-fallback-key"), request)
        .await;
    assert!(
        matches!(result, Err(WaveformError::ProviderFailed { failure }) if failure.provider == ProviderName::Gemini)
    );
    assert_eq!(lock(&fixture.tts[0].request_ids).len(), 1);
    assert!(lock(&fixture.tts[1].request_ids).is_empty());
    assert!(lock(&fixture.briefcase.store_requests).is_empty());
    assert_eq!(
        lock(&fixture.idempotency.releases)[0].failure_code,
        Some(crate::domain::error::ErrorCode::ProviderFailed)
    );
}

#[tokio::test]
async fn tts_rejects_controls_that_would_be_lost_by_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture(
        acquired(77),
        [
            Ok(audio_artifact()),
            Ok(audio_artifact()),
            Ok(audio_artifact()),
        ],
        provider_failures_for_stt(),
    );
    let mut request = tts_request();
    request.provider_options =
        serde_json::from_value(serde_json::json!({"gemini":{"scene":"Quiet room"}}))?;
    assert!(matches!(
        fixture
            .service
            .synthesize(context(request_id(78), "controls-fallback"), request)
            .await,
        Err(WaveformError::InvalidProviderOptions(_))
    ));
    assert!(
        fixture
            .tts
            .iter()
            .all(|provider| lock(&provider.request_ids).is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn reclaimed_tts_uses_the_original_request_id_for_every_side_effect() {
    let candidate_id = request_id(100);
    let canonical_id = request_id(7);
    let fixture = fixture(
        acquired(7),
        [
            provider_failure(ProviderName::Gemini),
            Ok(audio_artifact()),
            provider_failure(ProviderName::OpenAi),
        ],
        provider_failures_for_stt(),
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
    let stores = lock(&fixture.briefcase.store_requests);
    assert!(matches!(
        stores.as_slice(),
        [(request_id, filename)]
            if *request_id == canonical_id
                && filename.starts_with("tts_20260830_010203_")
                && filename.contains(&canonical_id.to_string())
    ));
    assert!(lock(&fixture.idempotency.releases).is_empty());
    assert!(matches!(
        lock(&fixture.idempotency.completions).as_slice(),
        [completion]
            if matches!(
                &completion.response,
                CompletedSpeechOperation::Tts(result)
                    if result.request_id == canonical_id
                        && matches!(result.storage, AudioStorage::Briefcase { .. })
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
    assert_eq!(*lock(&fixture.accounts.authorize_count), 0);
    assert!(lock(&fixture.idempotency.claims).is_empty());
}

#[tokio::test]
async fn tts_replay_reauthorizes_the_file_and_returns_the_permanent_url() {
    let completed = replayed_tts_result(request_id(11));
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
    assert_eq!(
        result,
        Ok(ServiceResponse::replayed(completed.clone().into_result()))
    );
    assert_eq!(*lock(&fixture.accounts.authorize_count), 1);
    assert_eq!(
        &*lock(&fixture.briefcase.verified_accesses),
        &[(attempt_request_id, briefcase_file_url().to_string())]
    );
    // A direct caller may create Briefcase access with its own token.
    assert_eq!(&*lock(&fixture.authority.calls), &[true]);
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
    let fixture = fixture(
        IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(replayed_tts_result(
            request_id(12),
        ))),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    *lock(&fixture.briefcase.access_result) = Err(BriefcaseError::Forbidden);
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
    assert!(lock(&fixture.idempotency.completions).is_empty());
    assert!(lock(&fixture.idempotency.releases).is_empty());
}

#[tokio::test]
async fn held_tts_replay_requires_the_audio_to_still_be_held() {
    let mut completed = replayed_tts_result(request_id(13));
    completed.storage = AudioStorage::Held {
        reason: HeldReason::BriefcaseUnavailable,
        audio_url: Url::parse("https://backend.waveform.test/api/v1/audio/x")
            .unwrap_or_else(|_| unreachable!()),
        expires_at: datetime!(2026-08-31 01:02:03 UTC),
    };
    let fixture = fixture(
        IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(completed.clone())),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    assert_eq!(
        fixture
            .service
            .synthesize(context(request_id(97), "held-replay"), tts_request())
            .await,
        Ok(ServiceResponse::replayed(completed.into_result()))
    );
    assert!(lock(&fixture.authority.calls).is_empty());
    assert!(lock(&fixture.briefcase.verified_accesses).is_empty());
    let expired = self::fixture(
        IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(CompletedTtsOperation {
            storage: AudioStorage::Held {
                reason: HeldReason::BriefcaseUnavailable,
                audio_url: Url::parse("https://backend.waveform.test/api/v1/audio/x")
                    .unwrap_or_else(|_| unreachable!()),
                expires_at: datetime!(2026-08-31 01:02:03 UTC),
            },
            ..replayed_tts_result(request_id(13))
        })),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    *lock(&expired.held.still_held) = false;
    assert_eq!(
        expired
            .service
            .synthesize(context(request_id(96), "held-replay"), tts_request())
            .await,
        Err(WaveformError::SourceNotFound)
    );
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
}

#[tokio::test]
async fn exhausted_stt_chain_releases_the_owned_lease() {
    let canonical_id = request_id(21);
    let fixture = fixture(
        acquired(21),
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
    assert_eq!(*lock(&fixture.accounts.authorize_count), 1);
    assert!(lock(&fixture.authority.calls).is_empty());
    assert!(lock(&fixture.briefcase.store_requests).is_empty());
    assert!(
        fixture
            .tts
            .iter()
            .all(|provider| lock(&provider.request_ids).is_empty())
    );
}

#[tokio::test]
async fn bearer_and_proof_for_the_same_account_share_idempotency_scope() {
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
    let bearer = fixture
        .service
        .synthesize(context(request_id(33), "shared-app-key"), tts_request())
        .await;
    let delegated = fixture
        .service
        .synthesize(
            proof_context(request_id(34), "shared-app-key"),
            tts_request(),
        )
        .await;
    assert!(matches!(
        bearer,
        Err(WaveformError::RequestInProgress { .. })
    ));
    assert!(matches!(
        delegated,
        Err(WaveformError::RequestInProgress { .. })
    ));
    let claims = lock(&fixture.idempotency.claims);
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0].scope, claims[1].scope);
}

#[tokio::test]
async fn completion_failure_releases_the_lease_without_masking_the_store_error() {
    let fixture = fixture(
        acquired(41),
        first_tts_succeeds(),
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
            dependency: Dependency::IdempotencyStore
        })
    );
    assert_eq!(lock(&fixture.idempotency.completions).len(), 1);
    assert_eq!(lock(&fixture.idempotency.releases).len(), 1);
}

struct BlockingTts(tokio::sync::Notify);

#[async_trait]
impl TextToSpeechProvider for BlockingTts {
    fn name(&self) -> ProviderName {
        ProviderName::Gemini
    }
    async fn synthesize(
        &self,
        _request: TtsProviderRequest,
        _id: RequestId,
    ) -> Result<AudioArtifact, ProviderError> {
        self.0.notify_one();
        std::future::pending().await
    }
}

async fn wait_for_release(store: &FakeIdempotency) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while lock(&store.releases).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("interrupted lease must be released"));
}

#[tokio::test]
async fn cancelling_inflight_speech_releases_the_current_lease() {
    let mut fixture = fixture(
        acquired(90),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    let blocker = Arc::new(BlockingTts(tokio::sync::Notify::new()));
    fixture.service.tts_providers[0] = blocker.clone();
    let service = fixture.service.clone();
    let task = tokio::spawn(async move {
        service
            .synthesize(
                context(request_id(91), "cancel-current-lease"),
                tts_request(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), blocker.0.notified())
        .await
        .unwrap_or_else(|_| panic!("provider must start"));
    task.abort();
    assert!(task.await.is_err());
    wait_for_release(&fixture.idempotency).await;
    let releases = lock(&fixture.idempotency.releases);
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].lease_id, lease_id());
    assert!(releases[0].failure_code.is_none());
    assert!(lock(&fixture.briefcase.store_requests).is_empty());
}

#[tokio::test]
async fn history_write_failure_prevents_provider_work() {
    let fixture = fixture(
        acquired(92),
        first_tts_succeeds(),
        provider_failures_for_stt(),
    );
    *lock(&fixture.idempotency.job_start_result) = Err(IdempotencyStoreError::Unavailable);
    let result = fixture
        .service
        .synthesize(
            context(request_id(93), "history-write-failed"),
            tts_request(),
        )
        .await;
    assert_eq!(
        result,
        Err(WaveformError::DependencyUnavailable {
            dependency: Dependency::IdempotencyStore
        })
    );
    wait_for_release(&fixture.idempotency).await;
    assert!(fixture.tts.iter().all(|p| lock(&p.request_ids).is_empty()));
    assert!(lock(&fixture.briefcase.store_requests).is_empty());
}

#[tokio::test]
async fn direct_tts_is_stored_in_briefcase_with_a_proof_made_from_the_callers_token() {
    let fixture = fixture(
        acquired(50),
        first_tts_succeeds(),
        provider_failures_for_stt(),
    );
    let result = fixture
        .service
        .synthesize(
            context(request_id(51), "stored-in-briefcase"),
            tts_request(),
        )
        .await
        .unwrap_or_else(|error| panic!("request should complete: {error}"))
        .result;
    assert_eq!(
        result.storage,
        AudioStorage::Briefcase {
            permanent_url: briefcase_file_url()
        }
    );
    assert_eq!(&*lock(&fixture.authority.calls), &[true]);
    assert!(lock(&fixture.held.holds).is_empty());
    assert_eq!(&*lock(&fixture.idempotency.job_starts), &[None]);
}

#[tokio::test]
async fn delegated_tts_is_held_and_never_asks_for_briefcase_access() {
    let fixture = fixture(
        acquired(52),
        first_tts_succeeds(),
        provider_failures_for_stt(),
    );
    let result = fixture
        .service
        .synthesize(
            proof_context(request_id(53), "delegated-speech"),
            tts_request(),
        )
        .await
        .unwrap_or_else(|error| panic!("request should complete: {error}"))
        .result;
    let AudioStorage::Held {
        reason, audio_url, ..
    } = &result.storage
    else {
        panic!("delegated speech must be held, got {:?}", result.storage);
    };
    assert_eq!(*reason, HeldReason::DelegatedCall);
    assert_eq!(
        audio_url.as_str(),
        format!(
            "https://backend.waveform.test/api/v1/audio/{}",
            request_id(52)
        )
    );
    assert!(lock(&fixture.authority.calls).is_empty());
    assert!(lock(&fixture.briefcase.store_requests).is_empty());
    assert_eq!(
        &*lock(&fixture.held.holds),
        &[(request_id(52), actor_id(1), HeldReason::DelegatedCall)]
    );
    // The issuing app is recorded on the job.
    assert_eq!(
        &*lock(&fixture.idempotency.job_starts),
        &[Some(application_id("interface"))]
    );
}

#[tokio::test]
async fn briefcase_failures_still_complete_with_held_audio_and_an_exact_reason() {
    for (authority, upload, expected) in [
        (
            Err(StorageAuthorityError::NotAuthorized),
            Ok(()),
            HeldReason::StorageNotAuthorized,
        ),
        (
            Err(StorageAuthorityError::Unavailable),
            Ok(()),
            HeldReason::StorageNotAuthorized,
        ),
        (
            Ok(()),
            Err(BriefcaseError::Unauthorized),
            HeldReason::BriefcaseRefused,
        ),
        (
            Ok(()),
            Err(BriefcaseError::Forbidden),
            HeldReason::BriefcaseRefused,
        ),
        (
            Ok(()),
            Err(BriefcaseError::Unavailable),
            HeldReason::BriefcaseUnavailable,
        ),
        (
            Ok(()),
            Err(BriefcaseError::Timeout),
            HeldReason::BriefcaseTimeout,
        ),
        (
            Ok(()),
            Err(BriefcaseError::ContractUnavailable),
            HeldReason::BriefcaseContractUnavailable,
        ),
    ] {
        let fixture = fixture(
            acquired(60),
            first_tts_succeeds(),
            provider_failures_for_stt(),
        );
        *lock(&fixture.authority.result) = authority;
        if let Err(error) = upload {
            *lock(&fixture.briefcase.stored) = Err(error);
        }
        let result = fixture
            .service
            .synthesize(context(request_id(61), "briefcase-failure"), tts_request())
            .await
            .unwrap_or_else(|error| panic!("{expected:?}: request should complete: {error}"))
            .result;
        assert!(
            matches!(result.storage, AudioStorage::Held { reason, .. } if reason == expected),
            "{expected:?}"
        );
        assert!(matches!(
            lock(&fixture.idempotency.completions).as_slice(),
            [completion] if matches!(&completion.response, CompletedSpeechOperation::Tts(done) if matches!(done.storage, AudioStorage::Held { .. }))
        ));
    }
}

#[tokio::test]
async fn audio_store_failure_after_briefcase_failure_fails_the_request() {
    let fixture = fixture(
        acquired(62),
        first_tts_succeeds(),
        provider_failures_for_stt(),
    );
    *lock(&fixture.briefcase.stored) = Err(BriefcaseError::Unavailable);
    *lock(&fixture.held.hold_result) = Err(HeldAudioError);
    assert_eq!(
        fixture
            .service
            .synthesize(context(request_id(63), "nowhere-to-keep"), tts_request())
            .await,
        Err(WaveformError::DependencyUnavailable {
            dependency: Dependency::AudioStore
        })
    );
    assert_eq!(lock(&fixture.idempotency.releases).len(), 1);
}

#[tokio::test]
async fn delegated_stt_uses_only_stored_briefcase_access() {
    let fixture = fixture(
        acquired(70),
        provider_failures_for_tts(),
        [
            Ok(SttProviderResult {
                transcript: Transcript::new("hello".to_owned()).unwrap_or_else(|_| unreachable!()),
                detected_language: None,
                duration: None,
            }),
            provider_failure(ProviderName::OpenAi),
            provider_failure(ProviderName::Deepgram),
        ],
    );
    let result = fixture
        .service
        .transcribe(
            proof_context(request_id(71), "delegated-stt"),
            stt_request(),
        )
        .await
        .unwrap_or_else(|error| panic!("request should complete: {error}"));
    assert_eq!(result.result.transcript.as_str(), "hello");
    // No subject token is offered for a delegated call: only a stored proof can be used.
    assert_eq!(&*lock(&fixture.authority.calls), &[false]);

    let without_access = self::fixture(
        acquired(72),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    *lock(&without_access.authority.result) = Err(StorageAuthorityError::NotAuthorized);
    assert_eq!(
        without_access
            .service
            .transcribe(
                proof_context(request_id(73), "delegated-stt-2"),
                stt_request()
            )
            .await,
        Err(WaveformError::StorageAuthorizationRequired)
    );
    assert!(lock(&without_access.briefcase.read_request_ids).is_empty());
    assert!(
        without_access
            .stt
            .iter()
            .all(|p| lock(&p.request_ids).is_empty())
    );
}

#[tokio::test]
async fn authentication_failures_keep_their_exact_reason() {
    let fixture = fixture(
        acquired(80),
        first_tts_succeeds(),
        provider_failures_for_stt(),
    );
    *lock(&fixture.accounts.rejection) = Some(AccountsError::Rejected(AuthFailure::SignInEnded));
    assert_eq!(
        fixture
            .service
            .synthesize(context(request_id(81), "signed-out"), tts_request())
            .await,
        Err(WaveformError::AuthenticationFailed(
            AuthFailure::SignInEnded
        ))
    );
    *lock(&fixture.accounts.rejection) = Some(AccountsError::Unavailable);
    assert_eq!(
        fixture
            .service
            .transcribe(context(request_id(82), "accounts-down"), stt_request())
            .await,
        Err(WaveformError::DependencyUnavailable {
            dependency: Dependency::Accounts
        })
    );
    assert!(lock(&fixture.idempotency.claims).is_empty());
}

struct PersonalKeys;

#[async_trait]
impl crate::application::ports::ProviderKeyStore for PersonalKeys {
    async fn load(
        &self,
        actor: &AuthorizedActor,
    ) -> Result<
        crate::application::ports::ProviderKeys,
        crate::application::ports::ProviderKeyStoreError,
    > {
        assert_eq!(actor.actor.id, actor_id(1));
        Ok(std::collections::HashMap::from([(
            ProviderName::OpenAi,
            secrecy::SecretString::from("personal-key".to_owned()),
        )]))
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "exercises saved and request credentials across both speech operations and shared-adapter isolation"
)]
async fn personal_keys_reach_tts_and_stt_without_changing_shared_providers()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::infrastructure::providers::{
        OPENAI_STT_MODEL, OPENAI_TTS_MODEL, OpenAiConfig, OpenAiProvider, ProviderHttpConfig,
    };
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .and(wiremock::matchers::header_exists("authorization"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(include_bytes!("../infrastructure/test-fixture.mp3").to_vec()),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(wiremock::matchers::header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"text":"personal transcription","usage":{"seconds":1.0}}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    let adapter = Arc::new(OpenAiProvider::new(
        reqwest::Client::new(),
        secrecy::SecretString::from(String::new()),
        OpenAiConfig {
            tts_http: ProviderHttpConfig::new(server.uri().parse()?),
            stt_http: ProviderHttpConfig::new(server.uri().parse()?),
            tts_model: OPENAI_TTS_MODEL.to_owned(),
            stt_model: OPENAI_STT_MODEL.to_owned(),
            voice: "alloy".to_owned(),
        },
    ));
    for (operation, request_override) in
        [("tts", false), ("stt", false), ("tts", true), ("stt", true)]
    {
        let mut fixture = fixture(
            acquired(77),
            provider_failures_for_tts(),
            provider_failures_for_stt(),
        );
        fixture.service.tts_providers[2] = adapter.clone();
        fixture.service.stt_providers[1] = adapter.clone();
        let service = fixture.service.with_provider_keys(Arc::new(PersonalKeys));
        let mut tts = tts_request().with_provider_order(vec![ProviderName::OpenAi]);
        tts.auto_fallback = false;
        let mut stt = stt_request();
        if request_override {
            let keys = serde_json::from_value::<crate::domain::provider_keys::ProviderApiKeys>(
                serde_json::json!({"openai":"request-only-key"}),
            )?;
            tts.provider_keys = keys.clone();
            stt.provider_keys = keys;
        }
        if operation == "tts" {
            assert_eq!(
                service
                    .synthesize(context(request_id(78), "personal-tts"), tts)
                    .await?
                    .result
                    .provider,
                ProviderName::OpenAi
            );
        } else {
            assert_eq!(
                service
                    .transcribe(context(request_id(79), "personal-stt"), stt)
                    .await?
                    .result
                    .transcript
                    .as_str(),
                "personal transcription"
            );
        }
    }
    // The shared deployment adapter still has no key and makes no network request.
    assert!(
        adapter
            .synthesize(
                TtsProviderRequest::for_provider(&tts_request(), ProviderName::OpenAi),
                request_id(80)
            )
            .await
            .is_err()
    );
    let received = server
        .received_requests()
        .await
        .ok_or("requests unavailable")?;
    assert_eq!(received.len(), 4);
    for (index, request) in received.iter().enumerate() {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .ok_or("missing auth")?
                .to_str()?,
            if index < 2 {
                "Bearer personal-key"
            } else {
                "Bearer request-only-key"
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_refused_briefcase_proof_is_renewed_once_and_the_speech_is_stored() {
    // A User verification proof ends with the sign-in it was issued under (one
    // machine's `waveform logout`), and Briefcase refuses it before it expires.
    let fixture = fixture(
        acquired(90),
        first_tts_succeeds(),
        provider_failures_for_stt(),
    );
    *lock(&fixture.briefcase.refusals) = 1;
    let result = fixture
        .service
        .synthesize(context(request_id(91), "renewed-proof"), tts_request())
        .await
        .unwrap_or_else(|error| panic!("request should complete: {error}"))
        .result;
    assert_eq!(
        result.storage,
        AudioStorage::Briefcase {
            permanent_url: briefcase_file_url()
        }
    );
    assert_eq!(
        &*lock(&fixture.briefcase.tokens),
        &["sap_briefcase-proof", "sap_briefcase-renewed"]
    );
    // The direct caller's token may back a replacement proof.
    assert_eq!(&*lock(&fixture.authority.renewals), &[true]);
    assert!(lock(&fixture.held.holds).is_empty());
}

#[tokio::test]
async fn a_refused_proof_that_cannot_be_renewed_holds_the_speech_with_the_exact_reason() {
    for (renewal, expected) in [
        (
            Err(StorageAuthorityError::NotAuthorized),
            HeldReason::StorageNotAuthorized,
        ),
        (
            Err(StorageAuthorityError::Unavailable),
            HeldReason::BriefcaseRefused,
        ),
    ] {
        let fixture = fixture(
            acquired(92),
            first_tts_succeeds(),
            provider_failures_for_stt(),
        );
        *lock(&fixture.briefcase.refusals) = 1;
        *lock(&fixture.authority.renew_result) = renewal;
        let result = fixture
            .service
            .synthesize(context(request_id(93), "not-renewed"), tts_request())
            .await
            .unwrap_or_else(|error| panic!("{expected:?}: request should complete: {error}"))
            .result;
        assert!(
            matches!(result.storage, AudioStorage::Held { reason, .. } if reason == expected),
            "{expected:?}: {:?}",
            result.storage
        );
        assert_eq!(lock(&fixture.briefcase.tokens).len(), 1, "{expected:?}");
        assert_eq!(&*lock(&fixture.authority.renewals), &[true]);
    }
}

#[tokio::test]
async fn delegated_stt_renews_a_refused_proof_without_the_accounts_token() {
    let fixture = fixture(
        acquired(94),
        provider_failures_for_tts(),
        [
            Ok(SttProviderResult {
                transcript: Transcript::new("hello".to_owned()).unwrap_or_else(|_| unreachable!()),
                detected_language: None,
                duration: None,
            }),
            provider_failure(ProviderName::OpenAi),
            provider_failure(ProviderName::Deepgram),
        ],
    );
    *lock(&fixture.briefcase.refusals) = 1;
    let result = fixture
        .service
        .transcribe(
            proof_context(request_id(95), "delegated-renewal"),
            stt_request(),
        )
        .await
        .unwrap_or_else(|error| panic!("request should complete: {error}"));
    assert_eq!(result.result.transcript.as_str(), "hello");
    // An app's proof can never back a new Briefcase proof: only a refresh is possible.
    assert_eq!(&*lock(&fixture.authority.renewals), &[false]);

    let ended = self::fixture(
        acquired(96),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    *lock(&ended.briefcase.refusals) = 1;
    *lock(&ended.authority.renew_result) = Err(StorageAuthorityError::NotAuthorized);
    assert_eq!(
        ended
            .service
            .transcribe(
                proof_context(request_id(97), "delegated-ended"),
                stt_request()
            )
            .await,
        Err(WaveformError::StorageAuthorizationRequired)
    );
}

#[tokio::test]
async fn tts_replay_renews_a_refused_proof_before_checking_the_file() {
    let completed = replayed_tts_result(request_id(14));
    let fixture = fixture(
        IdempotencyDecision::Replay(CompletedSpeechOperation::Tts(completed.clone())),
        provider_failures_for_tts(),
        provider_failures_for_stt(),
    );
    *lock(&fixture.briefcase.refusals) = 1;
    let result = fixture
        .service
        .synthesize(context(request_id(15), "replay-renewal"), tts_request())
        .await;
    assert_eq!(
        result,
        Ok(ServiceResponse::replayed(completed.into_result()))
    );
    assert_eq!(&*lock(&fixture.authority.renewals), &[true]);
    assert_eq!(lock(&fixture.briefcase.verified_accesses).len(), 1);
}
