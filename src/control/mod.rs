//! Account APIs (history, settings, keys, Briefcase access), discovery, the
//! Accounts webhook and operator maintenance.
//!
//! Every route authenticates with Silicon Accounts. Routes that manage account
//! data accept `?account=c:…|si:…|<uuid>`: a Carbon may select a Silicon it is
//! the custodian of; anyone may select themselves; nothing else is allowed.

#[cfg(test)]
mod account_tests;
mod accounts;
mod audio;
pub mod contracts;
mod events;
mod jobs;
mod reporting;
mod sessions;
mod source_target;
mod storage;
#[cfg(test)]
mod tests;
mod voices;
pub(crate) mod webhooks;

use std::{borrow::Cow, sync::Arc};

pub(crate) use accounts::{key_context, legacy_key_context};

use axum::{
    Json, Router,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http::{HeaderMap, StatusCode};
use secrecy::SecretString;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    application::ports::{AccountsError, AccountsPort as _},
    config::Settings,
    domain::{
        auth::{
            AuthFailure, AuthorizationRequest, AuthorizedActor, InboundCredentials, WaveformAction,
        },
        identity::ActorKind,
    },
    infrastructure::{
        authenticator::AccountsAuthenticator,
        briefcase::BriefcaseHttp,
        directory::{AccountDirectory, AccountRecord, Confirm, Selection, SweepReport},
        held_audio::PostgresHeldAudio,
        storage_proofs::StorageProofs,
        vault::Vault,
    },
};

/// Accounts asked about per run of the background account check (one run a
/// minute, the lookup budget's share for it).
const SWEEP_BATCH: i64 = 50;

/// Shared infrastructure for the control APIs.
pub struct ControlState {
    pool: PgPool,
    mail: Option<reporting::Mail>,
    station: Option<space_station::SpaceClient>,
    app_id: String,
    accounts_url: String,
    public_url: url::Url,
    authenticator: AccountsAuthenticator,
    storage_proofs: Option<StorageProofs>,
    briefcase: Arc<BriefcaseHttp>,
    held_audio: PostgresHeldAudio,
    vault: Option<Vault>,
    webhook_secrets: Vec<SecretString>,
}

