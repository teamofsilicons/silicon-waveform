//! Discovery and the signed-in account.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    response::{IntoResponse as _, Response},
};
use http::{HeaderMap, header};
use serde_json::{Value, json};

use super::{ControlError, ControlState};
use crate::{
    domain::auth::{InboundCredentials, WaveformAction},
    infrastructure::directory::AccountRecord,
};

/// Where to sign in and where the docs are. Public, no credentials.
pub(super) async fn discovery(State(state): State<Arc<ControlState>>) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "app_id": state.app_id,
            "accounts_url": state.accounts_url,
            "api_url": state.public_url.as_str().trim_end_matches('/'),
            "docs_url": "https://docs.waveform.teamofsilicons.com",
            "proof_scopes": crate::config::PROOF_SCOPES,
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
        .into_response()
}

fn account_json(record: &AccountRecord) -> Value {
    json!({
        "uuid": record.uuid,
        "id": record.current_id,
        "kind": record.kind,
        "display_name": record.display_name,
        "pfp_url": record.pfp_url,
        "custodian": record.custodian.as_ref().map(|(uuid, id)| json!({"uuid": uuid, "id": id})),
    })
}

/// The signed-in account (bearer tokens only).
pub(super) async fn me(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Response, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let record = match &caller.credentials {
        InboundCredentials::Bearer(token) => {
            state
                .directory()
                .complete_profile(caller.record.clone(), token.expose_secret())
                .await
        }
        InboundCredentials::Proof(_) => caller.record.clone(),
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "authenticated": true,
            "app_id": state.app_id,
            "account": account_json(&record),
            "via": "bearer",
            "expires_at": caller.actor.expires_at.and_then(|at| at.format(&time::format_description::well_known::Rfc3339).ok()),
        })),
    )
        .into_response())
}

/// The Silicons Waveform knows that the signed-in Carbon looks after, each one
/// confirmed with Silicon Accounts in the last ten minutes. `complete` is false
/// when some could not be confirmed just now (they are left out).
pub(super) async fn silicons(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let list = state.directory().silicons_of(&caller.record.uuid).await?;
    let items = list.items.iter().map(account_json).collect::<Vec<_>>();
    Ok(Json(json!({"items": items, "complete": list.complete})))
}
