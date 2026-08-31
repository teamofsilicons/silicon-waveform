//! PostgreSQL-backed, lease-based idempotency storage.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{PgPool, Postgres, Row as _, Transaction, postgres::types::PgInterval};
use subtle::ConstantTimeEq as _;
use time::OffsetDateTime;
use tokio::time::timeout;
use url::Url;
use uuid::Uuid;

use crate::{
    application::ports::{IdempotencyStore, IdempotencyStoreError},
    domain::{
        idempotency::{
            CompletedSpeechOperation, CompletedTtsOperation, IdempotencyClaim,
            IdempotencyCompletion, IdempotencyDecision, IdempotencyLease, IdempotencyLeaseId,
            IdempotencyRelease, IdempotencyScope,
        },
        identity::{ActorKind, RequestId},
        language::LanguageHint,
        media::{BriefcaseFileUrl, MediaDuration},
        provider::{ProviderName, STT_PROVIDER_CHAIN, TTS_PROVIDER_CHAIN},
        speech::{SttResult, Transcript},
    },
};

const QUERY_CANCELED_SQLSTATE: &str = "57014";

/// Authoritative idempotency store shared by every Waveform replica.
#[derive(Clone, Debug)]
pub struct PostgresIdempotencyStore {
    pool: PgPool,
    operation_timeout: Duration,
    retry_after: Duration,
    record_ttl: Duration,
}

impl PostgresIdempotencyStore {
    /// Constructs the adapter from an already configured PostgreSQL pool.
    #[must_use]
    pub const fn new(
        pool: PgPool,
        operation_timeout: Duration,
        retry_after: Duration,
        record_ttl: Duration,
    ) -> Self {
        Self {
            pool,
            operation_timeout,
            retry_after,
            record_ttl,
        }
    }

    /// Deletes a bounded batch of expired records without blocking other cleaners.
    ///
    /// # Errors
    ///
    /// Returns a normalized timeout or storage-unavailable error when the
    /// bounded cleanup statement cannot complete.
    pub async fn delete_expired(&self, batch_size: u32) -> Result<u64, IdempotencyStoreError> {
        self.with_timeout(async {
            sqlx::query(
                r"
                WITH database_time AS MATERIALIZED (
                    SELECT clock_timestamp() AS now
                ),
                expired AS (
                    SELECT records.ctid
                    FROM waveform_idempotency_records AS records
                    CROSS JOIN database_time
                    WHERE records.expires_at <= database_time.now
                      AND (
                          records.state = 'completed'
                          OR records.lease_expires_at <= database_time.now
                      )
                    ORDER BY records.expires_at
                    LIMIT $1
                    FOR UPDATE OF records SKIP LOCKED
                )
                DELETE FROM waveform_idempotency_records AS records
                USING expired
                WHERE records.ctid = expired.ctid
                ",
            )
            .bind(i64::from(batch_size))
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
            .map_err(map_sqlx_error)
        })
        .await
    }

    async fn with_timeout<T>(
        &self,
        operation: impl Future<Output = Result<T, IdempotencyStoreError>>,
    ) -> Result<T, IdempotencyStoreError> {
        timeout(self.operation_timeout, operation)
            .await
            .map_err(|_| IdempotencyStoreError::Timeout)?
    }

