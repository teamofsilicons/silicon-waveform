//! The Waveform API client: stateless, cheap to clone (clones share one connection pool).

use reqwest::{Client as HttpClient, Method, header::HeaderValue};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Serialize, de::DeserializeOwned};
use std::time::{Duration, Instant};
use url::Url;

use crate::error::{Error, Result, api_error};
use crate::types::{
    Account, AccountsInfo, BriefcaseAccess, Capabilities, HeldAudio, Job, JobPage, LoginStatus, Me,
    Preferences, PreferencesUpdate, ProviderKeyList, ReportReceipt, SourceFolder, SttRequest,
    SttResponse, TtsRequest, TtsResponse, VoiceProfile, validate_provider,
};

const JSON_LIMIT: usize = 4 * 1024 * 1024;
const AUDIO_LIMIT: usize = 64 * 1024 * 1024;

/// How requests authenticate.
#[derive(Clone)]
pub enum Auth {
    /// No credential: public routes only (capabilities, contracts, sign-in discovery).
    Anonymous,
    /// A Silicon Accounts access token issued to Waveform (`aud` = `waveform`): the account's
    /// own sign-in, from [`crate::signin::SignIn`] or an app's own sign-in flow.
    Bearer(SecretString),
    /// A Silicon Accounts User verification proof (`sap_…`) that another app holds for an
    /// account, issued for `receiving_app` `waveform`. Waveform accepts it on the speech
    /// routes only (`tts`, `stt`, `stt_source_folder`, `held_audio`) and acts as its account.
    Proof(SecretString),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anonymous => f.write_str("Anonymous"),
            Self::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            Self::Proof(_) => f.write_str("Proof(<redacted>)"),
        }
    }
}

/// A Waveform API client.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    base: Url,
    credential: Auth,
    account: Option<String>,
    speech_request_id: Option<uuid::Uuid>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .field("credential", &self.credential)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// Per-call options of the request plumbing.
#[derive(Clone, Copy, Default)]
struct Call<'a> {
    /// Send no credential (public routes).
    anonymous: bool,
    /// Add `?account=` when the client selected an account.
    account_scoped: bool,
    /// `Idempotency-Key` header.
    idempotency: Option<&'a str>,
    /// Add `X-Request-ID` when the client has a speech request id.
    speech: bool,
}

