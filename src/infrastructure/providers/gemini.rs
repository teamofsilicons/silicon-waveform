use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

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
    ProviderError, ProviderErrorKind, ProviderHttpConfig, ProviderRuntime, audio_artifact,
    common::TransientRetryBudget, map_provider_error, media_filename, transcription_result,
};

/// Exact Gemini TTS preview model used by Waveform.
pub const GEMINI_TTS_MODEL: &str = "gemini-3.1-flash-tts-preview";
/// Exact Gemini unary transcription model used by Waveform.
pub const GEMINI_STT_MODEL: &str = "gemini-3.5-transcribe";

const UPLOAD_URL_HEADER: &str = "x-goog-upload-url";
const FILE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Gemini adapter configuration.
#[derive(Clone, Debug)]
pub struct GeminiConfig {
    /// HTTP limits for text-to-speech interactions.
    pub tts_http: ProviderHttpConfig,
    /// HTTP limits for the complete upload/transcription exchange.
    pub stt_http: ProviderHttpConfig,
    /// Configured text-to-speech model identifier.
    pub tts_model: String,
    /// Configured speech-to-text model identifier.
    pub stt_model: String,
    /// Prebuilt Gemini TTS voice.
    pub voice: String,
}

/// Gemini Interactions TTS and Files/Interactions STT adapter.
#[derive(Clone, Debug)]
pub struct GeminiProvider {
    tts_runtime: ProviderRuntime,
    stt_runtime: ProviderRuntime,
    tts_model: String,
    stt_model: String,
    voice: String,
}

impl GeminiProvider {
    /// Creates the adapter with one semaphore shared by both capabilities.
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        api_key: secrecy::SecretString,
        config: GeminiConfig,
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

    /// Synthesizes audio through the Gemini Interactions API.
    async fn synthesize_audio(
        &self,
        text: &str,
        language: Option<&str>,
        voice: Option<&str>,
    ) -> Result<AudioArtifact, ProviderError> {
        let _permit = self.tts_runtime.try_acquire()?;
        let url = self.tts_runtime.url("v1beta/interactions")?;
        let body = GeminiTtsRequest {
            model: &self.tts_model,
            input: text,
            response_format: GeminiResponseFormat { kind: "audio" },
            generation_config: GeminiTtsGenerationConfig {
                speech_config: [GeminiSpeechConfig {
                    voice: voice.unwrap_or(&self.voice),
                    language,
                }],
            },
        };
        let response = self
            .tts_runtime
            .execute(|| {
                Ok(self
                    .tts_runtime
                    .client
                    .post(url.clone())
                    .header("x-goog-api-key", self.tts_runtime.api_key.expose_secret())
                    .json(&body))
            })
            .await?;
        let interaction = parse_completed_interaction(&response.body)?;
        let audio = interaction
            .steps
            .iter()
            .flat_map(|step| step.content.iter())
            .rev()
            .find(|content| content.kind == "audio")
            .ok_or_else(ProviderError::invalid_response)?;
        let encoded = audio
            .data
            .as_deref()
            .ok_or_else(ProviderError::invalid_response)?;
        let decoded = STANDARD
            .decode(encoded)
            .map_err(|_| ProviderError::invalid_response())?;
        if decoded.is_empty() || decoded.len() > self.tts_runtime.config.max_response_bytes {
            return Err(ProviderError::invalid_response());
        }
        audio_artifact(decoded.into(), &audio_media_type(audio))
    }

    /// Uploads bounded media with the Files API and transcribes it through an
    /// Interaction. The uploaded file is deleted on a best-effort basis.
    async fn transcribe_media(
        &self,
        request: &SttProviderRequest,
        request_id: RequestId,
    ) -> Result<SttProviderResult, ProviderError> {
        let _permit = self.stt_runtime.try_acquire()?;
        let deadline = Instant::now() + self.stt_runtime.config.timeout;
        let mut retry_budget = TransientRetryBudget::new();
        let file = tokio::time::timeout_at(deadline, self.upload_file(request))
            .await
            .map_err(|_| ProviderError::new(ProviderErrorKind::Timeout))??;
        let cleanup = GeminiFileCleanup::new(self.clone(), file.name.clone(), request_id);
        let result = match tokio::time::timeout_at(deadline, async {
            let active = self.wait_until_active(file, &mut retry_budget).await?;
            self.transcribe_file(
                &active,
                request
                    .language
                    .as_ref()
                    .map(crate::domain::language::LanguageHint::as_str),
                &mut retry_budget,
            )
            .await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ProviderError::new(ProviderErrorKind::Timeout)),
        };
        cleanup.run_until(deadline).await;
        result
    }

