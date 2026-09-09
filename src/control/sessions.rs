//! SLT-only application login; user credentials never enter Waveform.

use super::{ControlError, ControlState};
use axum::{
    Json,
    extract::State,
    response::{IntoResponse as _, Response},
};
use http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use silicon_iam_client::{Mutation, models};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Login {
    slt: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Refresh {
    refresh_token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Logout {
    token: String,
}

fn validate_token(token: &str, prefix: &str) -> Result<(), ControlError> {
    if !token.starts_with(prefix)
        || token.len() > 16_384
        || token.len() <= prefix.len()
        || !token.bytes().all(|value| value.is_ascii_graphic())
    {
        return Err(ControlError::bad_request("invalid_token"));
    }
    Ok(())
}

pub(super) async fn iam(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Response, ControlError> {
    // Discovery is public, but an explicit sandbox must still be valid.
    let testing_environment_id =
        if super::single_header(&headers, "x-testing-environment-key")?.is_some() {
            state.plane(&headers).await?.iam_environment_id
        } else {
            None
        };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "app_id": state.app_id,
            "iam_base_url": state.iam.base_url(),
            "testing_environment_id": testing_environment_id,
        })),
    )
        .into_response())
}

pub(super) async fn login(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Result<Response, ControlError> {
    // IAM calls this an SLT, but serializes it as an OAuth authorization code.
    validate_token(&body.slt, "oac_")?;
    let plane = state.plane(&headers).await?;
    let tokens = plane
        .iam
        .oauth()
        .login(&state.app_id, &body.slt, &Mutation::new())
        .await
        .map_err(ControlError::iam)?;
    if let Some(org) = &tokens.org_id {
        state
            .ensure_voice_default(plane.id, org, tokens.actor.principal_id)
            .await?;
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(tokens)).into_response())
}

pub(super) async fn refresh(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(body): Json<Refresh>,
) -> Result<Response, ControlError> {
    validate_token(&body.refresh_token, "ort_")?;
    let plane = state.plane(&headers).await?;
    let tokens = plane
        .iam
        .oauth()
        .refresh(&state.app_id, &body.refresh_token, &Mutation::new())
        .await
        .map_err(ControlError::iam)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(tokens)).into_response())
}

pub(super) async fn logout(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(body): Json<Logout>,
) -> Result<StatusCode, ControlError> {
    if !(body.token.starts_with("oat_") || body.token.starts_with("ort_")) {
        return Err(ControlError::bad_request("invalid_token"));
    }
    validate_token(&body.token, "o")?;
    let plane = state.plane(&headers).await?;
    plane
        .iam
        .oauth()
        .revoke(
            &models::OAuthRevocationRequest {
                token: body.token,
                token_type_hint: None,
            },
            &Mutation::new(),
        )
        .await
        .map_err(ControlError::iam)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn me(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Response, ControlError> {
    let identity = state
        .identity_for_org(&headers, super::single_header(&headers, "x-org-id")?)
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(identity.authority),
    )
        .into_response())
}
