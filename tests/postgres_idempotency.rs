//! PostgreSQL transaction tests run explicitly by CI against an isolated database.

use std::{str::FromStr as _, time::Duration};

use silicon_waveform::{
    application::ports::{IdempotencyStore as _, IdempotencyStoreError},
    domain::{
        idempotency::{
            CompletedSpeechOperation, CompletedTtsOperation, IdempotencyClaim,
            IdempotencyCompletion, IdempotencyDecision, IdempotencyKey, IdempotencyLeaseId,
            IdempotencyRelease, IdempotencyScope, RequestDigest, RequestDigestKey, SpeechOperation,
        },
        identity::{Actor, ActorId, ActorKind, OrganizationId, RequestId},
        media::{BriefcaseFileUrl, MediaDuration},
        provider::ProviderName,
        speech::{SpeechText, TtsRequest},
    },
    infrastructure::idempotency::PostgresIdempotencyStore,
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn concurrent_first_claim_has_one_owner_and_one_waiter()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, store) = test_store(Duration::from_hours(24)).await?;
    let scope = scope()?;
    let request = TtsRequest::new(SpeechText::new("hello".to_owned())?, None)?;
    let digest_key = digest_key()?;
    let digest = RequestDigest::for_tts(&request, &digest_key);
    let first_request_id = request_id()?;
    let second_request_id = request_id()?;

    let (first, second) = tokio::join!(
        store.claim(claim(
            scope.clone(),
            digest,
            first_request_id,
            lease_id()?,
            Duration::from_secs(60),
        )),
        store.claim(claim(
            scope.clone(),
            digest,
            second_request_id,
            lease_id()?,
            Duration::from_secs(60),
        )),
    );
    let first = first?;
    let second = second?;

    assert!(
        matches!(
            (&first, &second),
            (
                IdempotencyDecision::Acquired { request_id, .. },
                IdempotencyDecision::InProgress { .. }
            ) if *request_id == first_request_id
        ) || matches!(
            (&first, &second),
            (
                IdempotencyDecision::InProgress { .. },
                IdempotencyDecision::Acquired { request_id, .. }
            ) if *request_id == second_request_id
        )
    );

    let different_request = TtsRequest::new(SpeechText::new("different".to_owned())?, None)?;
    let conflict = store
        .claim(claim(
            scope,
            RequestDigest::for_tts(&different_request, &digest_key),
            request_id()?,
            lease_id()?,
            Duration::from_secs(60),
        ))
        .await?;
    assert_eq!(conflict, IdempotencyDecision::KeyReused);

    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn release_reclaim_preserves_identity_and_fences_stale_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, store) = test_store(Duration::from_hours(24)).await?;
    let scope = scope()?;
    let request = TtsRequest::new(SpeechText::new("hello".to_owned())?, None)?;
    let digest = RequestDigest::for_tts(&request, &digest_key()?);
    let original_request_id = request_id()?;
    let stale_lease_id = lease_id()?;

    let first = store
        .claim(claim(
            scope.clone(),
            digest,
            original_request_id,
            stale_lease_id,
            Duration::from_secs(60),
        ))
        .await?;
    let original_started_at = match first {
        IdempotencyDecision::Acquired {
            request_id,
            operation_started_at,
            ..
        } if request_id == original_request_id => operation_started_at,
        other => return Err(format!("unexpected first claim decision: {other:?}").into()),
    };

    store
        .release(IdempotencyRelease {
            scope: scope.clone(),
            lease_id: stale_lease_id,
        })
        .await?;

    let current_lease_id = lease_id()?;
    let reclaimed = store
        .claim(claim(
            scope.clone(),
            digest,
            request_id()?,
            current_lease_id,
            Duration::from_secs(60),
        ))
        .await?;
    assert!(matches!(
        reclaimed,
        IdempotencyDecision::Acquired {
            lease,
            request_id,
            operation_started_at,
        } if lease.id == current_lease_id
            && request_id == original_request_id
            && operation_started_at == original_started_at
    ));

    let response = CompletedSpeechOperation::Tts(completed_tts_operation(original_request_id)?);
    let stale_completion = store
        .complete(IdempotencyCompletion {
            scope: scope.clone(),
            lease_id: stale_lease_id,
            response: response.clone(),
        })
        .await;
    assert_eq!(stale_completion, Err(IdempotencyStoreError::LeaseLost));

    store
        .complete(IdempotencyCompletion {
            scope: scope.clone(),
            lease_id: current_lease_id,
            response: response.clone(),
        })
        .await?;
    let persisted_response = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT response_body FROM waveform_idempotency_records WHERE request_id = $1",
    )
    .bind(original_request_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(persisted_response.get("temporary_url").is_none());
    assert!(!persisted_response.to_string().contains("signature"));

    let replay = store
        .claim(claim(
            scope,
            digest,
            request_id()?,
            lease_id()?,
            Duration::from_secs(60),
        ))
        .await?;
    assert_eq!(replay, IdempotencyDecision::Replay(response));

    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn cleanup_never_deletes_an_expired_record_with_a_live_lease()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, store) = test_store(Duration::from_millis(50)).await?;
    let scope = scope()?;
    let request = TtsRequest::new(SpeechText::new("hello".to_owned())?, None)?;
    let digest = RequestDigest::for_tts(&request, &digest_key()?);
    let original_request_id = request_id()?;
    let original_lease_id = lease_id()?;
    let first = store
        .claim(claim(
            scope.clone(),
            digest,
            original_request_id,
            original_lease_id,
            Duration::from_secs(5),
        ))
        .await?;
    assert!(matches!(first, IdempotencyDecision::Acquired { .. }));

    tokio::time::sleep(Duration::from_millis(150)).await;
    let _deleted = store.delete_expired(100).await?;
    let duplicate = store
        .claim(claim(
            scope.clone(),
            digest,
            request_id()?,
            lease_id()?,
            Duration::from_secs(5),
        ))
        .await?;
    assert!(matches!(duplicate, IdempotencyDecision::InProgress { .. }));

    store
        .release(IdempotencyRelease {
            scope: scope.clone(),
            lease_id: original_lease_id,
        })
        .await?;
    let _deleted = store.delete_expired(100).await?;
    let new_request_id = request_id()?;
    let after_cleanup = store
        .claim(claim(
            scope,
            digest,
            new_request_id,
            lease_id()?,
            Duration::from_secs(5),
        ))
        .await?;
    assert!(matches!(
        after_cleanup,
        IdempotencyDecision::Acquired { request_id, .. } if request_id == new_request_id
    ));

    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn first_claim_refreshes_timestamps_after_a_unique_conflict_wait()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, store) = test_store(Duration::from_hours(24)).await?;
    let scope = scope()?;
    let request = TtsRequest::new(SpeechText::new("hello".to_owned())?, None)?;
    let digest = RequestDigest::for_tts(&request, &digest_key()?);
    let candidate_request_id = request_id()?;
    let candidate_lease_id = lease_id()?;

    let mut blocker = pool.begin().await?;
    sqlx::query(
        r"
        INSERT INTO waveform_idempotency_records (
            actor_type, actor_id, org_id, operation, idempotency_key,
            request_digest, request_id, state, lease_token,
            lease_expires_at, created_at, updated_at, expires_at
        ) VALUES (
            'carbon', $1, $2, 'tts', $3, $4, $5, 'pending', $6,
            clock_timestamp() + INTERVAL '5 minutes',
            clock_timestamp(), clock_timestamp(),
            clock_timestamp() + INTERVAL '24 hours'
        )
        ",
    )
    .bind(scope.actor.id.as_uuid())
    .bind(scope.organization_id.as_str())
    .bind(scope.key.as_str())
    .bind(digest.as_bytes().as_slice())
    .bind(request_id()?.as_uuid())
    .bind(lease_id()?.as_uuid())
    .execute(&mut *blocker)
    .await?;

    let pending_claim = claim(
        scope.clone(),
        digest,
        candidate_request_id,
        candidate_lease_id,
        Duration::from_secs(60),
    );
    let waiting_store = store.clone();
    let waiting_claim = tokio::spawn(async move { waiting_store.claim(pending_claim).await });
    wait_for_idempotency_lock_waiters(
        &mut blocker,
        "%INSERT INTO waveform_idempotency_records%",
        1,
    )
    .await?;

    let conflict_release_time = sqlx::query_scalar::<_, OffsetDateTime>("SELECT clock_timestamp()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query(
        r"
        DELETE FROM waveform_idempotency_records
        WHERE actor_type = 'carbon'
          AND actor_id = $1
          AND org_id = $2
          AND operation = 'tts'
          AND idempotency_key = $3
        ",
    )
    .bind(scope.actor.id.as_uuid())
    .bind(scope.organization_id.as_str())
    .bind(scope.key.as_str())
    .execute(&mut *blocker)
    .await?;
    blocker.commit().await?;

    let decision = waiting_claim.await??;
    assert!(matches!(
        decision,
        IdempotencyDecision::Acquired {
            lease,
            request_id,
            operation_started_at,
        } if lease.id == candidate_lease_id
            && request_id == candidate_request_id
            && operation_started_at >= conflict_release_time
            && lease.expires_at - operation_started_at >= time::Duration::seconds(60)
    ));

    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn late_completion_wins_when_it_precedes_reclaim_in_the_lock_queue()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, store) = test_store(Duration::from_hours(24)).await?;
    let scope = scope()?;
    let request = TtsRequest::new(SpeechText::new("hello".to_owned())?, None)?;
    let digest = RequestDigest::for_tts(&request, &digest_key()?);
    let canonical_request_id = request_id()?;
    let completing_lease_id = lease_id()?;
    let first = store
        .claim(claim(
            scope.clone(),
            digest,
            canonical_request_id,
            completing_lease_id,
            Duration::from_millis(50),
        ))
        .await?;
    assert!(matches!(first, IdempotencyDecision::Acquired { .. }));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut blocker = pool.begin().await?;
    sqlx::query(
        r"
        SELECT request_id
        FROM waveform_idempotency_records
        WHERE actor_type = 'carbon'
          AND actor_id = $1
          AND org_id = $2
          AND operation = 'tts'
          AND idempotency_key = $3
        FOR UPDATE
        ",
    )
    .bind(scope.actor.id.as_uuid())
    .bind(scope.organization_id.as_str())
    .bind(scope.key.as_str())
    .fetch_one(&mut *blocker)
    .await?;

    let response = CompletedSpeechOperation::Tts(completed_tts_operation(canonical_request_id)?);
    let completion_store = store.clone();
    let completion_scope = scope.clone();
    let completion_response = response.clone();
    let completion = tokio::spawn(async move {
        completion_store
            .complete(IdempotencyCompletion {
                scope: completion_scope,
                lease_id: completing_lease_id,
                response: completion_response,
            })
            .await
    });
    let locking_read_pattern = "%FROM waveform_idempotency_records%FOR UPDATE%";
    wait_for_idempotency_lock_waiters(&mut blocker, locking_read_pattern, 1).await?;

    let reclaim_store = store.clone();
    let reclaim_request_id = request_id()?;
    let reclaim_lease_id = lease_id()?;
    let reclaim = tokio::spawn(async move {
        reclaim_store
            .claim(claim(
                scope,
                digest,
                reclaim_request_id,
                reclaim_lease_id,
                Duration::from_secs(60),
            ))
            .await
    });
    // Completion is already proven to be queued on the row lock. Give the
    // independently scheduled reclaim future time to enter its claim before
    // releasing the blocker; the final decisions assert the linearized order.
    tokio::time::sleep(Duration::from_millis(50)).await;
    blocker.commit().await?;

    completion.await??;
    let reclaim_decision = reclaim.await??;
    assert_eq!(reclaim_decision, IdempotencyDecision::Replay(response));

    pool.close().await;
    Ok(())
}