    async fn upload_file(&self, request: &SttProviderRequest) -> Result<GeminiFile, ProviderError> {
        let start_url = self.stt_runtime.url("upload/v1beta/files")?;
        let content_length = request.media.len().to_string();
        let media_type = request.media.media_type().as_mime_str();
        let metadata = GeminiUploadMetadata {
            file: GeminiUploadFileMetadata {
                display_name: media_filename(request.media.media_type()),
            },
        };
        let start = self
            .stt_runtime
            .execute_without_retry(|| {
                Ok(self
                    .stt_runtime
                    .client
                    .post(start_url.clone())
                    .header("x-goog-api-key", self.stt_runtime.api_key.expose_secret())
                    .header("X-Goog-Upload-Protocol", "resumable")
                    .header("X-Goog-Upload-Command", "start")
                    .header("X-Goog-Upload-Header-Content-Length", &content_length)
                    .header("X-Goog-Upload-Header-Content-Type", media_type)
                    .json(&metadata))
            })
            .await?;
        let upload_url = start
            .headers
            .get(UPLOAD_URL_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| url::Url::parse(value).ok())
            .filter(|value| same_origin(value, &self.stt_runtime.config.base_url))
            .ok_or_else(ProviderError::invalid_response)?;

        let uploaded = self
            .stt_runtime
            .execute_without_retry(|| {
                Ok(self
                    .stt_runtime
                    .client
                    .post(upload_url.clone())
                    .header("Content-Length", &content_length)
                    .header("X-Goog-Upload-Offset", "0")
                    .header("X-Goog-Upload-Command", "upload, finalize")
                    .header("Content-Type", media_type)
                    .body(request.media.bytes().clone()))
            })
            .await?;
        let parsed: GeminiFileEnvelope = serde_json::from_slice(&uploaded.body)
            .map_err(|_| ProviderError::invalid_response())?;
        validate_file(&parsed.file)?;
        Ok(parsed.file)
    }

    async fn wait_until_active(
        &self,
        mut file: GeminiFile,
        retry_budget: &mut TransientRetryBudget,
    ) -> Result<GeminiFile, ProviderError> {
        loop {
            match file.state.as_str() {
                "ACTIVE" => return Ok(file),
                "FAILED" => return Err(ProviderError::new(ProviderErrorKind::Rejected)),
                "PROCESSING" => {
                    tokio::time::sleep(FILE_POLL_INTERVAL).await;
                    let url = self.stt_runtime.url(&format!("v1beta/{}", file.name))?;
                    let response =
                        self.stt_runtime
                            .execute_with_retry_budget(retry_budget, || {
                                Ok(self.stt_runtime.client.get(url.clone()).header(
                                    "x-goog-api-key",
                                    self.stt_runtime.api_key.expose_secret(),
                                ))
                            })
                            .await?;
                    file = serde_json::from_slice(&response.body)
                        .map_err(|_| ProviderError::invalid_response())?;
                    validate_file(&file)?;
                }
                _ => return Err(ProviderError::invalid_response()),
            }
        }
    }