    async fn claim_inner(
        &self,
        claim: IdempotencyClaim,
    ) -> Result<IdempotencyDecision, IdempotencyStoreError> {
        let lease_duration = postgres_interval(claim.lease_duration)?;
        let record_ttl = postgres_interval(self.record_ttl)?;
        let mut transaction = self.pool.begin().await.map_err(map_sqlx_error)?;
        let (stored, database_now) = loop {
            if insert_claim(&mut transaction, &claim, lease_duration, record_ttl).await? {
                let inserted =
                    initialize_inserted_claim(&mut transaction, &claim, lease_duration, record_ttl)
                        .await?;
                transaction.commit().await.map_err(map_sqlx_error)?;
                return Ok(IdempotencyDecision::Acquired {
                    lease: IdempotencyLease {
                        id: claim.lease_id,
                        expires_at: inserted.lease_expires_at,
                    },
                    request_id: claim.request_id,
                    operation_started_at: inserted.created_at,
                });
            }

            // A concurrent cleaner can remove an expired conflicting row after
            // `ON CONFLICT` observes it. Retry the insert when the locking read
            // confirms that no row remains; the outer operation deadline bounds
            // this loop under pathological churn.
            if let Some(stored) = load_claim(&mut transaction, &claim.scope).await? {
                let database_now = load_database_time(&mut transaction).await?;
                if record_is_purgeable(&stored, database_now)? {
                    delete_locked_claim(&mut transaction, &claim.scope).await?;
                    continue;
                }
                break (stored, database_now);
            }
        };
        if stored.digest.len() != claim.digest.as_bytes().len() {
            return Err(IdempotencyStoreError::InvalidRecord);
        }
        if !bool::from(stored.digest.ct_eq(claim.digest.as_bytes().as_slice())) {
            transaction.commit().await.map_err(map_sqlx_error)?;
            return Ok(IdempotencyDecision::KeyReused);
        }
        match stored.state.as_str() {
            "completed" => {
                let response = stored
                    .response
                    .ok_or(IdempotencyStoreError::InvalidRecord)
                    .and_then(decode_response)?;
                if response.operation() != claim.scope.operation
                    || response_request_id(&response) != stored.request_id
                {
                    return Err(IdempotencyStoreError::InvalidRecord);
                }
                transaction.commit().await.map_err(map_sqlx_error)?;
                Ok(IdempotencyDecision::Replay(response))
            }
            "pending" => {
                let current_lease_expires_at = stored
                    .lease_expires_at
                    .ok_or(IdempotencyStoreError::InvalidRecord)?;
                if current_lease_expires_at <= database_now {
                    let lease_expires_at =
                        reclaim_claim(&mut transaction, &claim, lease_duration).await?;
                    transaction.commit().await.map_err(map_sqlx_error)?;
                    Ok(IdempotencyDecision::Acquired {
                        lease: IdempotencyLease {
                            id: claim.lease_id,
                            expires_at: lease_expires_at,
                        },
                        request_id: stored.request_id,
                        operation_started_at: stored.created_at,
                    })
                } else {
                    let remaining = current_lease_expires_at - database_now;
                    let remaining_milliseconds =
                        u64::try_from(remaining.whole_milliseconds()).unwrap_or(u64::MAX);
                    let remaining_seconds = remaining_milliseconds.div_ceil(1_000).max(1);
                    transaction.commit().await.map_err(map_sqlx_error)?;
                    Ok(IdempotencyDecision::InProgress {
                        retry_after: self.retry_after.min(Duration::from_secs(remaining_seconds)),
                    })
                }
            }
            _ => Err(IdempotencyStoreError::InvalidRecord),
        }
    }

    async fn complete_inner(
        &self,
        completion: IdempotencyCompletion,
    ) -> Result<(), IdempotencyStoreError> {
        if completion.response.operation() != completion.scope.operation
            || response_request_id(&completion.response).as_uuid().is_nil()
        {
            return Err(IdempotencyStoreError::InvalidRecord);
        }
        let response = encode_response(&completion.response)?;
        let record_ttl = postgres_interval(self.record_ttl)?;
        let mut transaction = self.pool.begin().await.map_err(map_sqlx_error)?;
        let stored = load_claim(&mut transaction, &completion.scope)
            .await?
            .ok_or(IdempotencyStoreError::LeaseLost)?;
        let database_now = load_database_time(&mut transaction).await?;
        ensure_current_lease(
            &stored,
            completion.lease_id,
            Some(response_request_id(&completion.response)),
        )?;
        let updated = sqlx::query(
            r"
            UPDATE waveform_idempotency_records
            SET state = 'completed',
                lease_token = NULL,
                lease_expires_at = NULL,
                response_body = $7,
                updated_at = $9,
                expires_at = $9 + $10
            WHERE actor_type = $1::waveform_actor_type
              AND actor_id = $2
              AND org_id = $3
              AND operation = $4::waveform_operation
              AND idempotency_key = $5
              AND state = 'pending'
              AND lease_token = $6
              AND request_id = $8
            ",
        )
        .bind(actor_kind(completion.scope.actor.kind))
        .bind(completion.scope.actor.id.as_uuid())
        .bind(completion.scope.organization_id.as_str())
        .bind(completion.scope.operation.as_str())
        .bind(completion.scope.key.as_str())
        .bind(completion.lease_id.as_uuid())
        .bind(response)
        .bind(response_request_id(&completion.response).as_uuid())
        .bind(database_now)
        .bind(record_ttl)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?
        .rows_affected();
        if updated == 1 {
            transaction.commit().await.map_err(map_sqlx_error)
        } else {
            Err(IdempotencyStoreError::LeaseLost)
        }
    }

