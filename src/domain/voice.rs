//! Named voice profiles and provider-specific selections.

use serde::{Deserialize, Serialize};

/// Full, versioned mapping resolved once per synthesis attempt.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VoiceProfile {
    /// Stable public slug.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Delivery character, based on the Gemini voice description.
    pub description: String,
    /// Incremented whenever a mapping is revised.
    pub revision: i32,
    /// False until the cross-provider mapping has been reviewed by listening.
    pub auditioned: bool,
    /// Gemini prebuilt voice name.
    pub gemini_voice: String,
    /// `OpenAI` tts-1 voice name.
    pub openai_voice: String,
    /// `ElevenLabs` voice and delivery parameters.
    pub elevenlabs: ElevenLabsVoice,
}

/// `ElevenLabs` mapping for the fixed multilingual-v2 fallback model.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ElevenLabsVoice {
    /// Voice accessible to the active provider API key.
    pub voice_id: String,
    /// Provider model identifier.
    pub model_id: String,
    /// All delivery settings are explicit for reproducibility.
    pub voice_settings: ElevenLabsVoiceSettings,
}

/// Delivery controls supported by multilingual v2.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ElevenLabsVoiceSettings {
    /// Delivery consistency, 0 to 1.
    pub stability: f64,
    /// Fidelity to the selected voice, 0 to 1.
    pub similarity_boost: f64,
    /// Style exaggeration, 0 to 1.
    pub style: f64,
    /// Enhance similarity to the selected speaker.
    pub use_speaker_boost: bool,
    /// Speaking rate, 0.7 to 1.2.
    pub speed: f64,
}

/// Only the selected provider's configuration crosses its boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum ProviderVoice {
    /// Gemini prebuilt voice.
    Gemini(String),
    /// `OpenAI` tts-1 voice.
    OpenAi(String),
    /// `ElevenLabs` voice and delivery settings.
    ElevenLabs(ElevenLabsVoice),
}

/// Validates the stable profile slug without consulting provider catalogs.
#[must_use]
pub fn valid_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl VoiceProfile {
    /// Rejects malformed stored mappings before contacting any speech provider.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let settings = &self.elevenlabs.voice_settings;
        valid_profile_id(&self.id)
            && self.revision > 0
            && !self.gemini_voice.is_empty()
            && self.gemini_voice.len() <= 64
            && self.gemini_voice.bytes().all(|b| b.is_ascii_alphabetic())
            && [
                "alloy", "ash", "coral", "echo", "fable", "onyx", "nova", "sage", "shimmer",
            ]
            .contains(&self.openai_voice.as_str())
            && self.elevenlabs.model_id == "eleven_multilingual_v2"
            && !self.elevenlabs.voice_id.is_empty()
            && self.elevenlabs.voice_id.len() <= 128
            && self
                .elevenlabs
                .voice_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && [
                settings.stability,
                settings.similarity_boost,
                settings.style,
            ]
            .into_iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(&v))
            && settings.speed.is_finite()
            && (0.7..=1.2).contains(&settings.speed)
    }

    /// Selects this profile's mapping independently of provider order.
    #[must_use]
    pub fn for_provider(&self, provider: super::provider::ProviderName) -> Option<ProviderVoice> {
        use super::provider::ProviderName;
        match provider {
            ProviderName::Gemini => Some(ProviderVoice::Gemini(self.gemini_voice.clone())),
            ProviderName::OpenAi => Some(ProviderVoice::OpenAi(self.openai_voice.clone())),
            ProviderName::ElevenLabs => Some(ProviderVoice::ElevenLabs(self.elevenlabs.clone())),
            ProviderName::Deepgram => None,
        }
    }
}

/// Durable profile identity recorded with results and history.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct VoiceProfileRef {
    /// Profile selected for this operation.
    pub id: String,
    /// Mapping revision used for this operation.
    pub revision: i32,
}
impl From<&VoiceProfile> for VoiceProfileRef {
    fn from(profile: &VoiceProfile) -> Self {
        Self {
            id: profile.id.clone(),
            revision: profile.revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seeded_catalog_is_complete_valid_and_consistent_with_migration()
    -> Result<(), Box<dyn std::error::Error>> {
        let profiles: Vec<VoiceProfile> =
            serde_json::from_str(include_str!("voice_profiles.json"))?;
        assert_eq!(profiles.len(), 30);
        assert_eq!(
            profiles
                .iter()
                .map(|p| &p.id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            30
        );
        assert!(profiles.iter().all(VoiceProfile::is_valid));
        assert!(profiles.iter().all(|p| !p.auditioned));
        assert!(
            profiles
                .iter()
                .map(|p| &p.elevenlabs.voice_id)
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1
        );
        let sql = include_str!("../../migrations/0009_voice_profiles.sql");
        let seed = sql
            .split("$profiles$")
            .nth(1)
            .ok_or("missing SQL catalog")?;
        assert_eq!(profiles, serde_json::from_str::<Vec<VoiceProfile>>(seed)?);
        let mut profile = profiles[0].clone();
        for speed in [0.69, 1.21, f64::NAN, f64::INFINITY] {
            profile.elevenlabs.voice_settings.speed = speed;
            assert!(!profile.is_valid());
        }
        profile.elevenlabs.voice_settings.speed = 1.0;
        profile.elevenlabs.voice_settings.stability = 1.01;
        assert!(!profile.is_valid());
        profile.elevenlabs.voice_settings.stability = 0.0;
        profile.elevenlabs.voice_settings.use_speaker_boost = false;
        assert!(profile.is_valid());
        profile.elevenlabs.voice_id = "../../voices".into();
        assert!(!profile.is_valid());
        Ok(())
    }
}
