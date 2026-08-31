//! Deterministic public Waveform capabilities.

use super::{language::LanguageHint, media::SourceMediaType};

/// A language code deliberately included in the stable public contract.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SupportedLanguage(&'static str);

impl SupportedLanguage {
    const fn new(value: &'static str) -> Self {
        Self(value)
    }

    /// Returns the BCP 47 base language code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Documented Gemini 3.1 Flash TTS Preview language set.
pub const TTS_LANGUAGE_HINTS: [SupportedLanguage; 78] = [
    SupportedLanguage::new("af"),
    SupportedLanguage::new("am"),
    SupportedLanguage::new("ar"),
    SupportedLanguage::new("az"),
    SupportedLanguage::new("be"),
    SupportedLanguage::new("bg"),
    SupportedLanguage::new("bn"),
    SupportedLanguage::new("ca"),
    SupportedLanguage::new("ceb"),
    SupportedLanguage::new("cmn"),
    SupportedLanguage::new("cs"),
    SupportedLanguage::new("da"),
    SupportedLanguage::new("de"),
    SupportedLanguage::new("el"),
    SupportedLanguage::new("en"),
    SupportedLanguage::new("es"),
    SupportedLanguage::new("et"),
    SupportedLanguage::new("eu"),
    SupportedLanguage::new("fa"),
    SupportedLanguage::new("fi"),
    SupportedLanguage::new("fil"),
    SupportedLanguage::new("fr"),
    SupportedLanguage::new("gl"),
    SupportedLanguage::new("gu"),
    SupportedLanguage::new("he"),
    SupportedLanguage::new("hi"),
    SupportedLanguage::new("hr"),
    SupportedLanguage::new("ht"),
    SupportedLanguage::new("hu"),
    SupportedLanguage::new("hy"),
    SupportedLanguage::new("id"),
    SupportedLanguage::new("is"),
    SupportedLanguage::new("it"),
    SupportedLanguage::new("ja"),
    SupportedLanguage::new("jv"),
    SupportedLanguage::new("ka"),
    SupportedLanguage::new("kn"),
    SupportedLanguage::new("ko"),
    SupportedLanguage::new("kok"),
    SupportedLanguage::new("la"),
    SupportedLanguage::new("lb"),
    SupportedLanguage::new("lo"),
    SupportedLanguage::new("lt"),
    SupportedLanguage::new("lv"),
    SupportedLanguage::new("mai"),
    SupportedLanguage::new("mg"),
    SupportedLanguage::new("mk"),
    SupportedLanguage::new("ml"),
    SupportedLanguage::new("mn"),
    SupportedLanguage::new("mr"),
    SupportedLanguage::new("ms"),
    SupportedLanguage::new("my"),
    SupportedLanguage::new("nb"),
    SupportedLanguage::new("ne"),
    SupportedLanguage::new("nl"),
    SupportedLanguage::new("nn"),
    SupportedLanguage::new("or"),
    SupportedLanguage::new("pa"),
    SupportedLanguage::new("pl"),
    SupportedLanguage::new("ps"),
    SupportedLanguage::new("pt"),
    SupportedLanguage::new("ro"),
    SupportedLanguage::new("ru"),
    SupportedLanguage::new("sd"),
    SupportedLanguage::new("si"),
    SupportedLanguage::new("sk"),
    SupportedLanguage::new("sl"),
    SupportedLanguage::new("sq"),
    SupportedLanguage::new("sr"),
    SupportedLanguage::new("sv"),
    SupportedLanguage::new("sw"),
    SupportedLanguage::new("ta"),
    SupportedLanguage::new("te"),
    SupportedLanguage::new("th"),
    SupportedLanguage::new("tr"),
    SupportedLanguage::new("uk"),
    SupportedLanguage::new("ur"),
    SupportedLanguage::new("vi"),
];

/// Conservative common best-effort language set across the STT fallback chain.
pub const STT_LANGUAGE_HINTS: [SupportedLanguage; 10] = [
    SupportedLanguage::new("de"),
    SupportedLanguage::new("en"),
    SupportedLanguage::new("es"),
    SupportedLanguage::new("fr"),
    SupportedLanguage::new("hi"),
    SupportedLanguage::new("it"),
    SupportedLanguage::new("ja"),
    SupportedLanguage::new("nl"),
    SupportedLanguage::new("pt"),
    SupportedLanguage::new("ru"),
];

/// Stable TTS output encoding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TtsOutputFormat {
    /// MPEG Layer III audio (`audio/mpeg`).
    Mp3,
}

