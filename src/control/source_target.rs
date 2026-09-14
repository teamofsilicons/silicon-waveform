//! Provider-free preparation of a private, same-actor STT source destination.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    response::{IntoResponse as _, Response},
};
use http::{HeaderMap, header};
use secrecy::ExposeSecret as _;
use serde::Deserialize;

use super::{ControlError, ControlState};
use crate::{
    api::headers::SpeechHeaders,
    application::ports::BriefcaseError,
    domain::auth::InboundCredentials,
    infrastructure::{auth::SourceStorageDelegator, briefcase_reader::BriefcaseSdkReader},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Prepare {}

pub(super) async fn prepare(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(_body): Json<Prepare>,
) -> Result<Response, ControlError> {
    let parsed = SpeechHeaders::parse(&headers)
        .map_err(|_| ControlError::bad_request("invalid_speech_headers"))?;
    let InboundCredentials::Bearer(token) = parsed.credentials else {
        return Err(ControlError::bad_request("unsupported_authentication_mode"));
    };
    // This is the same selected-plane, current-membership authority used by
    // account/speech APIs. A root key alone cannot identify or authorize anyone.
    let identity = state.identity(&headers).await?;
    if !identity.authority.scopes.contains(&state.source_stt_action)
        || !identity
            .authority
            .scopes
            .iter()
            .any(|scope| scope == "self.identity.read")
        || identity.authority.org_id != parsed.organization_id.as_str()
    {
        return Err(ControlError::forbidden());
    }
    let actor = ControlState::authorized_actor_from_identity(&identity)?;
    let public_id = identity
        .authority
        .public_id
        .as_deref()
        .ok_or_else(ControlError::forbidden)?;
    let environment = if identity.plane.id.is_nil() {
        None
    } else {
        let id = identity.plane.id;
        let cipher: Vec<u8> = sqlx::query_scalar("SELECT briefcase_key_cipher FROM waveform_environments WHERE id=$1 AND deleted_at IS NULL")
            .bind(id).fetch_one(&state.pool).await?;
        if cipher.is_empty() {
            None
        } else {
            let key = state
                .vault()?
                .open(&cipher, &format!("{id}/briefcase-key"))
                .map_err(ControlError::internal)?;
            Some(
                briefcase_client::EnvironmentKey::new(key.expose_secret())
                    .map_err(|_| ControlError::unavailable("invalid_environment_binding"))?,
            )
        }
    };
    let testing_environment_id = identity.plane.iam_environment_id;
    let iam = Arc::new(SourceStorageDelegator {
        testing: testing_environment_id.is_some(),
        sdk: identity.plane.iam,
        application_id: state
            .app_id
            .parse()
            .map_err(|_| ControlError::unavailable("invalid_application"))?,
        audience: state.briefcase_settings.audience.clone(),
    });
    let target = BriefcaseSdkReader::new(state.briefcase_settings.clone(), iam)
        .prepare_source_target(&actor, public_id, token, parsed.request_id, environment)
        .await
        .map_err(|error| match error {
            BriefcaseError::Unauthorized => ControlError::unauthorized(),
            BriefcaseError::Forbidden => ControlError::forbidden(),
            BriefcaseError::NotFound => ControlError::not_found(),
            _ => ControlError::unavailable("briefcase_unavailable"),
        })?;
    let mut result = serde_json::to_value(target)
        .map_err(|_| ControlError::unavailable("briefcase_unavailable"))?;
    result["testing_environment_id"] = serde_json::json!(testing_environment_id);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}
