//! Deterministic speech fixtures used exclusively by Waveform test planes.
//!
//! A test plane must exercise the same application orchestration (authorization,
//! idempotency, Briefcase storage and history) without contacting a billable
//! speech provider.  These adapters keep the provider boundary intact while
//! returning the prescribed fixture values from `UNDERSTANDING.md`.
//!
//! The module intentionally does not inspect or accept production credentials;
//! runtime composition must select these adapters only after a test-plane root
//! key has been validated by the control plane.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use time::OffsetDateTime;
use url::Url;

use crate::{
    application::ports::{
        AudioNormalizationError, AudioNormalizer, BriefcaseError, BriefcaseFileAccessRequest,
        BriefcasePort, IamError, IamPort, ReadSourceMediaRequest, SpeechToTextProvider,
        StoreGeneratedAudioRequest, TextToSpeechProvider,
    },
    domain::{
        auth::{AuthorizationRequest, AuthorizedActor, DelegatedAuthorization, DelegationPurpose},
        identity::RequestId,
        identity::{Actor, ActorId, ActorKind, ApplicationId},
        media::{
            AudioArtifact, BriefcaseFileUrl, MediaDuration, NormalizedAudio, ProviderAudioFormat,
            SourceMedia, SourceMediaType, StoredAudio, TemporaryMediaUrl,
        },
        provider::{ProviderError, ProviderFailureKind, ProviderName},
        speech::{SttProviderRequest, SttProviderResult, Transcript, TtsProviderRequest},
    },
};

/// Exact TTS text returned by a deterministic test-plane request.
pub const TEST_TTS_TEXT: &str = "Hey, this is the test enviorment of silicon waveform, if you are listenting to this, TTS worked. We didn't actually run the TTS but in prod the TTS would work as intented. Let's go broo! To agents and humans.";

/// Exact STT transcript returned by a deterministic test-plane request.
pub const TEST_STT_TEXT: &str = "Hey, this is the test enviorment of silicon waveform, if you are reading this, STT worked. We didn't actually run the STT but in prod the STT would work as intented. Let's go mate! To agents and humans!";

/// Stable provider label used for successful fixture responses.
pub const TEST_FIXTURE_PROVIDER: ProviderName = ProviderName::Gemini;

/// Fixed decoded duration reported by the fixture audio.
///
/// This is the duration of the checked-in spoken fixture, measured from its
/// 16 kHz MP3 stream. Keeping the value stable means the testing plane does
/// not need to invoke a codec at request time.
pub const TEST_TTS_DURATION: MediaDuration = MediaDuration::from_millis(13_471);

/// The checked-in spoken fixture used for test-plane TTS uploads.
///
/// The fixture normalizer below deliberately does not invoke `FFmpeg`. The
/// bytes are a real MP3 recording of [`TEST_TTS_TEXT`], so consumers can
/// decode and play the same deterministic audio that a successful TTS call
/// would store in Briefcase.
fn fixture_mp3() -> Bytes {
    Bytes::from_static(include_bytes!("test-fixture.mp3"))
}

/// Deterministic TTS provider. Gemini succeeds; the other chain positions are
/// represented by bounded unavailable responses so the normal fallback loop is
/// still exercised when a caller deliberately chooses one of them.
#[derive(Clone, Copy, Debug)]
pub struct FixtureTtsProvider {
    provider: ProviderName,
}

