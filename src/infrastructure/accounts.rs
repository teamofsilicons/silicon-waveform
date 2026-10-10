//! Silicon Accounts gateway: local access-token verification against a cached
//! key set, proof verification and issuance, introspection and account lookups.
//!
//! Every call goes through `silicon-accounts-client`, except proof refresh:
//! version 0.4.0 cannot attach an `Idempotency-Key` to `POST /v1/proofs/refresh`,
//! and a retried refresh must return the same rotated tokens, so that one call is
//! made directly with Waveform's app credentials.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};
use silicon_accounts_client::{
    AccountSummary, AccountsClient, AppClient, Claims, Error as ClientError, IssueUserVerification,
    IssuedProof, Jwks, ProofRef, ProofVerification, TokenError, VerifyOptions,
};
use tokio::sync::RwLock;

use crate::{
    application::ports::AccountsError, config::AccountsSettings, domain::auth::AuthFailure,
};

const USER_AGENT: &str = concat!("silicon-waveform/", env!("CARGO_PKG_VERSION"));
/// Keys are re-read at least this often even when every `kid` is known.
const JWKS_MAX_AGE: Duration = Duration::from_secs(3_600);
/// An unknown `kid` triggers at most one refetch per this interval.
const JWKS_REFETCH_INTERVAL: Duration = Duration::from_secs(30);
/// Introspection answers are reused this long (revocation visible within 30 s).
const INTROSPECTION_TTL: Duration = Duration::from_secs(30);
const INTROSPECTION_CACHE_LIMIT: usize = 10_000;
/// Accounts allows 600 lookups per minute per app; one replica uses at most half.
const LOOKUPS_PER_MINUTE: u32 = 300;

/// Why a proof could not be issued or refreshed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProofError {
    /// Accounts refused (invalid subject token, revoked or reused refresh token, inactive membership…).
    #[error("Silicon Accounts refused the proof")]
    Refused {
        /// Why, as Accounts says it: `error.details.reason` (e.g. `revoked_by_account`,
        /// `sign_in_revoked`) when it gives one, else the error code (e.g. `proof_expired`).
        reason: Option<String>,
    },
    /// Accounts could not be reached or failed.
    #[error("Silicon Accounts is unavailable")]
    Unavailable,
    /// Accounts did not answer in time.
    #[error("Silicon Accounts timed out")]
    Timeout,
}

impl ProofError {
    /// A refusal because the account (or the issuing app's owner) revoked the proof
    /// on purpose, as opposed to the proof ending with the sign-in it stood on.
    #[must_use]
    pub fn is_explicit_revocation(&self) -> bool {
        matches!(self, Self::Refused { reason: Some(reason) }
            if matches!(reason.as_str(), "revoked_by_account" | "revoked_by_owner"))
    }
}

#[derive(Default)]
struct JwksState {
    keys: Option<Arc<Jwks>>,
    fetched_at: Option<Instant>,
    /// Last refetch forced by a token naming an unknown `kid` (rate-limited).
    last_forced: Option<Instant>,
}

struct LookupBudget {
    window_started: Instant,
    used: u32,
}

/// Shared, cheap-to-clone handle to Silicon Accounts for Waveform's app.
pub struct AccountsGateway {
    client: AccountsClient,
    http: reqwest::Client,
    api_url: url::Url,
    app_id: String,
    app_secret: SecretString,
    issuer: String,
    timeout: Duration,
    jwks: RwLock<JwksState>,
    introspections: Mutex<HashMap<[u8; 32], (bool, Instant)>>,
    lookups: Mutex<LookupBudget>,
}

