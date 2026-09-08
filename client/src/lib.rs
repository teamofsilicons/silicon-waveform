//! Stateless, typed Rust client for Silicon Waveform.
//!
//! Authentication is delegated to Silicon IAM. This package accepts an SLT or
//! an already-issued access token and never asks for credentials or persists a
//! session. Test planes are explicit and cannot silently fall back to production.
use reqwest::{Client as HttpClient, Method};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use url::Url;

pub mod update;

/// A bounded client failure that never includes bearer or app-secret material.
#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid Waveform configuration: {0}")]
    Invalid(String),
    #[error("Waveform transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Waveform returned HTTP {status}: {code}")]
    Api { status: u16, code: String },
    #[error("Waveform returned an invalid response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("IAM request failed: {0}")]
    Iam(#[from] silicon_iam_client::Error),
}
/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

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

/// Durable profile identity recorded with results and history.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct VoiceProfileRef {
    /// Profile selected for this operation.
    pub id: String,
    /// Mapping revision used for this operation.
    pub revision: i32,
}

/// Request for synthesis.
#[derive(Clone, Debug, Serialize)]
pub struct TtsRequest {
    pub text: String,
    /// Optional catalog profile; omitted uses the account default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_order: Option<Vec<String>>,
}
/// Request for transcription.
#[derive(Clone, Debug, Serialize)]
pub struct SttRequest {
    pub file_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_order: Option<Vec<String>>,
}
/// Normalized TTS response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TtsResponse {
    #[serde(default)]
    pub voice_profile: Option<VoiceProfileRef>,
    pub request_id: String,
    pub file_url: String,
    #[serde(default)]
    pub temporary_url: Option<String>,
    pub media_type: String,
    pub provider: String,
    pub duration_ms: u64,
}
/// Normalized STT response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SttResponse {
    pub request_id: String,
    pub transcript: String,
    pub detected_language: Option<String>,
    pub provider: String,
    pub duration_ms: Option<u64>,
}
/// A durable speech operation record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    #[serde(default)]
    pub voice_profile: Option<VoiceProfileRef>,
    pub id: String,
    pub operation: String,
    pub status: String,
    pub first_line: String,
    pub duration_ms: Option<u64>,
    pub provider: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

impl Job {
    /// Returns true once the server has reached a terminal job state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed")
    }
}
/// A paginated job-history response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobPage {
    pub items: Vec<Job>,
    pub next_cursor: Option<String>,
}
/// Speech provider capabilities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub tts: TtsCapabilities,
    pub stt: SttCapabilities,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TtsCapabilities {
    pub output_format: String,
    pub languages: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SttCapabilities {
    pub languages: Vec<String>,
    pub accepted_media_types: Vec<String>,
}
/// Request for an isolated test plane.
#[derive(Clone, Debug, Serialize)]
pub struct CreateTestEnvironment {
    pub name: String,
    pub description: Option<String>,
    pub iam_environment_id: String,
    pub iam_environment_key: String,
    pub app_secret: String,
    pub briefcase_environment_key: String,
}
/// Created test-plane metadata; the root key is returned once.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreatedTestEnvironment {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub key: TestEnvironmentKey,
    pub created_at: String,
}
impl<'de> Deserialize<'de> for TestEnvironmentKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A Waveform API client. Cloning is cheap and shares the HTTP pool.
#[derive(Clone)]
pub enum Auth {
    Anonymous,
    Bearer(SecretString),
}

/// A 32-character alphanumeric root key selecting one isolated test plane.
#[derive(Clone)]
pub struct TestEnvironmentKey(SecretString);

impl TestEnvironmentKey {
    /// Validates the test-plane root-key wire format.
    pub fn new(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if key.len() != 32 || !key.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(Error::Invalid(
                "test environment key must be 32 alphanumeric characters".into(),
            ));
        }
        Ok(Self(SecretString::from(key)))
    }
}

