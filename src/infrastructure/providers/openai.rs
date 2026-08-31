use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::{
    application::ports::{SpeechToTextProvider, TextToSpeechProvider},
    domain::{
        identity::RequestId,
        media::AudioArtifact,
        provider::{ProviderError as DomainProviderError, ProviderName},
        speech::{SttProviderRequest, SttProviderResult, TtsProviderRequest},
    },
};

use super::{
    ProviderError, ProviderHttpConfig, ProviderRuntime, map_provider_error, media_filename,
    mp3_response_artifact, transcription_result,
};

/// Exact `OpenAI` TTS model used by Waveform.
pub const OPENAI_TTS_MODEL: &str = "tts-1";
/// Exact `OpenAI` STT model used by Waveform.
pub const OPENAI_STT_MODEL: &str = "gpt-transcribe";

/// `OpenAI` adapter configuration.
#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    /// HTTP limits for binary synthesis.
    pub tts_http: ProviderHttpConfig,
    /// HTTP limits for multipart transcription.
    pub stt_http: ProviderHttpConfig,
    /// Configured text-to-speech model identifier.
    pub tts_model: String,
    /// Configured speech-to-text model identifier.
    pub stt_model: String,
    /// Built-in voice supported by `tts-1`.
    pub voice: String,
}

/// `OpenAI` binary TTS and multipart STT adapter.
#[derive(Clone, Debug)]
pub struct OpenAiProvider {
    tts_runtime: ProviderRuntime,
    stt_runtime: ProviderRuntime,
    tts_model: String,
    stt_model: String,
    voice: String,
}

impl OpenAiProvider {
    /// Creates the adapter with an already hardened HTTP client.
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        api_key: secrecy::SecretString,
        config: OpenAiConfig,
    ) -> Self {
        let semaphore = ProviderRuntime::shared_semaphore(config.tts_http.max_concurrency);
        Self {
            tts_runtime: ProviderRuntime::new_with_semaphore(
                client.clone(),
                api_key.clone(),
                config.tts_http,
                semaphore.clone(),
            ),
            stt_runtime: ProviderRuntime::new_with_semaphore(
                client,
                api_key,
                config.stt_http,
                semaphore,
            ),
            tts_model: config.tts_model,
            stt_model: config.stt_model,
            voice: config.voice,
        }
    }

    /// Calls the binary Speech endpoint with the exact `tts-1` model.
    async fn synthesize_audio(&self, text: &str) -> Result<AudioArtifact, ProviderError> {
        let _permit = self.tts_runtime.try_acquire()?;
        let url = self.tts_runtime.url("v1/audio/speech")?;
        let body = OpenAiSpeechRequest {
            model: &self.tts_model,
            input: text,
            voice: &self.voice,
            response_format: "mp3",
        };
        let response = self
            .tts_runtime
            .execute(|| {
                Ok(self
                    .tts_runtime
                    .client
                    .post(url.clone())
                    .bearer_auth(self.tts_runtime.api_key.expose_secret())
                    .json(&body))
            })
            .await?;
        mp3_response_artifact(response)
    }

    /// Calls the multipart Transcriptions endpoint with the exact
    /// `gpt-transcribe` model.
    async fn transcribe_media(
        &self,
        request: &SttProviderRequest,
    ) -> Result<SttProviderResult, ProviderError> {
        let _permit = self.stt_runtime.try_acquire()?;
        let url = self.stt_runtime.url("v1/audio/transcriptions")?;
        let media_type = request.media.media_type().as_mime_str();
        // Validate the MIME value before constructing a retryable multipart body.
        let _mime = media_type
            .parse::<mime::Mime>()
            .map_err(|_| ProviderError::invalid_response())?;
        let response = self
            .stt_runtime
            .execute(|| {
                let part = reqwest::multipart::Part::bytes(request.media.bytes().to_vec())
                    .file_name(media_filename(request.media.media_type()))
                    .mime_str(media_type)
                    .map_err(|_| ProviderError::invalid_response())?;
                let mut form = reqwest::multipart::Form::new()
                    .text("model", self.stt_model.clone())
                    .text("response_format", "json")
                    .part("file", part);
                if let Some(language) = request.language.as_ref() {
                    form = form.text("language", language.primary_language().to_owned());
                }
                Ok(self
                    .stt_runtime
                    .client
                    .post(url.clone())
                    .bearer_auth(self.stt_runtime.api_key.expose_secret())
                    .multipart(form))
            })
            .await?;
        let parsed: OpenAiTranscription = serde_json::from_slice(&response.body)
            .map_err(|_| ProviderError::invalid_response())?;
        let detected_language = parsed.language.or_else(|| {
            parsed
                .languages
                .first()
                .filter(|_| parsed.languages.len() == 1)
                .map(|language| language.code.clone())
        });
        let seconds = parsed
            .duration
            .or_else(|| parsed.usage.and_then(|usage| usage.seconds));
        transcription_result(
            parsed.text,
            detected_language,
            seconds.and_then(seconds_to_millis),
        )
    }
}