impl TtsOutputFormat {
    /// Returns the public MIME type.
    #[must_use]
    pub const fn as_mime_str(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
        }
    }

    /// Returns the public format label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
        }
    }
}

/// Text-to-speech capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TtsCapabilities {
    /// Guaranteed normalized output format.
    pub output_format: TtsOutputFormat,
    /// Accepted best-effort base language hints.
    pub languages: &'static [SupportedLanguage],
}

impl TtsCapabilities {
    /// Returns whether a validated hint belongs to the accepted TTS set.
    #[must_use]
    pub fn supports(self, hint: &LanguageHint) -> bool {
        supports(self.languages, hint)
    }
}

/// Speech-to-text capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SttCapabilities {
    /// Accepted best-effort base language hints.
    pub languages: &'static [SupportedLanguage],
    /// Canonical accepted media types.
    pub accepted_media_types: &'static [SourceMediaType],
}

impl SttCapabilities {
    /// Returns whether a validated hint belongs to the accepted STT set.
    #[must_use]
    pub fn supports(self, hint: &LanguageHint) -> bool {
        supports(self.languages, hint)
    }

    /// Returns whether a normalized source media type is accepted.
    #[must_use]
    pub fn accepts_media_type(self, media_type: SourceMediaType) -> bool {
        self.accepted_media_types.contains(&media_type)
    }
}

/// Stable provider-independent Waveform contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capabilities {
    /// Text-to-speech contract.
    pub tts: TtsCapabilities,
    /// Speech-to-text contract.
    pub stt: SttCapabilities,
}

impl Capabilities {
    /// Returns compile-time capabilities without probing billable providers.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            tts: TtsCapabilities {
                output_format: TtsOutputFormat::Mp3,
                languages: &TTS_LANGUAGE_HINTS,
            },
            stt: SttCapabilities {
                languages: &STT_LANGUAGE_HINTS,
                accepted_media_types: &SourceMediaType::ALL,
            },
        }
    }
}

fn supports(supported: &[SupportedLanguage], hint: &LanguageHint) -> bool {
    supported
        .iter()
        .any(|language| language.as_str() == hint.primary_language())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::{Capabilities, STT_LANGUAGE_HINTS, TTS_LANGUAGE_HINTS, TtsOutputFormat};
    use crate::domain::{language::LanguageHint, media::SourceMediaType};

    #[test]
    fn capabilities_are_complete_and_deterministic() {
        let capabilities = Capabilities::current();

        assert_eq!(capabilities.tts.output_format, TtsOutputFormat::Mp3);
        assert_eq!(capabilities.tts.languages, &TTS_LANGUAGE_HINTS);
        assert_eq!(capabilities.stt.languages, &STT_LANGUAGE_HINTS);
        assert_eq!(capabilities.stt.accepted_media_types, &SourceMediaType::ALL);
    }

    #[test]
    fn regional_variants_are_matched_by_primary_language() {
        let supported = LanguageHint::from_str("pt-BR");
        let unsupported = LanguageHint::from_str("cy-GB");
        let capabilities = Capabilities::current();

        assert!(matches!(supported, Ok(hint) if capabilities.stt.supports(&hint)));
        assert!(matches!(unsupported, Ok(hint) if !capabilities.stt.supports(&hint)));
    }

    #[test]
    fn tts_and_stt_sets_are_intentionally_distinct() {
        let mandarin = LanguageHint::from_str("cmn-Hans");
        let capabilities = Capabilities::current();

        assert!(matches!(
            mandarin,
            Ok(hint) if capabilities.tts.supports(&hint) && !capabilities.stt.supports(&hint)
        ));
    }
}
