//! Account-scoped provider configuration. API keys are write-only.

use super::{ControlError, ControlState};
use axum::{
    Json,
    extract::{Path, State},
};
use http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row as _;
use std::sync::Arc;

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

pub(super) async fn preferences(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    let row = sqlx::query("SELECT d.tts_order AS default_tts, d.stt_order AS default_stt, p.tts_order, p.stt_order, d.voice_profile AS default_voice_profile, p.voice_profile FROM waveform_provider_defaults d LEFT JOIN waveform_account_preferences p ON p.plane_id=d.plane_id AND p.org_id=$2 AND p.actor_id=$3 WHERE d.plane_id=$1")
        .bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.authority.principal_id).fetch_one(&state.pool).await?;
    let default_voice: String = row.try_get("default_voice_profile")?;
    let voice: Option<String> = row.try_get("voice_profile")?;
    let default_tts: Value = row.try_get("default_tts")?;
    let default_stt: Value = row.try_get("default_stt")?;
    let tts: Option<Value> = row.try_get("tts_order")?;
    let stt: Option<Value> = row.try_get("stt_order")?;
    Ok(Json(
        json!({"voice_profile": voice.unwrap_or_else(|| default_voice.clone()), "tts_order": tts.unwrap_or_else(|| default_tts.clone()), "stt_order": stt.unwrap_or_else(|| default_stt.clone()), "defaults": {"voice_profile": default_voice, "tts_order": default_tts, "stt_order": default_stt}}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    voice_profile: Option<String>,
    tts_order: Option<Vec<String>>,
    stt_order: Option<Vec<String>>,
}

pub(super) async fn update_preferences(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(body): Json<Preferences>,
) -> Result<Json<Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    if let Some(id) = &body.voice_profile {
        if !crate::domain::voice::valid_profile_id(id) {
            return Err(ControlError::bad_request("invalid_voice_profile"));
        }
        state.voice_profile(identity.plane.id, id).await?;
    }
    let row = sqlx::query(
        "SELECT tts_order, stt_order FROM waveform_provider_defaults WHERE plane_id=$1",
    )
    .bind(identity.plane.id)
    .fetch_one(&state.pool)
    .await?;
    let parse = |column| -> Result<Vec<String>, ControlError> {
        serde_json::from_value(row.try_get(column)?)
            .map_err(|_| ControlError::unavailable("invalid_provider_defaults"))
    };
    let tts = body
        .tts_order
        .map(|order| {
            resolve_order(&order, &parse("tts_order")?)
                .map(Json)
                .map_err(ControlError::bad_request)
        })
        .transpose()?;
    let stt = body
        .stt_order
        .map(|order| {
            resolve_order(&order, &parse("stt_order")?)
                .map(Json)
                .map_err(ControlError::bad_request)
        })
        .transpose()?;
    let row = sqlx::query("INSERT INTO waveform_account_preferences(plane_id,org_id,actor_id,tts_order,stt_order,voice_profile) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(plane_id,org_id,actor_id) DO UPDATE SET voice_profile=COALESCE(EXCLUDED.voice_profile,waveform_account_preferences.voice_profile), tts_order=COALESCE(EXCLUDED.tts_order,waveform_account_preferences.tts_order), stt_order=COALESCE(EXCLUDED.stt_order,waveform_account_preferences.stt_order), updated_at=now() RETURNING tts_order,stt_order,voice_profile")
        .bind(identity.plane.id).bind(identity.authority.org_id).bind(identity.authority.principal_id)
        .bind(tts.map(|v| sqlx::types::Json(v.0))).bind(stt.map(|v| sqlx::types::Json(v.0)))
        .bind(body.voice_profile).fetch_one(&state.pool).await?;
    Ok(Json(
        json!({"voice_profile": row.try_get::<Option<String>,_>("voice_profile")?, "tts_order": row.try_get::<Option<Value>,_>("tts_order")?, "stt_order": row.try_get::<Option<Value>,_>("stt_order")?}),
    ))
}

fn validate_provider(provider: &str) -> Result<(), ControlError> {
    if matches!(provider, "gemini" | "elevenlabs" | "openai" | "deepgram") {
        Ok(())
    } else {
        Err(ControlError::bad_request("invalid_provider"))
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
) -> Result<Json<Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    let rows = sqlx::query("SELECT provider, updated_at FROM waveform_provider_keys WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 ORDER BY provider")
        .bind(identity.plane.id).bind(identity.authority.org_id).bind(identity.authority.principal_id).fetch_all(&state.pool).await?;
    let items = rows.into_iter().map(|row| Ok(json!({"provider": row.try_get::<String,_>("provider")?, "configured": true, "updated_at": row.try_get::<time::OffsetDateTime,_>("updated_at")?.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()}))).collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Json(json!({"items": items})))
}

pub(super) async fn put_provider_key(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Json(body): Json<ProviderKey>,
) -> Result<StatusCode, ControlError> {
    validate_provider(&provider)?;
    if body.api_key.is_empty()
        || body.api_key.len() > 16_384
        || !body.api_key.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(ControlError::bad_request("invalid_api_key"));
    }
    let identity = state.identity(&headers).await?;
    let context = format!(
        "{}/{}/{}/{}",
        identity.plane.id, identity.authority.org_id, identity.authority.principal_id, provider
    );
    let encrypted = state
        .vault()?
        .seal(&body.api_key, &context)
        .map_err(ControlError::internal)?;
    sqlx::query("INSERT INTO waveform_provider_keys(plane_id,org_id,actor_id,provider,secret_cipher) VALUES($1,$2,$3,$4,$5) ON CONFLICT(plane_id,org_id,actor_id,provider) DO UPDATE SET secret_cipher=EXCLUDED.secret_cipher,updated_at=now()")
        .bind(identity.plane.id).bind(identity.authority.org_id).bind(identity.authority.principal_id).bind(provider).bind(encrypted).execute(&state.pool).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn delete_provider_key(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(provider): Path<String>,
) -> Result<StatusCode, ControlError> {
    validate_provider(&provider)?;
    let identity = state.identity(&headers).await?;
    sqlx::query("DELETE FROM waveform_provider_keys WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 AND provider=$4")
        .bind(identity.plane.id).bind(identity.authority.org_id).bind(identity.authority.principal_id).bind(provider).execute(&state.pool).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::resolve_order;
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
}