impl std::fmt::Debug for AccountsGateway {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountsGateway")
            .field("api_url", &self.api_url.as_str())
            .field("app_id", &self.app_id)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl AccountsGateway {
    /// Builds the gateway from validated settings.
    ///
    /// # Errors
    ///
    /// Returns a redacted reason when the client cannot be constructed.
    pub fn new(settings: &AccountsSettings) -> Result<Self, &'static str> {
        // Configuration already refused plain http for anything but loopback hosts.
        let insecure = settings.api_url.scheme() == "http";
        let client = AccountsClient::builder()
            .base_url(settings.api_url.as_str().trim_end_matches('/'))
            .allow_insecure_http(insecure)
            .timeout(settings.timeout)
            .connect_timeout(settings.timeout.min(Duration::from_secs(5)))
            .user_agent(USER_AGENT)
            .telemetry(false)
            .max_retries(1)
            .build()
            .map_err(|_| "could not construct the Silicon Accounts client")?;
        let http = reqwest::Client::builder()
            .timeout(settings.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| "could not construct the Silicon Accounts HTTP client")?;
        Ok(Self {
            client,
            http,
            api_url: settings.api_url.clone(),
            app_id: settings.app_id.clone(),
            app_secret: settings.app_secret.clone(),
            issuer: settings.issuer(),
            timeout: settings.timeout,
            jwks: RwLock::new(JwksState::default()),
            introspections: Mutex::new(HashMap::new()),
            lookups: Mutex::new(LookupBudget {
                window_started: Instant::now(),
                used: 0,
            }),
        })
    }

