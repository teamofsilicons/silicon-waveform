//! Bounded, authenticated browser diagnostics. No arbitrary browser data is accepted.
use super::{ControlError, ControlState};
use axum::{Json, extract::State};
use http::{HeaderMap, StatusCode};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Event {
    event: String,
    #[serde(default)]
    elapsed_ms: u64,
}
pub(super) async fn record(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(event): Json<Event>,
) -> Result<StatusCode, ControlError> {
    if !matches!(
        event.event.as_str(),
        "speech_completed" | "speech_failed" | "settings_saved" | "page_view"
    ) || event.elapsed_ms > 86_400_000
    {
        return Err(ControlError::bad_request("invalid_telemetry_event"));
    }
    let identity = state.identity(&headers).await?;
    if identity.plane.id.is_nil() {
        let enabled:bool=sqlx::query_scalar("SELECT COALESCE((SELECT telemetry_enabled FROM waveform_account_preferences WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3),true)").bind(Uuid::nil()).bind(&identity.authority.org_id).bind(identity.storage_actor_id).fetch_one(&state.pool).await?;
        if enabled && let Some(station) = &state.station {
            crate::telemetry::record(
                station,
                "web",
                &event.event,
                event.event != "speech_failed",
                event.elapsed_ms,
            );
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
