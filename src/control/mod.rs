//! Durable product APIs: application login, account settings and IAM events.

mod accounts;
mod environments;
mod jobs;
mod sessions;
#[cfg(test)]
mod tests;
mod vault;
mod voices;
mod webhooks;

use std::sync::Arc;

use axum::{
    Json, Router,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http::{HeaderMap, StatusCode};
use secrecy::ExposeSecret as _;
use serde_json::json;
use silicon_iam_client::{
    Client, Credential, EnvironmentKey, models,
    webhook::{WebhookSecret, WebhookSecretKeyring, WebhookVerifier},
};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

use crate::config::Settings;
use crate::domain::auth::AuthorizedActor;
use crate::domain::identity::{Actor, ActorId, ActorKind, OrganizationId};
use crate::domain::provider::{ProviderName, STT_PROVIDER_CHAIN, TTS_PROVIDER_CHAIN};
use vault::Vault;

/// Shared infrastructure for the user-facing control APIs.
pub struct ControlState {
    pool: PgPool,
    iam: Client,
    app_id: String,
    briefcase_settings: crate::config::BriefcaseSettings,
    iam_timeout: std::time::Duration,
    vault: Option<Vault>,
    verifier: Option<WebhookVerifier>,
}

impl ControlState {
    /// Builds the official IAM client and secret-storage boundary.
    ///
    /// # Errors
    /// Returns a redacted error for unusable integration or secret settings.
    pub fn new(pool: PgPool, settings: &Settings) -> Result<Self, &'static str> {
        let iam = Client::builder(settings.iam.base_url.as_str())
            .map_err(|_| "invalid IAM base URL")?
            .credential(Credential::application(&settings.iam.app_id, settings.iam.app_secret.expose_secret()))
            .timeout(settings.iam.timeout)
            // Backend deployments use a pinned lockfile, updated by CI.
            .auto_update(false)
            .build().map_err(|_| "could not construct IAM client")?;
        let vault = settings
            .control
            .encryption_key
            .as_ref()
            .map(Vault::new)
            .transpose()?;
        let verifier = settings
            .control
            .webhook_secret
            .as_ref()
            .map(|secret| {
                let secret = WebhookSecret::new(secret.expose_secret())
                    .map_err(|_| "invalid webhook secret")?;
                let keyring =
                    WebhookSecretKeyring::new(settings.control.webhook_key_version, secret)
                        .map_err(|_| "invalid webhook signing version")?;
                Ok::<_, &'static str>(WebhookVerifier::new(keyring))
            })
            .transpose()?;
        Ok(Self {
            pool,
            iam,
            app_id: settings.iam.app_id.clone(),
            briefcase_settings: settings.briefcase.clone(),
            iam_timeout: settings.iam.timeout,
            vault,
            verifier,
        })
    }

    fn vault(&self) -> Result<&Vault, ControlError> {
        self.vault
            .as_ref()
            .ok_or_else(|| ControlError::unavailable("secret_storage_unavailable"))
    }

    async fn plane(&self, headers: &HeaderMap) -> Result<Plane, ControlError> {
        // Keep request-time cleanup as a safety net for short-lived local
        // processes; the API runner also invokes this periodically.
        let _ = self.cleanup_environments().await;
        let Some(key) = single_header(headers, "x-testing-environment-key")? else {
            return Ok(Plane {
                id: Uuid::nil(),
                iam: self.iam.clone(),
                iam_environment_id: None,
            });
        };
        EnvironmentKey::new(key).map_err(|_| ControlError::unauthorized())?;
        let digest = self
            .vault()?
            .digest(key, "environment-root")
            .map_err(ControlError::internal)?;
        let row = sqlx::query("UPDATE waveform_environments SET last_activity_at=now() WHERE root_key_hash=$1 AND deleted_at IS NULL AND id <> $2 RETURNING id, iam_key_cipher, app_secret_cipher, iam_environment_id")
            .bind(digest).bind(Uuid::nil()).fetch_optional(&self.pool).await?
            .ok_or_else(ControlError::unauthorized)?;
        let id: Uuid = row.try_get("id")?;
        let encrypted: Vec<u8> = row.try_get("iam_key_cipher")?;
        let iam_key = self
            .vault()?
            .open(&encrypted, &format!("{id}/iam-key"))
            .map_err(ControlError::internal)?;
        let environment = EnvironmentKey::new(iam_key.expose_secret())
            .map_err(|_| ControlError::unavailable("invalid_environment_binding"))?;
        let app_cipher: Vec<u8> = row.try_get("app_secret_cipher")?;
        let app_secret = self
            .vault()?
            .open(&app_cipher, &format!("{id}/app-secret"))
            .map_err(ControlError::internal)?;
        let iam = Client::builder(self.iam.base_url().as_str())
            .map_err(|_| ControlError::unavailable("iam_unavailable"))?
            .credential(Credential::application(
                &self.app_id,
                app_secret.expose_secret(),
            ))
            .timeout(self.iam_timeout)
            .auto_update(false)
            .build()
            .map_err(|_| ControlError::unavailable("iam_unavailable"))?
            .with_environment(environment);
        Ok(Plane {
            id,
            iam,
            iam_environment_id: row.try_get("iam_environment_id")?,
        })
    }

    async fn identity(&self, headers: &HeaderMap) -> Result<Identity, ControlError> {
        let org = single_header(headers, "x-org-id")?
            .ok_or_else(|| ControlError::bad_request("organization_required"))?;
        self.identity_for_org(headers, Some(org)).await
    }

    async fn identity_for_org(
        &self,
        headers: &HeaderMap,
        org: Option<&str>,
    ) -> Result<Identity, ControlError> {
        let plane = self.plane(headers).await?;
        let token = bearer(headers)?;
        org.map(str::parse::<crate::domain::identity::OrganizationId>)
            .transpose()
            .map_err(|_| ControlError::bad_request("invalid_organization"))?;
        let authority = if let Some(org) = org {
            plane
                .iam
                .oauth()
                .authorization(token, Some(org))
                .await
                .map_err(ControlError::iam)?
        } else {
            // Unscoped application tokens have a list of current organization
            // snapshots. IAM returns them in handle order; use the first as the
            // initial workspace instead of assuming the application's owning org.
            plane
                .iam
                .oauth()
                .authorizations(token)
                .await
                .map_err(ControlError::iam)?
                .and_then(|items| items.into_iter().next())
        }
        .ok_or_else(ControlError::unauthorized)?;
        if org.is_some_and(|org| authority.org_id != org)
            || authority.audience != self.app_id
            || authority.testing_environment_id != plane.iam_environment_id
        {
            return Err(ControlError::forbidden());
        }
        self.ensure_voice_default(plane.id, &authority.org_id, authority.principal_id)
            .await?;
        Ok(Identity { plane, authority })
    }

    /// Authorizes a test-plane speech request against the selected IAM plane.
    /// This is deliberately separate from production management identity so a
    /// root test key can select the plane while the bearer still identifies the
    /// Carbon or Silicon performing the operation.
    pub(crate) async fn authorize_test_speech(
        &self,
        headers: &HeaderMap,
    ) -> Result<TestSpeechContext, ControlError> {
        let identity = self.identity(headers).await?;
        if identity.plane.id.is_nil() {
            return Err(ControlError::bad_request("test_environment_required"));
        }
        let plane_id = identity.plane.id;
        let actor = Self::authorized_actor_from_identity(&identity)?;
        let cipher: Vec<u8> = sqlx::query_scalar("SELECT briefcase_key_cipher FROM waveform_environments WHERE id=$1 AND deleted_at IS NULL")
            .bind(plane_id).fetch_one(&self.pool).await?;
        let key = self
            .vault()?
            .open(&cipher, &format!("{plane_id}/briefcase-key"))
            .map_err(ControlError::internal)?;
        let environment = briefcase_client::EnvironmentKey::new(key.expose_secret())
            .map_err(|_| ControlError::unavailable("invalid_environment_binding"))?;
        let iam = Arc::new(crate::infrastructure::auth::TestStorageDelegator {
            sdk: identity.plane.iam,
            application_id: self
                .app_id
                .parse()
                .map_err(|_| ControlError::unavailable("invalid_application"))?,
            audience: self.briefcase_settings.audience.clone(),
        });
        let briefcase = crate::infrastructure::briefcase::FailClosedBriefcaseStore::new(
            &self.briefcase_settings,
        )
        .map_err(|_| ControlError::unavailable("invalid_briefcase_configuration"))?
        .with_uploads(&self.briefcase_settings)
        .with_reads(&self.briefcase_settings, iam.clone())
        .with_environment(environment);
        Ok(TestSpeechContext {
            authorization: actor,
            plane_id,
            iam,
            briefcase: Arc::new(briefcase),
        })
    }

    /// Returns the authenticated actor's effective provider order for one
    /// operation. Defaults and account overrides are read from the selected
    /// plane so the fallback chain is data-driven rather than compiled into
    /// request handling.
    pub(crate) async fn effective_provider_order(
        &self,
        headers: &HeaderMap,
        operation: &str,
    ) -> Result<Vec<ProviderName>, ControlError> {
        let identity = self.identity(headers).await?;
        let (default_column, preference_column, base) = match operation {
            "tts" => ("default_tts", "tts_order", TTS_PROVIDER_CHAIN.as_slice()),
            "stt" => ("default_stt", "stt_order", STT_PROVIDER_CHAIN.as_slice()),
            _ => return Err(ControlError::bad_request("invalid_operation")),
        };
        let row = sqlx::query("SELECT d.tts_order AS default_tts, d.stt_order AS default_stt, p.tts_order, p.stt_order FROM waveform_provider_defaults d LEFT JOIN waveform_account_preferences p ON p.plane_id=d.plane_id AND p.org_id=$2 AND p.actor_id=$3 WHERE d.plane_id=$1")
            .bind(identity.plane.id)
            .bind(&identity.authority.org_id)
            .bind(identity.authority.principal_id)
            .fetch_one(&self.pool)
            .await?;
        let default: serde_json::Value = row.try_get(default_column)?;
        let preference: Option<serde_json::Value> = row.try_get(preference_column)?;
        let parse = |value: serde_json::Value| -> Result<Vec<ProviderName>, ControlError> {
            let parsed: Vec<ProviderName> = serde_json::from_value(value)
                .map_err(|_| ControlError::unavailable("invalid_provider_defaults"))?;
            let order = crate::domain::provider::resolve_order(&parsed, base)
                .ok_or_else(|| ControlError::unavailable("invalid_provider_defaults"))?;
            (order.len() == parsed.len())
                .then_some(order)
                .ok_or_else(|| ControlError::unavailable("invalid_provider_defaults"))
        };
        parse(preference.unwrap_or(default))
    }

    fn authorized_actor_from_identity(
        identity: &Identity,
    ) -> Result<AuthorizedActor, ControlError> {
        let actor_kind = match identity.authority.actor_type {
            models::ApplicationAuthorizationActorType::Carbon => ActorKind::Carbon,
            models::ApplicationAuthorizationActorType::Silicon => ActorKind::Silicon,
            models::ApplicationAuthorizationActorType::Other(_) => {
                return Err(ControlError::forbidden());
            }
        };
        let actor_id = ActorId::new(identity.authority.principal_id)
            .map_err(|_| ControlError::unavailable("invalid_actor"))?;
        Ok(AuthorizedActor {
            actor: Actor::new(actor_kind, actor_id),
            organization_id: identity
                .authority
                .org_id
                .parse::<OrganizationId>()
                .map_err(|_| ControlError::unavailable("invalid_organization"))?,
            originating_application: None,
            expires_at: None,
        })
    }

    /// Applies the documented test-plane inactivity and recovery retention
    /// windows. This is also called lazily on requests, but the process-level
    /// runner invokes it periodically so inactive planes are retired without
    /// requiring another request to arrive.
    pub(crate) async fn cleanup_environments(&self) -> Result<u64, sqlx::Error> {
        let purged = sqlx::query(
            "DELETE FROM waveform_environments WHERE id <> $1 AND deleted_at < now() - interval '30 days'",
        )
        .bind(Uuid::nil())
        .execute(&self.pool)
        .await?
        .rows_affected();
        let expired = sqlx::query(
            "UPDATE waveform_environments SET deleted_at=now() WHERE id <> $1 AND deleted_at IS NULL AND last_activity_at < now() - interval '15 days'",
        )
        .bind(Uuid::nil())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(purged.saturating_add(expired))
    }
}