impl Client {
    /// Builds a client for a Waveform API origin (`https://…`, or `http://` for this machine
    /// only, e.g. `http://127.0.0.1:4161`).
    ///
    /// # Errors
    /// An URL that is not an origin, uses another scheme, carries credentials, a path, a query
    /// or a fragment, or uses plain http for a host other than this machine.
    pub fn new(base: &str, credential: Auth) -> Result<Self> {
        let url = Url::parse(base.trim()).map_err(|error| {
            Error::Invalid(format!(
                "The Waveform URL `{base}` is not a valid absolute URL ({error}); use e.g. {}.",
                crate::DEFAULT_API_URL
            ))
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(Error::Invalid(format!(
                "The Waveform URL `{base}` must use https (or http for this machine)."
            )));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Error::Invalid(format!(
                "The Waveform URL `{base}` must be an origin only (scheme, host and port), without credentials, path, query or fragment."
            )));
        }
        if url.scheme() == "http" && !crate::is_loopback(&url) {
            return Err(Error::Invalid(format!(
                "The Waveform URL `{base}` uses plain http for a host that is not this machine, so tokens would travel unencrypted; use https."
            )));
        }
        Ok(Self {
            http: HttpClient::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(650))
                .user_agent(format!("silicon-waveform-client/{}", crate::VERSION))
                .build()?,
            base: url,
            credential,
            account: None,
            speech_request_id: None,
        })
    }

    /// The API origin this client calls.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    /// The same client, authenticating with an account's Waveform access token.
    #[must_use]
    pub fn with_bearer(&self, access_token: impl Into<String>) -> Self {
        Self {
            credential: Auth::Bearer(SecretString::from(access_token.into())),
            ..self.clone()
        }
    }

    /// The same client, authenticating with a User verification proof (`sap_…`) issued for
    /// `receiving_app` `waveform`. Speech routes only.
    ///
    /// # Errors
    /// A value that is not a proof token.
    pub fn with_proof(&self, proof: impl Into<String>) -> Result<Self> {
        let proof = proof.into();
        let proof = proof.trim();
        if !proof.starts_with("sap_")
            || proof.len() > 16_384
            || !proof.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(Error::Invalid(
                "A proof must be a Silicon Accounts User verification proof token (sap_…, visible ASCII). Get one with POST /v1/proofs/user-verification for receiving_app waveform; it is not an access or refresh token.".into(),
            ));
        }
        Ok(Self {
            credential: Auth::Proof(SecretString::from(proof.to_owned())),
            ..self.clone()
        })
    }

    /// The same client with another credential.
    #[must_use]
    pub fn with_auth(&self, credential: Auth) -> Self {
        Self {
            credential,
            ..self.clone()
        }
    }

    /// The same client, acting on another account's data: a Carbon selects a Silicon it is the
    /// custodian of (`si:scout` or its uuid) to read its history and manage its settings,
    /// provider keys and Briefcase access. Waveform refuses anyone else (`403 not_custodian`).
    /// Speech always runs as the credential's own account, so speech methods refuse a client
    /// with a selected account.
    ///
    /// # Errors
    /// An empty selector or one longer than 128 characters.
    pub fn for_account(&self, account: impl Into<String>) -> Result<Self> {
        let account = account.into().trim().to_owned();
        if account.is_empty()
            || account.len() > 128
            || account
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(Error::Invalid(
                "Select an account by its id (c:… or si:…) or uuid: 1-128 visible characters."
                    .into(),
            ));
        }
        Ok(Self {
            account: Some(account),
            ..self.clone()
        })
    }

    /// The same client acting on the credential's own account again.
    #[must_use]
    pub fn for_self(&self) -> Self {
        Self {
            account: None,
            ..self.clone()
        }
    }

    /// The account selected with [`Client::for_account`], if any.
    #[must_use]
    pub fn selected_account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    /// Sets the request/job UUID of one logical speech operation, so another task can poll
    /// [`Client::wait_for_job`] while the synchronous call runs. Use a fresh UUID for another
    /// operation; on retries the same idempotency key keeps its original job id.
    ///
    /// # Errors
    /// The nil UUID.
    pub fn with_speech_request_id(&self, id: uuid::Uuid) -> Result<Self> {
        if id.is_nil() {
            return Err(Error::Invalid(
                "The speech request id must not be the nil UUID.".into(),
            ));
        }
        Ok(Self {
            speech_request_id: Some(id),
            ..self.clone()
        })
    }

    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| Error::Invalid("The Waveform URL cannot carry a path.".into()))?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(key, value)| (*key, value.as_str())));
        }
        Ok(url)
    }

    fn api(&self, path: &[&str], call: Call<'_>, mut query: Vec<(&str, String)>) -> Result<Url> {
        if call.account_scoped
            && let Some(account) = &self.account
        {
            query.push(("account", account.clone()));
        }
        let mut segments = vec!["api", "v1"];
        segments.extend_from_slice(path);
        self.url(&segments, &query)
    }

    fn build(&self, method: Method, url: Url, call: Call<'_>) -> Result<reqwest::RequestBuilder> {
        let mut request = self
            .http
            .request(method, url)
            .header("x-waveform-api-version", "v1")
            .header("x-waveform-protocol-version", "1");
        if !call.anonymous {
            request = match &self.credential {
                Auth::Anonymous => request,
                Auth::Bearer(token) => request.header(
                    reqwest::header::AUTHORIZATION,
                    sensitive(&format!("Bearer {}", token.expose_secret()))?,
                ),
                Auth::Proof(proof) => request.header(
                    reqwest::header::AUTHORIZATION,
                    sensitive(&format!("Proof {}", proof.expose_secret()))?,
                ),
            };
        }
        if let Some(key) = call.idempotency {
            request = request.header("idempotency-key", key);
        }
        if call.speech
            && let Some(id) = self.speech_request_id
        {
            request = request.header("x-request-id", id.to_string());
        }
        Ok(request)
    }
}

fn sensitive(value: &str) -> Result<HeaderValue> {
    let mut header = HeaderValue::from_str(value).map_err(|_| {
        Error::Invalid("The credential contains characters that cannot be sent in a header.".into())
    })?;
    header.set_sensitive(true);
    Ok(header)
}

