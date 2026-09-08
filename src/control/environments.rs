//! Isolated Waveform test-plane lifecycle.

use super::{ControlError, ControlState};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use rand::{Rng as _, distr::Alphanumeric};
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateEnvironment {
    pub name: String,
    pub description: Option<String>,
    pub iam_environment_id: Uuid,
    pub iam_environment_key: String,
    pub app_secret: String,
    pub briefcase_environment_key: String,
}

#[derive(Serialize)]
pub(super) struct CreatedEnvironment {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

fn root_key() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn validate_key(value: &str) -> Result<(), ControlError> {
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(ControlError::bad_request("invalid_environment_key"))
    }
}

pub(super) async fn create(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(body): Json<CreateEnvironment>,
) -> Result<Json<CreatedEnvironment>, ControlError> {
    let identity = state.identity(&headers).await?;
    if !identity.plane.id.is_nil() {
        return Err(ControlError::bad_request("production_management_required"));
    }
    if body.name.trim().is_empty() || body.name.chars().count() > 128 || body.name.contains('/') {
        return Err(ControlError::bad_request("invalid_environment_name"));
    }
    validate_key(&body.iam_environment_key)?;
    validate_key(&body.briefcase_environment_key)?;
    if body.app_secret.trim().is_empty()
        || body.app_secret.len() > 512
        || body
            .app_secret
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(ControlError::bad_request("invalid_test_application_secret"));
    }
    let vault = state.vault()?;
    let id = Uuid::new_v4();
    let key = root_key();
    let root_hash = vault
        .digest(&key, "environment-root")
        .map_err(ControlError::internal)?;
    let iam_cipher = vault
        .seal(&body.iam_environment_key, &format!("{id}/iam-key"))
        .map_err(ControlError::internal)?;
    let app_secret_cipher = vault
        .seal(&body.app_secret, &format!("{id}/app-secret"))
        .map_err(ControlError::internal)?;
    let briefcase_cipher = vault
        .seal(
            &body.briefcase_environment_key,
            &format!("{id}/briefcase-key"),
        )
        .map_err(ControlError::internal)?;
    let mut transaction = state.pool.begin().await?;
    sqlx::query("INSERT INTO waveform_environments(id,org_id,creator_id,name,description,root_key_hash,root_key_cipher,iam_key_cipher,app_secret_cipher,briefcase_key_cipher,iam_environment_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(&identity.authority.org_id).bind(identity.authority.principal_id).bind(&body.name).bind(&body.description).bind(root_hash)
        .bind(vault.seal(&key, &format!("{id}/root-key")).map_err(ControlError::internal)?).bind(iam_cipher).bind(app_secret_cipher).bind(briefcase_cipher).bind(body.iam_environment_id)
        .execute(&mut *transaction).await?;
    sqlx::query("INSERT INTO waveform_voice_profiles(plane_id,id,profile) SELECT $1,id,profile FROM waveform_voice_profiles WHERE plane_id=$2")
        .bind(id).bind(Uuid::nil()).execute(&mut *transaction).await?;
    sqlx::query("INSERT INTO waveform_provider_defaults(plane_id,tts_order,stt_order,voice_profile) SELECT $1,tts_order,stt_order,voice_profile FROM waveform_provider_defaults WHERE plane_id=$2")
        .bind(id).bind(Uuid::nil()).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(Json(CreatedEnvironment {
        id,
        name: body.name,
        description: body.description,
        key,
        created_at: time::OffsetDateTime::now_utc(),
    }))
}