    async fn release_inner(
        &self,
        release: IdempotencyRelease,
    ) -> Result<(), IdempotencyStoreError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx_error)?;
        let stored = load_claim(&mut transaction, &release.scope)
            .await?
            .ok_or(IdempotencyStoreError::LeaseLost)?;
        let database_now = load_database_time(&mut transaction).await?;
        ensure_current_lease(&stored, release.lease_id, None)?;
        let released = sqlx::query(
            r"
            UPDATE waveform_idempotency_records
            SET lease_expires_at = $7,
                updated_at = $7
            WHERE actor_type = $1::waveform_actor_type
              AND actor_id = $2
              AND org_id = $3
              AND operation = $4::waveform_operation
              AND idempotency_key = $5
              AND state = 'pending'
              AND lease_token = $6
            ",
        )
        .bind(actor_kind(release.scope.actor.kind))
        .bind(release.scope.actor.id.as_uuid())
        .bind(release.scope.organization_id.as_str())
        .bind(release.scope.operation.as_str())
        .bind(release.scope.key.as_str())
        .bind(release.lease_id.as_uuid())
        .bind(database_now)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?
        .rows_affected();
        if released == 1 {
            transaction.commit().await.map_err(map_sqlx_error)
        } else {
            Err(IdempotencyStoreError::LeaseLost)
        }
    }
}

struct StoredClaimRecord {
    digest: Vec<u8>,
    request_id: RequestId,
    state: String,
    lease_token: Option<Uuid>,
    lease_expires_at: Option<OffsetDateTime>,
    response: Option<JsonValue>,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
}

struct InsertedClaim {
    created_at: OffsetDateTime,
    lease_expires_at: OffsetDateTime,
}

async fn insert_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &IdempotencyClaim,
    lease_duration: PgInterval,
    record_ttl: PgInterval,
) -> Result<bool, IdempotencyStoreError> {
    let row = sqlx::query(
        r"
        WITH database_time AS MATERIALIZED (
            SELECT clock_timestamp() AS now
        )
        INSERT INTO waveform_idempotency_records (
            actor_type, actor_id, org_id, operation, idempotency_key,
            request_digest, request_id, state, lease_token,
            lease_expires_at, created_at, updated_at, expires_at
        )
        SELECT
            $1::waveform_actor_type, $2, $3, $4::waveform_operation, $5,
            $6, $7, 'pending', $8,
            database_time.now + $9,
            database_time.now, database_time.now,
            database_time.now + $10
        FROM database_time
        ON CONFLICT (
            actor_type, actor_id, org_id, operation, idempotency_key
        ) DO NOTHING
        RETURNING 1 AS inserted
        ",
    )
    .bind(actor_kind(claim.scope.actor.kind))
    .bind(claim.scope.actor.id.as_uuid())
    .bind(claim.scope.organization_id.as_str())
    .bind(claim.scope.operation.as_str())
    .bind(claim.scope.key.as_str())
    .bind(claim.digest.as_bytes().as_slice())
    .bind(claim.request_id.as_uuid())
    .bind(claim.lease_id.as_uuid())
    .bind(lease_duration)
    .bind(record_ttl)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?;

    Ok(row.is_some())
}

