//! Waveform's request and response types. New optional fields are `#[serde(default)]` so this
//! client keeps reading answers from older and newer Waveform versions.

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::tts_options::TtsProviderOptions;

/// A Silicon Accounts account as Waveform shows it. Key on `uuid` (permanent, short and
/// case-sensitive, e.g. `zQo`); show `id` (`c:ada`, `si:scout`), which can change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Permanent Silicon Accounts uuid.
    pub uuid: String,
    /// Current public id, when known.
    #[serde(default)]
    pub id: Option<String>,
    /// `carbon` or `silicon`.
    pub kind: String,
    /// Display name, when known.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Profile photo URL, when known.
    #[serde(default)]
    pub pfp_url: Option<String>,
    /// A Silicon's custodian (the Carbon who looks after it).
    #[serde(default)]
    pub custodian: Option<AccountRef>,
}

impl Account {
    /// The id to show: the public id, or the uuid when the id is unknown.
    #[must_use]
    pub fn label(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.uuid)
    }

    /// True for a Silicon account.
    #[must_use]
    pub fn is_silicon(&self) -> bool {
        self.kind == "silicon"
    }
}

/// A reference to an account: its uuid and, when known, its current public id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRef {
    /// Permanent Silicon Accounts uuid.
    pub uuid: String,
    /// Current public id, when known.
    #[serde(default)]
    pub id: Option<String>,
}

/// `GET /api/v1/me`: the account an access token belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    /// Always true in a successful answer.
    pub authenticated: bool,
    /// `waveform`.
    pub app_id: String,
    /// The signed-in account.
    pub account: Account,
    /// How the caller authenticated (`bearer`).
    #[serde(default)]
    pub via: Option<String>,
    /// When the access token expires (RFC 3339).
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// Whether a credential is accepted by Waveform right now, without any token in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStatus {
    /// True only after Waveform accepted the credential.
    pub authenticated: bool,
    /// The account, when authenticated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    /// When the access token expires (RFC 3339), when authenticated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Why Waveform refused the credential (`token_expired`, `sign_in_ended`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Waveform's explanation of the refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `GET /api/v1/accounts`: where to sign in to this Waveform. Public.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsInfo {
    /// Waveform's app id at Silicon Accounts (`waveform`).
    pub app_id: String,
    /// The Silicon Accounts this Waveform trusts.
    pub accounts_url: String,
    /// This Waveform's API origin.
    pub api_url: String,
    /// Waveform's documentation.
    #[serde(default)]
    pub docs_url: Option<String>,
    /// Proof scopes this Waveform honours.
    #[serde(default)]
    pub proof_scopes: Vec<String>,
    /// The service version.
    #[serde(default)]
    pub version: Option<String>,
}

/// Durable voice-profile identity recorded with results and history.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct VoiceProfileRef {
    /// Profile selected for this operation.
    pub id: String,
    /// Mapping revision used for this operation.
    pub revision: i32,
}

/// Request for synthesis (`POST /api/v1/tts`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct TtsRequest {
    /// Text to speak (1-4096 characters, not only whitespace).
    pub text: String,
    /// Voice profile id; omitted uses the account default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_profile: Option<String>,
    /// BCP 47 language hint, passed only to Gemini.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Preferred providers, in order; omitted providers follow the account's order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_order: Option<Vec<String>>,
    /// Try later providers after a failure. Defaults to false: only the first provider runs.
    pub auto_fallback: bool,
    /// Provider-specific controls, available only with `auto_fallback` false.
    #[serde(skip_serializing_if = "TtsProviderOptions::is_empty")]
    pub provider_options: TtsProviderOptions,
    /// Request-only provider keys; they override saved and deployment keys and are never stored.
    #[serde(skip_serializing_if = "ProviderKeys::is_empty")]
    pub provider_keys: ProviderKeys,
}

/// Request for transcription (`POST /api/v1/stt`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct SttRequest {
    /// Permanent Briefcase URL of an audio file in the account's own Briefcase files.
    pub file_url: String,
    /// BCP 47 transcription hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Preferred providers, in order. STT always falls back automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_order: Option<Vec<String>>,
    /// Request-only provider keys.
    #[serde(skip_serializing_if = "ProviderKeys::is_empty")]
    pub provider_keys: ProviderKeys,
}

/// In-memory, request-only provider credentials. `Debug` output redacts every key.
#[derive(Clone, Default)]
pub struct ProviderKeys(BTreeMap<String, SecretString>);

impl ProviderKeys {
    /// Adds or replaces one provider credential without persisting it.
    ///
    /// # Errors
    /// An unknown provider, or a key that is empty, longer than 16384 bytes or contains
    /// whitespace or control characters.
    pub fn insert(&mut self, provider: impl Into<String>, key: impl Into<String>) -> Result<()> {
        let provider = provider.into();
        validate_provider(&provider)?;
        let key = key.into();
        if key.is_empty() || key.len() > 16384 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(Error::Invalid(
                "A provider key must be 1-16384 ASCII characters without whitespace or control characters.".into(),
            ));
        }
        self.0.insert(provider, SecretString::from(key));
        Ok(())
    }

    /// Whether this request uses only saved or deployment credentials.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ProviderKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|key| (key, "[REDACTED]")))
            .finish()
    }
}

