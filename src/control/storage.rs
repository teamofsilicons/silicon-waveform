//! Explicit Briefcase feature consent and encrypted, plane-bound rotating tokens.
use super::{ControlError, ControlState, Vault};
use crate::application::ports::IamError;
use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse as _, Response},
};
use base64::Engine as _;
use http::{HeaderMap, header};
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use serde_json::json;
use silicon_iam_client::{
    Client, IdempotencyKey, Mutation,
    models::{OboAuthorizationEndpoint, OboAuthorizationRequest, OboTokenPair},
};
use sqlx::{PgPool, Row as _};
use std::sync::Arc;
use subtle::ConstantTimeEq as _;
use time::OffsetDateTime;
use uuid::Uuid;

pub(crate) const ENDPOINTS: [&str; 4] = [
    "briefcase.uploads.reserve",
    "briefcase.uploads.commit",
    "briefcase.entries.list",
    "briefcase.files.read",
];

/// A separately approved feature grant; credentials never reach the browser.
#[derive(Clone)]
pub(crate) struct StorageGrants {
    pool: PgPool,
    vault: Vault,
}
impl StorageGrants {
    pub(crate) fn new(pool: PgPool, key: &secrecy::SecretString) -> Result<Self, &'static str> {
        Ok(Self {
            pool,
            vault: Vault::new(key)?,
        })
    }
    fn context(plane: Uuid, org: &str, actor: Uuid, endpoint: &str) -> String {
        format!("storage-grant/{plane}/{org}/{actor}/{endpoint}")
    }
    /// Serialize refreshes under the row lock. The same refresh token always
    /// derives the same opaque retry key, including after a lost HTTP response.
    pub(crate) async fn token(
        &self,
        sdk: &Client,
        plane: Uuid,
        org: &str,
        actor: Uuid,
        audience: &str,
        endpoint: &str,
    ) -> Result<OboTokenPair, IamError> {
        let mut tx = self.pool.begin().await.map_err(|_| IamError::Unavailable)?;
        let row=sqlx::query("SELECT token_cipher,invalidated_at FROM waveform_storage_grants WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 AND endpoint_id=$4 FOR UPDATE")
            .bind(plane).bind(org).bind(actor).bind(endpoint).fetch_optional(&mut *tx).await.map_err(|_|IamError::Unavailable)?.ok_or(IamError::StorageAuthorizationRequired)?;
        if row
            .try_get::<Option<OffsetDateTime>, _>("invalidated_at")
            .map_err(|_| IamError::InvalidResponse)?
            .is_some()
        {
            return Err(IamError::StorageAuthorizationRequired);
        }
        let context = Self::context(plane, org, actor, endpoint);
        let encrypted: Vec<u8> = row
            .try_get("token_cipher")
            .map_err(|_| IamError::InvalidResponse)?;
        let plain = self
            .vault
            .open(&encrypted, &context)
            .map_err(|_| IamError::InvalidResponse)?;
        let mut token: OboTokenPair =
            serde_json::from_str(plain.expose_secret()).map_err(|_| IamError::InvalidResponse)?;
        validate_pair(&token, audience, endpoint, !plane.is_nil())?;
        if token.expires_at <= OffsetDateTime::now_utc() + time::Duration::seconds(60) {
            let digest = self
                .vault
                .digest(&token.refresh_token, &format!("{context}/refresh"))
                .map_err(|_| IamError::InvalidResponse)?;
            let mutation =
                mutation(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest))
                    .map_err(|_| IamError::InvalidResponse)?;
            token = match sdk.obo().refresh(&token.refresh_token, &mutation).await {
                Ok(mut response) if response.items.len() == 1 => response.items.remove(0),
                Ok(_) => return Err(IamError::InvalidResponse),
                Err(error) => {
                    if matches!(&error,silicon_iam_client::Error::Api(api) if matches!(api.code.as_str(),"invalid_grant"|"obo_access_token_invalid"|"obo_token_invalid"|"obo_token_expired"|"obo_token_revoked"|"obo_authorization_denied"|"obo_consent_required"))
                    {
                        sqlx::query("UPDATE waveform_storage_grants SET invalidated_at=now() WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 AND endpoint_id=$4").bind(plane).bind(org).bind(actor).bind(endpoint).execute(&mut *tx).await.map_err(|_|IamError::Unavailable)?;
                        tx.commit().await.map_err(|_| IamError::Unavailable)?;
                        return Err(IamError::StorageAuthorizationRequired);
                    }
                    return Err(IamError::Unavailable);
                }
            };
            validate_pair(&token, audience, endpoint, !plane.is_nil())?;
            let text = zeroize::Zeroizing::new(
                serde_json::to_string(&token).map_err(|_| IamError::InvalidResponse)?,
            );
            let cipher = self
                .vault
                .seal(&text, &context)
                .map_err(|_| IamError::InvalidResponse)?;
            sqlx::query("UPDATE waveform_storage_grants SET token_cipher=$5,expires_at=$6,updated_at=now() WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 AND endpoint_id=$4").bind(plane).bind(org).bind(actor).bind(endpoint).bind(cipher).bind(token.expires_at).execute(&mut *tx).await.map_err(|_|IamError::Unavailable)?;
        }
        tx.commit().await.map_err(|_| IamError::Unavailable)?;
        Ok(token)
    }
}
fn validate_pair(
    token: &OboTokenPair,
    audience: &str,
    endpoint: &str,
    testing: bool,
) -> Result<(), IamError> {
    if token.audience != audience
        || token.endpoint_id != endpoint
        || !token.access_token.starts_with("oba_")
        || !token.refresh_token.starts_with("obr_")
        || token.org_id.is_empty()
        || token.actor.as_ref().is_none_or(|actor| {
            crate::infrastructure::actor_keys::canonical_kind(&actor.public_id).is_none()
        })
        || token.testing_context.is_some() != testing
        || token
            .testing_context
            .as_ref()
            .is_some_and(|c| c.app_id != audience || !c.app_secret.starts_with("ask_"))
    {
        return Err(IamError::InvalidResponse);
    }
    Ok(())
}
fn mutation(key: &str) -> Result<Mutation, ControlError> {
    IdempotencyKey::parse(key)
        .map(Mutation::with_key)
        .map_err(|_| ControlError::bad_request("invalid_idempotency_key"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Start {
    redirect_uri: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Complete {
    code: String,
    state: String,
}

impl ControlState {
    pub(crate) fn storage_grants(&self) -> Result<StorageGrants, ControlError> {
        Ok(StorageGrants {
            pool: self.pool.clone(),
            vault: self.vault()?.clone(),
        })
    }
}
fn response(row: &sqlx::postgres::PgRow, vault: &Vault) -> Result<Response, ControlError> {
    let id: Uuid = row.try_get("id")?;
    let state: Vec<u8> = row.try_get("state_cipher")?;
    let state = vault
        .open(&state, &format!("storage-authorization/{id}/state"))
        .map_err(ControlError::internal)?;
    Ok(([(header::CACHE_CONTROL,"no-store")],Json(json!({"authorization_id":id,"consent_url":row.try_get::<Option<String>,_>("consent_url")?,"state":state.expose_secret(),"status":row.try_get::<String,_>("status")?,"expires_at":row.try_get::<OffsetDateTime,_>("expires_at")?.format(&time::format_description::well_known::Rfc3339).map_err(|_|ControlError::unavailable("invalid_storage_authorization"))?}))).into_response())
}
pub(super) async fn start(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(input): Json<Start>,
) -> Result<Response, ControlError> {
    let identity = state.identity(&headers).await?;
    if let Some(uri) = input.redirect_uri.as_deref() {
        let url =
            url::Url::parse(uri).map_err(|_| ControlError::bad_request("invalid_redirect_uri"))?;
        if uri.len() > 2048
            || url.as_str() != uri
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || !(url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
        {
            return Err(ControlError::bad_request("invalid_redirect_uri"));
        }
    }
    let vault = state.vault()?;
    let key = super::single_header(&headers, "idempotency-key")?
        .ok_or_else(|| ControlError::bad_request("idempotency_key_required"))?;
    mutation(key)?;
    let id = Uuid::now_v7();
    let correlation = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let body = OboAuthorizationRequest {
        subject_token: super::bearer(&headers)?.to_owned(),
        org_id: identity.authority.org_id.clone(),
        endpoints: ENDPOINTS
            .iter()
            .map(|endpoint| OboAuthorizationEndpoint {
                audience: state.briefcase_settings.audience.clone(),
                endpoint_id: (*endpoint).to_owned(),
            })
            .collect(),
        state: input.redirect_uri.as_ref().map(|_| correlation.clone()),
        redirect_uri: input.redirect_uri,
    };
    let plain = zeroize::Zeroizing::new(
        serde_json::to_string(&body)
            .map_err(|_| ControlError::unavailable("storage_unavailable"))?,
    );
    let cipher = vault
        .seal(&plain, &format!("storage-authorization/{id}/request"))
        .map_err(ControlError::internal)?;
    let state_cipher = vault
        .seal(&correlation, &format!("storage-authorization/{id}/state"))
        .map_err(ControlError::internal)?;
    sqlx::query("INSERT INTO waveform_storage_authorizations(id,plane_id,org_id,actor_id,idempotency_key,request_cipher,state_digest,state_cipher) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(plane_id,org_id,actor_id,idempotency_key) DO NOTHING")
        .bind(id).bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.storage_actor_id).bind(key).bind(cipher).bind(vault.digest(&correlation,"storage-state").map_err(ControlError::internal)?).bind(state_cipher).execute(&state.pool).await?;
    let mut tx = state.pool.begin().await?;
    let mut row=sqlx::query("SELECT * FROM waveform_storage_authorizations WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 AND idempotency_key=$4 FOR UPDATE").bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.storage_actor_id).bind(key).fetch_one(&mut *tx).await?;
    if row.try_get::<OffsetDateTime, _>("expires_at")? <= OffsetDateTime::now_utc() {
        return Err(ControlError::bad_request("storage_authorization_expired"));
    }
    if row.try_get::<Option<Uuid>, _>("iam_id")?.is_none() {
        let id: Uuid = row.try_get("id")?;
        let plain = vault
            .open(
                &row.try_get::<Vec<u8>, _>("request_cipher")?,
                &format!("storage-authorization/{id}/request"),
            )
            .map_err(ControlError::internal)?;
        let body: OboAuthorizationRequest = serde_json::from_str(plain.expose_secret())
            .map_err(|_| ControlError::unavailable("storage_unavailable"))?;
        let result = identity
            .plane
            .iam
            .obo()
            .authorize(&body, &mutation(&format!("storage-start-{id}"))?)
            .await
            .map_err(ControlError::iam)?;
        let url = result
            .authorization_url
            .as_ref()
            .ok_or_else(|| ControlError::unavailable("invalid_storage_authorization"))?;
        let parsed = url::Url::parse(url)
            .map_err(|_| ControlError::unavailable("invalid_storage_authorization"))?;
        if !(parsed.scheme() == "https"
            || (parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"))))
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(ControlError::unavailable("invalid_storage_authorization"));
        }
        row=sqlx::query("UPDATE waveform_storage_authorizations SET iam_id=$2,consent_url=$3,expires_at=LEAST(expires_at,$4) WHERE id=$1 RETURNING *").bind(id).bind(result.id).bind(url).bind(result.expires_at).fetch_one(&mut *tx).await?;
    }
    tx.commit().await?;
    response(&row, vault)
}
pub(super) async fn status(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ControlError> {
    let identity = state.identity(&headers).await?;
    let row=sqlx::query("SELECT * FROM waveform_storage_authorizations WHERE id=$1 AND plane_id=$2 AND org_id=$3 AND actor_id=$4").bind(id).bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.storage_actor_id).fetch_optional(&state.pool).await?.ok_or_else(ControlError::not_found)?;
    response(&row, state.vault()?)
}
pub(super) async fn complete(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Complete>,
) -> Result<Response, ControlError> {
    let identity = state.identity(&headers).await?;
    let vault = state.vault()?;
    if body.code.len() > 16384 || !body.code.starts_with("obc_") || body.state.len() > 128 {
        return Err(ControlError::bad_request("invalid_storage_authorization"));
    }
    let mut tx = state.pool.begin().await?;
    let row=sqlx::query("SELECT * FROM waveform_storage_authorizations WHERE id=$1 AND plane_id=$2 AND org_id=$3 AND actor_id=$4 FOR UPDATE").bind(id).bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.storage_actor_id).fetch_optional(&mut *tx).await?.ok_or_else(ControlError::not_found)?;
    let expected: Vec<u8> = row.try_get("state_digest")?;
    if !bool::from(
        expected.ct_eq(
            &vault
                .digest(&body.state, "storage-state")
                .map_err(ControlError::internal)?,
        ),
    ) {
        return Err(ControlError::forbidden());
    }
    let code_digest = vault
        .digest(&body.code, &format!("storage-authorization/{id}/code"))
        .map_err(ControlError::internal)?;
    if row.try_get::<String, _>("status")? == "completed" {
        if row.try_get::<Option<Vec<u8>>, _>("code_digest")?.as_ref() != Some(&code_digest) {
            return Err(ControlError::forbidden());
        }
        return response(&row, vault);
    }
    if row.try_get::<OffsetDateTime, _>("expires_at")? <= OffsetDateTime::now_utc() {
        return Err(ControlError::bad_request("storage_authorization_expired"));
    }
    let iam_id: Uuid = row
        .try_get::<Option<Uuid>, _>("iam_id")?
        .ok_or_else(|| ControlError::bad_request("storage_authorization_pending"))?;
    let result = identity
        .plane
        .iam
        .obo()
        .exchange_code(
            iam_id,
            &body.code,
            &mutation(&format!(
                "storage-exchange-{id}-{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&code_digest)
            ))?,
        )
        .await
        .map_err(consent_exchange_error)?;
    if result.items.len() != ENDPOINTS.len() {
        return Err(ControlError::unavailable("invalid_storage_authorization"));
    }
    for endpoint in ENDPOINTS {
        let pair = result
            .items
            .iter()
            .find(|p| p.endpoint_id == endpoint)
            .ok_or_else(|| ControlError::unavailable("invalid_storage_authorization"))?;
        validate_pair(
            pair,
            &state.briefcase_settings.audience,
            endpoint,
            !identity.plane.id.is_nil(),
        )
        .map_err(|_| ControlError::unavailable("invalid_storage_authorization"))?;
        let text = zeroize::Zeroizing::new(
            serde_json::to_string(pair)
                .map_err(|_| ControlError::unavailable("storage_unavailable"))?,
        );
        let cipher = vault
            .seal(
                &text,
                &StorageGrants::context(
                    identity.plane.id,
                    &identity.authority.org_id,
                    identity.storage_actor_id,
                    endpoint,
                ),
            )
            .map_err(ControlError::internal)?;
        sqlx::query("INSERT INTO waveform_storage_grants(plane_id,org_id,actor_id,endpoint_id,token_cipher,expires_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(plane_id,org_id,actor_id,endpoint_id) DO UPDATE SET token_cipher=excluded.token_cipher,expires_at=excluded.expires_at,invalidated_at=NULL,updated_at=now()")
            .bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.storage_actor_id).bind(endpoint).bind(cipher).bind(pair.expires_at).execute(&mut *tx).await?;
    }
    let row=sqlx::query("UPDATE waveform_storage_authorizations SET status='completed',code_digest=$2,request_cipher=''::bytea WHERE id=$1 RETURNING *").bind(id).bind(code_digest).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    response(&row, vault)
}

// A rejected feature code must not invalidate the already authenticated session.
fn consent_exchange_error(error: silicon_iam_client::Error) -> ControlError {
    match error {
        silicon_iam_client::Error::Api(api)
            if api.code != "invalid_client"
                && matches!(api.status, 400 | 401 | 403 | 404 | 409 | 410 | 422) =>
        {
            ControlError::bad_request("invalid_storage_authorization")
        }
        other => ControlError::iam(other),
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
