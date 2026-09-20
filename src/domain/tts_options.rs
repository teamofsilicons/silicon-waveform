//! Validated, provider-specific controls for individual synthesis requests.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::provider::ProviderName;

/// Controls for one selected TTS provider; omitted values retain voice defaults.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TtsProviderOptions {
    /// Gemini performance instructions and voice selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<GeminiTtsOptions>,
    /// `ElevenLabs` delivery controls and voice selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevenlabs: Option<ElevenLabsTtsOptions>,
    /// `OpenAI` model, voice and delivery controls.
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

/// `ElevenLabs` controls applied only to this synthesis request.
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

/// `OpenAI` controls for a selected speech model.
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

    /// Copies only the destination provider's controls across its boundary.
    #[must_use]
    pub fn for_provider(&self, provider: ProviderName) -> Self {
        Self {
            gemini: (provider == ProviderName::Gemini)
                .then(|| self.gemini.clone())
                .flatten(),
            elevenlabs: (provider == ProviderName::ElevenLabs)
                .then(|| self.elevenlabs.clone())
                .flatten(),
            openai: (provider == ProviderName::OpenAi)
                .then(|| self.openai.clone())
                .flatten(),
        }
    }

    /// Rejects controls for a different provider and invalid control values.
    ///
    /// # Errors
    /// Returns a stable validation reason without reproducing caller input.
    pub fn validate_for(&self, provider: ProviderName) -> Result<(), &'static str> {
        if !self.for_other_providers(provider).is_empty() {
            return Err(
                "provider_options must belong to the first selected provider. Set provider_order to match the controls.",
            );
        }
        if let Some(options) = &self.gemini {
            options.validate()?;
        }
        if let Some(options) = &self.elevenlabs {
            options.validate()?;
        }
        if let Some(options) = &self.openai {
            options.validate()?;
        }
        Ok(())
    }

    fn for_other_providers(&self, provider: ProviderName) -> Self {
        Self {
            gemini: (provider != ProviderName::Gemini)
                .then(|| self.gemini.clone())
                .flatten(),
            elevenlabs: (provider != ProviderName::ElevenLabs)
                .then(|| self.elevenlabs.clone())
                .flatten(),
            openai: (provider != ProviderName::OpenAi)
                .then(|| self.openai.clone())
                .flatten(),
        }
    }
}

impl GeminiTtsOptions {
    fn validate(&self) -> Result<(), &'static str> {
        if self.voice.as_ref().is_some_and(|v| {
            v.is_empty() || v.len() > 64 || !v.bytes().all(|b| b.is_ascii_alphabetic())
        }) {
            return Err(
                "Gemini voice must be a supported prebuilt name containing 1–64 ASCII letters.",
            );
        }
        if [
            &self.scene,
            &self.audio_profile,
            &self.director_notes,
            &self.sample_context,
        ]
        .into_iter()
        .any(|v| !valid_text(v.as_deref(), 4096))
        {
            return Err(
                "Each Gemini direction field must contain 1–4096 characters, with no NUL character.",
            );
        }
        Ok(())
    }

    /// Renders the provider's documented prompt sections without altering text.
    #[must_use]
    pub fn prompt(&self, transcript: &str) -> String {
        let mut prompt = String::new();
        for (heading, value) in [
            ("# AUDIO PROFILE", &self.audio_profile),
            ("## THE SCENE", &self.scene),
            ("### DIRECTOR'S NOTES", &self.director_notes),
            ("### SAMPLE CONTEXT", &self.sample_context),
        ] {
            if let Some(value) = value {
                prompt.push_str(heading);
                prompt.push('\n');
                prompt.push_str(value);
                prompt.push_str("\n\n");
            }
        }
        if !prompt.is_empty() {
            prompt.push_str("#### TRANSCRIPT\n");
        }
        prompt.push_str(transcript);
        prompt
    }
}

impl ElevenLabsTtsOptions {
    fn validate(&self) -> Result<(), &'static str> {
        if [&self.voice_id, &self.model_id]
            .into_iter()
            .any(|v| v.as_ref().is_some_and(|v| !valid_identifier(v, 128)))
        {
            return Err(
                "ElevenLabs voice_id and model_id must contain 1–128 letters, digits, underscores or hyphens.",
            );
        }
        if [self.stability, self.similarity_boost, self.style]
            .into_iter()
            .any(|v| !valid_range(v, 0.0, 1.0))
            || !valid_range(self.speed, 0.7, 1.2)
        {
            return Err(
                "ElevenLabs stability, similarity_boost and style must be 0–1; speed must be 0.7–1.2.",
            );
        }
        if !valid_text(self.previous_text.as_deref(), 4096)
            || !valid_text(self.next_text.as_deref(), 4096)
        {
            return Err(
                "ElevenLabs previous_text and next_text must each contain 1–4096 characters, with no NUL character.",
            );
        }
        if self
            .apply_text_normalization
            .as_deref()
            .is_some_and(|v| !matches!(v, "auto" | "on" | "off"))
        {
            return Err("ElevenLabs apply_text_normalization must be auto, on or off.");
        }
        Ok(())
    }
}