    async fn transcribe_file(
        &self,
        file: &GeminiFile,
        language: Option<&str>,
        retry_budget: &mut TransientRetryBudget,
    ) -> Result<SttProviderResult, ProviderError> {
        let url = self.stt_runtime.url("v1beta/interactions")?;
        let languages = language
            .map(gemini_language_hint)
            .into_iter()
            .collect::<Vec<_>>();
        let body = GeminiSttRequest {
            model: &self.stt_model,
            input: [GeminiAudioInput {
                kind: "audio",
                uri: &file.uri,
                mime_type: &file.mime_type,
            }],
            generation_config: GeminiSttGenerationConfig {
                transcription_config: GeminiTranscriptionConfig {
                    language_codes: languages,
                },
            },
        };
        let response = self
            .stt_runtime
            .execute_with_retry_budget(retry_budget, || {
                Ok(self
                    .stt_runtime
                    .client
                    .post(url.clone())
                    .header("x-goog-api-key", self.stt_runtime.api_key.expose_secret())
                    .json(&body))
            })
            .await?;
        let interaction = parse_completed_interaction(&response.body)?;
        let text_parts = interaction
            .steps
            .iter()
            .flat_map(|step| step.content.iter())
            .filter(|content| content.kind == "text")
            .map(|content| {
                content
                    .text
                    .as_deref()
                    .ok_or_else(ProviderError::invalid_response)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if text_parts.is_empty() {
            return Err(ProviderError::invalid_response());
        }
        let text = text_parts.join("");
        // Gemini's documented unary response does not expose one stable
        // top-level detected-language field. Never guess it from the hint.
        transcription_result(text, None, None)
    }

    async fn delete_file(&self, name: &str) -> Result<(), ProviderError> {
        if !valid_file_name(name) {
            return Err(ProviderError::invalid_response());
        }
        let url = self.stt_runtime.url(&format!("v1beta/{name}"))?;
        self.stt_runtime
            .execute_without_retry(|| {
                Ok(self
                    .stt_runtime
                    .client
                    .delete(url.clone())
                    .header("x-goog-api-key", self.stt_runtime.api_key.expose_secret()))
            })
            .await?;
        Ok(())
    }
}

struct GeminiFileCleanup {
    provider: GeminiProvider,
    name: Option<String>,
    request_id: RequestId,
}

impl GeminiFileCleanup {
    fn new(provider: GeminiProvider, name: String, request_id: RequestId) -> Self {
        Self {
            provider,
            name: Some(name),
            request_id,
        }
    }

    async fn run_until(mut self, deadline: Instant) {
        let Some(name) = self.name.take() else {
            return;
        };
        if Instant::now() >= deadline {
            spawn_cleanup(self.provider.clone(), name, self.request_id);
            return;
        }
        match tokio::time::timeout_at(deadline, self.provider.delete_file(&name)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log_cleanup_failure(self.request_id, Some(error.kind)),
            Err(_) => {
                log_cleanup_failure(self.request_id, Some(ProviderErrorKind::Timeout));
                spawn_cleanup(self.provider.clone(), name, self.request_id);
            }
        }
    }
}

impl Drop for GeminiFileCleanup {
    fn drop(&mut self) {
        let Some(name) = self.name.take() else {
            return;
        };
        spawn_cleanup(self.provider.clone(), name, self.request_id);
    }
}

fn spawn_cleanup(provider: GeminiProvider, name: String, request_id: RequestId) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            match tokio::time::timeout(
                std::time::Duration::from_secs(1),
                provider.delete_file(&name),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log_cleanup_failure(request_id, Some(error.kind)),
                Err(_) => log_cleanup_failure(request_id, Some(ProviderErrorKind::Timeout)),
            }
        });
    } else {
        log_cleanup_failure(request_id, Some(ProviderErrorKind::Unavailable));
    }
}

fn log_cleanup_failure(request_id: RequestId, kind: Option<ProviderErrorKind>) {
    tracing::warn!(
        provider = "gemini",
        request_id = %request_id.as_uuid(),
        failure_kind = ?kind,
        "provider file cleanup failed"
    );
}

#[async_trait]
impl TextToSpeechProvider for GeminiProvider {
    fn with_api_key(
        &self,
        key: secrecy::SecretString,
    ) -> Option<std::sync::Arc<dyn TextToSpeechProvider>> {
        let mut provider = self.clone();
        provider.tts_runtime.api_key = key.clone();
        provider.stt_runtime.api_key = key.clone();
        Some(std::sync::Arc::new(provider))
    }

    fn name(&self) -> ProviderName {
        ProviderName::Gemini
    }

    async fn synthesize(
        &self,
        request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, DomainProviderError> {
        self.synthesize_audio(
            request.text.as_str(),
            request
                .language
                .as_ref()
                .map(crate::domain::language::LanguageHint::as_str),
            match &request.voice {
                Some(crate::domain::voice::ProviderVoice::Gemini(voice)) => Some(voice.as_str()),
                _ => None,
            },
        )
        .await
        .map_err(|error| map_provider_error(ProviderName::Gemini, error))
    }
}

#[async_trait]
impl SpeechToTextProvider for GeminiProvider {
    fn with_api_key(
        &self,
        key: secrecy::SecretString,
    ) -> Option<std::sync::Arc<dyn SpeechToTextProvider>> {
        let mut provider = self.clone();
        provider.tts_runtime.api_key = key.clone();
        provider.stt_runtime.api_key = key.clone();
        Some(std::sync::Arc::new(provider))
    }

    fn name(&self) -> ProviderName {
        ProviderName::Gemini
    }