impl Serialize for TestEnvironmentKey {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.expose_secret())
    }
}

impl std::fmt::Debug for TestEnvironmentKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TestEnvironmentKey(<redacted>)")
    }
}

#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    base: Url,
    credential: Auth,
    environment: Option<TestEnvironmentKey>,
    speech_request_id: Option<uuid::Uuid>,
    updater: Arc<update::AutomaticUpdater>,
}
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anonymous => f.write_str("Anonymous"),
            Self::Bearer(_) => f.write_str("Bearer(<redacted>)"),
        }
    }
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("credential", &self.credential)
            .field("environment", &self.environment)
            .finish()
    }
}
impl Client {
    /// Builds an anonymous or authenticated client for an API origin.
    pub fn new(base: &str, credential: Auth) -> Result<Self> {
        let url = Url::parse(base).map_err(|_| Error::Invalid("base URL is invalid".into()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(Error::Invalid("base URL must use HTTP(S)".into()));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Error::Invalid(
                "base URL must be a credential-free origin".into(),
            ));
        }
        Ok(Self {
            http: HttpClient::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(650))
                .build()?,
            base: url,
            credential,
            environment: None,
            speech_request_id: None,
            updater: update::AutomaticUpdater::new(update::UpdatePolicy::from_environment()),
        })
    }

    /// Sets the known job/request UUID for one logical speech operation.
    /// Use a fresh client clone/UUID for a different operation. On retries,
    /// an existing idempotency key still retains its original canonical job ID.
    pub fn with_speech_request_id(&self, id: uuid::Uuid) -> Result<Self> {
        if id.is_nil() {
            return Err(Error::Invalid("speech request ID must be non-nil".into()));
        }
        Ok(Self {
            speech_request_id: Some(id),
            ..self.clone()
        })
    }

    /// Enables or disables the hourly best-effort client update check.
    #[must_use]
    pub fn with_auto_update(&self, enabled: bool) -> Self {
        Self {
            updater: update::AutomaticUpdater::new(if enabled {
                update::UpdatePolicy::Automatic
            } else {
                update::UpdatePolicy::Disabled
            }),
            ..self.clone()
        }
    }

    /// Returns the last automatic update result observed by this client and its clones.
    #[must_use]
    pub fn update_status(&self) -> update::UpdateStatus {
        self.updater.status()
    }
    /// Reuses this client with an IAM bearer token.
    pub fn with_bearer(&self, token: impl Into<String>) -> Self {
        Self {
            credential: Auth::Bearer(SecretString::from(token.into())),
            ..self.clone()
        }
    }
    /// Executes subsequent calls in a selected test plane.
    pub fn with_test_environment(&self, key: TestEnvironmentKey) -> Self {
        Self {
            environment: Some(key),
            ..self.clone()
        }
    }
    /// Exchanges an IAM SLT through Waveform; app secrets stay on the backend.
    pub async fn login(&self, slt: &str) -> Result<silicon_iam_client::models::OAuthTokenResponse> {
        self.request(
            Method::POST,
            "auth/login",
            Some(serde_json::json!({"slt": slt})),
            None,
        )
        .await
    }

    /// Rotates a refresh token through Waveform's application credential.
    pub async fn refresh(
        &self,
        refresh_token: &str,
    ) -> Result<silicon_iam_client::models::OAuthTokenResponse> {
        self.request(
            Method::POST,
            "auth/refresh",
            Some(serde_json::json!({"refresh_token": refresh_token})),
            None,
        )
        .await
    }
    /// Generates speech synchronously.
    pub async fn tts(
        &self,
        organization: &str,
        actor: &str,
        request: &TtsRequest,
        idempotency: &str,
    ) -> Result<TtsResponse> {
        self.speech(
            Method::POST,
            "tts",
            organization,
            actor,
            request,
            idempotency,
        )
        .await
    }
    /// Transcribes a Briefcase URL synchronously.
    pub async fn stt(
        &self,
        organization: &str,
        actor: &str,
        request: &SttRequest,
        idempotency: &str,
    ) -> Result<SttResponse> {
        self.speech(
            Method::POST,
            "stt",
            organization,
            actor,
            request,
            idempotency,
        )
        .await
    }
    /// Creates an isolated test plane using production authorization.
    pub async fn create_test_environment(
        &self,
        organization: &str,
        request: &CreateTestEnvironment,
    ) -> Result<CreatedTestEnvironment> {
        self.org_request(
            Method::POST,
            "testing-environments",
            organization,
            Some(request),
        )
        .await
    }
    /// Reads metadata for the selected test plane.
    pub async fn test_environment(&self, organization: &str) -> Result<serde_json::Value> {
        self.org_request(
            Method::GET,
            "testing-environment",
            organization,
            None::<&()>,
        )
        .await
    }
    /// Atomically removes data from the selected test plane.
    pub async fn clean_test_environment(&self, organization: &str) -> Result<()> {
        self.request_empty_org(Method::POST, "testing-environment/clean", organization)
            .await
    }

    /// Lists all test environments owned by the authenticated organization.
    pub async fn test_environments(&self, organization: &str) -> Result<serde_json::Value> {
        self.org_request(
            Method::GET,
            "testing-environments",
            organization,
            None::<&()>,
        )
        .await
    }

    /// Reads one test-environment record by UUID.
    pub async fn test_environment_detail(
        &self,
        organization: &str,
        environment_id: &str,
    ) -> Result<serde_json::Value> {
        uuid::Uuid::parse_str(environment_id)
            .map_err(|_| Error::Invalid("environment ID is invalid".into()))?;
        self.org_request(
            Method::GET,
            &format!("testing-environments/{environment_id}"),
            organization,
            None::<&()>,
        )
        .await
    }

    /// Retrieves the current root key for an environment when authorized.
    pub async fn test_environment_key(
        &self,
        organization: &str,
        environment_id: &str,
    ) -> Result<serde_json::Value> {
        uuid::Uuid::parse_str(environment_id)
            .map_err(|_| Error::Invalid("environment ID is invalid".into()))?;
        self.org_request(
            Method::GET,
            &format!("testing-environments/{environment_id}/key"),
            organization,
            None::<&()>,
        )
        .await
    }

    /// Rotates an environment root key and returns the replacement.
    pub async fn rotate_test_environment_key(
        &self,
        organization: &str,
        environment_id: &str,
    ) -> Result<serde_json::Value> {
        uuid::Uuid::parse_str(environment_id)
            .map_err(|_| Error::Invalid("environment ID is invalid".into()))?;
        self.org_request(
            Method::POST,
            &format!("testing-environments/{environment_id}/rotate-key"),
            organization,
            None::<&()>,
        )
        .await
    }

    /// Soft-deletes an environment for its 30-day recovery window.
    pub async fn delete_test_environment(
        &self,
        organization: &str,
        environment_id: &str,
    ) -> Result<serde_json::Value> {
        uuid::Uuid::parse_str(environment_id)
            .map_err(|_| Error::Invalid("environment ID is invalid".into()))?;
        self.org_request(
            Method::POST,
            &format!("testing-environments/{environment_id}/delete"),
            organization,
            None::<&()>,
        )
        .await
    }

    /// Restores a soft-deleted environment within its recovery window.
    pub async fn restore_test_environment(
        &self,
        organization: &str,
        environment_id: &str,
    ) -> Result<serde_json::Value> {
        uuid::Uuid::parse_str(environment_id)
            .map_err(|_| Error::Invalid("environment ID is invalid".into()))?;
        self.org_request(
            Method::POST,
            &format!("testing-environments/{environment_id}/restore"),
            organization,
            None::<&()>,
        )
        .await
    }

    /// Revokes the current access or refresh token through Waveform.
    pub async fn logout(&self, token: &str) -> Result<()> {
        self.request_empty(
            Method::POST,
            "auth/logout",
            Some(&serde_json::json!({"token": token})),
            None,
        )
        .await
    }

    /// Reads the authenticated actor and organization authorization.
    pub async fn me(&self) -> Result<serde_json::Value> {
        self.request(Method::GET, "auth/me", None::<&()>, None)
            .await
    }

    /// Reads stable service capabilities without authentication.
    pub async fn capabilities(&self) -> Result<Capabilities> {
        self.request(Method::GET, "capabilities", None::<&()>, None)
            .await
    }

    /// Reads the caller's effective provider order and configured defaults.
    pub async fn preferences(&self, organization: &str, actor: &str) -> Result<serde_json::Value> {
        self.scoped_request(Method::GET, "preferences", organization, actor, None::<&()>)
            .await
    }

    /// Updates one or both account-level provider orders.
    pub async fn update_preferences(
        &self,
        organization: &str,
        actor: &str,
        tts_order: Option<Vec<String>>,
        stt_order: Option<Vec<String>>,
    ) -> Result<serde_json::Value> {
        self.scoped_request(
            Method::PATCH,
            "preferences",
            organization,
            actor,
            Some(&serde_json::json!({
                "tts_order": tts_order, "stt_order": stt_order,
            })),
        )
        .await
    }

    /// Lists profiles and all provider mappings in the selected environment.
    pub async fn voice_profiles(
        &self,
        organization: &str,
        actor: &str,
    ) -> Result<Vec<VoiceProfile>> {
        #[derive(Deserialize)]
        struct Catalog {
            items: Vec<VoiceProfile>,
        }
        let catalog: Catalog = self
            .scoped_request(
                Method::GET,
                "voice-profiles",
                organization,
                actor,
                None::<&()>,
            )
            .await?;
        Ok(catalog.items)
    }

    /// Updates account provider order and/or its default voice in one request.
    pub async fn update_preferences_with_voice(
        &self,
        organization: &str,
        actor: &str,
        tts_order: Option<Vec<String>>,
        stt_order: Option<Vec<String>>,
        voice_profile: Option<String>,
    ) -> Result<serde_json::Value> {
        self.scoped_request(Method::PATCH, "preferences", organization, actor,
            Some(&serde_json::json!({"tts_order": tts_order, "stt_order": stt_order, "voice_profile": voice_profile}))).await
    }

    /// Lists configured personal provider keys (secret values are never returned).
    pub async fn provider_keys(
        &self,
        organization: &str,
        actor: &str,
    ) -> Result<serde_json::Value> {
        self.scoped_request(
            Method::GET,
            "provider-keys",
            organization,
            actor,
            None::<&()>,
        )
        .await
    }

    /// Stores or replaces one personal provider key.
    pub async fn put_provider_key(
        &self,
        organization: &str,
        actor: &str,
        provider: &str,
        api_key: &str,
    ) -> Result<()> {
        validate_provider(provider)?;
        let _: () = self
            .scoped_empty_request(
                Method::PUT,
                &format!("provider-keys/{provider}"),
                organization,
                actor,
                Some(&serde_json::json!({"api_key": api_key})),
            )
            .await?;
        Ok(())
    }

    /// Removes one personal provider key.
    pub async fn delete_provider_key(
        &self,
        organization: &str,
        actor: &str,
        provider: &str,
    ) -> Result<()> {
        validate_provider(provider)?;
        self.scoped_empty_request(
            Method::DELETE,
            &format!("provider-keys/{provider}"),
            organization,
            actor,
            None::<&()>,
        )
        .await
    }
    /// Fetches one actor-scoped job by its durable identifier.
    pub async fn job(&self, job_id: &str, organization: &str, actor: &str) -> Result<Job> {
        if uuid::Uuid::parse_str(job_id).is_err() || organization.is_empty() || actor.is_empty() {
            return Err(Error::Invalid(
                "job ID, organization and actor are required".into(),
            ));
        }
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("base URL cannot carry credentials".into()))?;
            segments
                .pop_if_empty()
                .extend(["api", "v1", "jobs", job_id]);
        }
        let req = self
            .http
            .get(url)
            .header("x-org-id", organization)
            .header("x-actor-id", actor)
            .bearer_auth(match &self.credential {
                Auth::Bearer(token) => token.expose_secret(),
                Auth::Anonymous => return Err(Error::Invalid("authentication is required".into())),
            });
        let req = if let Some(env) = &self.environment {
            req.header("x-testing-environment-key", env.0.expose_secret())
        } else {
            req
        };
        self.send_response(req).await
    }

    /// Polls one job until it reaches `completed` or `failed`.
    ///
    /// The final observed record is returned for both terminal states. A
    /// bounded timeout includes each HTTP request. An initial 404 is retried
    /// while the speech request is being authorized and its row is created.
    pub async fn wait_for_job(
        &self,
        job_id: &str,
        organization: &str,
        actor: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Job> {
        if timeout.is_zero() || poll_interval.is_zero() {
            return Err(Error::Invalid(
                "job polling timeout and interval must be non-zero".into(),
            ));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::Invalid("job polling timeout is out of range".into()))?;
        loop {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.job(job_id, organization, actor),
            )
            .await
            {
                Ok(Ok(job)) if job.is_terminal() => return Ok(job),
                Ok(Ok(_)) | Ok(Err(Error::Api { status: 404, .. })) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(Error::Invalid("job polling timed out".into())),
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Invalid("job polling timed out".into()));
            }
            tokio::time::sleep(poll_interval.min(deadline.saturating_duration_since(now))).await;
        }
    }

    /// Lists the caller's own speech history. The backend never returns another actor's jobs.
    pub async fn jobs(
        &self,
        organization: &str,
        actor: &str,
        operation: Option<&str>,
    ) -> Result<JobPage> {
        self.jobs_page(organization, actor, operation, None, None)
            .await
    }

    /// Lists actor-scoped jobs with optional bounded pagination controls.
    pub async fn jobs_page(
        &self,
        organization: &str,
        actor: &str,
        operation: Option<&str>,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<JobPage> {
        if organization.is_empty() || actor.is_empty() {
            return Err(Error::Invalid("organization and actor are required".into()));
        }
        if limit.is_some_and(|value| !(1..=100).contains(&value)) {
            return Err(Error::Invalid(
                "job page limit must be between 1 and 100".into(),
            ));
        }
        if operation.is_some_and(|value| !matches!(value, "tts" | "stt")) {
            return Err(Error::Invalid("job operation must be tts or stt".into()));
        }
        let mut query = Vec::new();
        if let Some(operation) = operation {
            query.push(("operation", operation.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor.to_owned()));
        }
        self.scoped_get("jobs", organization, actor, query).await
    }

    async fn org_request<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        organization: &str,
        body: Option<B>,
    ) -> Result<R> {
        if organization.is_empty() {
            return Err(Error::Invalid("organization is required".into()));
        }
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("base URL cannot carry credentials".into()))?;
            segments
                .pop_if_empty()
                .extend(["api", "v1"])
                .extend(path.split('/'));
        }
        let mut req = self
            .http
            .request(method, url)
            .header("x-org-id", organization);
        req = match &self.credential {
            Auth::Anonymous => req,
            Auth::Bearer(token) => req.bearer_auth(token.expose_secret()),
        };
        if let Some(env) = &self.environment {
            req = req.header("x-testing-environment-key", env.0.expose_secret());
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        self.send_response(req).await
    }

    async fn request_empty_org(
        &self,
        method: Method,
        path: &str,
        organization: &str,
    ) -> Result<()> {
        if organization.is_empty() {
            return Err(Error::Invalid("organization is required".into()));
        }
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("base URL cannot carry credentials".into()))?;
            segments
                .pop_if_empty()
                .extend(["api", "v1"])
                .extend(path.split('/'));
        }
        let mut req = self
            .http
            .request(method, url)
            .header("x-org-id", organization);
        req = match &self.credential {
            Auth::Anonymous => req,
            Auth::Bearer(token) => req.bearer_auth(token.expose_secret()),
        };
        if let Some(env) = &self.environment {
            req = req.header("x-testing-environment-key", env.0.expose_secret());
        }
        self.send_empty_response(req).await
    }

    async fn scoped_get<R: DeserializeOwned>(
        &self,
        path: &str,
        organization: &str,
        actor: &str,
        query: Vec<(&str, String)>,
    ) -> Result<R> {
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("base URL cannot carry credentials".into()))?;
            segments
                .pop_if_empty()
                .extend(["api", "v1"])
                .extend(path.split('/'));
        }
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        let mut req = self
            .http
            .get(url)
            .header("x-org-id", organization)
            .header("x-actor-id", actor);
        req = match &self.credential {
            Auth::Bearer(token) => req.bearer_auth(token.expose_secret()),
            Auth::Anonymous => return Err(Error::Invalid("authentication is required".into())),
        };
        if let Some(env) = &self.environment {
            req = req.header("x-testing-environment-key", env.0.expose_secret());
        }
        self.send_response(req).await
    }

    async fn scoped_request<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        organization: &str,
        actor: &str,
        body: Option<B>,
    ) -> Result<R> {
        self.request(
            method,
            path,
            body,
            Some((organization, actor, "waveform-client")),
        )
        .await
    }

    async fn scoped_empty_request<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        organization: &str,
        actor: &str,
        body: Option<B>,
    ) -> Result<()> {
        self.request_empty(
            method,
            path,
            body,
            Some((organization, actor, "waveform-client")),
        )
        .await
    }

    async fn request_empty<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<B>,
        scope: Option<(&str, &str, &str)>,
    ) -> Result<()> {
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("base URL cannot carry credentials".into()))?;
            segments
                .pop_if_empty()
                .extend(["api", "v1"])
                .extend(path.split('/'));
        }
        let mut req = self.http.request(method, url);
        req = match &self.credential {
            Auth::Anonymous => req,
            Auth::Bearer(token) => req.bearer_auth(token.expose_secret()),
        };
        if let Some(env) = &self.environment {
            req = req.header("x-testing-environment-key", env.0.expose_secret());
        }
        if let Some((org, actor, key)) = scope {
            req = req
                .header("x-org-id", org)
                .header("x-actor-id", actor)
                .header("idempotency-key", key);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        self.send_empty_response(req).await
    }

    async fn speech<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        organization: &str,
        actor: &str,
        body: &B,
        idempotency: &str,
    ) -> Result<R> {
        if organization.is_empty() || actor.is_empty() || idempotency.len() < 8 {
            return Err(Error::Invalid(
                "organization, actor and an idempotency key are required".into(),
            ));
        }
        self.request(
            method,
            path,
            Some(body),
            Some((organization, actor, idempotency)),
        )
        .await
    }
    async fn request<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<B>,
        scope: Option<(&str, &str, &str)>,
    ) -> Result<R> {
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("base URL cannot carry credentials".into()))?;
            segments
                .pop_if_empty()
                .extend(["api", "v1"])
                .extend(path.split('/'));
        }
        let mut req = self.http.request(method, url);
        req = match &self.credential {
            Auth::Anonymous => req,
            Auth::Bearer(token) => req.bearer_auth(token.expose_secret()),
        };
        if let Some(env) = &self.environment {
            req = req.header("x-testing-environment-key", env.0.expose_secret());
        }
        if let Some((org, actor, key)) = scope {
            req = req
                .header("x-org-id", org)
                .header("x-actor-id", actor)
                .header("idempotency-key", key);
            if let Some(id) = self.speech_request_id {
                req = req.header("x-request-id", id.to_string());
            }
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        self.send_response(req).await
    }

    async fn send_empty_response(&self, request: reqwest::RequestBuilder) -> Result<()> {
        let result = match request.send().await {
            Ok(response) => decode_empty_response(response).await,
            Err(error) => Err(Error::Transport(error)),
        };
        self.updater.after_request().await;
        result
    }

    async fn send_response<R: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<R> {
        let result = match request.send().await {
            Ok(response) => decode_response(response).await,
            Err(error) => Err(Error::Transport(error)),
        };
        self.updater.after_request().await;
        result
    }
}