// ---- request plumbing -------------------------------------------------------------------

impl Client {
    async fn send_json<R: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        call: Call<'_>,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<R> {
        let mut request = self.build(method, url, call)?;
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let (bytes, _) = read_body(response, JSON_LIMIT).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn send_empty(
        &self,
        method: Method,
        url: Url,
        call: Call<'_>,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<()> {
        let mut request = self.build(method, url, call)?;
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        read_body(response, JSON_LIMIT).await.map(|_| ())
    }

    fn refuse_selected_account_for_speech(&self) -> Result<()> {
        match &self.account {
            Some(account) => Err(Error::Invalid(format!(
                "Speech always runs as the account that holds the credential; this client selected {account} with for_account, which only reads history and manages settings. A custodian never speaks as its Silicon: use a client without for_account (Client::for_self)."
            ))),
            None => Ok(()),
        }
    }
}

/// Reads a bounded body; a non-success status becomes [`Error::Api`].
async fn read_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(Vec<u8>, reqwest::header::HeaderMap)> {
    let status = response.status();
    let headers = response.headers().clone();
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(Error::Invalid(format!(
            "Waveform's answer is larger than this client accepts ({} MiB).",
            limit / (1024 * 1024)
        )));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(Error::Invalid(format!(
                "Waveform's answer is larger than this client accepts ({} MiB).",
                limit / (1024 * 1024)
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let retry_after = headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        return Err(api_error(status.as_u16(), retry_after, &bytes));
    }
    Ok((bytes, headers))
}

fn check_speech_key(idempotency: &str) -> Result<()> {
    if (8..=255).contains(&idempotency.len()) && idempotency.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(())
    } else {
        Err(Error::Invalid(
            "A speech request needs an idempotency key of 8-255 visible ASCII characters; reuse it to retry the same request safely.".into(),
        ))
    }
}

fn check_uuid(value: &str, what: &str) -> Result<()> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| Error::Invalid(format!("`{value}` is not a {what} (a UUID).")))
}

// ---- discovery and the signed-in account ------------------------------------------------

impl Client {
    /// `GET /api/v1/accounts`: Waveform's app id, the Silicon Accounts it trusts and its docs.
    /// Public; sends no credential.
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn accounts(&self) -> Result<AccountsInfo> {
        let call = Call {
            anonymous: true,
            ..Call::default()
        };
        let url = self.api(&["accounts"], call, Vec::new())?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// `GET /api/v1/capabilities`: languages, formats, provider controls. Public.
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn capabilities(&self) -> Result<Capabilities> {
        let call = Call {
            anonymous: true,
            ..Call::default()
        };
        let url = self.api(&["capabilities"], call, Vec::new())?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// `GET /api/contracts`: API versions, compatibility matrix and deprecation policy. Public,
    /// and still served after a version sunsets.
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn contracts(&self) -> Result<serde_json::Value> {
        let call = Call {
            anonymous: true,
            ..Call::default()
        };
        let url = self.url(&["api", "contracts"], &[])?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// `GET /api/v1/me`: the account the access token belongs to (bearer tokens only).
    ///
    /// # Errors
    /// `401` with the exact reason when the token is refused; transport and decoding failures.
    pub async fn me(&self) -> Result<Me> {
        let url = self.api(&["me"], Call::default(), Vec::new())?;
        self.send_json(Method::GET, url, Call::default(), None::<&()>)
            .await
    }

    /// Asks Waveform whether the credential works right now. An anonymous client answers
    /// `authenticated: false` without a request; a refused token (401/403) answers
    /// `authenticated: false` with Waveform's reason. Never refreshes anything.
    ///
    /// # Errors
    /// Transport, server and decoding failures (anything but a refusal).
    pub async fn login_status(&self) -> Result<LoginStatus> {
        if matches!(self.credential, Auth::Anonymous) {
            return Ok(LoginStatus::default());
        }
        match self.me().await {
            Ok(me) => Ok(LoginStatus {
                authenticated: true,
                account: Some(me.account),
                expires_at: me.expires_at,
                reason: None,
                message: None,
            }),
            Err(Error::Api(error)) if matches!(error.status, 401 | 403) => Ok(LoginStatus {
                authenticated: false,
                reason: error.reason.clone().or_else(|| Some(error.code.clone())),
                message: Some(error.message.clone()),
                ..LoginStatus::default()
            }),
            Err(error) => Err(error),
        }
    }

    /// `GET /api/v1/silicons`: the Silicons the signed-in Carbon looks after that Waveform
    /// knows (empty for a Silicon).
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn silicons(&self) -> Result<Vec<Account>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<Account>,
        }
        let url = self.api(&["silicons"], Call::default(), Vec::new())?;
        let items: Items = self
            .send_json(Method::GET, url, Call::default(), None::<&()>)
            .await?;
        Ok(items.items)
    }
}

// ---- speech -----------------------------------------------------------------------------

impl Client {
    /// `POST /api/v1/tts`: synthesizes speech synchronously and stores the MP3 in the account's
    /// Briefcase files (`file_url`), or, when Waveform cannot, holds it (`audio_url`,
    /// `storage.reason`). Reuse `idempotency` to retry the same request without running a
    /// provider twice.
    ///
    /// # Errors
    /// A selected account (custodians never speak as their Silicons), a bad idempotency key,
    /// then Waveform's refusals (provider failures name the provider and a safe reason).
    pub async fn tts(&self, request: &TtsRequest, idempotency: &str) -> Result<TtsResponse> {
        self.refuse_selected_account_for_speech()?;
        check_speech_key(idempotency)?;
        let call = Call {
            idempotency: Some(idempotency),
            speech: true,
            ..Call::default()
        };
        let url = self.api(&["tts"], call, Vec::new())?;
        self.send_json(Method::POST, url, call, Some(request)).await
    }