impl ControlState {
    /// Wires the control APIs.
    ///
    /// # Errors
    /// Returns a redacted error for unusable secret or mail settings.
    pub fn new(
        pool: PgPool,
        settings: &Settings,
        authenticator: AccountsAuthenticator,
        storage_proofs: Option<StorageProofs>,
        briefcase: Arc<BriefcaseHttp>,
        held_audio: PostgresHeldAudio,
    ) -> Result<Self, &'static str> {
        let vault = settings
            .control
            .encryption_key
            .as_ref()
            .map(Vault::new)
            .transpose()?;
        Ok(Self {
            mail: reporting::Mail::from_env()?,
            station: crate::telemetry::from_environment(),
            pool,
            app_id: settings.accounts.app_id.clone(),
            accounts_url: settings.accounts.issuer(),
            public_url: settings.public.base_url.clone(),
            authenticator,
            storage_proofs,
            briefcase,
            held_audio,
            vault,
            webhook_secrets: settings
                .accounts
                .webhook_secret
                .iter()
                .chain(settings.accounts.previous_webhook_secret.iter())
                .cloned()
                .collect(),
        })
    }

    fn vault(&self) -> Result<&Vault, ControlError> {
        self.vault.as_ref().ok_or_else(|| {
            ControlError::unavailable(
                "secret_storage_unavailable",
                "Waveform has no WAVEFORM_ENCRYPTION_KEY configured, so it cannot store secrets.",
            )
        })
    }

    fn storage_proofs(&self) -> Result<&StorageProofs, ControlError> {
        self.storage_proofs.as_ref().ok_or_else(|| {
            ControlError::unavailable(
                "secret_storage_unavailable",
                "Waveform has no WAVEFORM_ENCRYPTION_KEY configured, so it cannot hold Briefcase access.",
            )
        })
    }

    fn directory(&self) -> &AccountDirectory {
        self.authenticator.directory()
    }

    /// One batch of the background account check: refreshes the accounts
    /// Silicon Accounts was not asked about for a day and deletes Waveform's
    /// data for the ones it reports deleted (see `AccountDirectory::sweep`).
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn sweep_accounts(&self) -> Result<SweepReport, ControlError> {
        Ok(self.directory().sweep(SWEEP_BATCH).await?)
    }

    /// Authenticates the caller for one action.
    async fn caller(
        &self,
        headers: &HeaderMap,
        action: WaveformAction,
    ) -> Result<Caller, ControlError> {
        let credentials = crate::api::headers::parse_credentials(headers)
            .map_err(ControlError::authentication)?;
        let actor = self
            .authenticator
            .authorize(AuthorizationRequest {
                credentials: credentials.clone(),
                action,
                request_id: crate::api::headers::request_id(headers),
            })
            .await
            .map_err(ControlError::accounts)?;
        let record = self
            .directory()
            .load(&actor.account.uuid)
            .await?
            .ok_or_else(|| {
                ControlError::unavailable("database_unavailable", "The account record is missing.")
            })?;
        Ok(Caller {
            actor,
            credentials,
            record,
        })
    }

    /// Like [`Self::caller`], and for bearer tokens also asks Silicon Accounts
    /// whether the sign-in is still live (sensitive changes see revocation at once).
    async fn fresh_caller(
        &self,
        headers: &HeaderMap,
        action: WaveformAction,
    ) -> Result<Caller, ControlError> {
        let caller = self.caller(headers, action).await?;
        self.ensure_live(&caller).await?;
        Ok(caller)
    }

    /// Asks Silicon Accounts whether a bearer caller's sign-in is still live.
    async fn ensure_live(&self, caller: &Caller) -> Result<(), ControlError> {
        if let InboundCredentials::Bearer(token) = &caller.credentials {
            let active = self
                .authenticator
                .gateway()
                .introspect_active(token.expose_secret())
                .await
                .map_err(ControlError::accounts)?;
            if !active {
                return Err(ControlError::authentication(AuthFailure::TokenInactive));
            }
        }
        Ok(())
    }

    /// Resolves `?account=` to the account whose data a route acts on. A Carbon
    /// may name a Silicon it looks after: reads (`Confirm::Recent`) trust a
    /// custodian Silicon Accounts confirmed in the last ten minutes, writes
    /// (`Confirm::Always`) ask Silicon Accounts every time.
    async fn target(
        &self,
        caller: &Caller,
        selector: Option<&str>,
        confirm: Confirm,
    ) -> Result<Target, ControlError> {
        let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(Target {
                record: caller.record.clone(),
                as_custodian: false,
            });
        };
        if selector == caller.record.uuid.as_str()
            || caller.record.current_id.as_deref() == Some(selector)
        {
            return Ok(Target {
                record: caller.record.clone(),
                as_custodian: false,
            });
        }
        let not_custodian = || {
            ControlError::forbidden(
                "not_custodian",
                format!(
                    "{} is not the custodian of {selector}. Only a Silicon's custodian can manage its Waveform history, settings, keys and Briefcase access.",
                    caller.display_id()
                ),
            )
        };
        if caller.record.kind != ActorKind::Carbon {
            return Err(not_custodian());
        }
        match self
            .directory()
            .select_for_custodian(&caller.record.uuid, selector, confirm)
            .await
            .map_err(ControlError::accounts)?
        {
            Selection::Custodian(record) => Ok(Target {
                record: *record,
                as_custodian: true,
            }),
            Selection::NotCustodian => Err(not_custodian()),
            Selection::NotFound => Err(ControlError::not_found(
                "account_not_found",
                format!("No Silicon Accounts account has the id or uuid {selector}."),
            )),
            Selection::Throttled => Err(ControlError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "account_lookup_limit",
                "Naming this account needs a lookup at Silicon Accounts, and you reached the limit of 30 such lookups a minute.",
            )
            .with_hint("Wait a minute and retry, or name a Silicon by the uuid `waveform silicons` (GET /api/v1/silicons) shows; a Silicon Waveform already knows needs no lookup.")
            .with_retry_after(60)),
        }
    }
}

/// The authenticated caller of a control route.
pub(crate) struct Caller {
    actor: AuthorizedActor,
    credentials: InboundCredentials,
    record: AccountRecord,
}

impl Caller {
    fn display_id(&self) -> String {
        self.record
            .current_id
            .clone()
            .unwrap_or_else(|| self.record.uuid.to_string())
    }
}

/// The account a control route acts on (the caller, or a Silicon it looks after).
pub(crate) struct Target {
    record: AccountRecord,
    as_custodian: bool,
}

impl Target {
    const fn storage_key(&self) -> Uuid {
        self.record.storage_key
    }
}