    async fn transcribe(
        &self,
        request: SttProviderRequest,
        request_id: RequestId,
    ) -> Result<SttProviderResult, DomainProviderError> {
        self.transcribe_media(&request, request_id)
            .await
            .map_err(|error| map_provider_error(ProviderName::Gemini, error))
    }
}

fn validate_file(file: &GeminiFile) -> Result<(), ProviderError> {
    if file.name.is_empty() || file.uri.is_empty() || file.mime_type.is_empty() {
        return Err(ProviderError::invalid_response());
    }
    Ok(())
}

fn parse_completed_interaction(body: &[u8]) -> Result<GeminiInteraction, ProviderError> {
    let interaction: GeminiInteraction =
        serde_json::from_slice(body).map_err(|_| ProviderError::invalid_response())?;
    match interaction.status.as_str() {
        "completed" => Ok(interaction),
        "failed" | "cancelled" | "incomplete" | "budget_exceeded" | "queued" => {
            Err(ProviderError::new(ProviderErrorKind::Rejected))
        }
        _ => Err(ProviderError::invalid_response()),
    }
}

fn audio_media_type(content: &GeminiContent) -> String {
    let base = content.mime_type.as_deref().unwrap_or("audio/l16");
    if base.eq_ignore_ascii_case("audio/l16") {
        let rate = content.sample_rate.unwrap_or(24_000);
        let channels = content.channels.unwrap_or(1);
        format!("audio/L16;codec=pcm;rate={rate};channels={channels}")
    } else {
        base.to_owned()
    }
}

fn same_origin(candidate: &url::Url, configured: &url::Url) -> bool {
    candidate.scheme() == configured.scheme()
        && candidate.host_str() == configured.host_str()
        && candidate.port_or_known_default() == configured.port_or_known_default()
}

fn valid_file_name(name: &str) -> bool {
    name.strip_prefix("files/").is_some_and(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

fn gemini_language_hint(language: &str) -> &str {
    match language {
        "en" => "en-US",
        "es" => "es-419",
        "fr" => "fr-FR",
        "de" => "de-DE",
        "hi" => "hi-IN",
        "ru" => "ru-RU",
        "pt" => "pt-BR",
        "ja" => "ja-JP",
        "it" => "it-IT",
        "nl" => "nl-NL",
        other => other,
    }
}

#[derive(Serialize)]
struct GeminiTtsRequest<'a> {
    model: &'a str,
    input: &'a str,
    response_format: GeminiResponseFormat,
    generation_config: GeminiTtsGenerationConfig<'a>,
}

#[derive(Serialize)]
struct GeminiResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct GeminiTtsGenerationConfig<'a> {
    speech_config: [GeminiSpeechConfig<'a>; 1],
}

#[derive(Serialize)]
struct GeminiSpeechConfig<'a> {
    voice: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
}

#[derive(Serialize)]
struct GeminiUploadMetadata<'a> {
    file: GeminiUploadFileMetadata<'a>,
}

#[derive(Serialize)]
struct GeminiUploadFileMetadata<'a> {
    display_name: &'a str,
}

#[derive(Serialize)]
struct GeminiSttRequest<'a> {
    model: &'a str,
    input: [GeminiAudioInput<'a>; 1],
    generation_config: GeminiSttGenerationConfig<'a>,
}

#[derive(Serialize)]
struct GeminiAudioInput<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    uri: &'a str,
    mime_type: &'a str,
}

#[derive(Serialize)]
struct GeminiSttGenerationConfig<'a> {
    transcription_config: GeminiTranscriptionConfig<'a>,
}

#[derive(Serialize)]
struct GeminiTranscriptionConfig<'a> {
    language_codes: Vec<&'a str>,
}

#[derive(Deserialize)]
struct GeminiFileEnvelope {
    file: GeminiFile,
}

#[derive(Deserialize)]
struct GeminiFile {
    name: String,
    uri: String,
    #[serde(alias = "mimeType")]
    mime_type: String,
    state: String,
}

#[derive(Deserialize)]
struct GeminiInteraction {
    status: String,
    #[serde(default)]
    steps: Vec<GeminiStep>,
}

#[derive(Deserialize)]
struct GeminiStep {
    #[serde(default)]
    content: Vec<GeminiContent>,
}

