//! Public JSON request and response translation.

use std::str::FromStr as _;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::domain::{
    capabilities::Capabilities,
    language::{LanguageHint, LanguageHintError},
    media::{BriefcaseFileUrl, MediaError},
    provider::ProviderName,
    speech::{SpeechText, SpeechValidationError, SttRequest, SttResult, TtsRequest, TtsResult},
};

/// Public text-to-speech JSON body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsRequestBody {
    text: String,
    voice_profile: Option<String>,
    lang: Option<String>,
    /// Optional provider prefix, completed against account defaults by the
    /// delivery layer.
    provider_order: Option<Vec<ProviderName>>,
}

impl TtsRequestBody {
    /// Validates the public body into provider-independent domain input.
    ///
    /// # Errors
    ///
    /// Returns a bounded validation error for invalid text or language input.
    pub fn into_domain(self) -> Result<TtsRequest, RequestBodyError> {
        let provider_order = self.provider_order;
        let text = SpeechText::new(self.text)?;
        let language = self
            .lang
            .map(|value| LanguageHint::from_str(&value))
            .transpose()?;
        if self
            .voice_profile
            .as_deref()
            .is_some_and(|v| !crate::domain::voice::valid_profile_id(v))
        {
            return Err(RequestBodyError::InvalidVoiceProfile);
        }
        let mut request = TtsRequest::new(text, language)?;
        request.voice_profile = self.voice_profile;
        Ok(match provider_order {
            Some(order) => request.with_provider_order(order),
            None => request,
        })
    }
}

/// Public speech-to-text JSON body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SttRequestBody {
    file_url: String,
    language: Option<String>,
    /// Optional provider prefix, completed against account defaults by the
    /// delivery layer.
    provider_order: Option<Vec<ProviderName>>,
}

impl SttRequestBody {
    /// Validates the public body into provider-independent domain input.
    ///
    /// # Errors
    ///
    /// Returns a bounded validation error for an invalid URL or language hint.
    pub fn into_domain(self) -> Result<SttRequest, RequestBodyError> {
        let provider_order = self.provider_order;
        let source_url = Url::parse(&self.file_url)
            .map_err(|_| RequestBodyError::InvalidFileUrl)
            .and_then(|url| BriefcaseFileUrl::new(url).map_err(Into::into))?;
        let language = self
            .language
            .map(|value| LanguageHint::from_str(&value))
            .transpose()?;
        let request = SttRequest::new(source_url, language)?;
        Ok(match provider_order {
            Some(order) => request.with_provider_order(order),
            None => request,
        })
    }
}

/// Stable JSON representation of a successful TTS operation.
#[derive(Serialize)]
pub struct TtsResponseBody {
    voice_profile: Option<crate::domain::voice::VoiceProfileRef>,
    request_id: String,
    file_url: String,
    temporary_url: Option<String>,
    media_type: &'static str,
    provider: &'static str,
    duration_ms: u64,
}

impl From<TtsResult> for TtsResponseBody {
    fn from(result: TtsResult) -> Self {
        Self {
            voice_profile: result.voice_profile,
            request_id: result.request_id.to_string(),
            file_url: result.audio.permanent_url.to_string(),
            temporary_url: result.audio.temporary_url.map(|url| url.to_string()),
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
    /// A voice profile must be a stable lowercase catalog identifier.
    #[error("invalid voice profile")]
    InvalidVoiceProfile,
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
    fn profile_request_is_optional_and_rejects_raw_provider_controls()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = serde_json::from_value::<TtsRequestBody>(
            serde_json::json!({"text":"hi","voice_profile":"puck"}),
        )?
        .into_domain()?;
        assert_eq!(request.voice_profile.as_deref(), Some("puck"));
        for id in ["", "Kore", "../kore", "two words"] {
            assert!(
                serde_json::from_value::<TtsRequestBody>(
                    serde_json::json!({"text":"hi","voice_profile":id})
                )?
                .into_domain()
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<TtsRequestBody>(
                serde_json::json!({"text":"hi","voice_id":"secret-profile"})
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn request_debug_does_not_serialize_response_secrets() {
        let request = TtsRequestBody {
            text: "private words".to_owned(),
            voice_profile: None,
            lang: Some("en-US".to_owned()),
            provider_order: None,
        };
        let domain = request.into_domain();

        assert!(matches!(domain, Ok(value) if !format!("{value:?}").contains("private words")));
    }

    #[test]
    fn rejects_non_briefcase_shaped_urls_at_the_domain_boundary() {
        let request = SttRequestBody {
            file_url: "file:///etc/passwd".to_owned(),
            language: None,
            provider_order: None,
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
