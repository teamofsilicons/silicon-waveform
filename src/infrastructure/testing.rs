//! Deterministic speech fixtures for development and CI.
//!
//! With `WAVEFORM_SPEECH_FIXTURES=true` (refused in production) every paid
//! provider is replaced by these adapters, so the whole orchestration
//! (authentication, idempotency, Briefcase storage, history) runs end to end
//! without provider keys. They return the prerecorded clips and transcript
//! whose wording `UNDERSTANDING.md` prescribes.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;

use crate::{
    application::ports::{
        AudioNormalizationError, AudioNormalizer, SpeechToTextProvider, TextToSpeechProvider,
    },
    domain::{
        identity::RequestId,
        media::{AudioArtifact, MediaDuration, NormalizedAudio, ProviderAudioFormat, SourceMedia},
        provider::{ProviderError, ProviderFailureKind, ProviderName},
        speech::{SttProviderRequest, SttProviderResult, Transcript, TtsProviderRequest},
        voice::ProviderVoice,
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
/// 44.1 kHz MP3 stream. Keeping the value stable means the testing plane does
/// not need to invoke a codec at request time.
pub const TEST_TTS_DURATION: MediaDuration = MediaDuration::from_millis(15_230);

/// The checked-in spoken fixture used for test-plane TTS uploads.
///
/// The fixture normalizer below deliberately does not invoke `FFmpeg`. The
/// bytes are a real MP3 recording of [`TEST_TTS_TEXT`], so consumers can
/// decode and play the same deterministic audio that a successful TTS call
/// would store in Briefcase.
const TEST_TTS_CLIPS: &[(&str, &[u8])] = &[
    ("Zephyr", include_bytes!("test-audio/zephyr.mp3")),
    ("Puck", include_bytes!("test-audio/puck.mp3")),
    ("Charon", include_bytes!("test-audio/charon.mp3")),
    ("Kore", include_bytes!("test-audio/kore.mp3")),
    ("Fenrir", include_bytes!("test-audio/fenrir.mp3")),
    ("Leda", include_bytes!("test-audio/leda.mp3")),
    ("Orus", include_bytes!("test-audio/orus.mp3")),
    ("Aoede", include_bytes!("test-audio/aoede.mp3")),
    ("Callirrhoe", include_bytes!("test-audio/callirrhoe.mp3")),
    ("Autonoe", include_bytes!("test-audio/autonoe.mp3")),
    ("Enceladus", include_bytes!("test-audio/enceladus.mp3")),
    ("Iapetus", include_bytes!("test-audio/iapetus.mp3")),
    ("Umbriel", include_bytes!("test-audio/umbriel.mp3")),
    ("Algieba", include_bytes!("test-audio/algieba.mp3")),
    ("Despina", include_bytes!("test-audio/despina.mp3")),
    ("Erinome", include_bytes!("test-audio/erinome.mp3")),
    ("Algenib", include_bytes!("test-audio/algenib.mp3")),
    ("Rasalgethi", include_bytes!("test-audio/rasalgethi.mp3")),
    ("Laomedeia", include_bytes!("test-audio/laomedeia.mp3")),
    ("Achernar", include_bytes!("test-audio/achernar.mp3")),
    ("Alnilam", include_bytes!("test-audio/alnilam.mp3")),
    ("Schedar", include_bytes!("test-audio/schedar.mp3")),
    ("Gacrux", include_bytes!("test-audio/gacrux.mp3")),
    ("Pulcherrima", include_bytes!("test-audio/pulcherrima.mp3")),
    ("Achird", include_bytes!("test-audio/achird.mp3")),
    (
        "Zubenelgenubi",
        include_bytes!("test-audio/zubenelgenubi.mp3"),
    ),
    (
        "Vindemiatrix",
        include_bytes!("test-audio/vindemiatrix.mp3"),
    ),
    ("Sadachbia", include_bytes!("test-audio/sadachbia.mp3")),
    ("Sadaltager", include_bytes!("test-audio/sadaltager.mp3")),
    ("Sulafat", include_bytes!("test-audio/sulafat.mp3")),
];

fn fixture_mp3() -> Bytes {
    Bytes::from_static(include_bytes!("test-audio/kore.mp3"))
}

fn fixture_voice(request: &TtsProviderRequest) -> Result<&str, ProviderError> {
    if let Some(voice) = &request.fixture_voice {
        return TEST_TTS_CLIPS
            .iter()
            .any(|(name, _)| name == voice)
            .then_some(voice.as_str())
            .ok_or_else(|| {
                ProviderError::new(TEST_FIXTURE_PROVIDER, ProviderFailureKind::InvalidResponse)
            });
    }
    match &request.voice {
        None => Ok("Kore"),
        Some(ProviderVoice::Gemini(voice))
            if TEST_TTS_CLIPS.iter().any(|(name, _)| name == voice) =>
        {
            Ok(voice)
        }
        _ => Err(ProviderError::new(
            TEST_FIXTURE_PROVIDER,
            ProviderFailureKind::InvalidResponse,
        )),
    }
}

/// Deterministic TTS provider returning prerecorded audio for every selection.
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
        request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, ProviderError> {
        let voice = fixture_voice(&request)?;
        let (_, bytes) = TEST_TTS_CLIPS
            .iter()
            .find(|(name, _)| *name == voice)
            .ok_or_else(|| {
                ProviderError::new(self.provider, ProviderFailureKind::InvalidResponse)
            })?;
        AudioArtifact::new(ProviderAudioFormat::Mp3, Bytes::from_static(bytes))
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
        let duration = super::audio::mp3_duration_ms(artifact.bytes())
            .map_err(|_| AudioNormalizationError::InvalidAudio)?;
        NormalizedAudio::new(artifact.into_bytes(), MediaDuration::from_millis(duration))
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
    let mut transaction = pool.begin().await?;
    for (voice, bytes) in TEST_TTS_CLIPS {
        sqlx::query("INSERT INTO waveform_speech_fixtures(name,mp3) VALUES($1,$2) ON CONFLICT(name) DO UPDATE SET mp3=EXCLUDED.mp3 WHERE waveform_speech_fixtures.mp3 IS DISTINCT FROM EXCLUDED.mp3")
            .bind(format!("tts-v2:{voice}"))
            .bind(*bytes).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(())
}

struct DatabaseFixtureTtsProvider(sqlx::PgPool, ProviderName);

#[async_trait]
impl TextToSpeechProvider for DatabaseFixtureTtsProvider {
    fn name(&self) -> ProviderName {
        self.1
    }

    async fn synthesize(
        &self,
        request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, ProviderError> {
        let voice = fixture_voice(&request)?;
        let bytes: Vec<u8> =
            sqlx::query_scalar("SELECT mp3 FROM waveform_speech_fixtures WHERE name=$1")
                .bind(format!("tts-v2:{voice}"))
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
pub(crate) fn database_provider_chains(pool: &sqlx::PgPool) -> FixtureProviderChains {
    let (mut tts, stt) = provider_chains();
    for provider in &mut tts {
        *provider = Arc::new(DatabaseFixtureTtsProvider(pool.clone(), provider.name()));
    }
    (tts, stt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        media::{MediaSizeLimit, SourceMediaType},
        speech::{SpeechText, TtsRequest},
    };
    use sha2::Digest as _;
    use uuid::Uuid;

    fn request_id() -> RequestId {
        RequestId::new(Uuid::from_u128(1)).unwrap_or_else(|_| unreachable!())
    }

    #[tokio::test]
    async fn fixture_tts_is_exact_and_every_tts_provider_works_without_fallback() {
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
                .is_ok()
        );
    }

    #[tokio::test]
    async fn demo_catalog_matches_every_voice_and_validates_all_mp3s()
    -> Result<(), Box<dyn std::error::Error>> {
        let profiles: Vec<crate::domain::voice::VoiceProfile> =
            serde_json::from_str(include_str!("../domain/voice_profiles.json"))?;
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("test-audio/manifest.json"))?;
        assert_eq!(manifest["text"], TEST_TTS_TEXT);
        assert_eq!(TEST_TTS_CLIPS.len(), profiles.len());
        assert_eq!(
            manifest["clips"].as_object().ok_or("clips")?.len(),
            profiles.len()
        );
        let provider = FixtureTtsProvider::new(ProviderName::Gemini);
        let mut hashes = std::collections::HashSet::new();
        for profile in profiles {
            let mut request =
                TtsRequest::new(SpeechText::new("ignored test input".to_owned())?, None)?;
            request.resolved_voice = Some(profile.clone());
            let artifact = provider
                .synthesize(
                    TtsProviderRequest::for_provider(&request, ProviderName::Gemini),
                    request_id(),
                )
                .await?;
            let entry = &manifest["clips"][&profile.id];
            let hash = format!("{:x}", sha2::Sha256::digest(artifact.bytes()));
            assert_eq!(entry["sha256"], hash);
            assert!(hashes.insert(hash));
            assert_eq!(entry["voice"], profile.gemini_voice);
            let duration = super::super::audio::mp3_duration_ms(artifact.bytes())?;
            assert!(duration > 8000 && duration < 45000);
            assert_eq!(entry["duration_ms"], duration);
            let normalized = FixtureAudioNormalizer
                .normalize_to_mp3(artifact, request_id())
                .await?;
            assert_eq!(normalized.duration(), MediaDuration::from_millis(duration));
        }
        assert_eq!(
            super::super::audio::mp3_duration_ms(&fixture_mp3())?,
            TEST_TTS_DURATION.as_millis()
        );
        let mut request = TtsProviderRequest::for_provider(
            &TtsRequest::new(SpeechText::new("x".to_owned())?, None)?,
            ProviderName::Gemini,
        );
        request.voice = Some(ProviderVoice::Gemini("UnknownVoice".to_owned()));
        assert!(provider.synthesize(request, request_id()).await.is_err());
        Ok(())
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
}