pub(crate) struct TestSpeechContext {
    pub(crate) authorization: AuthorizedActor,
    pub(crate) plane_id: Uuid,
    pub(crate) iam: Arc<dyn crate::application::ports::IamPort>,
    pub(crate) briefcase: Arc<dyn crate::application::ports::BriefcasePort>,
}

struct Plane {
    id: Uuid,
    iam: Client,
    iam_environment_id: Option<Uuid>,
}

struct Identity {
    plane: Plane,
    authority: models::ApplicationAuthorization,
}

/// Routes merged before the main API's admission, deadlines and logging layers.
pub fn router(state: Arc<ControlState>) -> Router {
    Router::new()
        .route("/api/v1/auth/login", post(sessions::login))
        .route("/api/v1/auth/refresh", post(sessions::refresh))
        .route("/api/v1/auth/logout", post(sessions::logout))
        .route("/api/v1/auth/me", get(sessions::me))
        .route("/api/v1/jobs", get(jobs::list))
        .route("/api/v1/jobs/{job_id}", get(jobs::get))
        .route("/api/v1/testing-environments", post(environments::create))
        .route("/api/v1/testing-environments", get(environments::list))
        .route(
            "/api/v1/testing-environments/{environment_id}",
            get(environments::detail),
        )
        .route(
            "/api/v1/testing-environments/{environment_id}/key",
            get(environments::key),
        )
        .route(
            "/api/v1/testing-environments/{environment_id}/rotate-key",
            post(environments::rotate_key),
        )
        .route(
            "/api/v1/testing-environments/{environment_id}/delete",
            post(environments::delete),
        )
        .route(
            "/api/v1/testing-environments/{environment_id}/restore",
            post(environments::restore),
        )
        .route("/api/v1/testing-environment", get(environments::current))
        .route(
            "/api/v1/testing-environment/clean",
            post(environments::clean),
        )
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
        .route("/webhooks/", post(webhooks::receive))
        .route("/webhook/", post(webhooks::receive))
        .with_state(state)
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, ControlError> {
    let mut all = headers.get_all(name).iter();
    let first = all.next();
    if all.next().is_some() {
        return Err(ControlError::bad_request("duplicate_security_header"));
    }
    first
        .map(|value| {
            value
                .to_str()
                .map_err(|_| ControlError::bad_request("invalid_header"))
        })
        .transpose()
}

fn bearer(headers: &HeaderMap) -> Result<&str, ControlError> {
    if single_header(headers, "x-iam-obo-access-proof")?.is_some()
        || single_header(headers, "x-app-id")?.is_some()
    {
        return Err(ControlError::bad_request("unsupported_authentication_mode"));
    }
    let auth = single_header(headers, "authorization")?.ok_or_else(ControlError::unauthorized)?;
    let mut parts = auth.split_ascii_whitespace();
    let scheme = parts.next().ok_or_else(ControlError::unauthorized)?;
    let token = parts.next().ok_or_else(ControlError::unauthorized)?;
    if !scheme.eq_ignore_ascii_case("bearer") || parts.next().is_some() || token.len() > 16_384 {
        return Err(ControlError::unauthorized());
    }
    Ok(token)
}

pub(super) struct ControlError {
    status: StatusCode,
    code: &'static str,
}

impl ControlError {
    fn bad_request(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
        }
    }
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthenticated",
        }
    }
    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
        }
    }
    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
        }
    }
    fn unavailable(code: &'static str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
        }
    }
    fn internal(_detail: &'static str) -> Self {
        Self::unavailable("secret_storage_unavailable")
    }
    fn iam(error: silicon_iam_client::Error) -> Self {
        match error {
            silicon_iam_client::Error::Api(api) if api.status == 401 || api.status == 400 => {
                Self::unauthorized()
            }
            silicon_iam_client::Error::Api(api) if api.status == 403 => Self::forbidden(),
            silicon_iam_client::Error::RateLimited { .. } => Self {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "iam_rate_limited",
            },
            _ => Self::unavailable("iam_unavailable"),
        }
    }
}