pub(super) async fn current(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ControlError> {
    let plane = state.plane(&headers).await?;
    if plane.id.is_nil() {
        return Err(ControlError::bad_request("test_environment_required"));
    }
    let row = sqlx::query("SELECT id,name,description,created_at,last_activity_at FROM waveform_environments WHERE id=$1 AND deleted_at IS NULL").bind(plane.id).fetch_one(&state.pool).await?;
    let id: Uuid = row.try_get("id")?;
    let name: String = row.try_get("name")?;
    let description: Option<String> = row.try_get("description")?;
    let created_at: time::OffsetDateTime = row.try_get("created_at")?;
    let last_activity_at: time::OffsetDateTime = row.try_get("last_activity_at")?;
    Ok(Json(
        serde_json::json!({"id":id,"name":name,"description":description,"created_at": created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),"last_activity_at": last_activity_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()}),
    ))
}

fn can_manage(identity: &super::Identity, creator_id: Uuid) -> bool {
    identity.authority.principal_id == creator_id
        || matches!(
            identity.authority.org_role.as_deref(),
            Some("admin" | "org_admin" | "head" | "org_head" | "owner" | "org_owner")
        )
}

pub(super) async fn list(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    if !identity.plane.id.is_nil() {
        return Err(ControlError::bad_request("production_management_required"));
    }
    let rows = sqlx::query("SELECT id,name,description,created_at,last_activity_at,deleted_at,creator_id FROM waveform_environments WHERE org_id=$1 ORDER BY created_at DESC")
        .bind(&identity.authority.org_id)
        .fetch_all(&state.pool)
        .await?;
    let items = rows.into_iter().map(|row| {
        serde_json::json!({
            "id": row.try_get::<Uuid, _>("id").unwrap_or_default(),
            "name": row.try_get::<String, _>("name").unwrap_or_default(),
            "description": row.try_get::<Option<String>, _>("description").unwrap_or(None),
            "created_at": row.try_get::<time::OffsetDateTime, _>("created_at").ok().and_then(|value| value.format(&time::format_description::well_known::Rfc3339).ok()),
            "last_activity_at": row.try_get::<time::OffsetDateTime, _>("last_activity_at").ok().and_then(|value| value.format(&time::format_description::well_known::Rfc3339).ok()),
            "deleted_at": row.try_get::<Option<time::OffsetDateTime>, _>("deleted_at").ok().flatten().and_then(|value| value.format(&time::format_description::well_known::Rfc3339).ok()),
        })
    }).collect::<Vec<_>>();
    Ok(Json(serde_json::json!({"items": items})))
}

pub(super) async fn detail(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    if !identity.plane.id.is_nil() {
        return Err(ControlError::bad_request("production_management_required"));
    }
    let row = sqlx::query("SELECT id,name,description,created_at,last_activity_at,deleted_at,creator_id,org_id FROM waveform_environments WHERE id=$1 AND org_id=$2")
        .bind(environment_id).bind(&identity.authority.org_id).fetch_optional(&state.pool).await?
        .ok_or_else(ControlError::not_found)?;
    Ok(Json(serde_json::json!({
        "id": row.try_get::<Uuid,_>("id")?, "name": row.try_get::<String,_>("name")?,
        "description": row.try_get::<Option<String>,_>("description")?,
        "created_at": row.try_get::<time::OffsetDateTime,_>("created_at")?.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        "last_activity_at": row.try_get::<time::OffsetDateTime,_>("last_activity_at")?.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        "deleted_at": row.try_get::<Option<time::OffsetDateTime>,_>("deleted_at")?.and_then(|value| value.format(&time::format_description::well_known::Rfc3339).ok())
    })))
}

pub(super) async fn key(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    if !identity.plane.id.is_nil() {
        return Err(ControlError::bad_request("production_management_required"));
    }
    let row = sqlx::query(
        "SELECT creator_id,root_key_cipher FROM waveform_environments WHERE id=$1 AND org_id=$2 AND deleted_at IS NULL",
    )
    .bind(environment_id)
    .bind(&identity.authority.org_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ControlError::not_found)?;
    let creator: Uuid = row.try_get("creator_id")?;
    if !can_manage(&identity, creator) {
        return Err(ControlError::forbidden());
    }
    let cipher: Vec<u8> = row.try_get("root_key_cipher")?;
    let value = state
        .vault()?
        .open(&cipher, &format!("{environment_id}/root-key"))
        .map_err(ControlError::internal)?;
    Ok(Json(serde_json::json!({"key": value.expose_secret()})))
}

