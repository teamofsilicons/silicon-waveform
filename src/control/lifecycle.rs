//! Honeycomb's protected participant protocol, independent of test sessions.
use super::{ControlError, ControlState, bearer, single_header};
use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use http::{HeaderMap, StatusCode};
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{Postgres, Row as _, Transaction};
use std::sync::Arc;
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Instruction {
    operation_id: Uuid,
    environment_id: Uuid,
    org_id: String,
    app_id: String,
    environment_revision: i64,
    generation: i64,
    key_version: i64,
    action: String,
    #[serde(default)]
    testing_key: Option<String>,
    #[serde(default)]
    snapshot: Value,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    retired_apps: Vec<String>,
}
impl Instruction {
    fn receipt(&self, state: &str) -> Value {
        json!({"operation_id":self.operation_id,"environment_id":self.environment_id,
            "app_id":self.app_id,"environment_revision":self.environment_revision,
            "generation":self.generation,"key_version":self.key_version,"state":state,
            "retired_apps":self.retired_apps})
    }
}
/// An owned lock and capacity reservation, released together on cancellation.
pub(crate) struct Fence {
    _transaction: Transaction<'static, Postgres>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

fn conflict() -> ControlError {
    ControlError {
        status: StatusCode::CONFLICT,
        code: "lifecycle_revision_conflict",
    }
}
impl ControlState {
    pub(super) fn service_authority(&self, headers: &HeaderMap) -> Result<(), ControlError> {
        let expected = self
            .honeycomb_token
            .as_ref()
            .ok_or_else(|| ControlError::unavailable("honeycomb_lifecycle_not_configured"))?;
        if single_header(headers, "x-testing-environment-key")?.is_some()
            || !bool::from(
                Sha256::digest(bearer(headers)?.as_bytes())
                    .ct_eq(&Sha256::digest(expected.expose_secret().as_bytes())),
            )
        {
            return Err(ControlError::unauthorized());
        }
        Ok(())
    }

    /// Held through all side effects. PostgreSQL transaction locks are released on cancellation.
    pub(super) async fn test_fence(&self, id: Uuid) -> Result<Fence, ControlError> {
        // Separate capacity leaves the data pool free for queries made while fenced.
        // Try-acquire also prevents nested authorization fences from deadlocking.
        let permit = self
            .fence_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ControlError::unavailable("testing_capacity_busy_retry"))?;
        let guard_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with((*self.pool.connect_options()).clone())
            .await?;
        let mut tx = guard_pool.begin().await?;
        let acquired: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock_shared(hashtextextended($1,84))")
                .bind(id.to_string())
                .fetch_one(&mut *tx)
                .await?;
        if !acquired {
            return Err(ControlError::unavailable(
                "testing_environment_transition_pending",
            ));
        }
        let state: Option<String> =
            sqlx::query_scalar("SELECT state FROM waveform_lifecycle WHERE environment_id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        if state.is_some_and(|v| v != "active") {
            return Err(ControlError::unauthorized());
        }
        Ok(Fence {
            _transaction: tx,
            _permit: permit,
        })
    }
}

pub(super) async fn activity(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path((org, id)): Path<(String, Uuid)>,
) -> Result<Json<Value>, ControlError> {
    state.service_authority(&headers)?;
    let row=sqlx::query("SELECT environment_revision,generation,key_version,state,last_activity_at FROM waveform_lifecycle WHERE environment_id=$1 AND org_id=$2")
        .bind(id).bind(org).fetch_optional(&state.pool).await?.ok_or_else(ControlError::not_found)?;
    let time: Option<time::OffsetDateTime> = row.try_get("last_activity_at")?;
    Ok(Json(
        json!({"environment_id":id,"app_id":state.app_id,"environment_revision":row.try_get::<i64,_>("environment_revision")?,"generation":row.try_get::<i64,_>("generation")?,"key_version":row.try_get::<i64,_>("key_version")?,"state":row.try_get::<String,_>("state")?,"last_activity_at":time.and_then(|v|v.format(&time::format_description::well_known::Rfc3339).ok())}),
    ))
}