/// Routes merged under the main API's admission, deadline and logging layers.
pub fn router(state: Arc<ControlState>) -> Router {
    Router::new()
        .route("/api/contracts", get(contracts::describe))
        .route("/api/v1/contracts", get(contracts::describe))
        .route("/api/v1/accounts", get(sessions::discovery))
        .route("/api/v1/auth/me", get(sessions::me))
        .route("/api/v1/me", get(sessions::me))
        .route("/api/v1/silicons", get(sessions::silicons))
        .route("/api/v1/telemetry", post(events::record))
        .route("/api/v1/reports", post(reporting::submit))
        .route(
            "/api/v1/storage-authorizations",
            post(storage::approve)
                .get(storage::status)
                .delete(storage::revoke),
        )
        .route("/api/v1/stt/source-target", post(source_target::prepare))
        .route("/api/v1/audio/{request_id}", get(audio::download))
        .route("/api/v1/jobs", get(jobs::list))
        .route("/api/v1/jobs/{job_id}", get(jobs::get))
        .route(
            "/api/v1/preferences",
            get(accounts::preferences).patch(accounts::update_preferences),
        )
        .route("/api/v1/voice-profiles", get(voices::list))
        .route("/api/v1/provider-keys", get(accounts::provider_keys))
        .route(
            "/api/v1/provider-keys/{provider}",
            axum::routing::put(accounts::put_provider_key).delete(accounts::delete_provider_key),
        )
        .route("/webhooks/accounts", post(webhooks::receive))
        .route("/api/v1/iam", axum::routing::any(retired))
        .route("/api/v1/auth/login", axum::routing::any(retired))
        .route("/api/v1/auth/refresh", axum::routing::any(retired))
        .route("/api/v1/auth/logout", axum::routing::any(retired))
        .route("/api/v1/testing-environment", axum::routing::any(retired))
        .route("/api/v1/testing-environments", axum::routing::any(retired))
        .route("/webhooks/", axum::routing::any(retired))
        .route("/webhook/", axum::routing::any(retired))
        .with_state(state)
}

/// IAM- and Honeycomb-era routes answer 410 with what replaced them.
async fn retired() -> ControlError {
    ControlError::new(
        StatusCode::GONE,
        "route_retired",
        "This route was removed when Waveform moved to Silicon Accounts.",
    )
    .with_hint("CLIs sign in with Silicon Accounts directly (`waveform login`, or `waveform login --slt-stdin` for Silicons); apps acting for an account send Authorization: Proof sap_…; discovery is GET /api/v1/accounts.")
}

/// Control-route error: `{"error":{"code","message","hint"?}}`.
pub(crate) struct ControlError {
    status: StatusCode,
    code: &'static str,
    message: Cow<'static, str>,
    hint: Option<&'static str>,
    reason: Option<&'static str>,
    retry_after: Option<u32>,
}

impl ControlError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            hint: None,
            reason: None,
            retry_after: None,
        }
    }

    const fn with_hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }

    const fn with_retry_after(mut self, seconds: u32) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    fn bad_request(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// `?account=` on a speech route: speech always runs as the caller.
    fn speech_runs_as_yourself() -> Self {
        Self::bad_request(
            "speech_runs_as_yourself",
            "Speech always runs as the account that calls Waveform, so `account` cannot be given here: a custodian manages a Silicon's history, settings, keys and Briefcase access, but never speaks as it or fetches its audio.",
        )
        .with_hint("Remove ?account= and call as the account itself (a Silicon signs in with `silicon-accounts login --app waveform -q | waveform login --slt-stdin`).")
    }

    fn forbidden(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    fn not_found(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    fn unavailable(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
    }

    fn authentication(failure: AuthFailure) -> Self {
        let (status, code) = if failure.is_forbidden() {
            (StatusCode::FORBIDDEN, "forbidden")
        } else {
            (StatusCode::UNAUTHORIZED, "unauthenticated")
        };
        let mut error = Self::new(status, code, failure.message()).with_hint(failure.hint());
        error.reason = Some(failure.reason());
        error
    }

    fn accounts(error: AccountsError) -> Self {
        match error {
            AccountsError::Rejected(failure) => Self::authentication(failure),
            AccountsError::Unavailable => Self::unavailable(
                "dependency_unavailable",
                "Silicon Accounts is temporarily unavailable.",
            ),
            AccountsError::Timeout => Self::new(
                StatusCode::GATEWAY_TIMEOUT,
                "dependency_timeout",
                "Silicon Accounts did not answer in time.",
            ),
        }
    }
}

impl From<sqlx::Error> for ControlError {
    fn from(_: sqlx::Error) -> Self {
        Self::unavailable(
            "database_unavailable",
            "Waveform's database is unavailable.",
        )
    }
}

impl IntoResponse for ControlError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": {"code": self.code, "message": self.message}});
        if let Some(hint) = self.hint {
            body["error"]["hint"] = json!(hint);
        }
        if let Some(reason) = self.reason {
            body["error"]["reason"] = json!(reason);
        }
        let mut response = (self.status, Json(body)).into_response();
        let retry_after = self
            .retry_after
            .or((self.status == StatusCode::SERVICE_UNAVAILABLE).then_some(5));
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(http::header::RETRY_AFTER, http::HeaderValue::from(seconds));
        }
        response
    }
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, ControlError> {
    crate::api::headers::optional_header(headers, name).map_err(|_| {
        ControlError::bad_request(
            "invalid_header",
            format!("Header {name} must be sent at most once, as visible text."),
        )
    })
}
