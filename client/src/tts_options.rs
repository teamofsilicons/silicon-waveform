//! Typed provider controls. The server validates model support and value ranges.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Controls for one selected TTS provider; omitted values retain voice defaults.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TtsProviderOptions {
    /// Gemini performance instructions and voice selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<GeminiTtsOptions>,
    /// ElevenLabs delivery controls and voice selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevenlabs: Option<ElevenLabsTtsOptions>,
    /// OpenAI model, voice and delivery controls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai: Option<OpenAiTtsOptions>,
}

/// Gemini instructions become labeled prompt sections before the transcript.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GeminiTtsOptions {
    /// Prebuilt Gemini voice name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Physical setting and emotional atmosphere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    /// Character identity and vocal persona.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_profile: Option<String>,
    /// Performance guidance such as style, accent and pacing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub director_notes: Option<String>,
    /// Context that establishes how the performance begins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_context: Option<String>,
}

/// ElevenLabs controls applied only to this synthesis request.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ElevenLabsTtsOptions {
    /// Voice accessible to the selected provider key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    /// Provider TTS model identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Delivery consistency, from 0 through 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stability: Option<f64>,
    /// Similarity to the source voice, from 0 through 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_boost: Option<f64>,
    /// Style exaggeration, from 0 through 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<f64>,
    /// Speaking rate, from 0.7 through 1.2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    /// Enhance similarity to the selected speaker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_speaker_boost: Option<bool>,
    /// Best-effort deterministic sampling seed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    /// Preceding transcript for speech continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_text: Option<String>,
    /// Following transcript for speech continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_text: Option<String>,
    /// Text normalization policy: auto, on, or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_text_normalization: Option<String>,
}

/// OpenAI controls for a selected speech model.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenAiTtsOptions {
    /// Built-in voice supported by the selected model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// tts-1, tts-1-hd, or gpt-4o-mini-tts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Vocal performance instructions, requiring gpt-4o-mini-tts explicitly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Speaking rate, from 0.25 through 4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

// Debug output must never include caller-authored prompts or transcript context.
macro_rules! redacted_debug {
    ($name:ident) => {
        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), " { [REDACTED] }"))
            }
        }
    };
}
redacted_debug!(GeminiTtsOptions);
redacted_debug!(ElevenLabsTtsOptions);
redacted_debug!(OpenAiTtsOptions);

impl TtsProviderOptions {
    /// Whether no provider controls were supplied, including empty objects.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gemini
            .as_ref()
            .is_none_or(|v| *v == GeminiTtsOptions::default())
            && self
                .elevenlabs
                .as_ref()
                .is_none_or(|v| *v == ElevenLabsTtsOptions::default())
            && self
                .openai
                .as_ref()
                .is_none_or(|v| *v == OpenAiTtsOptions::default())
    }
}