async fn wait_for_idempotency_lock_waiters(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    query_pattern: &str,
    minimum_waiters: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let waiters = sqlx::query_scalar::<_, i64>(
            r"
            SELECT count(*)
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND pid <> pg_backend_pid()
              AND state = 'active'
              AND wait_event_type = 'Lock'
              AND query LIKE $1
            ",
        )
        .bind(query_pattern)
        .fetch_one(&mut **transaction)
        .await?;
        if waiters >= minimum_waiters {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "expected at least {minimum_waiters} idempotency lock waiters, observed {waiters}"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn initial_schema_rejects_persisted_temporary_urls() -> Result<(), Box<dyn std::error::Error>>
{
    let database_url = std::env::var("WAVEFORM_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    let mut transaction = pool.begin().await?;
    sqlx::query("SET LOCAL search_path TO pg_temp")
        .execute(&mut *transaction)
        .await?;
    sqlx::raw_sql(include_str!("../migrations/0001_idempotency.sql"))
        .execute(&mut *transaction)
        .await?;

    let operation_request_id = request_id()?;
    let durable_response = serde_json::json!({
        "operation": "tts",
        "request_id": operation_request_id,
        "file_url": "https://briefcase.example.test/files/generated",
        "provider": "gemini",
        "duration_ms": 1_234,
    });
    sqlx::query(
        r"
        INSERT INTO waveform_idempotency_records (
            actor_type, actor_id, org_id, operation, idempotency_key,
            request_digest, request_id, state, response_body, expires_at
        ) VALUES (
            'carbon', $1, 'migration-test', 'tts', 'migration-test-key',
            $2, $3, 'completed', $4, clock_timestamp() + INTERVAL '24 hours'
        )
        ",
    )
    .bind(Uuid::new_v4())
    .bind(vec![7_u8; 32])
    .bind(operation_request_id.as_uuid())
    .bind(durable_response)
    .execute(&mut *transaction)
    .await?;

    let rejected = sqlx::query(
        r#"
        UPDATE waveform_idempotency_records
        SET response_body = response_body ||
            '{"temporary_url":"https://cdn.example.test/?signature=reintroduced"}'::jsonb
        WHERE request_id = $1
        "#,
    )
    .bind(operation_request_id.as_uuid())
    .execute(&mut *transaction)
    .await;
    assert!(matches!(rejected, Err(sqlx::Error::Database(_))));

    transaction.rollback().await?;
    pool.close().await;
    Ok(())
}

async fn test_store(
    record_ttl: Duration,
) -> Result<(PgPool, PostgresIdempotencyStore), Box<dyn std::error::Error>> {
    let database_url = std::env::var("WAVEFORM_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let store = PostgresIdempotencyStore::new(
        pool.clone(),
        Duration::from_secs(3),
        Duration::from_secs(2),
        record_ttl,
    );
    Ok((pool, store))
}

fn claim(
    scope: IdempotencyScope,
    digest: RequestDigest,
    request_id: RequestId,
    lease_id: IdempotencyLeaseId,
    lease_duration: Duration,
) -> IdempotencyClaim {
    IdempotencyClaim {
        scope,
        digest,
        request_id,
        lease_id,
        lease_duration,
    }
}

fn scope() -> Result<IdempotencyScope, Box<dyn std::error::Error>> {
    Ok(IdempotencyScope {
        actor: Actor::new(ActorKind::Carbon, ActorId::new(Uuid::new_v4())?),
        organization_id: OrganizationId::from_str("integration-test")?,
        operation: SpeechOperation::Tts,
        key: IdempotencyKey::from_str(&format!("integration-{}", Uuid::new_v4()))?,
    })
}

fn request_id() -> Result<RequestId, Box<dyn std::error::Error>> {
    Ok(RequestId::new(Uuid::now_v7())?)
}

fn digest_key() -> Result<RequestDigestKey, Box<dyn std::error::Error>> {
    Ok(RequestDigestKey::new(
        b"postgres-integration-test-digest-key",
    )?)
}

fn lease_id() -> Result<IdempotencyLeaseId, Box<dyn std::error::Error>> {
    Ok(IdempotencyLeaseId::new(Uuid::new_v4())?)
}

fn completed_tts_operation(
    request_id: RequestId,
) -> Result<CompletedTtsOperation, Box<dyn std::error::Error>> {
    Ok(CompletedTtsOperation {
        request_id,
        permanent_url: BriefcaseFileUrl::new(Url::parse(
            "https://briefcase.example.test/files/generated",
        )?)?,
        provider: ProviderName::Gemini,
        duration: MediaDuration::from_millis(1_234),
    })
}
