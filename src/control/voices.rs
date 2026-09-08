//! Plane-scoped catalog and account defaults.

use super::{ControlError, ControlState};
use axum::{Json, extract::State};
use http::HeaderMap;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

impl ControlState {
    /// Called at first Waveform login/use; never overwrites an existing choice.
    pub(super) async fn ensure_voice_default(
        &self,
        plane: Uuid,
        org: &str,
        actor: Uuid,
    ) -> Result<(), ControlError> {
        sqlx::query("INSERT INTO waveform_account_preferences(plane_id,org_id,actor_id,voice_profile) SELECT plane_id,$2,$3,voice_profile FROM waveform_provider_defaults WHERE plane_id=$1 ON CONFLICT(plane_id,org_id,actor_id) DO NOTHING")
            .bind(plane).bind(org).bind(actor).execute(&self.pool).await?;
        Ok(())
    }

    pub(super) async fn voice_profile(
        &self,
        plane: Uuid,
        id: &str,
    ) -> Result<crate::domain::voice::VoiceProfile, ControlError> {
        let value: Value = sqlx::query_scalar(
            "SELECT profile FROM waveform_voice_profiles WHERE plane_id=$1 AND id=$2",
        )
        .bind(plane)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| ControlError::bad_request("invalid_voice_profile"))?;
        parse_profile(value)
    }
}

fn parse_profile(value: Value) -> Result<crate::domain::voice::VoiceProfile, ControlError> {
    let profile: crate::domain::voice::VoiceProfile = serde_json::from_value(value)
        .map_err(|_| ControlError::unavailable("invalid_voice_mapping"))?;
    if !profile.is_valid() {
        return Err(ControlError::unavailable("invalid_voice_mapping"));
    }
    Ok(profile)
}

pub(super) async fn list(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ControlError> {
    let identity = state.identity(&headers).await?;
    let values: Vec<Value> = sqlx::query_scalar(
        "SELECT profile FROM waveform_voice_profiles WHERE plane_id=$1 ORDER BY id",
    )
    .bind(identity.plane.id)
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
        plane: Uuid,
        actor: &crate::domain::auth::AuthorizedActor,
        requested: Option<&str>,
    ) -> Result<crate::domain::voice::VoiceProfile, crate::domain::error::WaveformError> {
        use crate::domain::error::{Dependency, WaveformError};
        let unavailable = |_| WaveformError::DependencyUnavailable {
            dependency: Dependency::VoiceProfiles,
        };
        self.ensure_voice_default(
            plane,
            actor.organization_id.as_str(),
            actor.actor.id.as_uuid(),
        )
        .await
        .map_err(unavailable)?;
        let id: String = sqlx::query_scalar("SELECT COALESCE($4,p.voice_profile,d.voice_profile) FROM waveform_provider_defaults d LEFT JOIN waveform_account_preferences p ON p.plane_id=d.plane_id AND p.org_id=$2 AND p.actor_id=$3 WHERE d.plane_id=$1")
            .bind(plane).bind(actor.organization_id.as_str()).bind(actor.actor.id.as_uuid()).bind(requested)
            .fetch_one(&self.pool).await.map_err(|_| WaveformError::DependencyUnavailable { dependency: Dependency::VoiceProfiles })?;
        self.voice_profile(plane, &id).await.map_err(|e| {
            if e.status == http::StatusCode::BAD_REQUEST {
                WaveformError::InvalidRequest
            } else {
                unavailable(e)
            }
        })
    }
}