#[derive(Deserialize)]
struct GeminiContent {
    #[serde(rename = "type")]
    kind: String,
    data: Option<String>,
    #[serde(alias = "mimeType")]
    mime_type: Option<String>,
    #[serde(alias = "sampleRate")]
    sample_rate: Option<u32>,
    channels: Option<u16>,
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path},
    };

    use super::*;

    fn provider(server: &MockServer) -> Option<GeminiProvider> {
        let base_url = Url::parse(&format!("{}/", server.uri())).ok()?;
        Some(GeminiProvider::new(
            reqwest::Client::new(),
            SecretString::from("unit-test-key".to_owned()),
            GeminiConfig {
                tts_http: ProviderHttpConfig::new(base_url.clone()),
                stt_http: ProviderHttpConfig::new(base_url),
                tts_model: GEMINI_TTS_MODEL.to_owned(),
                stt_model: GEMINI_STT_MODEL.to_owned(),
                voice: "Kore".to_owned(),
            },
        ))
    }

    #[tokio::test]
    async fn tts_uses_interactions_model_and_decodes_inline_audio() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/interactions"))
            .and(header("x-goog-api-key", "unit-test-key"))
            .and(body_json(serde_json::json!({
                "model": GEMINI_TTS_MODEL,
                "input": "hello",
                "response_format": {"type": "audio"},
                "generation_config": {"speech_config": [{
                    "voice": "Puck", "language": "en"
                }]}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "completed",
                "steps": [{"content": [{
                    "type": "audio",
                    "data": STANDARD.encode([1, 2, 3, 4]),
                    "mime_type": "audio/l16",
                    "sample_rate": 24000,
                    "channels": 1
                }]}]
            })))
            .mount(&server)
            .await;
        let Some(provider) = provider(&server) else {
            return;
        };

        let result = TextToSpeechProvider::synthesize(
            &provider,
            TtsProviderRequest {
                text: crate::domain::speech::SpeechText::new("hello".into())
                    .unwrap_or_else(|e| panic!("{e}")),
                language: Some("en".parse().unwrap_or_else(|e| panic!("{e}"))),
                voice: Some(crate::domain::voice::ProviderVoice::Gemini("Puck".into())),
            },
            RequestId::new(uuid::Uuid::new_v4()).unwrap_or_else(|e| panic!("{e}")),
        )
        .await;

        assert!(matches!(
            result,
            Ok(audio)
                if audio.bytes() == [1, 2, 3, 4].as_slice()
                    && matches!(
                        audio.format(),
                        crate::domain::media::ProviderAudioFormat::LinearPcm(specification)
                            if specification.sample_rate_hz() == 24_000
                                && specification.channels() == 1
                    )
        ));
    }

    #[tokio::test]
    async fn stt_uploads_then_calls_interactions_with_mapped_language() {
        let server = MockServer::start().await;
        let upload_url = format!("{}/upload-session", server.uri());
        Mock::given(method("POST"))
            .and(path("/upload/v1beta/files"))
            .and(header("x-goog-upload-protocol", "resumable"))
            .respond_with(
                ResponseTemplate::new(200).insert_header(UPLOAD_URL_HEADER, upload_url.as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/upload-session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "file": {
                    "name": "files/test_file",
                    "uri": "https://files.example/test",
                    "mime_type": "audio/wav",
                    "state": "PROCESSING"
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1beta/files/test_file"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "files/test_file",
                "uri": "https://files.example/test",
                "mimeType": "audio/wav",
                "state": "ACTIVE"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1beta/interactions"))
            .and(body_json(serde_json::json!({
                "model": GEMINI_STT_MODEL,
                "input": [{
                    "type": "audio",
                    "uri": "https://files.example/test",
                    "mime_type": "audio/wav"
                }],
                "generation_config": {"transcription_config": {
                    "language_codes": ["fr-FR"]
                }}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "completed",
                "steps": [{"content": [{"type": "text", "text": "bonjour"}]}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1beta/files/test_file"))
            .respond_with(ResponseTemplate::new(204))
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
        let language = "fr".parse();
        let (Ok(media), Ok(language)) = (media, language) else {
            return;
        };
        let input = SttProviderRequest {
            media,
            language: Some(language),
        };

        let request_id = RequestId::new(uuid::Uuid::new_v4());
        let Ok(request_id) = request_id else {
            return;
        };
        let result = provider.transcribe_media(&input, request_id).await;

        assert!(matches!(
            result,
            Ok(transcript) if transcript.transcript.as_str() == "bonjour"
        ));
    }

    #[tokio::test]
    async fn stt_does_not_retry_non_idempotent_upload_start() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/upload/v1beta/files"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
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
        let request_id = RequestId::new(uuid::Uuid::new_v4());
        let (Ok(media), Ok(request_id)) = (media, request_id) else {
            return;
        };

        let result = provider
            .transcribe_media(
                &SttProviderRequest {
                    media,
                    language: None,
                },
                request_id,
            )
            .await;

        assert!(matches!(
            result,
            Err(ProviderError {
                kind: ProviderErrorKind::Unavailable
            })
        ));
    }
}
