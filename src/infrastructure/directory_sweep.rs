//! Deleting accounts at Waveform, and the background account check that finds
//! the deletions no webhook announces.
//!
//! Silicon Accounts sends `account.deleted` only to apps the account is a live
//! member of. An account that used Waveform only through another app's proof,
//! or that removed Waveform's access before deleting itself, is never announced,
//! so Waveform asks Silicon Accounts about every account it holds at least once
//! a day and purges the ones it calls deleted (`404 account_deleted`; a uuid is
//! never reused). An answer of "no such account" is never treated as a deletion:
//! for a uuid Waveform learnt from a verified credential it means a
//! misconfigured Silicon Accounts URL, so it is only logged.

use std::str::FromStr as _;

use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{AccountDirectory, Lookup, LookupPurpose};
use crate::domain::identity::AccountUuid;

/// What deleting an account at Waveform did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Purge {
    /// Its data was deleted and a tombstone kept. Holds the Briefcase proof
    /// Waveform had for it, to revoke at Silicon Accounts once committed.
    Purged {
        /// Proof id to revoke, when Waveform held a live one.
        revoked_proof: Option<String>,
    },
    /// Waveform had not seen the account; the deletion is kept for its first sighting.
    Recorded,
}

/// What one batch of the background account check did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SweepReport {
    /// Accounts Silicon Accounts described (ids and custodians refreshed).
    pub checked: usize,
    /// Accounts Silicon Accounts called deleted, whose data was deleted.
    pub purged: usize,
    /// Accounts Silicon Accounts does not know (logged, nothing deleted).
    pub unknown: usize,
    /// True when Silicon Accounts could not be asked (outage or spent budget);
    /// the rest of the batch is retried about an hour later.
    pub stopped: bool,
}

/// Holds the account's lock until the transaction ends. Webhook events about an
/// account and the transaction that first creates its rows take it, so an event
/// that arrives while the account is first seen is never lost between them.
///
/// # Errors
///
/// Database failures.
pub async fn lock_account(
    transaction: &mut Transaction<'_, Postgres>,
    uuid: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("waveform-account:{uuid}"))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Deletes everything Waveform holds for a deleted account and keeps a
/// tombstone so its tokens stay refused: provider keys, Briefcase proofs,
/// preferences, job history, idempotency records, held audio, bug reports and
/// the earlier identity rows. Briefcase copies are the account's files and
/// Briefcase handles them. An account Waveform has not seen is recorded instead.
///
/// # Errors
///
/// Database failures.
pub async fn purge_account(
    transaction: &mut Transaction<'_, Postgres>,
    uuid: &str,
    at: OffsetDateTime,
) -> Result<Purge, sqlx::Error> {
    lock_account(transaction, uuid).await?;
    let storage_key: Option<Uuid> = sqlx::query_scalar(
        "SELECT storage_actor_id FROM waveform_account_keys WHERE account_uuid=$1 FOR UPDATE",
    )
    .bind(uuid)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(storage_key) = storage_key else {
        record_pending_event(transaction, uuid, at, "account_deleted", true).await?;
        return Ok(Purge::Recorded);
    };
    let revoked_proof: Option<String> = sqlx::query_scalar("SELECT proof_id FROM waveform_storage_proofs WHERE storage_actor_id=$1 AND invalidated_at IS NULL LIMIT 1")
        .bind(storage_key)
        .fetch_optional(&mut **transaction)
        .await?;
    for statement in [
        "DELETE FROM waveform_provider_keys WHERE actor_id=$1",
        "DELETE FROM waveform_storage_proofs WHERE storage_actor_id=$1",
        "DELETE FROM waveform_account_preferences WHERE actor_id=$1",
        "DELETE FROM waveform_jobs WHERE actor_id=$1",
        "DELETE FROM waveform_idempotency_records WHERE actor_id=$1",
        "DELETE FROM waveform_held_audio WHERE storage_actor_id=$1",
        "DELETE FROM waveform_bug_reports WHERE actor_id=$1",
        "DELETE FROM waveform_actor_keys WHERE storage_actor_id=$1",
        "DELETE FROM waveform_identity_links WHERE storage_actor_id=$1",
    ] {
        sqlx::query(statement)
            .bind(storage_key)
            .execute(&mut **transaction)
            .await?;
    }
    sqlx::query("UPDATE waveform_accounts SET deleted_at=COALESCE(deleted_at, $2), revoked_before=GREATEST(COALESCE(revoked_before, $2), $2), revoked_reason='account_deleted', current_id=NULL, display_name=NULL, pfp_url=NULL, custodian_uuid=NULL, custodian_id=NULL, updated_at=now() WHERE account_uuid=$1")
        .bind(uuid)
        .bind(at)
        .execute(&mut **transaction)
        .await?;
    sqlx::query("DELETE FROM waveform_account_pending_events WHERE account_uuid=$1")
        .bind(uuid)
        .execute(&mut **transaction)
        .await?;
    Ok(Purge::Purged { revoked_proof })
}

