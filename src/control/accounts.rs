//! Account settings: provider order, default voice and write-only provider
//! keys. A custodian may manage its Silicons' settings.
//!
//! Rows written in the Accounts era use the org sentinel `@`. Rows from before
//! (one per former org) are still read: the newest one stands in until the
//! account's `@` row exists, and `waveform-api link-identities` consolidates them.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
};
use http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row as _;
use uuid::Uuid;

use super::{ControlError, ControlState};
use crate::domain::{
    auth::{AuthorizedActor, WaveformAction},
    idempotency::{ACCOUNT_SCOPE, SpeechOperation},
    provider::{ProviderName, STT_PROVIDER_CHAIN, TTS_PROVIDER_CHAIN},
};
use crate::infrastructure::directory::Confirm;

/// `?account=` selector shared by the account routes.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountQuery {
    pub(super) account: Option<String>,
}

/// Partial request preference resolves against the next-lower configuration:
/// requested choices first, followed by all unmentioned choices in base order.
pub(crate) fn resolve_order(
    requested: &[String],
    base: &[String],
) -> Result<Vec<String>, &'static str> {
    if requested.len() > base.len() {
        return Err("invalid_provider_order");
    }
    let mut result = Vec::with_capacity(base.len());
    for provider in requested {
        if !base.contains(provider) || result.contains(provider) {
            return Err("invalid_provider_order");
        }
        result.push(provider.clone());
    }
    for provider in base {
        if !result.contains(provider) {
            result.push(provider.clone());
        }
    }
    Ok(result)
}

/// Encryption context of a provider key saved in the Accounts era.
pub(crate) fn key_context(storage_key: Uuid, provider: &str) -> String {
    format!("account/{storage_key}/{provider}")
}

/// Encryption context of a provider key saved before (bound to its legacy row).
pub(crate) fn legacy_key_context(org: &str, storage_key: Uuid, provider: &str) -> String {
    format!("{}/{org}/{storage_key}/{provider}", Uuid::nil())
}

