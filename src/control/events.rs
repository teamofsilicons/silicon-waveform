//! Bounded, authenticated browser diagnostics. No arbitrary browser data is accepted.
use super::{ControlError, ControlState};
use crate::domain::auth::WaveformAction;
use axum::{Json, extract::State};
use http::{HeaderMap, StatusCode};
use serde::Deserialize;
use std::sync::Arc;

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
        return Err(ControlError::bad_request(
            "invalid_telemetry_event",
            "event must be speech_completed, speech_failed, settings_saved or page_view, with elapsed_ms at most 86400000.",
        ));
    }
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    if state.telemetry_enabled(caller.record.storage_key).await?
        && let Some(station) = &state.station
    {
        crate::telemetry::record(
            station,
            "web",
            &event.event,
            event.event != "speech_failed",
            event.elapsed_ms,
        );
    }
    Ok(StatusCode::NO_CONTENT)
}
