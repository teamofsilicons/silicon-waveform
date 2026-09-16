//! Isolated Waveform test-plane lifecycle.

use super::{ControlError, ControlState};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use sqlx::Row as _;
use std::sync::Arc;
use uuid::Uuid;

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

/// Historical management URLs remain explicit migration errors.
pub(super) async fn managed_by_honeycomb() -> Result<Json<serde_json::Value>, ControlError> {
    Err(ControlError {
        status: http::StatusCode::CONFLICT,
        code: "manage_environment_in_honeycomb",
    })
}
