//! Provider-free preparation of the account's Waveform folder in Briefcase,
//! where a caller puts STT source audio before calling `/api/v1/stt`.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    response::{IntoResponse as _, Response},
};
use http::{HeaderMap, StatusCode, header};
use serde::Deserialize;

use super::{ControlError, ControlState};
use crate::{
    application::ports::{BriefcaseError, StorageAuthorityError, StorageAuthorityPort as _},
    domain::auth::WaveformAction,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Prepare {}

pub(super) async fn prepare(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(_body): Json<Prepare>,
) -> Result<Response, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::TranscribeSpeech)
        .await?;
    // A direct caller's own token can create Waveform's Briefcase access; an
    // app calling with a proof can only use access the account gave before.
    let subject = if caller.actor.is_delegated() {
        None
    } else {
        caller.credentials.subject_token()
    };
    let proof = state
        .storage_proofs()?
        .briefcase_proof(&caller.actor, subject)
        .await
        .map_err(|error| match error {
            StorageAuthorityError::NotAuthorized | StorageAuthorityError::Refused => {
                ControlError::forbidden(
                    "storage_authorization_required",
                    "Waveform has no Briefcase access for this account.",
                )
                .with_hint("The account can grant it with `waveform storage approve`, or by calling Waveform directly once.")
            }
            StorageAuthorityError::Unavailable => ControlError::unavailable(
                "dependency_unavailable",
                "Silicon Accounts is temporarily unavailable.",
            ),
            StorageAuthorityError::Timeout => ControlError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "dependency_timeout",
                "Silicon Accounts did not answer in time.",
            ),
        })?;
    let target = state
        .briefcase
        .prepare_source_target(&proof, crate::api::headers::request_id(&headers))
        .await
        .map_err(|error| match error {
            BriefcaseError::Unauthorized | BriefcaseError::Forbidden => ControlError::forbidden(
                "storage_authorization_required",
                "Briefcase refused Waveform's access for this account.",
            ),
            BriefcaseError::NotFound => ControlError::not_found(
                "briefcase_folder_not_found",
                "Briefcase has no Waveform folder for this account yet.",
            ),
            BriefcaseError::Timeout => ControlError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "dependency_timeout",
                "Briefcase did not answer in time.",
            ),
            _ => ControlError::unavailable("briefcase_unavailable", "Briefcase is unavailable."),
        })?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "app_id": state.app_id,
            "account": {"uuid": caller.record.uuid, "id": caller.record.current_id},
            "folder_id": target.folder_id,
            "folder_path": target.folder_path,
        })),
    )
        .into_response())
}
