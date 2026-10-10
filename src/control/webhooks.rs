//! `POST /webhooks/accounts`: Silicon Accounts events about Waveform's members.
//!
//! The signature is verified over the exact raw body (5-minute tolerance), with
//! the previous secret accepted during a rotation. Each event is applied in one
//! transaction together with its `event_id` dedupe row, so a failed delivery is
//! retried by Accounts and a repeated one changes nothing. Only content-free
//! metadata is stored; nothing from the body is logged.

use std::{str::FromStr as _, sync::Arc};

use axum::{Json, body::Bytes, extract::State};
use http::{HeaderMap, StatusCode};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use silicon_accounts_client::{
    DEFAULT_WEBHOOK_TOLERANCE, SIGNATURE_HEADER, TIMESTAMP_HEADER, WebhookError, WebhookEvent,
    WebhookPayload, verify_and_parse_webhook,
};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ControlError, ControlState};
use crate::{
    domain::identity::AccountUuid,
    infrastructure::directory::{Purge, lock_account, purge_account, record_pending_event},
};

pub(crate) async fn receive(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ControlError> {
    let event = verify(&state, &headers, &body)?;
    let occurred_at = event.occurred_at.unwrap_or_else(OffsetDateTime::now_utc);
    let account = account_of(&event.payload);
    let account = account.as_ref();
    let mut transaction = state.pool.begin().await?;
    let fresh = sqlx::query("INSERT INTO waveform_accounts_webhook_events(event_id, event_type, account_uuid, occurred_at, outcome) VALUES($1, $2, $3, $4, 'processing') ON CONFLICT (event_id) DO NOTHING")
        .bind(&event.event_id)
        .bind(&event.event_type)
        .bind(account.map(AccountUuid::as_str))
        .bind(occurred_at)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        == 1;
    if !fresh {
        return Ok((
            StatusCode::OK,
            Json(json!({"received": true, "event_id": event.event_id, "duplicate": true})),
        ));
    }
    let applied = apply(&mut transaction, &event, account, occurred_at).await?;
    sqlx::query("UPDATE waveform_accounts_webhook_events SET outcome=$2 WHERE event_id=$1")
        .bind(&event.event_id)
        .bind(applied.outcome)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    // Remote clean-up after the local state is durable: revoke the Briefcase
    // proof Waveform held (best effort; it also ends with the sign-in).
    if let (Some(proof_id), Some(proofs)) = (applied.revoked_proof, &state.storage_proofs) {
        proofs.revoke_remote_by_id(&proof_id).await;
    }
    tracing::info!(
        event_type = %event.event_type,
        outcome = applied.outcome,
        "Accounts webhook applied"
    );
    Ok((
        StatusCode::OK,
        Json(json!({"received": true, "event_id": event.event_id, "outcome": applied.outcome})),
    ))
}

fn verify(
    state: &ControlState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<WebhookEvent, ControlError> {
    if state.webhook_secrets.is_empty() {
        return Err(ControlError::unavailable(
            "webhook_not_configured",
            "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET is not configured, so webhook deliveries cannot be verified.",
        ));
    }
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let timestamp = header(TIMESTAMP_HEADER);
    let signature = header(SIGNATURE_HEADER);
    let mut last = None;
    for secret in &state.webhook_secrets {
        match verify_and_parse_webhook(
            secret.expose_secret(),
            &timestamp,
            &signature,
            body,
            DEFAULT_WEBHOOK_TOLERANCE,
        ) {
            Ok(event) => return Ok(event),
            Err(WebhookError::SignatureMismatch) => last = Some(WebhookError::SignatureMismatch),
            Err(error) => {
                last = Some(error);
                break;
            }
        }
    }
    Err(match last {
        Some(WebhookError::InvalidBody(_)) => ControlError::bad_request(
            "invalid_webhook_body",
            "The signed body is not a Silicon Accounts event.",
        ),
        Some(error) => ControlError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_webhook_signature",
            error.to_string(),
        ),
        None => ControlError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_webhook_signature",
            "The webhook signature does not match.",
        ),
    })
}

fn account_of(payload: &WebhookPayload) -> Option<AccountUuid> {
    let uuid = match payload {
        WebhookPayload::AccountIdChanged(change) => &change.uuid,
        WebhookPayload::AccountUpdated(update) => &update.uuid,
        WebhookPayload::AccountDeleted(event) | WebhookPayload::MembershipAccessRemoved(event) => {
            &event.uuid
        }
        WebhookPayload::MembershipSignedOut(event) => &event.uuid,
        WebhookPayload::CustodianChanged(change) => &change.uuid,
        _ => return None,
    };
    // A uuid that is not well formed cannot match any stored account.
    AccountUuid::from_str(uuid).ok()
}

struct Applied {
    outcome: &'static str,
    /// Briefcase proof to revoke at Accounts once the transaction committed.
    revoked_proof: Option<String>,
}

const fn applied(outcome: &'static str) -> Applied {
    Applied {
        outcome,
        revoked_proof: None,
    }
}