fn validate_provider(provider: &str) -> Result<()> {
    if matches!(provider, "gemini" | "elevenlabs" | "openai" | "deepgram") {
        Ok(())
    } else {
        Err(Error::Invalid(
            "provider must be gemini, elevenlabs, openai, or deepgram".into(),
        ))
    }
}

async fn decode_empty_response(response: reqwest::Response) -> Result<()> {
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        let bytes = response.bytes().await?;
        let code = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "unstructured_response".into());
        Err(Error::Api {
            status: status.as_u16(),
            code,
        })
    }
}

async fn decode_response<R: DeserializeOwned>(mut response: reqwest::Response) -> Result<R> {
    const LIMIT: usize = 4 * 1024 * 1024;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|size| size > LIMIT as u64)
    {
        return Err(Error::Invalid("response exceeds 4 MiB".into()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > LIMIT {
            return Err(Error::Invalid("response exceeds 4 MiB".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let code = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "unstructured_response".into());
        return Err(Error::Api {
            status: status.as_u16(),
            code,
        });
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn voice_profile_serialization_and_legacy_response_compatibility()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let request = TtsRequest {
            text: "hello".into(),
            lang: None,
            provider_order: None,
            voice_profile: Some("puck".into()),
        };
        assert_eq!(
            serde_json::to_value(request)?,
            serde_json::json!({"text":"hello", "voice_profile":"puck"})
        );
        let legacy = serde_json::json!({"request_id":"fixture", "file_url":"https://example.test/audio", "media_type":"audio/mpeg", "provider":"gemini", "duration_ms":100});
        assert!(
            serde_json::from_value::<TtsResponse>(legacy.clone())?
                .voice_profile
                .is_none()
        );
        let mut modern = legacy;
        modern["voice_profile"] = serde_json::json!({"id":"puck", "revision":3});
        assert_eq!(
            serde_json::from_value::<TtsResponse>(modern)?
                .voice_profile
                .map(|p| p.revision),
            Some(3)
        );
        Ok(())
    }

    #[test]
    fn debug_redacts_credential() {
        let c = Client::new(
            "http://localhost",
            Auth::Bearer(SecretString::from("oat_private")),
        )
        .expect("valid");
        assert!(!format!("{c:?}").contains("oat_private"));
    }
    #[test]
    fn test_keys_are_explicit() {
        assert!(TestEnvironmentKey::new("A".repeat(32)).is_ok());
    }

    #[test]
    fn job_terminal_state_is_explicit() {
        let mut job = Job {
            voice_profile: None,
            id: "job".into(),
            operation: "tts".into(),
            status: "running".into(),
            first_line: String::new(),
            duration_ms: None,
            provider: None,
            error_code: None,
            created_at: String::new(),
            finished_at: None,
        };
        assert!(!job.is_terminal());
        job.status = "failed".into();
        assert!(job.is_terminal());
        job.status = "completed".into();
        assert!(job.is_terminal());
    }

    #[tokio::test]
    async fn login_and_history_preserve_test_selection()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{body_json, header, method, path},
        };
        let server = MockServer::start().await;
        let root = "A".repeat(32);
        Mock::given(method("POST")).and(path("/api/v1/auth/login"))
            .and(header("x-testing-environment-key", root.as_str()))
            .and(body_json(serde_json::json!({"slt":"oac_one_use"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token":"oat_test", "refresh_token":"ort_test", "token_type":"Bearer", "expires_in":1800,"scope":"", "actor":{"principal_id":"00000000-0000-0000-0000-000000000001","type":"carbon","public_id":"12345678"}
            }))).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/jobs"))
            .and(header("x-testing-environment-key", root.as_str()))
            .and(header("authorization", "Bearer oat_test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"items":[],"next_cursor":null})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::new(&server.uri(), Auth::Anonymous)?
            .with_auto_update(false)
            .with_test_environment(TestEnvironmentKey::new(root)?);
        let session = client.login("oac_one_use").await?;
        assert!(
            client
                .with_bearer(session.access_token)
                .jobs("tos", "actor", None)
                .await?
                .items
                .is_empty()
        );
        Ok(())
    }
    fn job_json(id: uuid::Uuid, status: &str) -> serde_json::Value {
        serde_json::json!({"id":id,"operation":"tts","status":status,"first_line":"hello","duration_ms":1234,"provider":"gemini","error_code":null,"created_at":"2026-09-08T00:00:00Z","finished_at":null})
    }

    #[tokio::test]
    async fn explicit_speech_id_supports_polling_before_the_job_row_exists()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{header, method, path},
        };
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        Mock::given(method("POST")).and(path("/api/v1/tts")).and(header("x-request-id",id.to_string()))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(40)).set_body_json(serde_json::json!({"request_id":id,"file_url":"https://briefcase.test/files/test.mp3","temporary_url":null,"provider":"gemini","duration_ms":1234,"media_type":"audio/mpeg"}))).expect(1).mount(&server).await;
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/jobs/{id}")))
            .respond_with(move |_: &wiremock::Request| {
                match observed.fetch_add(1, Ordering::SeqCst) {
                    0 => ResponseTemplate::new(404)
                        .set_body_json(serde_json::json!({"error":{"code":"not_found"}})),
                    1 => ResponseTemplate::new(200).set_body_json(job_json(id, "running")),
                    _ => ResponseTemplate::new(200).set_body_json(job_json(id, "completed")),
                }
            })
            .expect(3)
            .mount(&server)
            .await;
        let client = Client::new(&server.uri(), Auth::Anonymous)?
            .with_auto_update(false)
            .with_bearer("oat_test");
        let speech = client.with_speech_request_id(id)?;
        assert!(client.with_speech_request_id(uuid::Uuid::nil()).is_err());
        let request = TtsRequest {
            text: "hello".into(),
            voice_profile: None,
            lang: None,
            provider_order: None,
        };
        let id_text = id.to_string();
        let (response, job) = tokio::join!(
            speech.tts("tos", "actor", &request, "poll-fixture-key"),
            client.wait_for_job(
                &id_text,
                "tos",
                "actor",
                Duration::from_secs(2),
                Duration::from_millis(1)
            )
        );
        assert_eq!(response?.request_id, id_text);
        assert_eq!(job?.id, id_text);
        assert_eq!(count.load(Ordering::SeqCst), 3);
        Ok(())
    }

    #[tokio::test]
    async fn polling_deadline_includes_a_slow_http_response()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_json(job_json(id, "completed")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::new(&server.uri(), Auth::Anonymous)?
            .with_auto_update(false)
            .with_bearer("oat_test");
        let result = client
            .wait_for_job(
                &id.to_string(),
                "tos",
                "actor",
                Duration::from_millis(30),
                Duration::from_millis(1),
            )
            .await;
        assert!(
            matches!(result, Err(Error::Invalid(message)) if message == "job polling timed out")
        );
        Ok(())
    }
}
