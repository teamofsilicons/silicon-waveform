use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde::Serialize;

use crate::{
    application::ports::TextToSpeechProvider,
    domain::{
        identity::RequestId,
        media::AudioArtifact,
        provider::{ProviderError as DomainProviderError, ProviderName},
        speech::TtsProviderRequest,
        tts_options::ElevenLabsTtsOptions,
        voice::{ElevenLabsVoice, ElevenLabsVoiceSettings, ProviderVoice},
    },
};

use super::{
    ProviderError, ProviderHttpConfig, ProviderRuntime, map_provider_error, mp3_response_artifact,
};

/// Exact `ElevenLabs` model used by the fallback chain.
pub const ELEVENLABS_MODEL: &str = "eleven_multilingual_v2";
const OUTPUT_FORMAT: &str = "mp3_44100_128";

/// `ElevenLabs` adapter configuration.
#[derive(Clone, Debug)]
pub struct ElevenLabsConfig {
    /// Shared HTTP limits and origin.
    pub http: ProviderHttpConfig,
    /// Configured text-to-speech model identifier.
    pub model: String,
    /// Voice selected for synthesis.
    pub voice_id: String,
    /// Whether `ElevenLabs` may retain request history. Secure deployments disable this.
    pub enable_logging: bool,
}

/// Binary `ElevenLabs` text-to-speech adapter.
#[derive(Clone, Debug)]
pub struct ElevenLabsProvider {
    runtime: ProviderRuntime,
    model: String,
    voice_id: String,
    enable_logging: bool,
}

impl ElevenLabsProvider {
    /// Creates an adapter. The caller owns validation of the configured voice ID.
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        api_key: secrecy::SecretString,
        config: ElevenLabsConfig,
    ) -> Self {
        Self {
            runtime: ProviderRuntime::new(client, api_key, config.http),
            model: config.model,
            voice_id: config.voice_id,
            enable_logging: config.enable_logging,
        }
    }

    /// Synthesizes MP3 bytes. Multilingual v2 infers language from the text and
    /// intentionally receives no unsupported `language_code` field.
    async fn synthesize_audio(
        &self,
        text: &str,
        voice: Option<&ElevenLabsVoice>,
        options: Option<&ElevenLabsTtsOptions>,
    ) -> Result<AudioArtifact, ProviderError> {
        let _permit = self.runtime.try_acquire()?;
        let url = self.speech_url(
            options
                .and_then(|options| options.voice_id.as_deref())
                .unwrap_or_else(|| voice.map_or(&self.voice_id, |v| &v.voice_id)),
        )?;
        let body = ElevenLabsSpeechRequest {
            text,
            model_id: options
                .and_then(|options| options.model_id.as_deref())
                .unwrap_or_else(|| voice.map_or(&self.model, |v| &v.model_id)),
            voice_settings: ElevenLabsSpeechSettings::merged(
                voice.map(|v| &v.voice_settings),
                options,
            ),
            seed: options.and_then(|options| options.seed),
            previous_text: options.and_then(|options| options.previous_text.as_deref()),
            next_text: options.and_then(|options| options.next_text.as_deref()),
            apply_text_normalization: options
                .and_then(|options| options.apply_text_normalization.as_deref()),
        };
        let response = self
            .runtime
            .execute_without_retry(|| {
                Ok(self
                    .runtime
                    .client
                    .post(url.clone())
                    .header("xi-api-key", self.runtime.api_key.expose_secret())
                    .json(&body))
            })
            .await?;
        mp3_response_artifact(response)
    }

    fn speech_url(&self, voice_id: &str) -> Result<url::Url, ProviderError> {
        let mut url = self.runtime.url("v1/text-to-speech/")?;
        url.path_segments_mut()
            .map_err(|()| ProviderError::invalid_response())?
            .push(voice_id);
        url.query_pairs_mut()
            .append_pair("output_format", OUTPUT_FORMAT)
            .append_pair(
                "enable_logging",
                if self.enable_logging { "true" } else { "false" },
            );
        Ok(url)
    }
}

