//! Exact-byte SDK verification followed by durable, plane-scoped deduplication.

use super::{ControlError, ControlState};
use axum::{body::Bytes, extract::State};
use http::{HeaderMap, StatusCode};
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use silicon_iam_client::EnvironmentKey;
use sqlx::Row as _;
use std::sync::Arc;
use subtle::ConstantTimeEq as _;
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
    let mut received_digest = None;
    if delivery.is_testing() {
        // Never persist or log an unmatched test delivery, or its embedded key.
        let rows = sqlx::query("SELECT id, iam_key_cipher, webhook_key_digest FROM waveform_environments WHERE id<>$1 AND deleted_at IS NULL")
            .bind(Uuid::nil()).fetch_all(&state.pool).await?;
        let mut matched = None;
        // Parse only after complete raw-body signature verification. Never persist the envelope.
        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| ControlError::unauthorized())?;
        let received = envelope
            .pointer("/test/testing_key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(ControlError::unauthorized)?;
        let digest = format!("{:x}", Sha256::digest(received.as_bytes()));
        received_digest = Some(digest.clone());
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let cipher: Vec<u8> = row.try_get("iam_key_cipher")?;
            let discovered: Option<String> = row.try_get("webhook_key_digest")?;
            if let Some(expected) = discovered {
                if bool::from(expected.as_bytes().ct_eq(digest.as_bytes()))
                    && matched.replace(id).is_some()
                {
                    return Err(ControlError::unavailable("ambiguous_environment_binding"));
                }
                continue;
            }
            if cipher.is_empty() {
                continue;
            }
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
    let _fence = if plane_id.is_nil() {
        None
    } else {
        Some(state.test_fence(plane_id).await?)
    };
    if let Some(received) = received_digest {
        let expected: Option<String> =
            sqlx::query_scalar("SELECT webhook_key_digest FROM waveform_environments WHERE id=$1")
                .bind(plane_id)
                .fetch_optional(&state.pool)
                .await?
                .flatten();
        if expected
            .is_some_and(|expected| !bool::from(expected.as_bytes().ct_eq(received.as_bytes())))
        {
            return Err(ControlError::unauthorized());
        }
    }
    let event = delivery.event();
    if !plane_id.is_nil() {
        let cleaned: Option<time::OffsetDateTime> =
            sqlx::query_scalar("SELECT cleaned_at FROM waveform_lifecycle WHERE environment_id=$1")
                .bind(plane_id)
                .fetch_optional(&state.pool)
                .await?
                .flatten();
        if cleaned.is_some_and(|time| event.occurred_at <= time) {
            return Ok(StatusCode::NO_CONTENT);
        }
    }
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