pub(super) async fn apply(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path((org, id, operation)): Path<(String, Uuid, Uuid)>,
    Json(op): Json<Instruction>,
) -> Result<Response, ControlError> {
    state.service_authority(&headers)?;
    if id.is_nil()
        || operation.is_nil()
        || id != op.environment_id
        || org != op.org_id
        || operation != op.operation_id
        || op.app_id != state.app_id
        || org.is_empty()
        || org.len() > 128
        || op.environment_revision < 1
        || op.generation < 1
        || op.key_version < 1
    {
        return Err(ControlError::bad_request("invalid_lifecycle_operation"));
    }
    if !matches!(
        op.action.as_str(),
        "prepare" | "rotate-key" | "clean" | "disable" | "restore" | "purge"
    ) {
        return Err(ControlError::bad_request("unsupported_lifecycle_action"));
    }
    let encoded = serde_json::to_vec(&op)
        .map_err(|_| ControlError::bad_request("invalid_lifecycle_operation"))?;
    // A keyed digest permits exact retries without retaining root secrets or raw snapshots.
    let hash = state
        .vault()?
        .digest(
            &String::from_utf8(encoded)
                .map_err(|_| ControlError::bad_request("invalid_lifecycle_operation"))?,
            "honeycomb-operation",
        )
        .map_err(ControlError::internal)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,85))")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    let replay=sqlx::query("SELECT request_hash,receipt FROM waveform_lifecycle_operations WHERE environment_id=$1 AND operation_id=$2")
        .bind(id).bind(operation).fetch_optional(&mut *tx).await?;
    if let Some(row) = &replay {
        if row.try_get::<Vec<u8>, _>("request_hash")? != hash {
            return Err(conflict());
        }
        let receipt: Value = row.try_get("receipt")?;
        if receipt["state"] == "completed" {
            return Ok(Json(receipt).into_response());
        }
    }
    let prior = sqlx::query("SELECT * FROM waveform_lifecycle WHERE environment_id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(row) = &prior {
        let revision: i64 = row.try_get("environment_revision")?;
        let generation: i64 = row.try_get("generation")?;
        let key: i64 = row.try_get("key_version")?;
        let previous: Uuid = row.try_get("operation_id")?;
        let status: String = row.try_get("state")?;
        if row.try_get::<String, _>("org_id")? != org
            || row.try_get::<String, _>("app_id")? != state.app_id
            || (previous != operation
                && (revision >= op.environment_revision || status == "pending"))
            || status == "purged"
            || op.generation < generation
            || op.key_version < key
            || (op.generation != generation && op.action != "clean")
            || (op.key_version != key && op.action != "rotate-key")
            || (previous != operation && op.action == "clean" && op.generation <= generation)
            || (previous != operation && op.action == "rotate-key" && op.key_version <= key)
            || (status == "disabled"
                && !matches!(op.action.as_str(), "restore" | "purge" | "disable"))
        {
            return Err(conflict());
        }
    } else if op.action != "prepare" {
        return Err(conflict());
    }
    let bound_org: Option<String> =
        sqlx::query_scalar("SELECT org_id FROM waveform_environments WHERE id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if bound_org.is_some_and(|bound| bound != org) {
        return Err(conflict());
    }
    // Publish a barrier first. New sessions fail while existing work drains.
    sqlx::query("INSERT INTO waveform_lifecycle(environment_id,org_id,app_id,environment_revision,generation,key_version,state,operation_id) VALUES($1,$2,$3,$4,$5,$6,'pending',$7) ON CONFLICT(environment_id) DO UPDATE SET environment_revision=$4,generation=$5,key_version=$6,state='pending',operation_id=$7")
        .bind(id).bind(&org).bind(&op.app_id).bind(op.environment_revision).bind(op.generation).bind(op.key_version).bind(operation).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO waveform_lifecycle_operations VALUES($1,$2,$3,$4) ON CONFLICT(environment_id,operation_id) DO UPDATE SET receipt=$4")
        .bind(id).bind(operation).bind(hash).bind(op.receipt("pending")).execute(&mut *tx).await?;
    tx.commit().await?;
    if let Ok(receipt) = finish(&state, &op).await {
        Ok(Json(receipt).into_response())
    } else {
        // Keep the barrier. The exact operation can retry after dependency recovery.
        let receipt = op.receipt("failed");
        sqlx::query("UPDATE waveform_lifecycle_operations SET receipt=$3 WHERE environment_id=$1 AND operation_id=$2 AND receipt->>'state'<>'completed'")
                .bind(id).bind(operation).bind(&receipt).execute(&state.pool).await?;
        Ok(Json(receipt).into_response())
    }
}
async fn finish(state: &ControlState, op: &Instruction) -> Result<Value, ControlError> {
    let id = op.environment_id;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,84))")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    let receipt:Value=sqlx::query_scalar("SELECT receipt FROM waveform_lifecycle_operations WHERE environment_id=$1 AND operation_id=$2 FOR UPDATE")
        .bind(id).bind(op.operation_id).fetch_one(&mut *tx).await?;
    if receipt["state"] == "completed" {
        return Ok(receipt);
    }
    if matches!(op.action.as_str(), "clean" | "purge") {
        for table in [
            "waveform_idempotency_records",
            "waveform_jobs",
            "waveform_webhook_events",
            "waveform_account_preferences",
            "waveform_provider_keys",
            "waveform_bug_reports",
            "waveform_provider_defaults",
            "waveform_voice_profiles",
            "waveform_test_contract_usage",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE plane_id=$1"
            )))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
    }
    if op.action == "purge" {
        sqlx::query("DELETE FROM waveform_environments WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    // The runtime binding is populated only by IAM-validated app-secret discovery.
    // Honeycomb's testing_key is administrative authority and is never a session credential.
    if op.action == "clean" {
        sqlx::query("UPDATE waveform_environments SET iam_cleaned_at=NULL WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if op.action == "rotate-key" {
        let digest = op
            .testing_key
            .as_ref()
            .map(|key| format!("{:x}", Sha256::digest(key.as_bytes())));
        sqlx::query("UPDATE waveform_environments SET webhook_key_digest=$2 WHERE id=$1")
            .bind(id)
            .bind(digest)
            .execute(&mut *tx)
            .await?;
    }
    let status = match op.action.as_str() {
        "disable" => "disabled",
        "purge" => "purged",
        _ => "active",
    };
    sqlx::query(
        "UPDATE waveform_lifecycle SET state=$2,cleaned_at=CASE WHEN $4 THEN now() ELSE cleaned_at END WHERE environment_id=$1 AND operation_id=$3",
    )
    .bind(id)
    .bind(status)
    .bind(op.operation_id)
    .bind(op.action == "clean")
    .execute(&mut *tx)
    .await?;
    let receipt = op.receipt("completed");
    sqlx::query("UPDATE waveform_lifecycle_operations SET receipt=$3 WHERE environment_id=$1 AND operation_id=$2")
        .bind(id).bind(op.operation_id).bind(&receipt).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(receipt)
}

pub(super) async fn receipt(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path((org, id, operation)): Path<(String, Uuid, Uuid)>,
) -> Result<Json<Value>, ControlError> {
    state.service_authority(&headers)?;
    let receipt=sqlx::query_scalar("SELECT o.receipt FROM waveform_lifecycle_operations o JOIN waveform_lifecycle e ON e.environment_id=o.environment_id WHERE o.environment_id=$1 AND o.operation_id=$2 AND e.org_id=$3 AND e.app_id=$4")
        .bind(id).bind(operation).bind(org).bind(&state.app_id).fetch_optional(&state.pool).await?.ok_or_else(ControlError::not_found)?;
    Ok(Json(receipt))
}
