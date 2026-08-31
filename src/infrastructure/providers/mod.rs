//! Bounded HTTP adapters for external speech providers.

mod common;
mod deepgram;
mod elevenlabs;
mod gemini;
mod openai;

pub use common::{
    PROVIDER_RETRY_AFTER_CAP, PROVIDER_RETRY_BASE_DELAY, PROVIDER_RETRY_MAX_JITTER,
    ProviderHttpConfig, TransientRetry,
};
pub use deepgram::{DEEPGRAM_MODEL, DeepgramConfig, DeepgramProvider};
pub use elevenlabs::{ELEVENLABS_MODEL, ElevenLabsConfig, ElevenLabsProvider};
pub use gemini::{GEMINI_STT_MODEL, GEMINI_TTS_MODEL, GeminiConfig, GeminiProvider};
pub use openai::{OPENAI_STT_MODEL, OPENAI_TTS_MODEL, OpenAiConfig, OpenAiProvider};

use std::str::FromStr;

use crate::domain::{
    language::LanguageHint,
    media::{AudioArtifact, MediaDuration, PcmSpecification, ProviderAudioFormat, SourceMediaType},
    provider::{ProviderError as DomainProviderError, ProviderFailureKind, ProviderName},
    speech::{SttProviderResult, Transcript},
};

use common::{BoundedResponse, ProviderError, ProviderErrorKind, ProviderRuntime};

fn map_provider_error(provider: ProviderName, error: ProviderError) -> DomainProviderError {
    let kind = match error.kind {
        ProviderErrorKind::Saturated => ProviderFailureKind::Saturated,
        ProviderErrorKind::Timeout => ProviderFailureKind::Timeout,
        ProviderErrorKind::Network => ProviderFailureKind::Network,
        ProviderErrorKind::RateLimited => ProviderFailureKind::RateLimited,
        ProviderErrorKind::Rejected => ProviderFailureKind::RejectedRequest,
        ProviderErrorKind::Configuration => ProviderFailureKind::Authentication,
        ProviderErrorKind::InvalidResponse => ProviderFailureKind::InvalidResponse,
        ProviderErrorKind::Unavailable => ProviderFailureKind::Unavailable,
    };
    DomainProviderError::new(provider, kind)
}

fn audio_artifact(bytes: bytes::Bytes, media_type: &str) -> Result<AudioArtifact, ProviderError> {
    let essence = media_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default();
    let format = if essence.eq_ignore_ascii_case("audio/mpeg")
        || essence.eq_ignore_ascii_case("audio/mp3")
    {
        ProviderAudioFormat::Mp3
    } else if essence.eq_ignore_ascii_case("audio/wav")
        || essence.eq_ignore_ascii_case("audio/x-wav")
    {
        ProviderAudioFormat::Wav
    } else if essence.eq_ignore_ascii_case("audio/l16") {
        let rate = media_parameter(media_type, "rate")
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(ProviderError::invalid_response)?;
        let channels = media_parameter(media_type, "channels")
            .map_or(Some(1), |value| value.parse::<u32>().ok())
            .ok_or_else(ProviderError::invalid_response)?;
        let specification = PcmSpecification::new(rate, channels, 16)
            .map_err(|_| ProviderError::invalid_response())?;
        ProviderAudioFormat::LinearPcm(specification)
    } else {
        return Err(ProviderError::invalid_response());
    };
    AudioArtifact::new(format, bytes).map_err(|_| ProviderError::invalid_response())
}

fn mp3_response_artifact(response: BoundedResponse) -> Result<AudioArtifact, ProviderError> {
    let content_type = response
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ProviderError::invalid_response)?;
    if !matches!(
        content_type
            .split(';')
            .next()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("audio/mpeg" | "audio/mp3")
    ) || !crate::infrastructure::audio::validate_mp3_bytes(&response.body)
    {
        return Err(ProviderError::invalid_response());
    }
    audio_artifact(response.body, "audio/mpeg")
}

fn transcription_result(
    text: String,
    detected_language: Option<String>,
    duration_ms: Option<u64>,
) -> Result<SttProviderResult, ProviderError> {
    let transcript = Transcript::new(text).map_err(|_| ProviderError::invalid_response())?;
    let detected_language =
        detected_language.and_then(|language| LanguageHint::from_str(&language).ok());
    Ok(SttProviderResult {
        transcript,
        detected_language,
        duration: duration_ms.map(MediaDuration::from_millis),
    })
}

fn media_filename(media_type: SourceMediaType) -> &'static str {
    match media_type {
        SourceMediaType::Mpeg => "audio.mp3",
        SourceMediaType::Wav => "audio.wav",
        SourceMediaType::Flac => "audio.flac",
        SourceMediaType::Ogg => "audio.ogg",
        SourceMediaType::WebM => "audio.webm",
        SourceMediaType::Mp4 => "audio.mp4",
        SourceMediaType::Aac => "audio.aac",
        SourceMediaType::M4a => "audio.m4a",
        SourceMediaType::Mp4Video => "media.mp4",
        SourceMediaType::MpegVideo => "media.mpeg",
        SourceMediaType::WebMVideo => "media.webm",
    }
}

fn media_parameter<'a>(media_type: &'a str, expected: &str) -> Option<&'a str> {
    media_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case(expected)
            .then_some(value.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_mp3_frame() -> bytes::Bytes {
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        frame.into()
    }

    #[test]
    fn binary_mp3_requires_matching_content_type_and_framing() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("audio/mpeg"),
        );
        assert!(
            mp3_response_artifact(BoundedResponse {
                headers: headers.clone(),
                body: valid_mp3_frame(),
            })
            .is_ok()
        );

        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        assert!(
            mp3_response_artifact(BoundedResponse {
                headers,
                body: valid_mp3_frame(),
            })
            .is_err()
        );
    }
}