#[async_trait]
impl TextToSpeechProvider for OpenAiProvider {
    fn name(&self) -> ProviderName {
        ProviderName::OpenAi
    }

    async fn synthesize(
        &self,
        request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, DomainProviderError> {
        self.synthesize_audio(request.text.as_str())
            .await
            .map_err(|error| map_provider_error(ProviderName::OpenAi, error))
    }
}

#[async_trait]
impl SpeechToTextProvider for OpenAiProvider {
    fn name(&self) -> ProviderName {
        ProviderName::OpenAi
    }

    async fn transcribe(
        &self,
        request: SttProviderRequest,
        _request_id: RequestId,
    ) -> Result<SttProviderResult, DomainProviderError> {
        self.transcribe_media(&request)
            .await
            .map_err(|error| map_provider_error(ProviderName::OpenAi, error))
    }
}

#[derive(Serialize)]
struct OpenAiSpeechRequest<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'static str,
}

#[derive(Deserialize)]
struct OpenAiTranscription {
    text: String,
    language: Option<String>,
    #[serde(default)]
    languages: Vec<OpenAiLanguage>,
    duration: Option<f64>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiLanguage {
    code: String,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    seconds: Option<f64>,
}

fn seconds_to_millis(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds.is_sign_negative() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((seconds * 1_000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string_contains, header, method, path},
    };

    use super::*;

    fn provider(server: &MockServer) -> Option<OpenAiProvider> {
        let base_url = Url::parse(&format!("{}/", server.uri())).ok()?;
        Some(OpenAiProvider::new(
            reqwest::Client::new(),
            SecretString::from("unit-test-key".to_owned()),
            OpenAiConfig {
                tts_http: ProviderHttpConfig::new(base_url.clone()),
                stt_http: ProviderHttpConfig::new(base_url),
                tts_model: OPENAI_TTS_MODEL.to_owned(),
                stt_model: OPENAI_STT_MODEL.to_owned(),
                voice: "alloy".to_owned(),
            },
        ))
    }

    fn valid_mp3_frame() -> Vec<u8> {
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        frame
    }

    #[tokio::test]
    async fn tts_uses_exact_model_and_binary_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .and(header("authorization", "Bearer unit-test-key"))
            .and(body_json(serde_json::json!({
                "model": OPENAI_TTS_MODEL,
                "input": "hello",
                "voice": "alloy",
                "response_format": "mp3"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(valid_mp3_frame()),
            )
            .mount(&server)
            .await;
        let Some(provider) = provider(&server) else {
            return;
        };

        let result = provider.synthesize_audio("hello").await;

        assert!(matches!(result, Ok(audio) if audio.bytes() == valid_mp3_frame().as_slice()));
    }

    #[tokio::test]
    async fn stt_sends_multipart_model_and_language_hint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer unit-test-key"))
            .and(body_string_contains(OPENAI_STT_MODEL))
            .and(body_string_contains("en"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "hello world",
                "languages": [{"code": "en"}],
                "usage": {"seconds": 1.25}
            })))
            .mount(&server)
            .await;
        let Some(provider) = provider(&server) else {
            return;
        };
        let media = crate::domain::media::SourceMedia::new(
            crate::domain::media::SourceMediaType::Wav,
            bytes::Bytes::from_static(b"wave-data"),
            crate::domain::media::MediaSizeLimit::default(),
        );
        let language = "en".parse();
        let (Ok(media), Ok(language)) = (media, language) else {
            return;
        };
        let input = SttProviderRequest {
            media,
            language: Some(language),
        };

        let result = provider.transcribe_media(&input).await;

        assert!(matches!(
            result,
            Ok(result)
                if result.detected_language.as_ref().is_some_and(|language| language.as_str() == "en")
                    && result.duration.is_some_and(|duration| duration.as_millis() == 1_250)
        ));
    }
}
