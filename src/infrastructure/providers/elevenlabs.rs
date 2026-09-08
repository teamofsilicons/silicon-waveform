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
    ) -> Result<AudioArtifact, ProviderError> {
        let _permit = self.runtime.try_acquire()?;
        let url = self.speech_url(voice.map_or(&self.voice_id, |v| &v.voice_id))?;
        let body = ElevenLabsSpeechRequest {
            text,
            model_id: voice.map_or(&self.model, |v| &v.model_id),
            voice_settings: voice.map(|v| &v.voice_settings),
        };
        let response = self
            .runtime
            .execute(|| {
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
    voice_settings: Option<&'a ElevenLabsVoiceSettings>,
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
            text: crate::domain::speech::SpeechText::new("hello".into())?,
            language: None,
            voice: Some(ProviderVoice::ElevenLabs(voice)),
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

            let result = provider.synthesize_audio("hello", None).await;

            assert!(result.is_ok());
        }
    }
}