    /// `POST /api/v1/stt`: transcribes a file in the account's own Briefcase files
    /// synchronously (automatic provider fallback).
    ///
    /// # Errors
    /// As [`Client::tts`]; `403 storage_authorization_required` when Waveform has no Briefcase
    /// access for the account (approve it with [`Client::approve_storage`]).
    pub async fn stt(&self, request: &SttRequest, idempotency: &str) -> Result<SttResponse> {
        self.refuse_selected_account_for_speech()?;
        check_speech_key(idempotency)?;
        let call = Call {
            idempotency: Some(idempotency),
            speech: true,
            ..Call::default()
        };
        let url = self.api(&["stt"], call, Vec::new())?;
        self.send_json(Method::POST, url, call, Some(request)).await
    }

    /// `POST /api/v1/stt/source-target`: makes sure the account's Waveform folder exists in its
    /// Briefcase drive (`apps/waveform`) and returns it. Upload an STT source there with
    /// Briefcase, then transcribe its permanent URL. No speech provider runs.
    ///
    /// # Errors
    /// A selected account, then Waveform's refusals.
    pub async fn stt_source_folder(&self) -> Result<SourceFolder> {
        self.refuse_selected_account_for_speech()?;
        let url = self.api(&["stt", "source-target"], Call::default(), Vec::new())?;
        self.send_json(
            Method::POST,
            url,
            Call::default(),
            Some(&serde_json::json!({})),
        )
        .await
    }

    /// `GET /api/v1/audio/{request_id}`: downloads an MP3 Waveform holds for the account (TTS
    /// answered `storage.status = "held"`), until `storage.expires_at`.
    ///
    /// # Errors
    /// A request id that is not a UUID; `404 audio_not_found` when Waveform holds nothing for
    /// this account under that id (stored in Briefcase, expired, or another account's).
    pub async fn held_audio(&self, request_id: &str) -> Result<HeldAudio> {
        check_uuid(request_id, "speech request id")?;
        let url = self.api(&["audio", request_id], Call::default(), Vec::new())?;
        let response = self
            .build(Method::GET, url, Call::default())?
            .send()
            .await?;
        let (bytes, headers) = read_body(response, AUDIO_LIMIT).await?;
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let filename = text("content-disposition").and_then(|value| {
            value
                .split("filename=")
                .nth(1)
                .map(|name| name.trim().trim_matches('"').to_owned())
                .filter(|name| {
                    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
                })
        });
        Ok(HeldAudio {
            bytes,
            media_type: text("content-type").unwrap_or_else(|| "audio/mpeg".into()),
            expires_at: text("x-waveform-audio-expires-at"),
            filename,
        })
    }
}

// ---- history ----------------------------------------------------------------------------

impl Client {
    /// The first page of the account's speech history (newest first, content-free).
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn jobs(&self, operation: Option<&str>) -> Result<JobPage> {
        self.jobs_page(operation, None, None).await
    }