/// Keeps a sign-out (`revoked_before`) or a deletion for an account Waveform has
/// not seen yet; reads of the account honour it from its first sighting.
///
/// # Errors
///
/// Database failures.
pub async fn record_pending_event(
    transaction: &mut Transaction<'_, Postgres>,
    uuid: &str,
    at: OffsetDateTime,
    reason: &str,
    deleted: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO waveform_account_pending_events(account_uuid, revoked_before, revoked_reason, deleted_at) VALUES($1, $2, $3, $4) ON CONFLICT (account_uuid) DO UPDATE SET revoked_before = GREATEST(waveform_account_pending_events.revoked_before, EXCLUDED.revoked_before), revoked_reason = CASE WHEN EXCLUDED.deleted_at IS NOT NULL OR EXCLUDED.revoked_before >= waveform_account_pending_events.revoked_before THEN EXCLUDED.revoked_reason ELSE waveform_account_pending_events.revoked_reason END, deleted_at = COALESCE(waveform_account_pending_events.deleted_at, EXCLUDED.deleted_at), updated_at = now()")
        .bind(uuid)
        .bind(at)
        .bind(reason)
        .bind(deleted.then_some(at))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Applies what was recorded for an account before Waveform first saw it, inside
/// the transaction that creates its rows: a deletion purges (and tombstones) it,
/// a sign-out becomes its revocation marker.
pub(super) async fn fold_pending_events(
    transaction: &mut Transaction<'_, Postgres>,
    uuid: &str,
) -> Result<(), sqlx::Error> {
    let deleted: Option<OffsetDateTime> = sqlx::query_scalar("SELECT deleted_at FROM waveform_account_pending_events WHERE account_uuid=$1 AND deleted_at IS NOT NULL")
        .bind(uuid)
        .fetch_optional(&mut **transaction)
        .await?;
    if let Some(at) = deleted {
        purge_account(transaction, uuid, at).await?;
        return Ok(());
    }
    sqlx::query("UPDATE waveform_accounts a SET revoked_before = GREATEST(a.revoked_before, p.revoked_before), revoked_reason = CASE WHEN p.revoked_before > COALESCE(a.revoked_before, '-infinity'::timestamptz) THEN p.revoked_reason ELSE a.revoked_reason END, updated_at = now() FROM waveform_account_pending_events p WHERE a.account_uuid = $1 AND p.account_uuid = a.account_uuid")
        .bind(uuid)
        .execute(&mut **transaction)
        .await?;
    sqlx::query("DELETE FROM waveform_account_pending_events WHERE account_uuid=$1")
        .bind(uuid)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

impl AccountDirectory {
    /// Deletes Waveform's data for an account Silicon Accounts says was deleted.
    /// No Briefcase proof is revoked remotely: Silicon Accounts ended them with
    /// the account.
    pub(super) async fn purge_deleted(&self, uuid: &AccountUuid) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        let purge =
            purge_account(&mut transaction, uuid.as_str(), OffsetDateTime::now_utc()).await?;
        transaction.commit().await?;
        if matches!(purge, Purge::Purged { .. }) {
            tracing::info!(
                "Silicon Accounts reports an account deleted; Waveform deleted its data and kept a tombstone"
            );
        }
        Ok(())
    }

    /// Notes that Silicon Accounts was asked about an account it does not know,
    /// so the background check asks again tomorrow rather than every minute.
    pub(super) async fn note_unknown(&self, uuid: &AccountUuid) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE waveform_accounts SET looked_up_at = now() WHERE account_uuid = $1")
            .bind(uuid.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// One batch of the background account check: asks Silicon Accounts about
    /// up to `limit` accounts not asked about for a day (never asked first),
    /// refreshing ids and custodians and purging deleted accounts. The batch is
    /// claimed first (`FOR UPDATE SKIP LOCKED`), so replicas never ask twice; an
    /// account whose check could not run is retried about an hour later. Also
    /// forgets pending events every token they could concern has outlived.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn sweep(&self, limit: i64) -> Result<SweepReport, sqlx::Error> {
        sqlx::query("DELETE FROM waveform_account_pending_events WHERE updated_at < now() - interval '7 days'")
            .execute(&self.pool)
            .await?;
        let due: Vec<String> = sqlx::query_scalar("UPDATE waveform_accounts SET looked_up_at = now() - interval '23 hours' WHERE account_uuid IN (SELECT account_uuid FROM waveform_accounts WHERE deleted_at IS NULL AND (looked_up_at IS NULL OR looked_up_at < now() - interval '24 hours') ORDER BY looked_up_at NULLS FIRST, account_uuid LIMIT $1 FOR UPDATE SKIP LOCKED) RETURNING account_uuid")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        let mut report = SweepReport::default();
        for uuid in due {
            let Ok(uuid) = AccountUuid::from_str(&uuid) else {
                continue;
            };
            let started = OffsetDateTime::now_utc();
            match self
                .gateway
                .lookup(uuid.as_str(), LookupPurpose::Sweep)
                .await
            {
                Ok(Lookup::Found(summary)) => {
                    if self.store_summary(&summary, started).await.is_err() {
                        report.stopped = true;
                        break;
                    }
                    report.checked += 1;
                }
                Ok(Lookup::Deleted) => {
                    self.purge_deleted(&uuid).await?;
                    report.purged += 1;
                }
                Ok(Lookup::NotFound) => {
                    tracing::warn!(
                        "Silicon Accounts does not know an account Waveform holds; nothing was deleted (check ACCOUNTS_API_URL)"
                    );
                    self.note_unknown(&uuid).await?;
                    report.unknown += 1;
                }
                Err(_) => {
                    report.stopped = true;
                    break;
                }
            }
        }
        Ok(report)
    }
}
