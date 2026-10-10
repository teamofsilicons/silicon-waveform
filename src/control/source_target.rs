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
                .with_hint("The account gives it with `waveform storage approve`; its first direct Waveform request also gives it, unless the account or its custodian revoked it.")
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
    let request_id = crate::api::headers::request_id(&headers);
    let target = match state
        .briefcase
        .prepare_source_target(&proof, request_id)
        .await
    {
        // The stored proof ended (with the sign-in it stood on) or was revoked:
        // renew it once, as speech does, and look again.
        Err(BriefcaseError::Unauthorized) => {
            match state
                .storage_proofs()?
                .renew_refused_proof(&caller.actor, subject, &proof)
                .await
            {
                Ok(renewed) => {
                    state
                        .briefcase
                        .prepare_source_target(&renewed, request_id)
                        .await
                }
                Err(_) => Err(BriefcaseError::Unauthorized),
            }
        }
        other => other,
    }
    .map_err(|error| match error {
        BriefcaseError::Unauthorized | BriefcaseError::Forbidden => ControlError::forbidden(
            "storage_authorization_required",
            "Briefcase refused Waveform's access for this account.",
        )
        .with_hint("The account can give it again with `waveform storage approve`."),
        BriefcaseError::NotFound => ControlError::not_found(
            "briefcase_folder_not_found",
            "Briefcase did not show Waveform its folder (apps/waveform) for this account: Briefcase shows an app its folder only through the files in it, and it is still empty.",
        )
        .with_hint("Store one speech there first (`waveform tts …`), or upload the source into apps/waveform with your own Briefcase access and pass its permanent URL to `waveform stt`."),
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
