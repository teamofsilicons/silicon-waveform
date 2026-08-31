//! Public JSON request and response translation.

use std::str::FromStr as _;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::domain::{
    capabilities::Capabilities,
    language::{LanguageHint, LanguageHintError},
    media::{BriefcaseFileUrl, MediaError},
    speech::{SpeechText, SpeechValidationError, SttRequest, SttResult, TtsRequest, TtsResult},
};

/// Public text-to-speech JSON body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsRequestBody {
    text: String,
    lang: Option<String>,
}

impl TtsRequestBody {
    /// Validates the public body into provider-independent domain input.
    ///
    /// # Errors
    ///
    /// Returns a bounded validation error for invalid text or language input.
    pub fn into_domain(self) -> Result<TtsRequest, RequestBodyError> {
        let text = SpeechText::new(self.text)?;
        let language = self
            .lang
            .map(|value| LanguageHint::from_str(&value))
            .transpose()?;
        TtsRequest::new(text, language).map_err(Into::into)
    }
}

/// Public speech-to-text JSON body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SttRequestBody {
    file_url: String,
    language: Option<String>,
}

impl SttRequestBody {
    /// Validates the public body into provider-independent domain input.
    ///
    /// # Errors
    ///
    /// Returns a bounded validation error for an invalid URL or language hint.
    pub fn into_domain(self) -> Result<SttRequest, RequestBodyError> {
        let source_url = Url::parse(&self.file_url)
            .map_err(|_| RequestBodyError::InvalidFileUrl)
            .and_then(|url| BriefcaseFileUrl::new(url).map_err(Into::into))?;
        let language = self
            .language
            .map(|value| LanguageHint::from_str(&value))
            .transpose()?;
        SttRequest::new(source_url, language).map_err(Into::into)
    }
}

/// Stable JSON representation of a successful TTS operation.
#[derive(Serialize)]
pub struct TtsResponseBody {
    request_id: String,
    file_url: String,
    temporary_url: String,
    media_type: &'static str,
    provider: &'static str,
    duration_ms: u64,
}

impl From<TtsResult> for TtsResponseBody {
    fn from(result: TtsResult) -> Self {
        Self {
            request_id: result.request_id.to_string(),
            file_url: result.audio.permanent_url.to_string(),
            temporary_url: result.audio.temporary_url.to_string(),
            media_type: result.output_format.as_mime_str(),
            provider: result.provider.as_str(),
            duration_ms: result.duration.as_millis(),
        }
    }
}

/// Stable JSON representation of a successful STT operation.
#[derive(Serialize)]
pub struct SttResponseBody {
    request_id: String,
    transcript: String,
    detected_language: Option<String>,
    provider: &'static str,
    duration_ms: Option<u64>,
}

impl From<SttResult> for SttResponseBody {
    fn from(result: SttResult) -> Self {
        Self {
            request_id: result.request_id.to_string(),
            transcript: result.transcript.as_str().to_owned(),
            detected_language: result
                .detected_language
                .map(|language| language.to_string()),
            provider: result.provider.as_str(),
            duration_ms: result
                .duration
                .map(super::super::domain::media::MediaDuration::as_millis),
        }
    }
}

/// Deterministic JSON representation returned by `/capabilities`.
#[derive(Debug, Serialize)]
pub struct CapabilitiesResponseBody {
    tts: TtsCapabilitiesBody,
    stt: SttCapabilitiesBody,
}

#[derive(Debug, Serialize)]
struct TtsCapabilitiesBody {
    output_format: &'static str,
    languages: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct SttCapabilitiesBody {
    languages: Vec<&'static str>,
    accepted_media_types: Vec<&'static str>,
}

impl From<Capabilities> for CapabilitiesResponseBody {
    fn from(capabilities: Capabilities) -> Self {
        Self {
            tts: TtsCapabilitiesBody {
                output_format: capabilities.tts.output_format.as_str(),
                languages: capabilities
                    .tts
                    .languages
                    .iter()
                    .map(|language| language.as_str())
                    .collect(),
            },
            stt: SttCapabilitiesBody {
                languages: capabilities
                    .stt
                    .languages
                    .iter()
                    .map(|language| language.as_str())
                    .collect(),
                accepted_media_types: capabilities
                    .stt
                    .accepted_media_types
                    .iter()
                    .copied()
                    .map(crate::domain::media::SourceMediaType::as_mime_str)
                    .collect(),
            },
        }
    }
}

/// Validation failure for a syntactically valid JSON body.
#[derive(Debug, Error)]
pub enum RequestBodyError {
    /// Text or capability validation failed.
    #[error(transparent)]
    Speech(#[from] SpeechValidationError),
    /// A language hint is not valid BCP 47.
    #[error(transparent)]
    Language(#[from] LanguageHintError),
    /// A media URL failed domain validation.
    #[error(transparent)]
    Media(#[from] MediaError),
    /// The supplied URL cannot be parsed.
    #[error("file_url must be a valid permanent Briefcase HTTPS URL")]
    InvalidFileUrl,
}

#[cfg(test)]
mod tests {
    use super::{CapabilitiesResponseBody, RequestBodyError, SttRequestBody, TtsRequestBody};
    use crate::domain::capabilities::Capabilities;

    #[test]
    fn request_debug_does_not_serialize_response_secrets() {
        let request = TtsRequestBody {
            text: "private words".to_owned(),
            lang: Some("en-US".to_owned()),
        };
        let domain = request.into_domain();

        assert!(matches!(domain, Ok(value) if !format!("{value:?}").contains("private words")));
    }

    #[test]
    fn rejects_non_briefcase_shaped_urls_at_the_domain_boundary() {
        let request = SttRequestBody {
            file_url: "file:///etc/passwd".to_owned(),
            language: None,
        };

        assert!(matches!(
            request.into_domain(),
            Err(RequestBodyError::Media(_))
        ));
    }

    #[test]
    fn capabilities_always_include_nested_fields() {
        let body = CapabilitiesResponseBody::from(Capabilities::current());
        let json = serde_json::to_value(body);

        assert!(matches!(json, Ok(value) if value["tts"]["output_format"] == "mp3"));
    }
}
