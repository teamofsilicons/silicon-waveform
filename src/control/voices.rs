//! Waveform's voice catalog and each account's default voice.

use super::{ControlError, ControlState};
use crate::domain::{auth::WaveformAction, idempotency::ACCOUNT_SCOPE};
use axum::{Json, extract::State};
use http::HeaderMap;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

impl ControlState {
    pub(super) async fn voice_profile(
        &self,
        id: &str,
    ) -> Result<crate::domain::voice::VoiceProfile, ControlError> {
        let value: Value = sqlx::query_scalar(
            "SELECT profile FROM waveform_voice_profiles WHERE plane_id=$1 AND id=$2",
        )
        .bind(Uuid::nil())
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            ControlError::bad_request(
                "invalid_voice_profile",
                format!("There is no voice profile {id}; GET /api/v1/voice-profiles lists them."),
            )
        })?;
        parse_profile(value)
    }
}

fn parse_profile(value: Value) -> Result<crate::domain::voice::VoiceProfile, ControlError> {
    let invalid = || {
        ControlError::unavailable(
            "invalid_voice_mapping",
            "A stored voice profile mapping is invalid.",
        )
    };
    let profile: crate::domain::voice::VoiceProfile =
        serde_json::from_value(value).map_err(|_| invalid())?;
    if !profile.is_valid() {
        return Err(invalid());
    }
    Ok(profile)
}

pub(super) async fn list(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ControlError> {
    state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let values: Vec<Value> = sqlx::query_scalar(
        "SELECT profile FROM waveform_voice_profiles WHERE plane_id=$1 ORDER BY id",
    )
    .bind(Uuid::nil())
    .fetch_all(&state.pool)
    .await?;
    let profiles = values
        .into_iter()
        .map(parse_profile)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({"items": profiles})))
}

#[async_trait::async_trait]
impl crate::application::ports::VoiceProfileStore for ControlState {
    async fn resolve(
        &self,
        actor: &crate::domain::auth::AuthorizedActor,
        requested: Option<&str>,
    ) -> Result<crate::domain::voice::VoiceProfile, crate::domain::error::WaveformError> {
        use crate::domain::error::{Dependency, WaveformError};
        let unavailable = || WaveformError::DependencyUnavailable {
            dependency: Dependency::VoiceProfiles,
        };
        // An account's default voice is assigned the first time it uses Waveform.
        self.ensure_preferences(actor.actor.id.as_uuid())
            .await
            .map_err(|_| unavailable())?;
        let id: String = sqlx::query_scalar("SELECT COALESCE($3, p.voice_profile, d.voice_profile) FROM waveform_provider_defaults d LEFT JOIN waveform_account_preferences p ON p.plane_id=d.plane_id AND p.org_id=$4 AND p.actor_id=$2 WHERE d.plane_id=$1")
            .bind(Uuid::nil())
            .bind(actor.actor.id.as_uuid())
            .bind(requested)
            .bind(ACCOUNT_SCOPE)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| unavailable())?;
        self.voice_profile(&id).await.map_err(|error| {
            if error.status == http::StatusCode::BAD_REQUEST {
                WaveformError::InvalidRequest
            } else {
                unavailable()
            }
        })
    }
}