async fn apply(
    transaction: &mut Transaction<'_, Postgres>,
    event: &WebhookEvent,
    account: Option<&AccountUuid>,
    occurred_at: OffsetDateTime,
) -> Result<Applied, ControlError> {
    let Some(account) = account else {
        if event.payload == WebhookPayload::Ping {
            return Ok(applied("ping"));
        }
        tracing::info!(event_type = %event.event_type, "ignoring an Accounts event Waveform does not use");
        return Ok(applied("ignored"));
    };
    let uuid = account.as_str();
    let outcome = match &event.payload {
        WebhookPayload::AccountIdChanged(change) => {
            let updated = sqlx::query("UPDATE waveform_accounts SET current_id=$2, id_changed_at=$3, updated_at=now() WHERE account_uuid=$1 AND deleted_at IS NULL AND (id_changed_at IS NULL OR id_changed_at <= $3)")
                .bind(uuid).bind(&change.new_id).bind(occurred_at)
                .execute(&mut **transaction).await?.rows_affected();
            // Silicons show their custodian's current id too.
            sqlx::query("UPDATE waveform_accounts SET custodian_id=$2, updated_at=now() WHERE custodian_uuid=$1")
                .bind(uuid).bind(&change.new_id)
                .execute(&mut **transaction).await?;
            if updated == 1 {
                "id_updated"
            } else {
                "stale_or_unknown_account"
            }
        }
        WebhookPayload::AccountUpdated(update) => match &update.account {
            Some(profile) => {
                let updated = sqlx::query("UPDATE waveform_accounts SET display_name=NULLIF($2,''), pfp_url=NULLIF($3,''), current_id=COALESCE(NULLIF($4,''), current_id), profile_version=$5, updated_at=now() WHERE account_uuid=$1 AND deleted_at IS NULL AND profile_version < $5")
                    .bind(uuid).bind(&profile.display_name).bind(&profile.pfp_url).bind(&profile.id).bind(profile.version)
                    .execute(&mut **transaction).await?.rows_affected();
                if updated == 1 {
                    "profile_updated"
                } else {
                    "stale_or_unknown_account"
                }
            }
            None => "no_profile_in_event",
        },
        WebhookPayload::CustodianChanged(change) => match &change.to {
            Some(to) if AccountUuid::from_str(&to.uuid).is_ok() => {
                let updated = sqlx::query("UPDATE waveform_accounts SET custodian_uuid=$2, custodian_id=NULLIF($3,''), custodian_changed_at=$4, custodian_checked_at=now(), updated_at=now() WHERE account_uuid=$1 AND kind='silicon' AND deleted_at IS NULL AND (custodian_changed_at IS NULL OR custodian_changed_at <= $4)")
                    .bind(uuid).bind(&to.uuid).bind(&to.id).bind(occurred_at)
                    .execute(&mut **transaction).await?.rows_affected();
                if updated == 1 {
                    "custodian_updated"
                } else {
                    "stale_or_unknown_account"
                }
            }
            _ => "no_custodian_in_event",
        },
        WebhookPayload::MembershipSignedOut(signed_out) => {
            // `app_revoked` is Waveform ending one sign-in itself (one machine's
            // logout, the web's sign-out); the account's other sign-ins stay.
            if signed_out.reason.as_deref() == Some("app_revoked") {
                return Ok(applied("ignored_app_revoked"));
            }
            let reason = signed_out
                .reason
                .clone()
                .unwrap_or_else(|| "signed_out".to_owned());
            return end_sign_ins(transaction, uuid, occurred_at, &reason).await;
        }
        WebhookPayload::MembershipAccessRemoved(_) => {
            return end_sign_ins(transaction, uuid, occurred_at, "access_removed").await;
        }
        WebhookPayload::AccountDeleted(_) => {
            return delete_account(transaction, uuid, occurred_at).await;
        }
        _ => "ignored",
    };
    Ok(applied(outcome))
}

/// Refuses tokens issued before `at` and ends Waveform's Briefcase access. For an
/// account Waveform has not seen yet (it signed in but never called Waveform),
/// the sign-out is kept for its first sighting, so its earlier tokens stay refused.
async fn end_sign_ins(
    transaction: &mut Transaction<'_, Postgres>,
    uuid: &str,
    at: OffsetDateTime,
    reason: &str,
) -> Result<Applied, ControlError> {
    lock_account(transaction, uuid).await?;
    let storage_key: Option<Uuid> = sqlx::query_scalar("UPDATE waveform_accounts a SET revoked_before=GREATEST(COALESCE(a.revoked_before, $2), $2), revoked_reason=$3, updated_at=now() FROM waveform_account_keys k WHERE a.account_uuid=$1 AND k.account_uuid=a.account_uuid RETURNING k.storage_actor_id")
        .bind(uuid).bind(at).bind(reason)
        .fetch_optional(&mut **transaction).await?;
    let Some(storage_key) = storage_key else {
        record_pending_event(transaction, uuid, at, reason, false).await?;
        return Ok(applied("sign_ins_ended_before_first_use"));
    };
    let revoked_proof = invalidate_storage_proof(transaction, storage_key, reason).await?;
    Ok(Applied {
        outcome: "sign_ins_ended",
        revoked_proof,
    })
}

async fn invalidate_storage_proof(
    transaction: &mut Transaction<'_, Postgres>,
    storage_key: Uuid,
    reason: &str,
) -> Result<Option<String>, ControlError> {
    Ok(sqlx::query_scalar("UPDATE waveform_storage_proofs SET token_cipher=''::bytea, refresh_cipher=NULL, invalidated_at=now(), invalidated_reason=$2, updated_at=now() WHERE storage_actor_id=$1 AND invalidated_at IS NULL RETURNING proof_id")
        .bind(storage_key).bind(reason)
        .fetch_optional(&mut **transaction).await?)
}

/// Deletes the account's data held by Waveform and keeps a tombstone so its
/// tokens stay refused (see [`purge_account`]); an account Waveform has not
/// seen yet is recorded as deleted for its first sighting.
async fn delete_account(
    transaction: &mut Transaction<'_, Postgres>,
    uuid: &str,
    at: OffsetDateTime,
) -> Result<Applied, ControlError> {
    Ok(match purge_account(transaction, uuid, at).await? {
        Purge::Purged { revoked_proof } => Applied {
            outcome: "account_deleted",
            revoked_proof,
        },
        Purge::Recorded => applied("deleted_before_first_use"),
    })
}
