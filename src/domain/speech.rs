//! Validated TTS and STT requests, provider inputs, and normalized results.

use std::fmt;

use thiserror::Error;

use super::{
    capabilities::{Capabilities, TtsOutputFormat},
    identity::RequestId,
    language::LanguageHint,
    media::{BriefcaseFileUrl, MediaDuration, SourceMedia, StoredAudio},
    provider::ProviderName,
};

/// Maximum TTS input length in Unicode scalar values.
pub const MAX_TTS_TEXT_CHARACTERS: usize = 4_096;

/// Maximum normalized transcript size accepted from a provider.
pub const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;

/// Validated private TTS input text. Debug output never reveals content.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct SpeechText(String);

impl SpeechText {
    /// Rejects blank or over-limit synthesis text.
    ///
    /// # Errors
    ///
    /// Returns [`SpeechValidationError::BlankText`] or
    /// [`SpeechValidationError::TextTooLong`] when the public TTS limits are
    /// violated.
    pub fn new(value: String) -> Result<Self, SpeechValidationError> {
        if value.trim().is_empty() {
            return Err(SpeechValidationError::BlankText);
        }
        let character_count = value.chars().count();
        if character_count > MAX_TTS_TEXT_CHARACTERS {
            return Err(SpeechValidationError::TextTooLong {
                character_limit: MAX_TTS_TEXT_CHARACTERS,
            });
        }
        Ok(Self(value))
    }

    /// Returns the text for explicit use at a provider boundary.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the Unicode scalar count used by quotas and telemetry.
    #[must_use]
    pub fn character_count(&self) -> usize {
        self.0.chars().count()
    }
}

impl fmt::Debug for SpeechText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpeechText")
            .field("character_count", &self.character_count())
            .finish_non_exhaustive()
    }
}

/// Validated public text-to-speech request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TtsRequest {
    /// Private source text.
    pub text: SpeechText,
    /// Optional supported BCP 47 hint.
    pub language: Option<LanguageHint>,
}

impl TtsRequest {
    /// Constructs a request and validates its capability-level language hint.
    ///
    /// # Errors
    ///
    /// Returns [`SpeechValidationError::UnsupportedTtsLanguage`] when the
    /// hint's primary language is outside the stable TTS capability set.
    pub fn new(
        text: SpeechText,
        language: Option<LanguageHint>,
    ) -> Result<Self, SpeechValidationError> {
        if language
            .as_ref()
            .is_some_and(|hint| !Capabilities::current().tts.supports(hint))
        {
            return Err(SpeechValidationError::UnsupportedTtsLanguage);
        }
        Ok(Self { text, language })
    }
}

/// Provider-ready TTS input with language policy already applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TtsProviderRequest {
    /// Private text to synthesize.
    pub text: SpeechText,
    /// Hint present only for a provider allowed to receive it.
    pub language: Option<LanguageHint>,
}

impl TtsProviderRequest {
    /// Applies provider-independent language-routing policy.
    #[must_use]
    pub fn for_provider(request: &TtsRequest, provider: ProviderName) -> Self {
        Self {
            text: request.text.clone(),
            language: provider
                .accepts_tts_language_hint()
                .then(|| request.language.clone())
                .flatten(),
        }
    }
}

/// Stable normalized TTS response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TtsResult {
    /// Original request correlation ID.
    pub request_id: RequestId,
    /// Durable and temporary Briefcase references.
    pub audio: StoredAudio,
    /// Provider that produced the successful artifact.
    pub provider: ProviderName,
    /// Guaranteed normalized encoding.
    pub output_format: TtsOutputFormat,
    /// Measured decoded duration.
    pub duration: MediaDuration,
}

impl TtsResult {
    /// Constructs the normalized response shape.
    #[must_use]
    pub const fn new(
        request_id: RequestId,
        audio: StoredAudio,
        provider: ProviderName,
        duration: MediaDuration,
    ) -> Self {
        Self {
            request_id,
            audio,
            provider,
            output_format: TtsOutputFormat::Mp3,
            duration,
        }
    }
}

/// Validated public speech-to-text request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SttRequest {
    /// Stable authenticated Briefcase source URL.
    pub source_url: BriefcaseFileUrl,
    /// Optional supported BCP 47 hint.
    pub language: Option<LanguageHint>,
}

impl SttRequest {
    /// Constructs a request and validates its capability-level language hint.
    ///
    /// # Errors
    ///
    /// Returns [`SpeechValidationError::UnsupportedSttLanguage`] when the
    /// hint's primary language is outside the stable STT capability set.
    pub fn new(
        source_url: BriefcaseFileUrl,
        language: Option<LanguageHint>,
    ) -> Result<Self, SpeechValidationError> {
        if language
            .as_ref()
            .is_some_and(|hint| !Capabilities::current().stt.supports(hint))
        {
            return Err(SpeechValidationError::UnsupportedSttLanguage);
        }
        Ok(Self {
            source_url,
            language,
        })
    }
}

/// Provider-ready STT input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SttProviderRequest {
    /// Authorized source media loaded through Briefcase.
    pub media: SourceMedia,
    /// Optional best-effort BCP 47 hint passed to every STT adapter.
    pub language: Option<LanguageHint>,
}