impl ControlState {
    /// Creates the account's `@` preferences row on first use: from its newest
    /// legacy row when there is one, else with Waveform's default voice.
    pub(super) async fn ensure_preferences(&self, storage_key: Uuid) -> Result<(), ControlError> {
        sqlx::query("INSERT INTO waveform_account_preferences(plane_id, org_id, actor_id, tts_order, stt_order, voice_profile) SELECT d.plane_id, $2, $3, legacy.tts_order, legacy.stt_order, COALESCE(legacy.voice_profile, d.voice_profile) FROM waveform_provider_defaults d LEFT JOIN LATERAL (SELECT tts_order, stt_order, voice_profile FROM waveform_account_preferences p WHERE p.plane_id = d.plane_id AND p.actor_id = $3 AND p.org_id <> $2 ORDER BY p.updated_at DESC LIMIT 1) legacy ON true WHERE d.plane_id = $1 ON CONFLICT (plane_id, org_id, actor_id) DO NOTHING")
            .bind(Uuid::nil())
            .bind(ACCOUNT_SCOPE)
            .bind(storage_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

pub(super) async fn preferences(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Recent)
        .await?;
    state.ensure_preferences(target.storage_key()).await?;
    let row = sqlx::query("SELECT d.tts_order AS default_tts, d.stt_order AS default_stt, p.tts_order, p.stt_order, d.voice_profile AS default_voice_profile, p.voice_profile FROM waveform_provider_defaults d LEFT JOIN waveform_account_preferences p ON p.plane_id=d.plane_id AND p.org_id=$2 AND p.actor_id=$3 WHERE d.plane_id=$1")
        .bind(Uuid::nil()).bind(ACCOUNT_SCOPE).bind(target.storage_key()).fetch_one(&state.pool).await?;
    let default_voice: String = row.try_get("default_voice_profile")?;
    let voice: Option<String> = row.try_get("voice_profile")?;
    let default_tts: Value = row.try_get("default_tts")?;
    let default_stt: Value = row.try_get("default_stt")?;
    let tts: Option<Value> = row.try_get("tts_order")?;
    let stt: Option<Value> = row.try_get("stt_order")?;
    Ok(Json(json!({
        "account": {"uuid": target.record.uuid, "id": target.record.current_id},
        "voice_profile": voice.unwrap_or_else(|| default_voice.clone()),
        "tts_order": tts.unwrap_or_else(|| default_tts.clone()),
        "stt_order": stt.unwrap_or_else(|| default_stt.clone()),
        "defaults": {"voice_profile": default_voice, "tts_order": default_tts, "stt_order": default_stt}
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    /// Accepted and ignored: clients before 0.7 may still send it, and Waveform records no
    /// telemetry.
    #[serde(default, rename = "telemetry_enabled")]
    _retired_telemetry_choice: serde::de::IgnoredAny,
    voice_profile: Option<String>,
    tts_order: Option<Vec<String>>,
    stt_order: Option<Vec<String>>,
}

pub(super) async fn update_preferences(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
    Json(body): Json<Preferences>,
) -> Result<Json<Value>, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Always)
        .await?;
    if target.as_custodian {
        // A custodian changing a Silicon's settings must still be signed in right now.
        state.ensure_live(&caller).await?;
    }
    if let Some(id) = &body.voice_profile {
        if !crate::domain::voice::valid_profile_id(id) {
            return Err(ControlError::bad_request(
                "invalid_voice_profile",
                format!("{id} is not a voice profile id (lowercase letters, digits and hyphens)."),
            ));
        }
        state.voice_profile(id).await?;
    }
    let row = sqlx::query(
        "SELECT tts_order, stt_order FROM waveform_provider_defaults WHERE plane_id=$1",
    )
    .bind(Uuid::nil())
    .fetch_one(&state.pool)
    .await?;
    let parse = |column| -> Result<Vec<String>, ControlError> {
        serde_json::from_value(row.try_get(column)?).map_err(|_| {
            ControlError::unavailable(
                "invalid_provider_defaults",
                "Waveform's stored default provider order is invalid.",
            )
        })
    };
    let invalid_order = |_| {
        ControlError::bad_request(
            "invalid_provider_order",
            "A provider order lists each provider of that operation at most once (TTS: gemini, elevenlabs, openai; STT: gemini, openai, deepgram).",
        )
    };
    let tts = body
        .tts_order
        .map(|order| resolve_order(&order, &parse("tts_order")?).map_err(invalid_order))
        .transpose()?;
    let stt = body
        .stt_order
        .map(|order| resolve_order(&order, &parse("stt_order")?).map_err(invalid_order))
        .transpose()?;
    state.ensure_preferences(target.storage_key()).await?;
    let row = sqlx::query("UPDATE waveform_account_preferences SET voice_profile=COALESCE($4, voice_profile), tts_order=COALESCE($5, tts_order), stt_order=COALESCE($6, stt_order), updated_at=now() WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 RETURNING tts_order, stt_order, voice_profile")
        .bind(Uuid::nil())
        .bind(ACCOUNT_SCOPE)
        .bind(target.storage_key())
        .bind(body.voice_profile)
        .bind(tts.map(sqlx::types::Json))
        .bind(stt.map(sqlx::types::Json))
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(json!({
        "account": {"uuid": target.record.uuid, "id": target.record.current_id},
        "voice_profile": row.try_get::<Option<String>, _>("voice_profile")?,
        "tts_order": row.try_get::<Option<Value>, _>("tts_order")?,
        "stt_order": row.try_get::<Option<Value>, _>("stt_order")?,
    })))
}

fn validate_provider(provider: &str) -> Result<(), ControlError> {
    if matches!(provider, "gemini" | "elevenlabs" | "openai" | "deepgram") {
        Ok(())
    } else {
        Err(ControlError::bad_request(
            "invalid_provider",
            format!("{provider} is not a provider; use gemini, elevenlabs, openai or deepgram."),
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderKey {
    api_key: String,
}

pub(super) async fn provider_keys(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Recent)
        .await?;
    let rows = sqlx::query("SELECT provider, max(updated_at) AS updated_at FROM waveform_provider_keys WHERE plane_id=$1 AND actor_id=$2 GROUP BY provider ORDER BY provider")
        .bind(Uuid::nil())
        .bind(target.storage_key())
        .fetch_all(&state.pool)
        .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(json!({
                "provider": row.try_get::<String, _>("provider")?,
                "configured": true,
                "updated_at": row.try_get::<time::OffsetDateTime, _>("updated_at")?.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
            }))
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Json(json!({
        "account": {"uuid": target.record.uuid, "id": target.record.current_id},
        "items": items
    })))
}

pub(super) async fn put_provider_key(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Query(query): Query<AccountQuery>,
    Json(body): Json<ProviderKey>,
) -> Result<StatusCode, ControlError> {
    validate_provider(&provider)?;
    if body.api_key.is_empty()
        || body.api_key.len() > 16_384
        || !body.api_key.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(ControlError::bad_request(
            "invalid_api_key",
            "An API key is 1 to 16384 visible ASCII characters with no spaces.",
        ));
    }
    let caller = state
        .fresh_caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Always)
        .await?;
    let encrypted = state
        .vault()?
        .seal(&body.api_key, &key_context(target.storage_key(), &provider))
        .map_err(|_| {
            ControlError::unavailable(
                "secret_storage_unavailable",
                "The key could not be encrypted.",
            )
        })?;
    let mut transaction = state.pool.begin().await?;
    sqlx::query("INSERT INTO waveform_provider_keys(plane_id,org_id,actor_id,provider,secret_cipher) VALUES($1,$2,$3,$4,$5) ON CONFLICT(plane_id,org_id,actor_id,provider) DO UPDATE SET secret_cipher=EXCLUDED.secret_cipher,updated_at=now()")
        .bind(Uuid::nil()).bind(ACCOUNT_SCOPE).bind(target.storage_key()).bind(&provider).bind(encrypted)
        .execute(&mut *transaction).await?;
    // The new key replaces any key this account saved before, in every former scope.
    sqlx::query("DELETE FROM waveform_provider_keys WHERE plane_id=$1 AND actor_id=$2 AND provider=$3 AND org_id<>$4")
        .bind(Uuid::nil()).bind(target.storage_key()).bind(&provider).bind(ACCOUNT_SCOPE)
        .execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn delete_provider_key(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Query(query): Query<AccountQuery>,
) -> Result<StatusCode, ControlError> {
    validate_provider(&provider)?;
    let caller = state
        .fresh_caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Always)
        .await?;
    sqlx::query(
        "DELETE FROM waveform_provider_keys WHERE plane_id=$1 AND actor_id=$2 AND provider=$3",
    )
    .bind(Uuid::nil())
    .bind(target.storage_key())
    .bind(provider)
    .execute(&state.pool)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[async_trait::async_trait]
impl crate::application::ports::ProviderKeyStore for ControlState {
    async fn load(
        &self,
        actor: &AuthorizedActor,
    ) -> Result<
        crate::application::ports::ProviderKeys,
        crate::application::ports::ProviderKeyStoreError,
    > {
        use crate::application::ports::{ProviderKeyStoreError, ProviderKeys};
        let storage_key = actor.actor.id.as_uuid();
        // One key per provider: the Accounts-era row, else the newest legacy row.
        let rows = sqlx::query("SELECT DISTINCT ON (provider) provider, org_id, secret_cipher FROM waveform_provider_keys WHERE plane_id=$1 AND actor_id=$2 ORDER BY provider, (org_id = $3) DESC, updated_at DESC")
            .bind(Uuid::nil())
            .bind(storage_key)
            .bind(ACCOUNT_SCOPE)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| ProviderKeyStoreError)?;
        let mut keys = ProviderKeys::new();
        for row in rows {
            let name: String = row.try_get("provider").map_err(|_| ProviderKeyStoreError)?;
            let org: String = row.try_get("org_id").map_err(|_| ProviderKeyStoreError)?;
            let provider: ProviderName =
                serde_json::from_value(json!(name)).map_err(|_| ProviderKeyStoreError)?;
            let cipher: Vec<u8> = row
                .try_get("secret_cipher")
                .map_err(|_| ProviderKeyStoreError)?;
            let context = if org == ACCOUNT_SCOPE {
                key_context(storage_key, &name)
            } else {
                legacy_key_context(&org, storage_key, &name)
            };
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

#[async_trait::async_trait]
impl crate::application::ports::ProviderOrderStore for ControlState {
    async fn provider_order(
        &self,
        actor: &AuthorizedActor,
        operation: SpeechOperation,
    ) -> Result<Vec<ProviderName>, crate::domain::error::WaveformError> {
        use crate::domain::error::{Dependency, WaveformError};
        let unavailable = || WaveformError::DependencyUnavailable {
            dependency: Dependency::IdempotencyStore,
        };
        let (default_column, preference_column, base) = match operation {
            SpeechOperation::Tts => ("default_tts", "tts_order", TTS_PROVIDER_CHAIN.as_slice()),
            SpeechOperation::Stt => ("default_stt", "stt_order", STT_PROVIDER_CHAIN.as_slice()),
        };
        let row = sqlx::query("SELECT d.tts_order AS default_tts, d.stt_order AS default_stt, p.tts_order, p.stt_order FROM waveform_provider_defaults d LEFT JOIN LATERAL (SELECT tts_order, stt_order FROM waveform_account_preferences p WHERE p.plane_id=d.plane_id AND p.actor_id=$2 ORDER BY (p.org_id = $3) DESC, p.updated_at DESC LIMIT 1) p ON true WHERE d.plane_id=$1")
            .bind(Uuid::nil())
            .bind(actor.actor.id.as_uuid())
            .bind(ACCOUNT_SCOPE)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| unavailable())?;
        let default: Value = row.try_get(default_column).map_err(|_| unavailable())?;
        let preference: Option<Value> =
            row.try_get(preference_column).map_err(|_| unavailable())?;
        let parsed: Vec<ProviderName> =
            serde_json::from_value(preference.unwrap_or(default)).map_err(|_| unavailable())?;
        let order =
            crate::domain::provider::resolve_order(&parsed, base).ok_or_else(unavailable)?;
        if order.len() == parsed.len() {
            Ok(order)
        } else {
            Err(unavailable())
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::{key_context, legacy_key_context, resolve_order};
    use uuid::Uuid;

    #[test]
    fn preference_completion_preserves_base_order_and_rejects_duplicates() {
        let base = ["gemini", "elevenlabs", "openai"].map(str::to_owned);
        assert_eq!(
            resolve_order(&["openai".to_owned()], &base),
            Ok(["openai", "gemini", "elevenlabs"]
                .map(str::to_owned)
                .to_vec())
        );
        assert!(resolve_order(&["openai".to_owned(), "openai".to_owned()], &base).is_err());
        assert!(resolve_order(&["deepgram".to_owned()], &base).is_err());
    }

    #[test]
    fn key_contexts_bind_the_owner_and_keep_legacy_rows_readable() {
        let key = Uuid::from_u128(7);
        assert_eq!(key_context(key, "openai"), format!("account/{key}/openai"));
        assert_eq!(
            legacy_key_context("tos", key, "openai"),
            format!("{}/tos/{key}/openai", Uuid::nil())
        );
    }
}