impl FixtureTtsProvider {
    /// Creates a fixture adapter occupying one documented TTS chain position.
    #[must_use]
    pub const fn new(provider: ProviderName) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl TextToSpeechProvider for FixtureTtsProvider {
    fn name(&self) -> ProviderName {
        self.provider
    }

    async fn synthesize(
        &self,
        _request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, ProviderError> {
        if self.provider != TEST_FIXTURE_PROVIDER {
            return Err(ProviderError::new(
                self.provider,
                ProviderFailureKind::Unavailable,
            ));
        }
        AudioArtifact::new(ProviderAudioFormat::Mp3, fixture_mp3())
            .map_err(|_| ProviderError::new(self.provider, ProviderFailureKind::InvalidResponse))
    }
}

/// Deterministic STT provider. The first documented STT position returns the
/// prescribed transcript for every valid source media input.
#[derive(Clone, Copy, Debug)]
pub struct FixtureSttProvider {
    provider: ProviderName,
}

impl FixtureSttProvider {
    /// Creates a fixture adapter occupying one documented STT chain position.
    #[must_use]
    pub const fn new(provider: ProviderName) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl SpeechToTextProvider for FixtureSttProvider {
    fn name(&self) -> ProviderName {
        self.provider
    }

    async fn transcribe(
        &self,
        request: SttProviderRequest,
        _request_id: RequestId,
    ) -> Result<SttProviderResult, ProviderError> {
        if self.provider != TEST_FIXTURE_PROVIDER || request.media.is_empty() {
            return Err(ProviderError::new(
                self.provider,
                ProviderFailureKind::Unavailable,
            ));
        }
        Ok(SttProviderResult {
            transcript: Transcript::new(TEST_STT_TEXT.to_owned()).map_err(|_| {
                ProviderError::new(self.provider, ProviderFailureKind::InvalidResponse)
            })?,
            detected_language: None,
            duration: Some(TEST_TTS_DURATION),
        })
    }
}

/// Normalizer for deterministic fixture artifacts. It preserves bytes and
/// reports the measured fixture duration without invoking a local codec.
#[derive(Clone, Copy, Debug, Default)]
pub struct FixtureAudioNormalizer;

#[async_trait]
impl AudioNormalizer for FixtureAudioNormalizer {
    async fn source_duration(
        &self,
        _media: &SourceMedia,
        _request_id: RequestId,
    ) -> Result<MediaDuration, AudioNormalizationError> {
        Ok(TEST_TTS_DURATION)
    }

    async fn normalize_to_mp3(
        &self,
        artifact: AudioArtifact,
        _request_id: RequestId,
    ) -> Result<NormalizedAudio, AudioNormalizationError> {
        NormalizedAudio::new(artifact.into_bytes(), TEST_TTS_DURATION)
            .map_err(|_| AudioNormalizationError::InvalidAudio)
    }

    async fn check_ready(&self) -> Result<(), AudioNormalizationError> {
        Ok(())
    }
}

/// Provider vectors used to compose a deterministic test-plane service.
pub type FixtureProviderChains = (
    Vec<Arc<dyn TextToSpeechProvider>>,
    Vec<Arc<dyn SpeechToTextProvider>>,
);

/// Constructs complete fixture provider chains in the documented order.
#[must_use]
pub fn provider_chains() -> FixtureProviderChains {
    (
        vec![
            Arc::new(FixtureTtsProvider::new(ProviderName::Gemini)),
            Arc::new(FixtureTtsProvider::new(ProviderName::ElevenLabs)),
            Arc::new(FixtureTtsProvider::new(ProviderName::OpenAi)),
        ],
        vec![
            Arc::new(FixtureSttProvider::new(ProviderName::Gemini)),
            Arc::new(FixtureSttProvider::new(ProviderName::OpenAi)),
            Arc::new(FixtureSttProvider::new(ProviderName::Deepgram)),
        ],
    )
}

/// Seeds the prescribed audio in the shared database, replacing an older
/// packaged revision only when its bytes changed.
pub(crate) async fn seed_audio(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO waveform_speech_fixtures(name,mp3) VALUES('tts-v1',$1) ON CONFLICT(name) DO UPDATE SET mp3=EXCLUDED.mp3 WHERE waveform_speech_fixtures.mp3 IS DISTINCT FROM EXCLUDED.mp3")
        .bind(fixture_mp3().as_ref()).execute(pool).await?;
    Ok(())
}

struct DatabaseFixtureTtsProvider(sqlx::PgPool);

#[async_trait]
impl TextToSpeechProvider for DatabaseFixtureTtsProvider {
    fn name(&self) -> ProviderName {
        TEST_FIXTURE_PROVIDER
    }

    async fn synthesize(
        &self,
        _request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, ProviderError> {
        let bytes: Vec<u8> =
            sqlx::query_scalar("SELECT mp3 FROM waveform_speech_fixtures WHERE name='tts-v1'")
                .fetch_one(&self.0)
                .await
                .map_err(|_| {
                    ProviderError::new(TEST_FIXTURE_PROVIDER, ProviderFailureKind::Unavailable)
                })?;
        AudioArtifact::new(ProviderAudioFormat::Mp3, Bytes::from(bytes)).map_err(|_| {
            ProviderError::new(TEST_FIXTURE_PROVIDER, ProviderFailureKind::InvalidResponse)
        })
    }
}

/// Runtime fixtures load the prescribed audio from the authoritative database.
pub(crate) fn database_provider_chains(pool: sqlx::PgPool) -> FixtureProviderChains {
    let (mut tts, stt) = provider_chains();
    tts[0] = Arc::new(DatabaseFixtureTtsProvider(pool));
    (tts, stt)
}

/// IAM port for a test-plane composition. It keeps the application boundary
/// and actor/org metadata intact while avoiding a network call to IAM in local
/// deterministic runs. The surrounding request router must select this port
/// only after validating the test-plane root key.
#[derive(Clone, Debug)]
pub struct FixtureIam {
    actor: Actor,
    briefcase_application: ApplicationId,
}

impl FixtureIam {
    /// Creates a test IAM fixture with a stable actor identity.
    ///
    /// # Errors
    ///
    /// Returns an identity error when `actor_id` is nil.
    pub fn new(
        actor_kind: ActorKind,
        actor_id: uuid::Uuid,
        briefcase_application: ApplicationId,
    ) -> Result<Self, crate::domain::identity::IdentityError> {
        Ok(Self {
            actor: Actor::new(actor_kind, ActorId::new(actor_id)?),
            briefcase_application,
        })
    }
}

#[async_trait]
impl IamPort for FixtureIam {
    async fn authorize(&self, request: AuthorizationRequest) -> Result<AuthorizedActor, IamError> {
        // Header parsing already guarantees a non-empty, bounded credential;
        // preserving the actor metadata here lets history remain actor-scoped.
        Ok(AuthorizedActor {
            actor: self.actor,
            organization_id: request.organization_id,
            originating_application: None,
            expires_at: None,
        })
    }

    async fn delegate(
        &self,
        request: crate::domain::auth::DelegationRequest,
    ) -> Result<DelegatedAuthorization, IamError> {
        let proof = crate::domain::auth::OboProof::new("test-plane-fixture-proof".to_owned())
            .map_err(|_| IamError::InvalidResponse)?;
        Ok(DelegatedAuthorization {
            application_id: self.briefcase_application.clone(),
            proof,
            purpose: request.purpose,
            expires_at: fixture_expiry(),
        })
    }
}

/// Briefcase port for a test-plane composition. Generated bytes are recorded
/// in an in-memory ledger and every upload receives a URL containing its unique
/// generated filename, mirroring the production upload contract.
#[derive(Clone, Debug)]
pub struct FixtureBriefcase {
    permanent_origin: url::Origin,
    cdn_origin: url::Origin,
    application: ApplicationId,
    ledger: FixtureUploadLedger,
}

impl FixtureBriefcase {
    /// Creates an isolated fixture store rooted at the supplied HTTPS origins.
    ///
    /// # Errors
    ///
    /// Returns [`BriefcaseError::InvalidResponse`] when either origin is not
    /// HTTPS.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        application: ApplicationId,
        permanent_origin: Url,
        cdn_origin: Url,
        ledger: FixtureUploadLedger,
    ) -> Result<Self, BriefcaseError> {
        if permanent_origin.scheme() != "https" || cdn_origin.scheme() != "https" {
            return Err(BriefcaseError::InvalidResponse);
        }
        Ok(Self {
            permanent_origin: permanent_origin.origin(),
            cdn_origin: cdn_origin.origin(),
            application,
            ledger,
        })
    }

    fn validate_access(
        &self,
        application: &ApplicationId,
        purpose: DelegationPurpose,
        actual: DelegationPurpose,
        expires_at: OffsetDateTime,
    ) -> Result<(), BriefcaseError> {
        if application != &self.application
            || purpose != actual
            || expires_at <= OffsetDateTime::now_utc()
        {
            return Err(BriefcaseError::Unauthorized);
        }
        Ok(())
    }

    fn file_urls(
        &self,
        filename: &str,
    ) -> Result<(BriefcaseFileUrl, TemporaryMediaUrl), BriefcaseError> {
        let permanent = Url::parse(&format!(
            "{}/waveform/{filename}",
            self.permanent_origin.ascii_serialization()
        ))
        .map_err(|_| BriefcaseError::InvalidResponse)?;
        let temporary = Url::parse(&format!(
            "{}/waveform/{filename}?fixture=1",
            self.cdn_origin.ascii_serialization()
        ))
        .map_err(|_| BriefcaseError::InvalidResponse)?;
        if permanent.origin() != self.permanent_origin || temporary.origin() != self.cdn_origin {
            return Err(BriefcaseError::InvalidResponse);
        }
        Ok((
            BriefcaseFileUrl::new(permanent).map_err(|_| BriefcaseError::InvalidResponse)?,
            TemporaryMediaUrl::new(temporary).map_err(|_| BriefcaseError::InvalidResponse)?,
        ))
    }
}

#[async_trait]
impl BriefcasePort for FixtureBriefcase {
    async fn verify_file_read_access(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<(), BriefcaseError> {
        if request.file_url.as_url().origin() != self.permanent_origin {
            return Err(BriefcaseError::Forbidden);
        }
        Ok(())
    }

    async fn issue_temporary_url(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<TemporaryMediaUrl, BriefcaseError> {
        self.verify_file_read_access(request.clone()).await?;
        let filename = request
            .file_url
            .as_url()
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .ok_or(BriefcaseError::NotFound)?;
        Ok(self.file_urls(filename)?.1)
    }

    async fn read_source_media(
        &self,
        request: ReadSourceMediaRequest,
    ) -> Result<SourceMedia, BriefcaseError> {
        if request.source_url.as_url().origin() != self.permanent_origin {
            return Err(BriefcaseError::Forbidden);
        }
        SourceMedia::new(
            SourceMediaType::Mpeg,
            Bytes::from_static(b"test-plane-uploaded-audio"),
            request.size_limit,
        )
        .map_err(|_| BriefcaseError::InvalidResponse)
    }

    async fn store_generated_audio(
        &self,
        request: StoreGeneratedAudioRequest,
    ) -> Result<StoredAudio, BriefcaseError> {
        self.validate_access(
            &request.delegated_authorization.application_id,
            DelegationPurpose::StoreGeneratedAudio,
            request.delegated_authorization.purpose,
            request.delegated_authorization.expires_at,
        )?;
        let filename = request.filename.as_str();
        self.ledger
            .record(filename.to_owned(), request.audio.bytes().clone());
        let (permanent_url, temporary_url) = self.file_urls(filename)?;
        Ok(StoredAudio {
            permanent_url,
            temporary_url: Some(temporary_url),
        })
    }
}

/// Small observable record useful to test-plane Briefcase fixtures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureUpload {
    /// Caller-selected generated filename.
    pub filename: String,
    /// Bytes written to the test storage boundary.
    pub bytes: Bytes,
}

/// In-memory upload ledger for request-level test-plane integration tests.
#[derive(Clone, Debug, Default)]
pub struct FixtureUploadLedger {
    uploads: Arc<Mutex<Vec<FixtureUpload>>>,
}

impl FixtureUploadLedger {
    /// Records one generated TTS upload.
    pub fn record(&self, filename: impl Into<String>, bytes: Bytes) {
        if let Ok(mut uploads) = self.uploads.lock() {
            uploads.push(FixtureUpload {
                filename: filename.into(),
                bytes,
            });
        }
    }

    /// Returns a snapshot of uploads made by this test plane.
    #[must_use]
    pub fn uploads(&self) -> Vec<FixtureUpload> {
        self.uploads
            .lock()
            .map(|uploads| uploads.clone())
            .unwrap_or_default()
    }
}

/// Returns the fixture permanent URL used by an upload assertion.
///
/// Each request should append its own generated filename (which includes the
/// request ID), preserving the same per-request upload semantics as production.
#[must_use]
pub fn fixture_permanent_url(filename: &str) -> Url {
    Url::parse(&format!("https://briefcase.test/waveform/{filename}")).unwrap_or_else(|_| {
        Url::parse("https://briefcase.test/waveform/fixture.mp3").unwrap_or_else(|_| unreachable!())
    })
}

/// Returns the fixture temporary URL used by an upload assertion.
#[must_use]
pub fn fixture_temporary_url(filename: &str) -> Url {
    Url::parse(&format!(
        "https://cdn.briefcase.test/waveform/{filename}?fixture=1"
    ))
    .unwrap_or_else(|_| {
        Url::parse("https://cdn.briefcase.test/waveform/fixture.mp3?fixture=1")
            .unwrap_or_else(|_| unreachable!())
    })
}

/// Fixture timestamp helper used to keep test records deterministic.
#[must_use]
pub fn fixture_expiry() -> OffsetDateTime {
    OffsetDateTime::now_utc() + time::Duration::minutes(5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::{
            ports::{BriefcasePort, IdempotencyStore, IdempotencyStoreError, LeaseIdGenerator},
            service::{ServicePolicy, SpeechRequestContext, WaveformService},
        },
        domain::{
            auth::{
                AuthorizationRequest, DelegationPurpose, DelegationRequest, InboundCredentials,
            },
            idempotency::{
                IdempotencyClaim, IdempotencyCompletion, IdempotencyDecision, IdempotencyKey,
                IdempotencyLease, IdempotencyLeaseId, IdempotencyRelease, RequestDigestKey,
            },
            identity::{ActorKind, OrganizationId},
            media::{
                BriefcaseFileUrl, BriefcaseOrigin, GeneratedAudioFileName, MediaSizeLimit,
                SourceMediaType,
            },
            speech::{SpeechText, SttRequest, TtsRequest},
        },
    };
    use async_trait::async_trait;
    use std::str::FromStr as _;
    use std::time::Duration;
    use time::OffsetDateTime;
    use uuid::Uuid;

    struct MemoryIdempotency;

    #[async_trait]
    impl IdempotencyStore for MemoryIdempotency {
        async fn claim(
            &self,
            claim: IdempotencyClaim,
        ) -> Result<IdempotencyDecision, IdempotencyStoreError> {
            Ok(IdempotencyDecision::Acquired {
                lease: IdempotencyLease {
                    id: claim.lease_id,
                    expires_at: OffsetDateTime::now_utc()
                        + time::Duration::try_from(claim.lease_duration)
                            .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
                },
                request_id: claim.request_id,
                operation_started_at: OffsetDateTime::now_utc(),
            })
        }

        async fn complete(
            &self,
            _completion: IdempotencyCompletion,
        ) -> Result<(), IdempotencyStoreError> {
            Ok(())
        }

        async fn release(&self, _release: IdempotencyRelease) -> Result<(), IdempotencyStoreError> {
            Ok(())
        }

        async fn check_ready(&self) -> Result<(), IdempotencyStoreError> {
            Ok(())
        }
    }

    struct FixedLeaseId;

    impl LeaseIdGenerator for FixedLeaseId {
        fn new_idempotency_lease_id(&self) -> IdempotencyLeaseId {
            IdempotencyLeaseId::new(Uuid::from_u128(99)).unwrap_or_else(|_| unreachable!())
        }
    }

    fn request_id() -> RequestId {
        RequestId::new(Uuid::from_u128(1)).unwrap_or_else(|_| unreachable!())
    }

    #[tokio::test]
    async fn fixture_tts_is_exact_and_only_first_chain_position_succeeds() {
        let (tts, _) = provider_chains();
        let request = TtsRequest::new(
            SpeechText::new("hello".to_owned()).unwrap_or_else(|_| unreachable!()),
            None,
        )
        .unwrap_or_else(|_| unreachable!());
        let artifact = tts[0]
            .synthesize(
                TtsProviderRequest::for_provider(&request, ProviderName::Gemini),
                request_id(),
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(artifact.format(), ProviderAudioFormat::Mp3);
        assert!(artifact.bytes().len() > 10_000);
        assert_eq!(&artifact.bytes()[..3], b"ID3");
        assert!(
            artifact
                .bytes()
                .windows(2)
                .any(|window| window == [0xff, 0xfb])
        );
        assert_eq!(tts[1].name(), ProviderName::ElevenLabs);
        assert!(
            tts[1]
                .synthesize(
                    TtsProviderRequest::for_provider(&request, ProviderName::ElevenLabs),
                    request_id(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fixture_stt_returns_prescribed_text_for_any_nonempty_media() {
        let (_, stt) = provider_chains();
        let media = SourceMedia::new(
            SourceMediaType::Mpeg,
            Bytes::from_static(b"uploaded-audio"),
            MediaSizeLimit::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        let result = stt[0]
            .transcribe(
                SttProviderRequest {
                    media,
                    language: None,
                },
                request_id(),
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(result.transcript.as_str(), TEST_STT_TEXT);
        assert_eq!(result.duration, Some(TEST_TTS_DURATION));
    }

    #[test]
    fn fixture_urls_are_credential_free_and_upload_names_are_per_request() {
        let first = fixture_permanent_url("tts-one.mp3");
        let second = fixture_permanent_url("tts-two.mp3");
        assert_eq!(first.origin(), second.origin());
        assert_ne!(first.path(), second.path());
        assert!(first.username().is_empty());
        assert!(first.password().is_none());
        assert!(fixture_temporary_url("tts-one.mp3").query().is_some());
    }

    #[tokio::test]
    async fn fixture_request_boundary_records_each_tts_upload() {
        let app = ApplicationId::from_str("tos>briefcase").unwrap_or_else(|_| unreachable!());
        let iam = FixtureIam::new(ActorKind::Carbon, Uuid::from_u128(9), app.clone())
            .unwrap_or_else(|_| unreachable!());
        let ledger = FixtureUploadLedger::default();
        let permanent_origin =
            Url::parse("https://briefcase.test").unwrap_or_else(|_| unreachable!());
        let cdn_origin =
            Url::parse("https://cdn.briefcase.test").unwrap_or_else(|_| unreachable!());
        let briefcase = FixtureBriefcase::new(app, permanent_origin, cdn_origin, ledger.clone())
            .unwrap_or_else(|_| unreachable!());
        let org = OrganizationId::from_str("tos").unwrap_or_else(|_| unreachable!());
        let request_id = request_id();
        let authorization = iam
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::Bearer(
                    crate::domain::auth::AccessToken::new("test-token".to_owned())
                        .unwrap_or_else(|_| unreachable!()),
                ),
                organization_id: org,
                action: crate::domain::auth::WaveformAction::SynthesizeSpeech,
                request_id,
            })
            .await
            .unwrap_or_else(|_| unreachable!());
        let delegated = iam
            .delegate(DelegationRequest {
                authorization,
                purpose: DelegationPurpose::StoreGeneratedAudio,
                request_id,
                subject_token: None,
                upload: None,
                manifest: None,
            })
            .await
            .unwrap_or_else(|_| unreachable!());
        let audio = NormalizedAudio::new(Bytes::from_static(b"fixture-mp3"), TEST_TTS_DURATION)
            .unwrap_or_else(|_| unreachable!());
        for suffix in ["one", "two"] {
            let name = GeneratedAudioFileName::for_request(
                OffsetDateTime::now_utc(),
                RequestId::new(if suffix == "one" {
                    Uuid::from_u128(10)
                } else {
                    Uuid::from_u128(11)
                })
                .unwrap_or_else(|_| unreachable!()),
            );
            let _ = briefcase
                .store_generated_audio(StoreGeneratedAudioRequest {
                    authorization: AuthorizedActor {
                        actor: Actor::new(
                            ActorKind::Carbon,
                            ActorId::new(Uuid::from_u128(9)).unwrap_or_else(|_| unreachable!()),
                        ),
                        organization_id: OrganizationId::from_str("tos")
                            .unwrap_or_else(|_| unreachable!()),
                        originating_application: None,
                        expires_at: None,
                    },
                    delegated_authorization: delegated.clone(),
                    filename: name,
                    audio: audio.clone(),
                    idempotency_key: IdempotencyKey::from_str(&format!("fixture-{suffix}"))
                        .unwrap_or_else(|_| unreachable!()),
                    request_id,
                })
                .await
                .unwrap_or_else(|_| unreachable!());
        }
        let uploads = ledger.uploads();
        assert_eq!(uploads.len(), 2);
        assert_ne!(uploads[0].filename, uploads[1].filename);
        assert!(
            uploads
                .iter()
                .all(|upload| upload.bytes == Bytes::from_static(b"fixture-mp3"))
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn fixture_service_runs_tts_and_stt_request_workflows_without_network() {
        let application =
            ApplicationId::from_str("tos>briefcase").unwrap_or_else(|_| unreachable!());
        let iam = Arc::new(
            FixtureIam::new(ActorKind::Silicon, Uuid::from_u128(12), application.clone())
                .unwrap_or_else(|_| unreachable!()),
        );
        let ledger = FixtureUploadLedger::default();
        let briefcase_origin =
            Url::parse("https://briefcase.test").unwrap_or_else(|_| unreachable!());
        let cdn_origin =
            Url::parse("https://cdn.briefcase.test").unwrap_or_else(|_| unreachable!());
        let briefcase = Arc::new(
            FixtureBriefcase::new(
                application,
                briefcase_origin.clone(),
                cdn_origin,
                ledger.clone(),
            )
            .unwrap_or_else(|_| unreachable!()),
        );
        let (tts, stt) = provider_chains();
        let origin = BriefcaseOrigin::new(briefcase_origin).unwrap_or_else(|_| unreachable!());
        let policy = ServicePolicy::new(
            Duration::from_secs(30),
            MediaSizeLimit::default(),
            origin,
            4_096,
            RequestDigestKey::new(b"fixture-service-request-digest-key-012345")
                .unwrap_or_else(|_| unreachable!()),
        )
        .unwrap_or_else(|_| unreachable!());
        let service = WaveformService::new(
            iam.clone(),
            briefcase,
            Arc::new(MemoryIdempotency),
            Arc::new(FixtureAudioNormalizer),
            Arc::new(FixedLeaseId),
            tts,
            stt,
            policy,
        )
        .unwrap_or_else(|_| unreachable!());
        let organization = OrganizationId::from_str("tos").unwrap_or_else(|_| unreachable!());
        let authorization = iam
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::Bearer(
                    crate::domain::auth::AccessToken::new("test-token".to_owned())
                        .unwrap_or_else(|_| unreachable!()),
                ),
                organization_id: organization.clone(),
                action: crate::domain::auth::WaveformAction::SynthesizeSpeech,
                request_id: request_id(),
            })
            .await
            .unwrap_or_else(|_| unreachable!());
        let tts_context = SpeechRequestContext {
            request_id: RequestId::new(Uuid::from_u128(21)).unwrap_or_else(|_| unreachable!()),
            plane_id: Uuid::nil(),
            organization_id: organization.clone(),
            credentials: InboundCredentials::Bearer(
                crate::domain::auth::AccessToken::new("test-token".to_owned())
                    .unwrap_or_else(|_| unreachable!()),
            ),
            idempotency_key: IdempotencyKey::from_str("fixture-tts-request")
                .unwrap_or_else(|_| unreachable!()),
        };
        let tts_result = service
            .synthesize_pre_authorized(
                tts_context,
                TtsRequest::new(
                    SpeechText::new("arbitrary production text".to_owned())
                        .unwrap_or_else(|_| unreachable!()),
                    None,
                )
                .unwrap_or_else(|_| unreachable!()),
                authorization,
            )
            .await
            .unwrap_or_else(|_| unreachable!())
            .result;
        assert_eq!(tts_result.provider, TEST_FIXTURE_PROVIDER);
        assert_eq!(tts_result.duration, TEST_TTS_DURATION);
        assert_eq!(ledger.uploads().len(), 1);

        let stt_context = SpeechRequestContext {
            request_id: RequestId::new(Uuid::from_u128(22)).unwrap_or_else(|_| unreachable!()),
            plane_id: Uuid::nil(),
            organization_id: organization,
            credentials: InboundCredentials::Bearer(
                crate::domain::auth::AccessToken::new("test-token".to_owned())
                    .unwrap_or_else(|_| unreachable!()),
            ),
            idempotency_key: IdempotencyKey::from_str("fixture-stt-request")
                .unwrap_or_else(|_| unreachable!()),
        };
        let source_url = BriefcaseFileUrl::new(fixture_permanent_url("input.mp3"))
            .unwrap_or_else(|_| unreachable!());
        let stt_result = service
            .transcribe_pre_authorized(
                stt_context,
                SttRequest::new(source_url, None).unwrap_or_else(|_| unreachable!()),
                iam.authorize(AuthorizationRequest {
                    credentials: InboundCredentials::Bearer(
                        crate::domain::auth::AccessToken::new("test-token".to_owned())
                            .unwrap_or_else(|_| unreachable!()),
                    ),
                    organization_id: OrganizationId::from_str("tos")
                        .unwrap_or_else(|_| unreachable!()),
                    action: crate::domain::auth::WaveformAction::TranscribeSpeech,
                    request_id: RequestId::new(Uuid::from_u128(22))
                        .unwrap_or_else(|_| unreachable!()),
                })
                .await
                .unwrap_or_else(|_| unreachable!()),
            )
            .await
            .unwrap_or_else(|_| unreachable!())
            .result;
        assert_eq!(stt_result.provider, TEST_FIXTURE_PROVIDER);
        assert_eq!(stt_result.transcript.as_str(), TEST_STT_TEXT);
    }
}
