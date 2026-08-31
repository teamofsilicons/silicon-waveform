use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::{
    application::ports::SpeechToTextProvider,
    domain::{
        identity::RequestId,
        provider::{ProviderError as DomainProviderError, ProviderName},
        speech::{SttProviderRequest, SttProviderResult},
    },
};

use super::{
    ProviderError, ProviderHttpConfig, ProviderRuntime, map_provider_error, transcription_result,
};

/// Exact Deepgram model used by the final STT fallback.
pub const DEEPGRAM_MODEL: &str = "nova-3";

/// Deepgram adapter configuration.
#[derive(Clone, Debug)]
pub struct DeepgramConfig {
    /// Shared HTTP limits and origin.
    pub http: ProviderHttpConfig,
    /// Configured speech-to-text model identifier.
    pub model: String,
    /// Whether each request opts out of Deepgram's Model Improvement Program.
    pub mip_opt_out: bool,
}

/// Deepgram raw-media pre-recorded transcription adapter.
#[derive(Clone, Debug)]
pub struct DeepgramProvider {
    runtime: ProviderRuntime,
    model: String,
    mip_opt_out: bool,
}

impl DeepgramProvider {
    /// Creates the adapter with an already hardened HTTP client.
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        api_key: secrecy::SecretString,
        config: DeepgramConfig,
    ) -> Self {
        Self {
            runtime: ProviderRuntime::new(client, api_key, config.http),
            model: config.model,
            mip_opt_out: config.mip_opt_out,
        }
    }

    /// Sends bounded raw media to `/v1/listen`.
    async fn transcribe_media(
        &self,
        request: &SttProviderRequest,
    ) -> Result<SttProviderResult, ProviderError> {
        let _permit = self.runtime.try_acquire()?;
        let mut url = self.runtime.url("v1/listen")?;
        url.query_pairs_mut()
            .append_pair("model", &self.model)
            .append_pair(
                "language",
                request
                    .language
                    .as_ref()
                    .map_or("multi", |language| language.primary_language()),
            )
            .append_pair("smart_format", "true")
            .append_pair(
                "mip_opt_out",
                if self.mip_opt_out { "true" } else { "false" },
            );
        let media_type = request.media.media_type().as_mime_str();
        let response = self
            .runtime
            .execute(|| {
                Ok(self
                    .runtime
                    .client
                    .post(url.clone())
                    .header(
                        "Authorization",
                        format!("Token {}", self.runtime.api_key.expose_secret()),
                    )
                    .header("Content-Type", media_type)
                    .body(request.media.bytes().clone()))
            })
            .await?;
        let parsed: DeepgramResponse = serde_json::from_slice(&response.body)
            .map_err(|_| ProviderError::invalid_response())?;
        let channel = parsed
            .results
            .channels
            .first()
            .ok_or_else(ProviderError::invalid_response)?;
        let alternative = channel
            .alternatives
            .first()
            .ok_or_else(ProviderError::invalid_response)?;
        let detected_language = channel.detected_language.clone().or_else(|| {
            alternative
                .languages
                .first()
                .filter(|_| alternative.languages.len() == 1)
                .cloned()
        });
        transcription_result(
            alternative.transcript.clone(),
            detected_language,
            seconds_to_millis(parsed.metadata.duration),
        )
    }
}

#[async_trait]
impl SpeechToTextProvider for DeepgramProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Deepgram
    }

    async fn transcribe(
        &self,
        request: SttProviderRequest,
        _request_id: RequestId,
    ) -> Result<SttProviderResult, DomainProviderError> {
        self.transcribe_media(&request)
            .await
            .map_err(|error| map_provider_error(self.name(), error))
    }
}

#[derive(Deserialize)]
struct DeepgramResponse {
    metadata: DeepgramMetadata,
    results: DeepgramResults,
}

#[derive(Deserialize)]
struct DeepgramMetadata {
    duration: Option<f64>,
}

#[derive(Deserialize)]
struct DeepgramResults {
    channels: Vec<DeepgramChannel>,
}

#[derive(Deserialize)]
struct DeepgramChannel {
    alternatives: Vec<DeepgramAlternative>,
    detected_language: Option<String>,
}

#[derive(Deserialize)]
struct DeepgramAlternative {
    transcript: String,
    #[serde(default)]
    languages: Vec<String>,
}

fn seconds_to_millis(seconds: Option<f64>) -> Option<u64> {
    let seconds = seconds?;
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
        matchers::{body_bytes, header, method, path, query_param},
    };

    use super::*;

    #[tokio::test]
    async fn sends_raw_media_with_exact_model_and_hint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/listen"))
            .and(query_param("model", DEEPGRAM_MODEL))
            .and(query_param("language", "fr"))
            .and(query_param("smart_format", "true"))
            .and(query_param("mip_opt_out", "true"))
            .and(header("authorization", "Token unit-test-key"))
            .and(header("content-type", "audio/wav"))
            .and(body_bytes(b"wave-data".as_slice()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"duration": 1.5},
                "results": {"channels": [{
                    "detected_language": "fr",
                    "alternatives": [{
                    "transcript": "bonjour",
                    "languages": ["fr"],
                    "words": []
                }]}]}
            })))
            .mount(&server)
            .await;
        let Some(base_url) = Url::parse(&format!("{}/", server.uri())).ok() else {
            return;
        };
        let provider = DeepgramProvider::new(
            reqwest::Client::new(),
            SecretString::from("unit-test-key".to_owned()),
            DeepgramConfig {
                http: ProviderHttpConfig::new(base_url),
                model: DEEPGRAM_MODEL.to_owned(),
                mip_opt_out: true,
            },
        );
        let media = crate::domain::media::SourceMedia::new(
            crate::domain::media::SourceMediaType::Wav,
            bytes::Bytes::from_static(b"wave-data"),
            crate::domain::media::MediaSizeLimit::default(),
        );
        let language = "fr-FR".parse();
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
                if result.detected_language.as_ref().is_some_and(|language| language.as_str() == "fr")
                    && result.duration.is_some_and(|duration| duration.as_millis() == 1_500)
        ));
    }
}