impl Serialize for ProviderKeys {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (provider, key) in &self.0 {
            map.serialize_entry(provider, key.expose_secret())?;
        }
        map.end()
    }
}

pub(crate) fn validate_provider(provider: &str) -> Result<()> {
    if matches!(provider, "gemini" | "elevenlabs" | "openai" | "deepgram") {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "`{provider}` is not a provider; use gemini, elevenlabs, openai or deepgram."
        )))
    }
}

/// Where generated speech is: in the account's Briefcase files, or held by Waveform.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageInfo {
    /// `stored` (Briefcase, see `file_url`) or `held` (Waveform, see `audio_url`).
    pub status: String,
    /// Why the audio is held: `delegated_call`, `storage_not_authorized`, `briefcase_refused`,
    /// `briefcase_unavailable`, `briefcase_timeout` or `briefcase_contract_unavailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Exact explanation when held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// When Waveform deletes held audio (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl StorageInfo {
    /// True when Waveform holds the audio instead of Briefcase.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.status == "held"
    }
}

/// Result of a synthesis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TtsResponse {
    /// Voice profile and revision used.
    #[serde(default)]
    pub voice_profile: Option<VoiceProfileRef>,
    /// Canonical operation id (also the job id), kept on idempotent replays.
    pub request_id: String,
    /// Permanent Briefcase URL; `None` when Waveform holds the audio instead.
    #[serde(default)]
    pub file_url: Option<String>,
    /// Always `None`; kept for older servers.
    #[serde(default)]
    pub temporary_url: Option<String>,
    /// Waveform URL of held audio (download it with [`crate::Client::held_audio`]).
    #[serde(default)]
    pub audio_url: Option<String>,
    /// Where the audio is, and why when it is not in Briefcase.
    #[serde(default)]
    pub storage: Option<StorageInfo>,
    /// `audio/mpeg`.
    pub media_type: String,
    /// Provider that completed synthesis.
    pub provider: String,
    /// Length of the audio in milliseconds.
    pub duration_ms: u64,
}

/// Result of a transcription.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SttResponse {
    /// Canonical operation id (also the job id), kept on idempotent replays.
    pub request_id: String,
    /// The transcript.
    pub transcript: String,
    /// Language the provider detected, when it said.
    pub detected_language: Option<String>,
    /// Provider that completed transcription.
    pub provider: String,
    /// Length of the source audio in milliseconds, when known.
    pub duration_ms: Option<u64>,
}

/// A content-free speech job record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    /// Voice profile and revision used (TTS).
    #[serde(default)]
    pub voice_profile: Option<VoiceProfileRef>,
    /// Job id (the speech request id).
    pub id: String,
    /// App that called Waveform for the account with a proof, when one did.
    #[serde(default)]
    pub issuing_app: Option<String>,
    /// `tts` or `stt`.
    pub operation: String,
    /// `running`, `completed` or `failed`.
    pub status: String,
    /// First line of the text (TTS) or transcript (STT).
    pub first_line: String,
    /// Audio length in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Provider that completed it.
    pub provider: Option<String>,
    /// Why it failed.
    #[serde(default)]
    pub error_code: Option<String>,
    /// When it started (RFC 3339).
    pub created_at: String,
    /// When it finished (RFC 3339).
    pub finished_at: Option<String>,
}

impl Job {
    /// True once the job reached `completed` or `failed`.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed")
    }
}

/// One page of speech history, newest first.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobPage {
    /// The account whose history this is.
    #[serde(default)]
    pub account: Option<AccountRef>,
    /// The jobs.
    pub items: Vec<Job>,
    /// Pass to the next call to continue; `None` on the last page.
    pub next_cursor: Option<String>,
}

/// Waveform-level speech capabilities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    /// Text to speech.
    pub tts: TtsCapabilities,
    /// Speech to text.
    pub stt: SttCapabilities,
    /// Provider-key support; absent on older servers.
    #[serde(default)]
    pub byok: Option<ByokCapabilities>,
}

/// Text-to-speech capabilities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TtsCapabilities {
    /// Output format (`mp3`).
    pub output_format: String,
    /// Supported language hints.
    pub languages: Vec<String>,
    /// Default synthesis fallback policy; absent on older servers.
    #[serde(default)]
    pub auto_fallback_default: Option<bool>,
    /// Supported control field names grouped by provider.
    #[serde(default)]
    pub provider_options: BTreeMap<String, Vec<String>>,
}

/// Saved and request-only provider key support.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ByokCapabilities {
    /// Keys can be saved for an account.
    pub saved: bool,
    /// Keys can be sent with one request.
    pub per_request: bool,
    /// Credential sources from highest to lowest priority.
    pub precedence: Vec<String>,
}

/// Speech-to-text capabilities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SttCapabilities {
    /// Supported language hints.
    pub languages: Vec<String>,
    /// Accepted source media types.
    pub accepted_media_types: Vec<String>,
}