#[async_trait]
impl TextToSpeechProvider for ElevenLabsProvider {
    fn with_api_key(
        &self,
        key: secrecy::SecretString,
    ) -> Option<std::sync::Arc<dyn TextToSpeechProvider>> {
        let mut provider = self.clone();
        provider.runtime.api_key = key.clone();
        Some(std::sync::Arc::new(provider))
    }

    fn name(&self) -> ProviderName {
        ProviderName::ElevenLabs
    }

    async fn synthesize(
        &self,
        request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, DomainProviderError> {
        self.synthesize_audio(
            request.text.as_str(),
            match &request.voice {
                Some(ProviderVoice::ElevenLabs(voice)) => Some(voice),
                _ => None,
            },
            request.options.elevenlabs.as_ref(),
        )
        .await
        .map_err(|error| map_provider_error(self.name(), error))
    }
}

#[derive(Serialize)]
struct ElevenLabsSpeechRequest<'a> {
    text: &'a str,
    model_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_settings: Option<ElevenLabsSpeechSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply_text_normalization: Option<&'a str>,
}

#[derive(Default, Serialize)]
struct ElevenLabsSpeechSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    stability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity_boost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    style: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    use_speaker_boost: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f64>,
}

impl ElevenLabsSpeechSettings {
    fn merged(
        profile: Option<&ElevenLabsVoiceSettings>,
        options: Option<&ElevenLabsTtsOptions>,
    ) -> Option<Self> {
        let mut result = profile.map_or_else(Self::default, |profile| Self {
            stability: Some(profile.stability),
            similarity_boost: Some(profile.similarity_boost),
            style: Some(profile.style),
            use_speaker_boost: Some(profile.use_speaker_boost),
            speed: Some(profile.speed),
        });
        if let Some(options) = options {
            result.stability = options.stability.or(result.stability);
            result.similarity_boost = options.similarity_boost.or(result.similarity_boost);
            result.style = options.style.or(result.style);
            result.use_speaker_boost = options.use_speaker_boost.or(result.use_speaker_boost);
            result.speed = options.speed.or(result.speed);
        }
        (result.stability.is_some()
            || result.similarity_boost.is_some()
            || result.style.is_some()
            || result.use_speaker_boost.is_some()
            || result.speed.is_some())
        .then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use secrecy::SecretString;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path, query_param},
    };

    use super::*;

    #[tokio::test]
    async fn profile_sends_voice_path_model_and_every_setting()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let voice = ElevenLabsVoice {
            voice_id: "selected-voice".into(),
            model_id: ELEVENLABS_MODEL.into(),
            voice_settings: ElevenLabsVoiceSettings {
                stability: 0.0,
                similarity_boost: 0.75,
                style: 0.0,
                use_speaker_boost: false,
                speed: 1.2,
            },
        };
        Mock::given(method("POST")).and(path("/v1/text-to-speech/selected-voice"))
            .and(body_json(serde_json::json!({"text":"hello", "model_id":ELEVENLABS_MODEL, "voice_settings": {"stability":0.0,"similarity_boost":0.75,"style":0.0,"use_speaker_boost":false,"speed":1.2}})))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type","audio/mpeg").set_body_bytes(valid_mp3_frame())).expect(1).mount(&server).await;
        let provider = ElevenLabsProvider::new(
            reqwest::Client::new(),
            SecretString::from("test-key"),
            ElevenLabsConfig {
                http: ProviderHttpConfig::new(server.uri().parse()?),
                model: ELEVENLABS_MODEL.into(),
                voice_id: "server-default".into(),
                enable_logging: false,
            },
        );
        let request = TtsProviderRequest {
            fixture_voice: None,
            text: crate::domain::speech::SpeechText::new("hello".into())?,
            language: None,
            voice: Some(ProviderVoice::ElevenLabs(voice)),
            options: crate::domain::tts_options::TtsProviderOptions::default(),
        };
        provider
            .synthesize(request, RequestId::new(uuid::Uuid::new_v4())?)
            .await?;
        Ok(())
    }

    fn valid_mp3_frame() -> Vec<u8> {
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        frame
    }

    #[tokio::test]
    async fn tts_returns_provider_failure_without_retrying_generation()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/text-to-speech/voice-id"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"detail":{"code":"quota_exceeded","message":"Private upstream detail"}})))
            .expect(1).mount(&server).await;
        let provider = ElevenLabsProvider::new(
            reqwest::Client::new(),
            SecretString::from("test-key"),
            ElevenLabsConfig {
                http: ProviderHttpConfig::new(server.uri().parse()?),
                model: ELEVENLABS_MODEL.into(),
                voice_id: "voice-id".into(),
                enable_logging: false,
            },
        );
        let error = provider
            .synthesize_audio("Hello.", None, None)
            .await
            .err()
            .ok_or("expected provider failure")?;
        assert_eq!(error.kind, super::super::ProviderErrorKind::RateLimited);
        assert_eq!(error.status, Some(429));
        Ok(())
    }

    #[tokio::test]
    async fn request_controls_override_selected_profile_and_preserve_its_other_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/text-to-speech/custom-voice"))
            .and(body_json(serde_json::json!({
                "text":"Hello.","model_id":"eleven_flash_v2_5",
                "voice_settings":{"stability":0.2,"similarity_boost":0.75,"style":0.6,"use_speaker_boost":false,"speed":0.8},
                "seed":42,"previous_text":"Before.","next_text":"After.","apply_text_normalization":"off"
            })))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type","audio/mpeg").set_body_bytes(valid_mp3_frame()))
            .expect(1).mount(&server).await;
        let provider = ElevenLabsProvider::new(
            reqwest::Client::new(),
            SecretString::from("test-key"),
            ElevenLabsConfig {
                http: ProviderHttpConfig::new(server.uri().parse()?),
                model: ELEVENLABS_MODEL.into(),
                voice_id: "server-default".into(),
                enable_logging: false,
            },
        );
        provider
            .synthesize(
                TtsProviderRequest {
                    fixture_voice: None,
                    text: crate::domain::speech::SpeechText::new("Hello.".into())?,
                    language: None,
                    voice: Some(ProviderVoice::ElevenLabs(ElevenLabsVoice {
                        voice_id: "profile-voice".into(),
                        model_id: ELEVENLABS_MODEL.into(),
                        voice_settings: ElevenLabsVoiceSettings {
                            stability: 0.5,
                            similarity_boost: 0.75,
                            style: 0.0,
                            use_speaker_boost: true,
                            speed: 1.0,
                        },
                    })),
                    options: crate::domain::tts_options::TtsProviderOptions {
                        elevenlabs: Some(ElevenLabsTtsOptions {
                            voice_id: Some("custom-voice".into()),
                            model_id: Some("eleven_flash_v2_5".into()),
                            stability: Some(0.2),
                            style: Some(0.6),
                            use_speaker_boost: Some(false),
                            speed: Some(0.8),
                            seed: Some(42),
                            previous_text: Some("Before.".into()),
                            next_text: Some("After.".into()),
                            apply_text_normalization: Some("off".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                },
                RequestId::new(uuid::Uuid::new_v4())?,
            )
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn sends_exact_model_and_returns_binary_mp3() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/text-to-speech/voice-id"))
            .and(query_param("output_format", OUTPUT_FORMAT))
            .and(query_param("enable_logging", "false"))
            .and(header("xi-api-key", "unit-test-key"))
            .and(body_json(serde_json::json!({
                "text": "hello",
                "model_id": ELEVENLABS_MODEL
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(valid_mp3_frame()),
            )
            .expect(2)
            .mount(&server)
            .await;
        for suffix in ["/v1", "/v1/"] {
            let Some(base_url) = Url::parse(&format!("{}{suffix}", server.uri())).ok() else {
                continue;
            };
            let mut http = ProviderHttpConfig::new(base_url);
            http.timeout = Duration::from_secs(1);
            let provider = ElevenLabsProvider::new(
                reqwest::Client::new(),
                SecretString::from("unit-test-key".to_owned()),
                ElevenLabsConfig {
                    http,
                    model: ELEVENLABS_MODEL.to_owned(),
                    voice_id: "voice-id".to_owned(),
                    enable_logging: false,
                },
            );

            let result = provider.synthesize_audio("hello", None, None).await;

            assert!(result.is_ok());
        }
    }
}