    /// One page of the account's speech history: `operation` `tts`/`stt`, `limit` 1-100,
    /// `cursor` = the previous page's `next_cursor`.
    ///
    /// # Errors
    /// Bad filters, then transport, HTTP or decoding failures.
    pub async fn jobs_page(
        &self,
        operation: Option<&str>,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<JobPage> {
        if limit.is_some_and(|value| !(1..=100).contains(&value)) {
            return Err(Error::Invalid("A history page holds 1 to 100 jobs.".into()));
        }
        if operation.is_some_and(|value| !matches!(value, "tts" | "stt")) {
            return Err(Error::Invalid(
                "The job operation filter is tts or stt.".into(),
            ));
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
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["jobs"], call, query)?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// One of the account's jobs by id.
    ///
    /// # Errors
    /// A job id that is not a UUID; `404` for an unknown job (or another account's).
    pub async fn job(&self, job_id: &str) -> Result<Job> {
        check_uuid(job_id, "job id")?;
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["jobs", job_id], call, Vec::new())?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// Polls one job until it is `completed` or `failed` and returns the final record. The
    /// timeout covers each request as well as the waits; an initial 404 is retried while the
    /// speech request is still being authorised and its row created.
    ///
    /// # Errors
    /// Zero durations, a timeout, or any failure other than that initial 404.
    pub async fn wait_for_job(
        &self,
        job_id: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Job> {
        if timeout.is_zero() || poll_interval.is_zero() {
            return Err(Error::Invalid(
                "Job polling needs a non-zero timeout and interval.".into(),
            ));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::Invalid("The job polling timeout is out of range.".into()))?;
        let timed_out = || {
            Error::Invalid(format!(
                "Job {job_id} did not finish within {} ms; it may still be running. Poll it again with the same job id.",
                timeout.as_millis()
            ))
        };
        loop {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.job(job_id),
            )
            .await
            {
                Ok(Ok(job)) if job.is_terminal() => return Ok(job),
                Ok(Ok(_)) => {}
                Ok(Err(error)) if error.status() == Some(404) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(timed_out()),
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(timed_out());
            }
            tokio::time::sleep(poll_interval.min(deadline.saturating_duration_since(now))).await;
        }
    }
}

// ---- settings, provider keys, Briefcase access, reports --------------------------------

impl Client {
    /// The account's provider orders, default voice and telemetry choice, with Waveform's
    /// defaults.
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn preferences(&self) -> Result<Preferences> {
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["preferences"], call, Vec::new())?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// Changes the account's settings; fields left `None` stay as they are. A custodian
    /// changing a Silicon's settings must have a live sign-in (Waveform checks it with Silicon
    /// Accounts).
    ///
    /// # Errors
    /// An empty update, then Waveform's refusals (unknown voice, invalid order…).
    pub async fn update_preferences(&self, update: &PreferencesUpdate) -> Result<Preferences> {
        if update.is_empty() {
            return Err(Error::Invalid(
                "Nothing to change: set a voice profile, a TTS or STT order, or the telemetry choice."
                    .into(),
            ));
        }
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["preferences"], call, Vec::new())?;
        self.send_json(Method::PATCH, url, call, Some(update)).await
    }

    /// Turns Waveform's diagnostics for the account on or off.
    ///
    /// # Errors
    /// As [`Client::update_preferences`].
    pub async fn set_telemetry(&self, enabled: bool) -> Result<Preferences> {
        self.update_preferences(&PreferencesUpdate {
            telemetry_enabled: Some(enabled),
            ..PreferencesUpdate::default()
        })
        .await
    }

    /// The voice profiles and their provider mappings.
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn voice_profiles(&self) -> Result<Vec<VoiceProfile>> {
        #[derive(serde::Deserialize)]
        struct Catalog {
            items: Vec<VoiceProfile>,
        }
        let url = self.api(&["voice-profiles"], Call::default(), Vec::new())?;
        let catalog: Catalog = self
            .send_json(Method::GET, url, Call::default(), None::<&()>)
            .await?;
        Ok(catalog.items)
    }

    /// The providers the account saved a key for (never the keys).
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn provider_keys(&self) -> Result<ProviderKeyList> {
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["provider-keys"], call, Vec::new())?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// Saves (replaces) the account's key for one provider. Waveform encrypts it and never
    /// returns it; it checks with Silicon Accounts that the sign-in is live first.
    ///
    /// # Errors
    /// An unknown provider or an invalid key, then Waveform's refusals.
    pub async fn put_provider_key(&self, provider: &str, api_key: &str) -> Result<()> {
        validate_provider(provider)?;
        let mut check = crate::ProviderKeys::default();
        check.insert(provider, api_key)?;
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["provider-keys", provider], call, Vec::new())?;
        self.send_empty(
            Method::PUT,
            url,
            call,
            Some(&serde_json::json!({ "api_key": api_key })),
        )
        .await
    }

