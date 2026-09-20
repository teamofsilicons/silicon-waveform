//! Request-local provider credentials. Never persisted or exposed through Debug.

use std::{collections::HashMap, fmt};

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Deserializer, de::Error as _};

use super::provider::ProviderName;

/// Optional request-only keys, overriding saved and shared keys for their provider.
#[derive(Clone, Default)]
pub struct ProviderApiKeys(HashMap<ProviderName, SecretString>);

impl ProviderApiKeys {
    /// Iterates credentials in a stable order for keyed request fingerprints.
    pub fn iter(&self) -> impl Iterator<Item = (ProviderName, &SecretString)> {
        [
            ProviderName::Gemini,
            ProviderName::ElevenLabs,
            ProviderName::OpenAi,
            ProviderName::Deepgram,
        ]
        .into_iter()
        .filter_map(|name| self.0.get(&name).map(|key| (name, key)))
    }

    /// Whether the request supplied any keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Rejects keys for providers outside this speech operation.
    #[must_use]
    pub fn supports_only(&self, providers: &[ProviderName]) -> bool {
        self.0.keys().all(|provider| providers.contains(provider))
    }
}

impl fmt::Debug for ProviderApiKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderApiKeys")
            .field("providers", &self.0.keys())
            .finish_non_exhaustive()
    }
}

impl PartialEq for ProviderApiKeys {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.iter().all(|(name, key)| {
                other
                    .0
                    .get(&name)
                    .is_some_and(|value| value.expose_secret() == key.expose_secret())
            })
    }
}
impl Eq for ProviderApiKeys {}

impl<'de> Deserialize<'de> for ProviderApiKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = HashMap::<ProviderName, SecretString>::deserialize(deserializer)?;
        if values.values().any(|key| {
            let value = key.expose_secret();
            value.is_empty() || value.len() > 16_384 || !value.bytes().all(|b| b.is_ascii_graphic())
        }) {
            return Err(D::Error::custom("invalid provider API key"));
        }
        Ok(Self(values))
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderApiKeys;

    #[test]
    fn keys_are_bounded_and_never_debugged() -> Result<(), Box<dyn std::error::Error>> {
        let keys: ProviderApiKeys = serde_json::from_str(r#"{"openai":"private-provider-key"}"#)?;
        assert!(!format!("{keys:?}").contains("private-provider-key"));
        for body in [
            r#"{"openai":""}"#,
            r#"{"openai":"has whitespace"}"#,
            r#"{"unknown":"key"}"#,
        ] {
            assert!(serde_json::from_str::<ProviderApiKeys>(body).is_err());
        }
        Ok(())
    }
}