pub(super) async fn rotate_key(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    if !identity.plane.id.is_nil() {
        return Err(ControlError::bad_request("production_management_required"));
    }
    let row = sqlx::query("SELECT creator_id FROM waveform_environments WHERE id=$1 AND org_id=$2 AND deleted_at IS NULL")
        .bind(environment_id).bind(&identity.authority.org_id).fetch_optional(&state.pool).await?
        .ok_or_else(ControlError::not_found)?;
    let creator: Uuid = row.try_get("creator_id")?;
    if !can_manage(&identity, creator) {
        return Err(ControlError::forbidden());
    }
    let next = root_key();
    let hash = state
        .vault()?
        .digest(&next, "environment-root")
        .map_err(ControlError::internal)?;
    let cipher = state
        .vault()?
        .seal(&next, &format!("{environment_id}/root-key"))
        .map_err(ControlError::internal)?;
    sqlx::query("UPDATE waveform_environments SET root_key_hash=$1,root_key_cipher=$2,last_activity_at=now() WHERE id=$3")
        .bind(hash).bind(cipher).bind(environment_id).execute(&state.pool).await?;
    Ok(Json(serde_json::json!({"key": next})))
}

pub(super) async fn delete(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    if !identity.plane.id.is_nil() {
        return Err(ControlError::bad_request("production_management_required"));
    }
    let row = sqlx::query("SELECT creator_id FROM waveform_environments WHERE id=$1 AND org_id=$2 AND deleted_at IS NULL")
        .bind(environment_id).bind(&identity.authority.org_id).fetch_optional(&state.pool).await?
        .ok_or_else(ControlError::not_found)?;
    let creator: Uuid = row.try_get("creator_id")?;
    if !can_manage(&identity, creator) {
        return Err(ControlError::forbidden());
    }
    sqlx::query("UPDATE waveform_environments SET deleted_at=now() WHERE id=$1")
        .bind(environment_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(
        serde_json::json!({"deleted": true, "recoverable_for_days": 30}),
    ))
}

pub(super) async fn restore(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    if !identity.plane.id.is_nil() {
        return Err(ControlError::bad_request("production_management_required"));
    }
    let row = sqlx::query("SELECT creator_id FROM waveform_environments WHERE id=$1 AND org_id=$2 AND deleted_at IS NOT NULL AND deleted_at > now() - interval '30 days'")
        .bind(environment_id).bind(&identity.authority.org_id).fetch_optional(&state.pool).await?
        .ok_or_else(ControlError::not_found)?;
    let creator: Uuid = row.try_get("creator_id")?;
    if !can_manage(&identity, creator) {
        return Err(ControlError::forbidden());
    }
    sqlx::query(
        "UPDATE waveform_environments SET deleted_at=NULL,last_activity_at=now() WHERE id=$1",
    )
    .bind(environment_id)
    .execute(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({"restored": true})))
}

pub(super) async fn clean(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ControlError> {
    let plane = state.plane(&headers).await?;
    if plane.id.is_nil() {
        return Err(ControlError::bad_request("test_environment_required"));
    }
    let mut transaction = state.pool.begin().await?;
    sqlx::query(
        "SELECT id FROM waveform_environments WHERE id=$1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(plane.id)
    .fetch_one(&mut *transaction)
    .await?;
    for table in [
        "waveform_idempotency_records",
        "waveform_jobs",
        "waveform_webhook_events",
        "waveform_account_preferences",
        "waveform_provider_keys",
    ] {
        let statement = format!("DELETE FROM {table} WHERE plane_id=$1");
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(plane.id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(Json(serde_json::json!({"cleaned": true})))
}