    /// Deletes the account's saved key for one provider (Waveform's own key is used again).
    ///
    /// # Errors
    /// An unknown provider, then Waveform's refusals.
    pub async fn delete_provider_key(&self, provider: &str) -> Result<()> {
        validate_provider(provider)?;
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["provider-keys", provider], call, Vec::new())?;
        self.send_empty(Method::DELETE, url, call, None::<&()>)
            .await
    }

    /// `POST /api/v1/storage-authorizations`: gives Waveform Briefcase access for the
    /// signed-in account (or replaces it). Waveform asks Silicon Accounts for a User
    /// verification proof for Briefcase from this access token and keeps it encrypted. Only the
    /// account itself can do this (not a custodian), with its own bearer token.
    ///
    /// # Errors
    /// A selected account, then Waveform's refusals.
    pub async fn approve_storage(&self) -> Result<BriefcaseAccess> {
        if let Some(account) = &self.account {
            return Err(Error::Invalid(format!(
                "Only the account itself can give Waveform Briefcase access, because the proof is made from its own sign-in; {account} gets it on its next direct Waveform request, or by running the approval while signed in as itself."
            )));
        }
        let url = self.api(&["storage-authorizations"], Call::default(), Vec::new())?;
        self.send_json(Method::POST, url, Call::default(), None::<&()>)
            .await
    }

    /// Status of Waveform's Briefcase access for the account (`active`, `revoked`, `missing`).
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn storage(&self) -> Result<BriefcaseAccess> {
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["storage-authorizations"], call, Vec::new())?;
        self.send_json(Method::GET, url, call, None::<&()>).await
    }

    /// Revokes Waveform's Briefcase access for the account at Silicon Accounts. Generated
    /// speech is then held by Waveform until access is given again.
    ///
    /// # Errors
    /// Transport, HTTP or decoding failures.
    pub async fn revoke_storage(&self) -> Result<BriefcaseAccess> {
        let call = Call {
            account_scoped: true,
            ..Call::default()
        };
        let url = self.api(&["storage-authorizations"], call, Vec::new())?;
        self.send_json(Method::DELETE, url, call, None::<&()>).await
    }

    /// `POST /api/v1/reports`: reports a bug to the Waveform maintainers, optionally with the
    /// pull request that fixes it (`https://github.com/teamofsilicons/silicon-waveform/pull/N`).
    /// Reuse `idempotency_key` (1-128 letters, digits, `-`, `_`) to retry the same report.
    ///
    /// # Errors
    /// A bad key, then Waveform's refusals (`429` after ten reports an hour, credentials in the
    /// text, an unrelated PR link).
    pub async fn report(
        &self,
        message: &str,
        pr: Option<&str>,
        idempotency_key: &str,
    ) -> Result<ReportReceipt> {
        if idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || !idempotency_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::Invalid(
                "A report idempotency key is 1-128 letters, digits, hyphens or underscores.".into(),
            ));
        }
        let call = Call {
            idempotency: Some(idempotency_key),
            ..Call::default()
        };
        let url = self.api(&["reports"], call, Vec::new())?;
        self.send_json(
            Method::POST,
            url,
            call,
            Some(&serde_json::json!({
                "message": message,
                "pr": pr,
                "client_version": crate::VERSION,
            })),
        )
        .await
    }
}