/// Provider transcript before the application adds request metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SttProviderResult {
    /// Normalized transcript text.
    pub transcript: Transcript,
    /// Provider-reported language; never inferred from the request hint.
    pub detected_language: Option<LanguageHint>,
    /// Provider- or media-inspector-reported source duration.
    pub duration: Option<MediaDuration>,
}

/// Private transcript text. Silence may legitimately produce an empty string.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct Transcript(String);

impl Transcript {
    /// Constructs a bounded transcript.
    ///
    /// # Errors
    ///
    /// Returns [`SpeechValidationError::TranscriptTooLong`] when a provider
    /// response exceeds the transcript byte ceiling.
    pub fn new(value: String) -> Result<Self, SpeechValidationError> {
        if value.len() > MAX_TRANSCRIPT_BYTES {
            return Err(SpeechValidationError::TranscriptTooLong {
                byte_limit: MAX_TRANSCRIPT_BYTES,
            });
        }
        Ok(Self(value))
    }

    /// Returns the transcript for explicit response-boundary translation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the transcript byte length for bounded telemetry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether no speech was transcribed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Transcript {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Transcript")
            .field("byte_length", &self.len())
            .finish_non_exhaustive()
    }
}

/// Stable normalized STT response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SttResult {
    /// Original request correlation ID.
    pub request_id: RequestId,
    /// Private normalized transcript.
    pub transcript: Transcript,
    /// Provider-reported language, if reliable.
    pub detected_language: Option<LanguageHint>,
    /// Provider that produced the successful transcript.
    pub provider: ProviderName,
    /// Source duration, if reliably available.
    pub duration: Option<MediaDuration>,
}

impl SttResult {
    /// Adds request and provider metadata to a successful provider result.
    #[must_use]
    pub fn from_provider(
        request_id: RequestId,
        provider: ProviderName,
        result: SttProviderResult,
    ) -> Self {
        Self {
            request_id,
            transcript: result.transcript,
            detected_language: result.detected_language,
            provider,
            duration: result.duration,
        }
    }
}

/// Speech request or provider-output validation failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SpeechValidationError {
    /// Whitespace-only synthesis has no meaningful output.
    #[error("text must contain at least one non-whitespace character")]
    BlankText,
    /// Text exceeded the synchronous endpoint limit.
    #[error("text exceeds the {character_limit}-character limit")]
    TextTooLong {
        /// Enforced Unicode scalar limit.
        character_limit: usize,
    },
    /// The hint is valid BCP 47 but outside the TTS capability set.
    #[error("language hint is not accepted for text-to-speech")]
    UnsupportedTtsLanguage,
    /// The hint is valid BCP 47 but outside the STT capability set.
    #[error("language hint is not accepted for speech-to-text")]
    UnsupportedSttLanguage,
    /// A provider returned an unreasonably large transcript.
    #[error("transcript exceeds the {byte_limit}-byte limit")]
    TranscriptTooLong {
        /// Enforced byte limit.
        byte_limit: usize,
    },
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::{
        MAX_TTS_TEXT_CHARACTERS, SpeechText, SpeechValidationError, Transcript, TtsProviderRequest,
        TtsRequest,
    };
    use crate::domain::{language::LanguageHint, provider::ProviderName};

    #[test]
    fn text_limit_counts_unicode_scalars_not_utf8_bytes() {
        let text = "🦀".repeat(MAX_TTS_TEXT_CHARACTERS);
        assert!(SpeechText::new(text).is_ok());
        assert!(matches!(
            SpeechText::new("🦀".repeat(MAX_TTS_TEXT_CHARACTERS + 1)),
            Err(SpeechValidationError::TextTooLong { .. })
        ));
    }

    #[test]
    fn text_and_transcript_debug_are_content_free() {
        let text = SpeechText::new("private synthesis text".to_owned());
        let transcript = Transcript::new("private transcript".to_owned());

        assert!(matches!(text, Ok(value) if !format!("{value:?}").contains("private")));
        assert!(matches!(transcript, Ok(value) if !format!("{value:?}").contains("private")));
    }

    #[test]
    fn whitespace_only_text_is_rejected() {
        assert_eq!(
            SpeechText::new(" \n\t".to_owned()),
            Err(SpeechValidationError::BlankText)
        );
    }

    #[test]
    fn tts_language_is_routed_only_to_gemini() {
        let text = SpeechText::new("hello".to_owned());
        let language = LanguageHint::from_str("en-US");
        let request = text.and_then(|text| {
            language
                .map_err(|_| SpeechValidationError::UnsupportedTtsLanguage)
                .and_then(|language| TtsRequest::new(text, Some(language)))
        });

        assert!(matches!(
            &request,
            Ok(request)
                if TtsProviderRequest::for_provider(request, ProviderName::Gemini)
                    .language
                    .is_some()
        ));
        assert!(matches!(
            &request,
            Ok(request)
                if TtsProviderRequest::for_provider(request, ProviderName::ElevenLabs)
                    .language
                    .is_none()
        ));
    }

    #[test]
    fn operation_specific_language_capabilities_are_enforced() {
        let text = SpeechText::new("hello".to_owned());
        let mandarin = LanguageHint::from_str("cmn-Hans");
        assert!(matches!(
            (text, mandarin),
            (Ok(text), Ok(language))
                if TtsRequest::new(text.clone(), Some(language.clone())).is_ok()
        ));
    }
}