    /// Waveform's own app id.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The Accounts public URL tokens are issued by.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    fn app(&self) -> AppClient<'_> {
        self.client.as_app(
            self.app_id.clone(),
            self.app_secret.expose_secret().to_owned(),
        )
    }

    async fn bounded<T>(
        &self,
        call: impl Future<Output = Result<T, ClientError>>,
    ) -> Result<Result<T, ClientError>, AccountsError> {
        tokio::time::timeout(self.timeout, call)
            .await
            .map_err(|_| AccountsError::Timeout)
    }

    async fn key_set(&self, unknown_kid: bool) -> Result<Arc<Jwks>, AccountsError> {
        {
            let state = self.jwks.read().await;
            if let (Some(keys), Some(fetched_at)) = (&state.keys, state.fetched_at)
                && !unknown_kid
                && fetched_at.elapsed() < JWKS_MAX_AGE
            {
                return Ok(keys.clone());
            }
        }
        // Single flight: the write lock is held while fetching.
        let mut state = self.jwks.write().await;
        if let (Some(keys), Some(fetched_at)) = (&state.keys, state.fetched_at) {
            let recently_forced = state
                .last_forced
                .is_some_and(|forced| forced.elapsed() < JWKS_REFETCH_INTERVAL);
            if (!unknown_kid && fetched_at.elapsed() < JWKS_MAX_AGE)
                || (unknown_kid && recently_forced)
            {
                return Ok(keys.clone());
            }
        }
        if unknown_kid {
            state.last_forced = Some(Instant::now());
        }
        match self.bounded(self.client.jwks()).await {
            Ok(Ok(keys)) => {
                let keys = Arc::new(keys);
                state.keys = Some(keys.clone());
                state.fetched_at = Some(Instant::now());
                Ok(keys)
            }
            failure => {
                tracing::warn!(
                    timed_out = failure.is_err(),
                    "could not fetch the Silicon Accounts signing keys"
                );
                // A stale key set still verifies tokens signed by known keys.
                state.keys.clone().ok_or(AccountsError::Unavailable)
            }
        }
    }

    /// Verifies a Waveform access token locally: `EdDSA` signature by a published
    /// key, `aud` = Waveform, `iss` = the configured Accounts URL, `exp`/`nbf`.
    ///
    /// # Errors
    ///
    /// [`AccountsError::Rejected`] with the exact reason, or `Unavailable` when no
    /// key set could ever be fetched.
    pub async fn verify_access_token(&self, token: &str) -> Result<Claims, AccountsError> {
        let options = VerifyOptions::for_app(&self.app_id).with_issuer(self.issuer.clone());
        let keys = self.key_set(false).await?;
        match silicon_accounts_client::verify_access_token(&keys, token, &options) {
            Err(ClientError::Token(TokenError::UnknownKey { .. })) => {
                let refreshed = self.key_set(true).await?;
                silicon_accounts_client::verify_access_token(&refreshed, token, &options)
                    .map_err(|error| token_failure(&error))
            }
            other => other.map_err(|error| token_failure(&error)),
        }
    }

    /// The account behind an access token, as Accounts shows it to Waveform
    /// (`GET /v1/userinfo`): display name, photo and profile version.
    ///
    /// # Errors
    ///
    /// `Unavailable`/`Timeout` when Accounts cannot answer or refuses the token.
    pub async fn userinfo(
        &self,
        token: &str,
    ) -> Result<silicon_accounts_client::UserInfo, AccountsError> {
        self.bounded(self.app().userinfo(token))
            .await?
            .map_err(|_| AccountsError::Unavailable)
    }

    /// Asks Accounts whether a token is still active (sees revocation at once).
    /// Answers are cached for 30 seconds per token.
    ///
    /// # Errors
    ///
    /// `Unavailable`/`Timeout` when Accounts cannot answer.
    pub async fn introspect_active(&self, token: &str) -> Result<bool, AccountsError> {
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if let Ok(cache) = self.introspections.lock()
            && let Some((active, checked)) = cache.get(&digest)
            && checked.elapsed() < INTROSPECTION_TTL
        {
            return Ok(*active);
        }
        let answer = self.bounded(self.app().introspect(token)).await?;
        let active = answer.map_err(|_| AccountsError::Unavailable)?.active;
        if let Ok(mut cache) = self.introspections.lock() {
            if cache.len() >= INTROSPECTION_CACHE_LIMIT {
                cache.retain(|_, (_, checked)| checked.elapsed() < INTROSPECTION_TTL);
                if cache.len() >= INTROSPECTION_CACHE_LIMIT {
                    cache.clear();
                }
            }
            cache.insert(digest, (active, Instant::now()));
        }
        Ok(active)
    }

    /// Verifies a proof token with Waveform's credentials (`POST /v1/proofs/verify`).
    ///
    /// # Errors
    ///
    /// `Unavailable`/`Timeout` when Accounts cannot answer; an invalid proof is
    /// `Ok(ProofVerification::Invalid)`.
    pub async fn verify_proof(&self, token: &str) -> Result<ProofVerification, AccountsError> {
        self.bounded(self.app().verify_proof(token))
            .await?
            .map_err(|error| {
                tracing::warn!(
                    code = error.code(),
                    "proof verification failed at Silicon Accounts"
                );
                AccountsError::Unavailable
            })
    }

    fn spend_lookup(&self) -> Result<(), AccountsError> {
        let mut budget = self
            .lookups
            .lock()
            .map_err(|_| AccountsError::Unavailable)?;
        if budget.window_started.elapsed() >= Duration::from_secs(60) {
            budget.window_started = Instant::now();
            budget.used = 0;
        }
        if budget.used >= LOOKUPS_PER_MINUTE {
            tracing::warn!("Silicon Accounts lookup budget exhausted for this minute");
            return Err(AccountsError::Unavailable);
        }
        budget.used += 1;
        Ok(())
    }

    /// `GET /v1/accounts/{uuid}`; `Ok(None)` when no such account exists.
    ///
    /// # Errors
    ///
    /// `Unavailable` (including an exhausted lookup budget) or `Timeout`.
    pub async fn lookup(&self, uuid: &str) -> Result<Option<AccountSummary>, AccountsError> {
        self.spend_lookup()?;
        found(self.bounded(self.app().lookup(uuid)).await?)
    }

    /// `GET /v1/accounts/by-id/{id}`; `Ok(None)` when no account has this id.
    ///
    /// # Errors
    ///
    /// `Unavailable` (including an exhausted lookup budget) or `Timeout`.
    pub async fn lookup_by_id(&self, id: &str) -> Result<Option<AccountSummary>, AccountsError> {
        self.spend_lookup()?;
        found(self.bounded(self.app().lookup_by_id(id)).await?)
    }

    /// Issues a User verification proof for `receiving_app` from the account's
    /// current Waveform access token.
    ///
    /// # Errors
    ///
    /// [`ProofError`] by category; the subject token is never logged.
    pub async fn issue_user_verification(
        &self,
        subject_token: &str,
        receiving_app: &str,
        scopes: &[&str],
        idempotency_key: &str,
    ) -> Result<IssuedProof, ProofError> {
        let input = IssueUserVerification {
            subject_token: subject_token.to_owned(),
            receiving_app: receiving_app.to_owned(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            access_ttl_seconds: Some(1_800),
        };
        match tokio::time::timeout(
            self.timeout,
            self.app()
                .issue_user_verification(&input, Some(idempotency_key)),
        )
        .await
        {
            Err(_) => Err(ProofError::Timeout),
            Ok(Ok(proof)) => Ok(proof),
            Ok(Err(error)) => Err(proof_failure(&error)),
        }
    }

    /// Rotates a proof refresh token (`POST /v1/proofs/refresh`) with an
    /// idempotency key, so a retry after a lost answer gets the same tokens.
    ///
    /// # Errors
    ///
    /// [`ProofError`] by category.
    pub async fn refresh_proof(
        &self,
        refresh_token: &str,
        idempotency_key: &str,
    ) -> Result<IssuedProof, ProofError> {
        let mut url = self.api_url.clone();
        url.set_path("/v1/proofs/refresh");
        let response = self
            .http
            .post(url)
            .basic_auth(&self.app_id, Some(self.app_secret.expose_secret()))
            .header("idempotency-key", idempotency_key)
            .json(&serde_json::json!({
                "proof_refresh_token": refresh_token,
                "access_ttl_seconds": 1_800,
            }))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ProofError::Timeout
                } else {
                    ProofError::Unavailable
                }
            })?;
        let status = response.status();
        if status.is_success() {
            return response
                .json::<IssuedProof>()
                .await
                .map_err(|_| ProofError::Unavailable);
        }
        if status.is_client_error() && status.as_u16() != 429 && status.as_u16() != 401 {
            let body = response
                .json::<serde_json::Value>()
                .await
                .unwrap_or_default();
            return Err(ProofError::Refused {
                reason: refusal_reason(&body["error"]["details"], &body["error"]["code"]),
            });
        }
        Err(ProofError::Unavailable)
    }

    /// Revokes a proof Waveform issued (`POST /v1/proofs/revoke`). Revoking an
    /// already revoked or unknown proof is not an error.
    ///
    /// # Errors
    ///
    /// `Unavailable`/`Timeout` when Accounts cannot answer.
    pub async fn revoke_proof(&self, proof_id: &str) -> Result<(), AccountsError> {
        match self
            .bounded(self.app().revoke_proof(&ProofRef::Id(proof_id.to_owned())))
            .await?
        {
            Ok(()) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(_) => Err(AccountsError::Unavailable),
        }
    }
}