/// A voice profile and its mapping to every TTS provider.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VoiceProfile {
    /// Stable id (e.g. `puck`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Delivery character, based on the Gemini voice description.
    pub description: String,
    /// Incremented whenever the mapping is revised.
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

/// `ElevenLabs` mapping for the multilingual-v2 fallback model.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ElevenLabsVoice {
    /// Voice id.
    pub voice_id: String,
    /// Model id.
    pub model_id: String,
    /// Delivery settings.
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

/// An account's speech settings (`GET`/`PATCH /api/v1/preferences`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// The account these settings belong to.
    #[serde(default)]
    pub account: Option<AccountRef>,
    /// Whether Waveform records diagnostics for this account (default true).
    #[serde(default)]
    pub telemetry_enabled: Option<bool>,
    /// Default voice profile.
    #[serde(default)]
    pub voice_profile: Option<String>,
    /// TTS provider order.
    #[serde(default)]
    pub tts_order: Option<Vec<String>>,
    /// STT provider order.
    #[serde(default)]
    pub stt_order: Option<Vec<String>>,
    /// Waveform's defaults (in `GET` answers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<PreferenceDefaults>,
}

/// Waveform's default settings for accounts that changed nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferenceDefaults {
    /// Default voice profile.
    #[serde(default)]
    pub voice_profile: Option<String>,
    /// Default TTS order.
    #[serde(default)]
    pub tts_order: Vec<String>,
    /// Default STT order.
    #[serde(default)]
    pub stt_order: Vec<String>,
}

/// A change to an account's settings; `None` fields stay as they are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PreferencesUpdate {
    /// Turn Waveform's diagnostics for the account on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_enabled: Option<bool>,
    /// New default voice profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_profile: Option<String>,
    /// Preferred TTS providers first; the rest follow Waveform's order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_order: Option<Vec<String>>,
    /// Preferred STT providers first; the rest follow Waveform's order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_order: Option<Vec<String>>,
}

impl PreferencesUpdate {
    /// True when nothing would change.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The account's saved provider keys (never their values).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderKeyList {
    /// The account the keys belong to.
    #[serde(default)]
    pub account: Option<AccountRef>,
    /// One entry per provider with a saved key.
    pub items: Vec<ProviderKeyInfo>,
}

/// A saved provider key, without its value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderKeyInfo {
    /// `gemini`, `elevenlabs`, `openai` or `deepgram`.
    pub provider: String,
    /// Always true.
    #[serde(default)]
    pub configured: bool,
    /// When it was saved (RFC 3339).
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Waveform's Briefcase access for an account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefcaseAccess {
    /// The account.
    pub account: AccountRef,
    /// The proof Waveform holds for it (never the token).
    pub briefcase: BriefcaseAccessStatus,
}

/// Status of the User verification proof Waveform holds for Briefcase.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefcaseAccessStatus {
    /// `active`, `revoked` or `missing`.
    pub status: String,
    /// `briefcase`.
    #[serde(default)]
    pub receiving_app: Option<String>,
    /// Briefcase scopes the proof carries.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Silicon Accounts proof id.
    #[serde(default)]
    pub proof_id: Option<String>,
    /// When the current proof token expires.
    #[serde(default)]
    pub token_expires_at: Option<String>,
    /// When the proof can no longer be refreshed.
    #[serde(default)]
    pub refresh_expires_at: Option<String>,
    /// When it last changed.
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Why it was revoked (`access_removed`, `signed_out`, `revoked_by_account`…).
    #[serde(default)]
    pub revoked_reason: Option<String>,
}

/// The account's Waveform folder in Briefcase, where STT sources are uploaded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFolder {
    /// `waveform`.
    pub app_id: String,
    /// The account whose drive holds the folder.
    pub account: AccountRef,
    /// Briefcase folder id.
    pub folder_id: String,
    /// Folder path in the drive (`apps/waveform`).
    pub folder_path: String,
}

/// An MP3 Waveform holds for the account (`GET /api/v1/audio/{request_id}`).
#[derive(Clone, PartialEq, Eq)]
pub struct HeldAudio {
    /// The audio bytes.
    pub bytes: Vec<u8>,
    /// Media type (`audio/mpeg`).
    pub media_type: String,
    /// When Waveform deletes it (RFC 3339), when it said.
    pub expires_at: Option<String>,
    /// Suggested file name, when Waveform sent one.
    pub filename: Option<String>,
}

impl std::fmt::Debug for HeldAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldAudio")
            .field("bytes", &self.bytes.len())
            .field("media_type", &self.media_type)
            .field("expires_at", &self.expires_at)
            .field("filename", &self.filename)
            .finish()
    }
}

/// Receipt of a bug report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportReceipt {
    /// Report id.
    pub id: String,
    /// Always true: the report was saved.
    #[serde(default)]
    pub submitted: bool,
    /// Mail delivery state: `queued` (saved, delivery pending or retrying) or `sent`.
    #[serde(default)]
    pub notification: Option<String>,
    /// Where to send a fix.
    #[serde(default)]
    pub repository: Option<String>,
}