async fn initialize_inserted_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &IdempotencyClaim,
    lease_duration: PgInterval,
    record_ttl: PgInterval,
) -> Result<InsertedClaim, IdempotencyStoreError> {
    // `ON CONFLICT` may wait for a transaction that ultimately deletes its
    // row. Refresh all lifecycle timestamps after that wait so a new owner
    // always receives the full configured lease and retention periods.
    let row = sqlx::query(
        r"
        WITH database_time AS MATERIALIZED (
            SELECT clock_timestamp() AS now
        )
        UPDATE waveform_idempotency_records AS records
        SET lease_expires_at = database_time.now + $7,
            created_at = database_time.now,
            updated_at = database_time.now,
            expires_at = database_time.now + $8
        FROM database_time
        WHERE records.actor_type = $1::waveform_actor_type
          AND records.actor_id = $2
          AND records.org_id = $3
          AND records.operation = $4::waveform_operation
          AND records.idempotency_key = $5
          AND records.state = 'pending'
          AND records.lease_token = $6
        RETURNING records.created_at, records.lease_expires_at
        ",
    )
    .bind(actor_kind(claim.scope.actor.kind))
    .bind(claim.scope.actor.id.as_uuid())
    .bind(claim.scope.organization_id.as_str())
    .bind(claim.scope.operation.as_str())
    .bind(claim.scope.key.as_str())
    .bind(claim.lease_id.as_uuid())
    .bind(lease_duration)
    .bind(record_ttl)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?
    .ok_or(IdempotencyStoreError::InvalidRecord)?;

    Ok(InsertedClaim {
        created_at: row
            .try_get("created_at")
            .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
        lease_expires_at: row
            .try_get("lease_expires_at")
            .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
    })
}

