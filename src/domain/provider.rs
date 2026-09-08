//! Provider identities, fallback policy, and safe failure classification.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Canonical provider identifier exposed in normalized responses.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderName {
    /// Google Gemini speech services.
    #[serde(rename = "gemini")]
    Gemini,
    /// `ElevenLabs` speech synthesis.
    #[serde(rename = "elevenlabs")]
    ElevenLabs,
    /// `OpenAI` speech services.
    #[serde(rename = "openai")]
    OpenAi,
    /// Deepgram speech transcription.
    #[serde(rename = "deepgram")]
    Deepgram,
}

impl ProviderName {
    /// Canonical public provider name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::ElevenLabs => "elevenlabs",
            Self::OpenAi => "openai",
            Self::Deepgram => "deepgram",
        }
    }

    /// Whether the TTS adapter may receive the public language hint.
    #[must_use]
    pub const fn accepts_tts_language_hint(self) -> bool {
        matches!(self, Self::Gemini)
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Required text-to-speech provider order.
pub const TTS_PROVIDER_CHAIN: [ProviderName; 3] = [
    ProviderName::Gemini,
    ProviderName::ElevenLabs,
    ProviderName::OpenAi,
];

/// Required speech-to-text provider order.
pub const STT_PROVIDER_CHAIN: [ProviderName; 3] = [
    ProviderName::Gemini,
    ProviderName::OpenAi,
    ProviderName::Deepgram,
];

/// Completes a caller's preferred prefix against a configured provider order.
///
/// The preference may name one or more providers, but every name must be in
/// the operation's chain and may occur only once.  This keeps request-level
/// overrides bounded while allowing account defaults to supply the remainder.
#[must_use]
pub fn resolve_order(
    requested: &[ProviderName],
    base: &[ProviderName],
) -> Option<Vec<ProviderName>> {
    if requested.len() > base.len() {
        return None;
    }
    let mut result = Vec::with_capacity(base.len());
    for provider in requested {
        if !base.contains(provider) || result.contains(provider) {
            return None;
        }
        result.push(*provider);
    }
    for provider in base {
        if !result.contains(provider) {
            result.push(*provider);
        }
    }
    Some(result)
}

/// Safe, provider-independent provider failure category.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProviderFailureKind {
    /// Provider concurrency budget was exhausted.
    Saturated,
    /// Attempt deadline elapsed.
    Timeout,
    /// Transport could not complete.
    Network,
    /// Provider asked the caller to reduce request frequency.
    RateLimited,
    /// Provider service reported temporary unavailability.
    Unavailable,
    /// Provider credentials or configuration were rejected.
    Authentication,
    /// Provider rejected an otherwise domain-valid request.
    RejectedRequest,
    /// Provider returned malformed, empty, or unsupported content.
    InvalidResponse,
}

impl ProviderFailureKind {
    /// Stable privacy-safe label for structured attempt telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Saturated => "saturated",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "unavailable",
            Self::Authentication => "authentication",
            Self::RejectedRequest => "rejected_request",
            Self::InvalidResponse => "invalid_response",
        }
    }
}

/// Redacted provider failure used to drive bounded fallback.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("{provider} {kind:?} failure")]
pub struct ProviderError {
    /// Provider that failed.
    pub provider: ProviderName,
    /// Stable failure category; private response bodies are never retained.
    pub kind: ProviderFailureKind,
}

impl ProviderError {
    /// Constructs a redacted failure.
    #[must_use]
    pub const fn new(provider: ProviderName, kind: ProviderFailureKind) -> Self {
        Self { provider, kind }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderError, ProviderFailureKind, ProviderName, STT_PROVIDER_CHAIN, TTS_PROVIDER_CHAIN,
    };

    #[test]
    fn provider_chains_match_product_order() {
        assert_eq!(
            TTS_PROVIDER_CHAIN,
            [
                ProviderName::Gemini,
                ProviderName::ElevenLabs,
                ProviderName::OpenAi
            ]
        );
        assert_eq!(
            STT_PROVIDER_CHAIN,
            [
                ProviderName::Gemini,
                ProviderName::OpenAi,
                ProviderName::Deepgram
            ]
        );
    }

    #[test]
    fn only_gemini_receives_tts_language_hints() {
        assert!(ProviderName::Gemini.accepts_tts_language_hint());
        assert!(!ProviderName::ElevenLabs.accepts_tts_language_hint());
        assert!(!ProviderName::OpenAi.accepts_tts_language_hint());
    }

    #[test]
    fn provider_errors_contain_no_private_detail() {
        let error = ProviderError::new(ProviderName::OpenAi, ProviderFailureKind::InvalidResponse);
        assert_eq!(error.to_string(), "openai InvalidResponse failure");
        assert_eq!(error.kind.as_str(), "invalid_response");
    }

    #[test]
    fn preference_prefix_is_completed_against_account_order() {
        let account = [
            ProviderName::OpenAi,
            ProviderName::Gemini,
            ProviderName::ElevenLabs,
        ];
        assert_eq!(
            super::resolve_order(&[ProviderName::ElevenLabs], &account),
            Some(vec![
                ProviderName::ElevenLabs,
                ProviderName::OpenAi,
                ProviderName::Gemini,
            ])
        );
        assert_eq!(
            super::resolve_order(&[ProviderName::OpenAi, ProviderName::OpenAi], &account),
            None
        );
    }
}
