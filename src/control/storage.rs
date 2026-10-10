//! Waveform's Briefcase access for an account: a User verification proof
//! Waveform holds to store generated speech in the account's own files and read
//! STT sources there.
//!
//! - `POST`: issue (or replace) it with the account's own access token. Only the
//!   account itself can do this; a custodian cannot approve for a Silicon.
//! - `GET`: status, for the account or a custodian (`?account=si:…`).
//! - `DELETE`: revoke, for the account or a custodian.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
};
use http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

use super::{ControlError, ControlState, accounts::AccountQuery};
use crate::{
    application::ports::StorageAuthorityError,
    domain::auth::{InboundCredentials, WaveformAction},
    infrastructure::{directory::Confirm, storage_proofs::StorageProofStatus},
};

fn status_json(
    target_uuid: &str,
    target_id: Option<&String>,
    status: &StorageProofStatus,
) -> Value {
    json!({
        "account": {"uuid": target_uuid, "id": target_id},
        "briefcase": status,
    })
}

pub(super) async fn approve(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
) -> Result<(StatusCode, Json<Value>), ControlError> {
    let caller = state
        .fresh_caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Recent)
        .await?;
    if target.as_custodian {
        return Err(ControlError::forbidden(
            "approval_needs_the_account",
            "Only the account itself can give Waveform Briefcase access, because the proof is made from its own Waveform sign-in. A Silicon gives it itself, with `waveform storage approve` (its first direct Waveform request also gives it, unless the access was revoked).",
        ));
    }
    let InboundCredentials::Bearer(token) = &caller.credentials else {
        return Err(ControlError::forbidden(
            "approval_needs_the_account",
            "Briefcase access can only be given with the account's own access token.",
        ));
    };
    let status = state
        .storage_proofs()?
        .approve(&caller.actor, token)
        .await
        .map_err(|error| match error {
            StorageAuthorityError::NotAuthorized | StorageAuthorityError::Refused => {
                ControlError::forbidden(
                    "briefcase_access_refused",
                    "Silicon Accounts refused to issue Waveform's Briefcase proof for this sign-in (the sign-in ended, or Briefcase is not an active app).",
                )
                .with_hint("Sign in to Waveform again and retry.")
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
    Ok((
        StatusCode::CREATED,
        Json(status_json(
            target.record.uuid.as_str(),
            target.record.current_id.as_ref(),
            &status,
        )),
    ))
}

pub(super) async fn status(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Recent)
        .await?;
    let status = state.storage_proofs()?.status(target.storage_key()).await?;
    Ok(Json(status_json(
        target.record.uuid.as_str(),
        target.record.current_id.as_ref(),
        &status,
    )))
}

pub(super) async fn revoke(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>, ControlError> {
    let caller = state
        .fresh_caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let target = state
        .target(&caller, query.account.as_deref(), Confirm::Always)
        .await?;
    let reason = if target.as_custodian {
        "revoked_by_custodian"
    } else {
        "revoked_by_account"
    };
    let proofs = state.storage_proofs()?;
    proofs.revoke(target.storage_key(), reason).await?;
    let status = proofs.status(target.storage_key()).await?;
    Ok(Json(status_json(
        target.record.uuid.as_str(),
        target.record.current_id.as_ref(),
        &status,
    )))
}