async fn load_claim(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &IdempotencyScope,
) -> Result<Option<StoredClaimRecord>, IdempotencyStoreError> {
    let row = sqlx::query(
        r"
        SELECT request_digest, request_id, state::text AS state,
               lease_token, lease_expires_at, response_body, created_at, expires_at
        FROM waveform_idempotency_records
        WHERE actor_type = $1::waveform_actor_type
          AND actor_id = $2
          AND org_id = $3
          AND operation = $4::waveform_operation
          AND idempotency_key = $5
        FOR UPDATE
        ",
    )
    .bind(actor_kind(scope.actor.kind))
    .bind(scope.actor.id.as_uuid())
    .bind(scope.organization_id.as_str())
    .bind(scope.operation.as_str())
    .bind(scope.key.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?;

    row.map(|row| {
        Ok(StoredClaimRecord {
            digest: row
                .try_get("request_digest")
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
            request_id: request_id_from_row(&row)?,
            state: row
                .try_get("state")
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
            lease_token: row
                .try_get("lease_token")
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
            lease_expires_at: row
                .try_get("lease_expires_at")
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
            response: row
                .try_get("response_body")
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
            created_at: row
                .try_get("created_at")
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
            expires_at: row
                .try_get("expires_at")
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
        })
    })
    .transpose()
}

async fn load_database_time(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<OffsetDateTime, IdempotencyStoreError> {
    // This query deliberately runs only after a conflicting row has been
    // locked. Unlike `transaction_timestamp()`, `clock_timestamp()` cannot be
    // stale because the transaction spent time waiting for that lock.
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx_error)
}

fn record_is_purgeable(
    stored: &StoredClaimRecord,
    database_now: OffsetDateTime,
) -> Result<bool, IdempotencyStoreError> {
    if stored.expires_at > database_now {
        return Ok(false);
    }
    match stored.state.as_str() {
        "completed" => Ok(true),
        "pending" => stored
            .lease_expires_at
            .map(|lease_expires_at| lease_expires_at <= database_now)
            .ok_or(IdempotencyStoreError::InvalidRecord),
        _ => Err(IdempotencyStoreError::InvalidRecord),
    }
}

async fn delete_locked_claim(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &IdempotencyScope,
) -> Result<(), IdempotencyStoreError> {
    let deleted = sqlx::query(
        r"
        DELETE FROM waveform_idempotency_records
        WHERE actor_type = $1::waveform_actor_type
          AND actor_id = $2
          AND org_id = $3
          AND operation = $4::waveform_operation
          AND idempotency_key = $5
        ",
    )
    .bind(actor_kind(scope.actor.kind))
    .bind(scope.actor.id.as_uuid())
    .bind(scope.organization_id.as_str())
    .bind(scope.operation.as_str())
    .bind(scope.key.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?
    .rows_affected();
    if deleted == 1 {
        Ok(())
    } else {
        Err(IdempotencyStoreError::InvalidRecord)
    }
}

fn ensure_current_lease(
    stored: &StoredClaimRecord,
    expected_lease_id: IdempotencyLeaseId,
    expected_request_id: Option<RequestId>,
) -> Result<(), IdempotencyStoreError> {
    match stored.state.as_str() {
        "completed" => return Err(IdempotencyStoreError::LeaseLost),
        "pending" => {}
        _ => return Err(IdempotencyStoreError::InvalidRecord),
    }
    let _lease_expires_at = stored
        .lease_expires_at
        .ok_or(IdempotencyStoreError::InvalidRecord)?;
    // Expiry makes this record reclaimable; it does not invalidate the current
    // token by itself. The locking read linearizes a late completion/release
    // against reclaim: whichever transaction owns the row lock first wins,
    // and the other observes either completed state or a different token.
    if stored.lease_token != Some(expected_lease_id.as_uuid())
        || expected_request_id.is_some_and(|request_id| request_id != stored.request_id)
    {
        return Err(IdempotencyStoreError::LeaseLost);
    }
    Ok(())
}

async fn reclaim_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &IdempotencyClaim,
    lease_duration: PgInterval,
) -> Result<OffsetDateTime, IdempotencyStoreError> {
    let row = sqlx::query(
        r"
        WITH database_time AS MATERIALIZED (
            SELECT clock_timestamp() AS now
        )
        UPDATE waveform_idempotency_records AS records
        SET lease_token = $6,
            lease_expires_at = database_time.now + $7,
            updated_at = database_time.now
        FROM database_time
        WHERE records.actor_type = $1::waveform_actor_type
          AND records.actor_id = $2
          AND records.org_id = $3
          AND records.operation = $4::waveform_operation
          AND records.idempotency_key = $5
          AND records.state = 'pending'
          AND records.lease_expires_at <= database_time.now
        RETURNING records.lease_expires_at
        ",
    )
    .bind(actor_kind(claim.scope.actor.kind))
    .bind(claim.scope.actor.id.as_uuid())
    .bind(claim.scope.organization_id.as_str())
    .bind(claim.scope.operation.as_str())
    .bind(claim.scope.key.as_str())
    .bind(claim.lease_id.as_uuid())
    .bind(lease_duration)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?
    .ok_or(IdempotencyStoreError::LeaseLost)?;

    row.try_get("lease_expires_at")
        .map_err(|_| IdempotencyStoreError::InvalidRecord)
}

#[async_trait]
impl IdempotencyStore for PostgresIdempotencyStore {
    async fn claim(
        &self,
        claim: IdempotencyClaim,
    ) -> Result<IdempotencyDecision, IdempotencyStoreError> {
        self.with_timeout(self.claim_inner(claim)).await
    }

    async fn complete(
        &self,
        completion: IdempotencyCompletion,
    ) -> Result<(), IdempotencyStoreError> {
        self.with_timeout(self.complete_inner(completion)).await
    }

    async fn release(&self, release: IdempotencyRelease) -> Result<(), IdempotencyStoreError> {
        self.with_timeout(self.release_inner(release)).await
    }

    async fn check_ready(&self) -> Result<(), IdempotencyStoreError> {
        self.with_timeout(async {
            sqlx::query("SELECT 1")
                .execute(&self.pool)
                .await
                .map(|_| ())
                .map_err(map_sqlx_error)
        })
        .await
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum StoredResponse {
    Tts {
        request_id: RequestId,
        file_url: String,
        provider: ProviderName,
        duration_ms: u64,
    },
    Stt {
        request_id: RequestId,
        transcript: String,
        detected_language: Option<LanguageHint>,
        provider: ProviderName,
        duration_ms: Option<u64>,
    },
}

fn encode_response(
    response: &CompletedSpeechOperation,
) -> Result<JsonValue, IdempotencyStoreError> {
    let stored = match response {
        CompletedSpeechOperation::Tts(result) if TTS_PROVIDER_CHAIN.contains(&result.provider) => {
            StoredResponse::Tts {
                request_id: result.request_id,
                file_url: result.permanent_url.to_string(),
                provider: result.provider,
                duration_ms: result.duration.as_millis(),
            }
        }
        CompletedSpeechOperation::Stt(result) if STT_PROVIDER_CHAIN.contains(&result.provider) => {
            StoredResponse::Stt {
                request_id: result.request_id,
                transcript: result.transcript.as_str().to_owned(),
                detected_language: result.detected_language.clone(),
                provider: result.provider,
                duration_ms: result.duration.map(MediaDuration::as_millis),
            }
        }
        CompletedSpeechOperation::Tts(_) | CompletedSpeechOperation::Stt(_) => {
            return Err(IdempotencyStoreError::InvalidRecord);
        }
    };
    serde_json::to_value(stored).map_err(|_| IdempotencyStoreError::InvalidRecord)
}

fn decode_response(value: JsonValue) -> Result<CompletedSpeechOperation, IdempotencyStoreError> {
    let stored = serde_json::from_value::<StoredResponse>(value)
        .map_err(|_| IdempotencyStoreError::InvalidRecord)?;
    match stored {
        StoredResponse::Tts {
            request_id,
            file_url,
            provider,
            duration_ms,
        } => {
            if !TTS_PROVIDER_CHAIN.contains(&provider) {
                return Err(IdempotencyStoreError::InvalidRecord);
            }
            let permanent_url = BriefcaseFileUrl::new(parse_url(&file_url)?)
                .map_err(|_| IdempotencyStoreError::InvalidRecord)?;
            let duration = MediaDuration::from_millis(duration_ms);
            Ok(CompletedSpeechOperation::Tts(CompletedTtsOperation {
                request_id,
                permanent_url,
                provider,
                duration,
            }))
        }
        StoredResponse::Stt {
            request_id,
            transcript,
            detected_language,
            provider,
            duration_ms,
        } => {
            if !STT_PROVIDER_CHAIN.contains(&provider) {
                return Err(IdempotencyStoreError::InvalidRecord);
            }
            Ok(CompletedSpeechOperation::Stt(SttResult {
                request_id,
                transcript: Transcript::new(transcript)
                    .map_err(|_| IdempotencyStoreError::InvalidRecord)?,
                detected_language,
                provider,
                duration: duration_ms.map(MediaDuration::from_millis),
            }))
        }
    }
}

fn parse_url(value: &str) -> Result<Url, IdempotencyStoreError> {
    Url::parse(value).map_err(|_| IdempotencyStoreError::InvalidRecord)
}

fn response_request_id(response: &CompletedSpeechOperation) -> RequestId {
    match response {
        CompletedSpeechOperation::Tts(result) => result.request_id,
        CompletedSpeechOperation::Stt(result) => result.request_id,
    }
}

fn request_id_from_row(row: &sqlx::postgres::PgRow) -> Result<RequestId, IdempotencyStoreError> {
    row.try_get::<Uuid, _>("request_id")
        .map_err(|_| IdempotencyStoreError::InvalidRecord)
        .and_then(|value| RequestId::new(value).map_err(|_| IdempotencyStoreError::InvalidRecord))
}

fn actor_kind(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Carbon => "carbon",
        ActorKind::Silicon => "silicon",
    }
}

fn postgres_interval(duration: Duration) -> Result<PgInterval, IdempotencyStoreError> {
    if duration.is_zero() {
        return Err(IdempotencyStoreError::InvalidRecord);
    }
    PgInterval::try_from(duration).map_err(|_| IdempotencyStoreError::InvalidRecord)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies sqlx::Error by value; diagnostics are intentionally never rendered"
)]
fn map_sqlx_error(error: sqlx::Error) -> IdempotencyStoreError {
    // PostgreSQL diagnostics can include the rejected row. The row may contain
    // a transcript or signed URL, so even the error display is intentionally
    // excluded from telemetry.
    match &error {
        sqlx::Error::PoolTimedOut => {
            tracing::warn!(
                failure_kind = "pool_timeout",
                "idempotency store operation failed"
            );
            IdempotencyStoreError::Timeout
        }
        sqlx::Error::Database(database)
            if database.code().as_deref() == Some(QUERY_CANCELED_SQLSTATE) =>
        {
            tracing::warn!(
                failure_kind = "database_timeout",
                sqlstate = QUERY_CANCELED_SQLSTATE,
                "idempotency store operation failed"
            );
            IdempotencyStoreError::Timeout
        }
        _ => {
            tracing::warn!(
                failure_kind = "unavailable",
                "idempotency store operation failed"
            );
            IdempotencyStoreError::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, error::Error as StdError, fmt, io, str::FromStr as _, time::Duration};

    use serde_json::json;
    use sqlx::error::{DatabaseError, ErrorKind};
    use url::Url;
    use uuid::Uuid;

    use super::{decode_response, encode_response, map_sqlx_error, postgres_interval};
    use crate::application::ports::IdempotencyStoreError;
    use crate::domain::{
        idempotency::{CompletedSpeechOperation, CompletedTtsOperation},
        identity::RequestId,
        language::LanguageHint,
        media::{BriefcaseFileUrl, MediaDuration},
        provider::ProviderName,
        speech::{SttResult, Transcript},
    };

    #[derive(Debug)]
    struct TestDatabaseError {
        sqlstate: &'static str,
    }

    impl fmt::Display for TestDatabaseError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("sensitive database diagnostic")
        }
    }

    impl StdError for TestDatabaseError {}

    impl DatabaseError for TestDatabaseError {
        fn message(&self) -> &'static str {
            "sensitive database diagnostic"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.sqlstate))
        }

        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    #[test]
    fn stt_response_codec_preserves_exact_replay_fields() {
        let request_id = RequestId::new(Uuid::from_u128(42));
        let transcript = Transcript::new("private transcript".to_owned());
        let language = LanguageHint::from_str("en-US");
        let response = match (request_id, transcript, language) {
            (Ok(request_id), Ok(transcript), Ok(language)) => {
                Some(CompletedSpeechOperation::Stt(SttResult {
                    request_id,
                    transcript,
                    detected_language: Some(language),
                    provider: ProviderName::Gemini,
                    duration: Some(MediaDuration::from_millis(1_234)),
                }))
            }
            _ => None,
        };

        let round_trip = response
            .as_ref()
            .ok_or(())
            .and_then(|response| encode_response(response).map_err(|_| ()))
            .and_then(|value| decode_response(value).map_err(|_| ()));

        assert_eq!(round_trip.ok(), response);
    }

    #[test]
    fn tts_response_codec_persists_only_durable_replay_fields() -> Result<(), Box<dyn StdError>> {
        let response = completed_tts_response()?;

        let encoded = encode_response(&response)?;
        let encoded_text = serde_json::to_string(&encoded)?;
        let Some(encoded_object) = encoded.as_object() else {
            return Err("encoded TTS response must be a JSON object".into());
        };

        assert_eq!(encoded_object.len(), 5);
        assert!(
            [
                "operation",
                "request_id",
                "file_url",
                "provider",
                "duration_ms",
            ]
            .iter()
            .all(|field| encoded_object.contains_key(*field))
        );
        assert!(!encoded_object.contains_key("temporary_url"));
        assert!(!encoded_text.contains("temporary_url"));
        assert!(!encoded_text.contains("signature"));
        assert_eq!(decode_response(encoded)?, response);
        Ok(())
    }

    #[test]
    fn tts_response_codec_discards_legacy_temporary_url() -> Result<(), Box<dyn StdError>> {
        let expected = completed_tts_response()?;
        let CompletedSpeechOperation::Tts(expected_tts) = &expected else {
            return Err("test fixture must be a TTS response".into());
        };
        let legacy = json!({
            "operation": "tts",
            "request_id": expected_tts.request_id,
            "file_url": expected_tts.permanent_url.to_string(),
            "temporary_url": "https://cdn.example.test/generated?signature=legacy-secret",
            "provider": expected_tts.provider,
            "duration_ms": expected_tts.duration.as_millis(),
        });

        let decoded = decode_response(legacy)?;
        assert_eq!(decoded, expected);

        let reencoded = encode_response(&decoded)?;
        let reencoded_text = serde_json::to_string(&reencoded)?;
        assert!(reencoded.get("temporary_url").is_none());
        assert!(!reencoded_text.contains("legacy-secret"));
        Ok(())
    }

    #[test]
    fn response_codec_rejects_providers_from_the_other_operation() -> Result<(), Box<dyn StdError>>
    {
        let mut tts_response = completed_tts_response()?;
        let CompletedSpeechOperation::Tts(tts) = &mut tts_response else {
            return Err("test fixture must be a TTS response".into());
        };
        tts.provider = ProviderName::Deepgram;
        assert_eq!(
            encode_response(&tts_response),
            Err(IdempotencyStoreError::InvalidRecord)
        );

        let request_id = RequestId::new(Uuid::from_u128(84))?;
        let invalid_tts = json!({
            "operation": "tts",
            "request_id": request_id,
            "file_url": "https://briefcase.example.test/files/generated",
            "provider": "deepgram",
            "duration_ms": 1_234,
        });
        let invalid_stt = json!({
            "operation": "stt",
            "request_id": request_id,
            "transcript": "private transcript",
            "detected_language": null,
            "provider": "elevenlabs",
            "duration_ms": 1_234,
        });
        assert_eq!(
            decode_response(invalid_tts),
            Err(IdempotencyStoreError::InvalidRecord)
        );
        assert_eq!(
            decode_response(invalid_stt),
            Err(IdempotencyStoreError::InvalidRecord)
        );
        Ok(())
    }

    #[test]
    fn pool_and_statement_deadlines_are_classified_as_timeouts() {
        assert_eq!(
            map_sqlx_error(sqlx::Error::PoolTimedOut),
            IdempotencyStoreError::Timeout
        );
        assert_eq!(
            map_sqlx_error(sqlx::Error::Database(Box::new(TestDatabaseError {
                sqlstate: "57014",
            }))),
            IdempotencyStoreError::Timeout
        );
    }

    #[test]
    fn other_database_failures_are_classified_as_unavailable() {
        assert_eq!(
            map_sqlx_error(sqlx::Error::Database(Box::new(TestDatabaseError {
                sqlstate: "23505",
            }))),
            IdempotencyStoreError::Unavailable
        );
        assert_eq!(
            map_sqlx_error(sqlx::Error::PoolClosed),
            IdempotencyStoreError::Unavailable
        );
        assert_eq!(
            map_sqlx_error(sqlx::Error::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "test timeout diagnostic",
            ))),
            IdempotencyStoreError::Unavailable
        );
    }

    #[test]
    fn postgres_intervals_reject_zero_and_sub_microsecond_precision() {
        assert_eq!(
            postgres_interval(Duration::ZERO),
            Err(IdempotencyStoreError::InvalidRecord)
        );
        assert_eq!(
            postgres_interval(Duration::from_nanos(1)),
            Err(IdempotencyStoreError::InvalidRecord)
        );
        assert!(postgres_interval(Duration::from_millis(1)).is_ok());
    }

    fn completed_tts_response() -> Result<CompletedSpeechOperation, Box<dyn StdError>> {
        Ok(CompletedSpeechOperation::Tts(CompletedTtsOperation {
            request_id: RequestId::new(Uuid::from_u128(42))?,
            permanent_url: BriefcaseFileUrl::new(Url::parse(
                "https://briefcase.example.test/files/generated",
            )?)?,
            provider: ProviderName::Gemini,
            duration: MediaDuration::from_millis(1_234),
        }))
    }
}