impl From<sqlx::Error> for ControlError {
    fn from(_: sqlx::Error) -> Self {
        Self::unavailable("database_unavailable")
    }
}

impl IntoResponse for ControlError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": {"code": self.code, "message": self.code.replace('_', " ")}})),
        )
            .into_response()
    }
}

#[async_trait::async_trait]
impl crate::application::ports::ProviderKeyStore for ControlState {
    async fn load(
        &self,
        plane_id: Uuid,
        actor: &AuthorizedActor,
    ) -> Result<
        crate::application::ports::ProviderKeys,
        crate::application::ports::ProviderKeyStoreError,
    > {
        use crate::application::ports::{ProviderKeyStoreError, ProviderKeys};
        let rows = sqlx::query("SELECT provider,secret_cipher FROM waveform_provider_keys WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3")
            .bind(plane_id).bind(actor.organization_id.as_str()).bind(actor.actor.id.as_uuid())
            .fetch_all(&self.pool).await.map_err(|_| ProviderKeyStoreError)?;
        let mut keys = ProviderKeys::new();
        for row in rows {
            let name: String = row.try_get("provider").map_err(|_| ProviderKeyStoreError)?;
            let provider: ProviderName =
                serde_json::from_value(json!(name)).map_err(|_| ProviderKeyStoreError)?;
            let cipher: Vec<u8> = row
                .try_get("secret_cipher")
                .map_err(|_| ProviderKeyStoreError)?;
            let context = format!(
                "{}/{}/{}/{}",
                plane_id, actor.organization_id, actor.actor.id, name
            );
            let key = self
                .vault()
                .map_err(|_| ProviderKeyStoreError)?
                .open(&cipher, &context)
                .map_err(|_| ProviderKeyStoreError)?;
            keys.insert(provider, key);
        }
        Ok(keys)
    }
}