impl OpenAiTtsOptions {
    fn validate(&self) -> Result<(), &'static str> {
        if self
            .model
            .as_deref()
            .is_some_and(|v| !matches!(v, "tts-1" | "tts-1-hd" | "gpt-4o-mini-tts"))
        {
            return Err("OpenAI model must be tts-1, tts-1-hd or gpt-4o-mini-tts.");
        }
        if self.instructions.is_some() && self.model.as_deref() != Some("gpt-4o-mini-tts") {
            return Err(
                "OpenAI instructions require model gpt-4o-mini-tts explicitly; tts-1 and tts-1-hd do not support them.",
            );
        }
        if !valid_text(self.instructions.as_deref(), 4096) || !valid_range(self.speed, 0.25, 4.0) {
            return Err(
                "OpenAI instructions must contain 1–4096 characters without NUL; speed must be 0.25–4.",
            );
        }
        if let Some(voice) = self.voice.as_deref() {
            let legacy = matches!(
                voice,
                "alloy" | "ash" | "coral" | "echo" | "fable" | "onyx" | "nova" | "sage" | "shimmer"
            );
            let modern = self.model.as_deref() == Some("gpt-4o-mini-tts")
                && matches!(voice, "ballad" | "verse" | "marin" | "cedar");
            if !legacy && !modern {
                return Err(
                    "OpenAI voice is not supported by the selected model; newer voices require gpt-4o-mini-tts explicitly.",
                );
            }
        }
        Ok(())
    }
}

fn valid_identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn valid_text(value: Option<&str>, limit: usize) -> bool {
    value.is_none_or(|v| !v.trim().is_empty() && v.chars().count() <= limit && !v.contains('\0'))
}

fn valid_range(value: Option<f64>, min: f64, max: f64) -> bool {
    value.is_none_or(|v| v.is_finite() && (min..=max).contains(&v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_controls_validate_boundaries_and_never_cross_providers()
    -> Result<(), Box<dyn std::error::Error>> {
        let options: TtsProviderOptions = serde_json::from_value(
            json!({"elevenlabs":{"stability":0.0,"similarity_boost":1.0,"style":0.4,"speed":1.2,"seed":u32::MAX}}),
        )?;
        assert_eq!(options.validate_for(ProviderName::ElevenLabs), Ok(()));
        assert_eq!(
            options.validate_for(ProviderName::Gemini),
            Err(
                "provider_options must belong to the first selected provider. Set provider_order to match the controls."
            )
        );
        assert!(options.for_provider(ProviderName::OpenAi).is_empty());
        for settings in [
            json!({"speed":1.21}),
            json!({"stability":-0.1}),
            json!({"voice_id":"../escape"}),
            json!({"apply_text_normalization":"maybe"}),
        ] {
            let options: TtsProviderOptions =
                serde_json::from_value(json!({"elevenlabs":settings}))?;
            assert!(options.validate_for(ProviderName::ElevenLabs).is_err());
        }
        assert!(
            serde_json::from_value::<TtsProviderOptions>(json!({"gemini":{"unsupported":true}}))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn openai_instructions_and_voices_require_the_correct_model()
    -> Result<(), Box<dyn std::error::Error>> {
        for model in [None, Some("tts-1"), Some("tts-1-hd")] {
            let options = TtsProviderOptions {
                openai: Some(OpenAiTtsOptions {
                    model: model.map(str::to_owned),
                    instructions: Some("Speak softly".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            assert!(options.validate_for(ProviderName::OpenAi).is_err());
        }
        let options: TtsProviderOptions = serde_json::from_value(
            json!({"openai":{"model":"gpt-4o-mini-tts","voice":"marin","instructions":"Speak softly","speed":0.25}}),
        )?;
        assert_eq!(options.validate_for(ProviderName::OpenAi), Ok(()));
        assert!(!format!("{options:?}").contains("Speak softly"));
        Ok(())
    }

    #[test]
    fn gemini_directions_are_distinct_from_the_transcript() {
        let options = GeminiTtsOptions {
            scene: Some("A quiet library".into()),
            director_notes: Some("Speak softly".into()),
            ..Default::default()
        };
        assert_eq!(
            options.prompt("Hello."),
            "## THE SCENE\nA quiet library\n\n### DIRECTOR'S NOTES\nSpeak softly\n\n#### TRANSCRIPT\nHello."
        );
        assert_eq!(GeminiTtsOptions::default().prompt("Hello."), "Hello.");
        assert!(!format!("{options:?}").contains("quiet library"));
        assert!(
            GeminiTtsOptions {
                scene: Some("a".repeat(4097)),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