fn found(
    answer: Result<AccountSummary, ClientError>,
) -> Result<Option<AccountSummary>, AccountsError> {
    match answer {
        Ok(account) => Ok(Some(account)),
        // A malformed id (`invalid_id`, e.g. too long) can belong to no account: the caller
        // answers 404 naming it, not "Silicon Accounts is unavailable".
        Err(error) if error.is_not_found() || error.is_code("invalid_id") => Ok(None),
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "account lookup failed at Silicon Accounts"
            );
            Err(AccountsError::Unavailable)
        }
    }
}

fn proof_failure(error: &ClientError) -> ProofError {
    if error.is_transport() {
        return ProofError::Unavailable;
    }
    match error.status() {
        Some(status) if (400..500).contains(&status) && status != 429 && status != 401 => {
            ProofError::Refused {
                reason: refusal_reason(
                    error.details().unwrap_or(&serde_json::Value::Null),
                    &serde_json::Value::String(error.code().to_owned()),
                ),
            }
        }
        _ => ProofError::Unavailable,
    }
}

/// `details.reason` when Accounts gives one, else the error code; bounded, never a token.
fn refusal_reason(details: &serde_json::Value, code: &serde_json::Value) -> Option<String> {
    details["reason"]
        .as_str()
        .or_else(|| code.as_str())
        .filter(|reason| {
            !reason.is_empty()
                && reason.len() <= 64
                && reason
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
        .map(str::to_owned)
}

fn token_failure(error: &ClientError) -> AccountsError {
    let failure = match error {
        ClientError::Token(TokenError::Expired { .. }) => AuthFailure::TokenExpired,
        ClientError::Token(TokenError::WrongAudience { .. }) => AuthFailure::WrongAudience,
        ClientError::Token(TokenError::WrongIssuer { .. }) => AuthFailure::WrongIssuer,
        ClientError::Token(TokenError::UnknownKey { .. }) => AuthFailure::UnknownSigningKey,
        _ => AuthFailure::InvalidToken,
    };
    AccountsError::Rejected(failure)
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
