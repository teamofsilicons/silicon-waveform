//! Exact-byte SDK verification followed by durable, plane-scoped deduplication.

use super::{ControlError, ControlState};
use axum::{body::Bytes, extract::State};
use http::{HeaderMap, StatusCode};
use secrecy::ExposeSecret as _;
use silicon_iam_client::EnvironmentKey;
use sqlx::Row as _;
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn receive(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ControlError> {
    let verifier = state
        .verifier
        .as_ref()
        .ok_or_else(|| ControlError::unavailable("webhook_not_configured"))?;
    let delivery = verifier
        .verify(&headers, &body)
        .map_err(|_| ControlError::unauthorized())?;
    let mut plane_id = Uuid::nil();
    if delivery.is_testing() {
        // Never persist or log an unmatched test delivery, or its embedded key.
        let rows = sqlx::query("SELECT id, iam_key_cipher FROM waveform_environments WHERE id<>$1 AND deleted_at IS NULL")
            .bind(Uuid::nil()).fetch_all(&state.pool).await?;
        let mut matched = None;
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let cipher: Vec<u8> = row.try_get("iam_key_cipher")?;
            let key = state
                .vault()?
                .open(&cipher, &format!("{id}/iam-key"))
                .map_err(ControlError::internal)?;
            let expected = EnvironmentKey::new(key.expose_secret())
                .map_err(|_| ControlError::unavailable("invalid_environment_binding"))?;
            if delivery.verify_testing_environment(&expected).is_ok() {
                if matched.is_some() {
                    return Err(ControlError::unavailable("ambiguous_environment_binding"));
                }
                matched = Some(id);
            }
        }
        plane_id = matched.ok_or_else(ControlError::unauthorized)?;
    }
    let event = delivery.event();
    let aggregate_id = event
        .aggregate
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|v| v.parse::<Uuid>().ok())
        .ok_or_else(ControlError::unauthorized)?;
    let aggregate_version = event
        .aggregate
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(ControlError::unauthorized)?;
    // Authority is always re-read online. No stale local authorization cache is
    // granted by event arrival, ordering or replay. Store content-free metadata.
    sqlx::query("INSERT INTO waveform_webhook_events(plane_id,event_id,event_type,aggregate_id,aggregate_version,occurred_at) SELECT id,$2,$3,$4,$5,$6 FROM waveform_environments WHERE id=$1 AND deleted_at IS NULL ON CONFLICT(plane_id,event_id) DO NOTHING")
        .bind(plane_id).bind(event.event_id).bind(&event.event_type).bind(aggregate_id).bind(aggregate_version).bind(event.occurred_at).execute(&state.pool).await?;
    Ok(StatusCode::NO_CONTENT)
}
