//! Content-free speech history, owned by the account that made each request.
//! A custodian may read its Silicons' history (`?account=si:…`).

use super::{ControlError, ControlState};
use crate::{domain::auth::WaveformAction, infrastructure::directory::Confirm};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::sync::Arc;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Deserialize)]
pub(super) struct Filters {
    operation: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
    account: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AccountFilter {
    account: Option<String>,
}
#[derive(Serialize)]
pub(super) struct Job {
    voice_profile: Option<serde_json::Value>,
    id: String,
    /// App that called Waveform for the account with a proof, when one did.
    issuing_app: Option<String>,
    operation: String,
    status: String,
    first_line: String,
    duration_ms: Option<i64>,
    provider: Option<String>,
    error_code: Option<String>,
    created_at: String,
    finished_at: Option<String>,
}
#[derive(Serialize)]
pub(super) struct Page {
    account: serde_json::Value,
    items: Vec<Job>,
    next_cursor: Option<String>,
}

/// The history cursor is an opaque pair because ordering is by creation time
/// and UUID.  Keeping both values avoids skipping records created in a
/// different millisecond whose UUID happens to sort below the previous row.
fn parse_cursor(value: Option<&str>) -> Result<Option<(OffsetDateTime, Uuid)>, ControlError> {
    let Some(value) = value else { return Ok(None) };
    let invalid = || {
        ControlError::bad_request(
            "invalid_cursor",
            "cursor must be the next_cursor value of the previous page, unchanged.",
        )
    };
    let Some((timestamp, id)) = value.split_once('|') else {
        return Err(invalid());
    };
    let timestamp = OffsetDateTime::parse(timestamp, &Rfc3339).map_err(|_| invalid())?;
    let id = Uuid::parse_str(id).map_err(|_| invalid())?;
    Ok(Some((timestamp, id)))
}

pub(super) async fn list(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Query(filters): Query<Filters>,
) -> Result<Json<Page>, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, filters.account.as_deref(), Confirm::Recent)
        .await?;
    crate::infrastructure::idempotency::recover_expired_jobs(&state.pool)
        .await
        .map_err(|_| {
            ControlError::unavailable(
                "database_unavailable",
                "Waveform's database is unavailable.",
            )
        })?;
    let limit = filters.limit.unwrap_or(50).clamp(1, 100);
    if filters
        .operation
        .as_deref()
        .is_some_and(|value| !matches!(value, "tts" | "stt"))
    {
        return Err(ControlError::bad_request(
            "invalid_operation",
            "operation must be tts or stt.",
        ));
    }
    let cursor = parse_cursor(filters.cursor.as_deref())?;
    let (cursor_time, cursor_id) = cursor.map_or((None, None), |(time, id)| (Some(time), Some(id)));
    let rows = sqlx::query("SELECT id,issuing_app,operation,status,first_line,duration_ms,provider,error_code,created_at,finished_at,voice_profile FROM waveform_jobs WHERE plane_id=$1 AND actor_id=$2 AND ($3::text IS NULL OR operation=$3) AND ($4::timestamptz IS NULL OR created_at < $4 OR (created_at = $4 AND id < $5)) ORDER BY created_at DESC,id DESC LIMIT $6")
        .bind(Uuid::nil()).bind(target.storage_key()).bind(filters.operation).bind(cursor_time).bind(cursor_id).bind(limit + 1).fetch_all(&state.pool).await?;
    let page_limit = usize::try_from(limit).unwrap_or(100);
    let has_more = rows.len() > page_limit;
    let rows = rows.into_iter().take(page_limit).collect::<Vec<_>>();
    let next_cursor = if has_more {
        rows.last().and_then(|row| {
            let created_at = row.try_get::<OffsetDateTime, _>("created_at").ok()?;
            let id = row.try_get::<Uuid, _>("id").ok()?;
            Some(format!("{}|{id}", created_at.format(&Rfc3339).ok()?))
        })
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| -> Result<Job, sqlx::Error> {
            Ok(Job {
                id: row.try_get::<uuid::Uuid, _>("id")?.to_string(),
                issuing_app: row.try_get("issuing_app")?,
                operation: row.try_get("operation")?,
                status: row.try_get("status")?,
                first_line: row.try_get("first_line")?,
                duration_ms: row.try_get("duration_ms")?,
                provider: row.try_get("provider")?,
                voice_profile: row.try_get("voice_profile")?,
                error_code: row.try_get("error_code")?,
                created_at: row
                    .try_get::<time::OffsetDateTime, _>("created_at")?
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
                finished_at: row
                    .try_get::<Option<time::OffsetDateTime>, _>("finished_at")?
                    .map(|value| {
                        value
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default()
                    }),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(Page {
        account: serde_json::json!({"uuid": target.record.uuid, "id": target.record.current_id}),
        items,
        next_cursor,
    }))
}

pub(super) async fn get(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(job_id): Path<Uuid>,
    Query(filter): Query<AccountFilter>,
) -> Result<Json<Job>, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, filter.account.as_deref(), Confirm::Recent)
        .await?;
    crate::infrastructure::idempotency::recover_expired_jobs(&state.pool)
        .await
        .map_err(|_| {
            ControlError::unavailable(
                "database_unavailable",
                "Waveform's database is unavailable.",
            )
        })?;
    // Another account's job id answers exactly like an unknown one.
    let row = sqlx::query("SELECT id,issuing_app,operation,status,first_line,duration_ms,provider,error_code,created_at,finished_at,voice_profile FROM waveform_jobs WHERE id=$1 AND plane_id=$2 AND actor_id=$3")
        .bind(job_id)
        .bind(Uuid::nil())
        .bind(target.storage_key())
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ControlError::not_found("not_found", format!("There is no job {job_id} in this account's history.")))?;
    Ok(Json(Job {
        id: row.try_get::<Uuid, _>("id")?.to_string(),
        issuing_app: row.try_get("issuing_app")?,
        operation: row.try_get("operation")?,
        status: row.try_get("status")?,
        first_line: row.try_get("first_line")?,
        duration_ms: row.try_get("duration_ms")?,
        provider: row.try_get("provider")?,
        voice_profile: row.try_get("voice_profile")?,
        error_code: row.try_get("error_code")?,
        created_at: row
            .try_get::<time::OffsetDateTime, _>("created_at")?
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        finished_at: row
            .try_get::<Option<time::OffsetDateTime>, _>("finished_at")?
            .map(|value| {
                value
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default()
            }),
    }))
}

#[cfg(test)]
mod tests {
    use super::parse_cursor;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    use uuid::Uuid;

    #[test]
    fn cursor_requires_timestamp_and_uuid() {
        let id = Uuid::from_u128(7);
        let timestamp = OffsetDateTime::UNIX_EPOCH;
        let encoded = format!("{}|{id}", timestamp.format(&Rfc3339).unwrap_or_default());
        let parsed = parse_cursor(Some(&encoded));
        assert!(parsed.is_ok());
        assert_eq!(parsed.ok(), Some(Some((timestamp, id))));
        assert!(parse_cursor(Some("not-a-cursor")).is_err());
        assert!(parse_cursor(Some("2026-01-01T00:00:00Z|not-a-uuid")).is_err());
    }
}
