//! Operator commands of the `waveform-api` binary (host access only).
//!
//! `link-identities` re-keys IAM-era data to Silicon Accounts at cutover. It
//! reads `iam_principal_id,accounts_uuid` rows (a principal is the IAM-era
//! public id `c:…`/`si:…`, or a legacy principal UUID), maps each account uuid
//! to the private storage key that principal's rows already use, and
//! consolidates the account's settings from every former org into the
//! Accounts-era `@` rows. Nothing is deleted: legacy rows stay, IAM-era storage
//! grants are only marked invalidated. Everything runs in one transaction; a
//! conflict or an invalid row rolls the whole run back. Re-running is a no-op.

use std::{collections::BTreeMap, str::FromStr as _};

use serde::Serialize;
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use uuid::Uuid;

use crate::{
    domain::{
        idempotency::ACCOUNT_SCOPE,
        identity::{AccountUuid, ActorKind, public_id_kind},
    },
    infrastructure::vault::Vault,
};

/// Options of one `link-identities` run.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinkOptions {
    /// Report what would change, then roll back.
    pub dry_run: bool,
    /// Allow pointing an account uuid that already has an unused Accounts-era key at the legacy key.
    pub merge: bool,
    /// Allow re-pointing a legacy key that an earlier run linked to another uuid.
    pub relink: bool,
}

/// What a run did (or would do).
#[derive(Debug, Default, Serialize)]
pub struct LinkReport {
    /// True when nothing was committed.
    pub dry_run: bool,
    /// Mapping rows read.
    pub rows: usize,
    /// Accounts newly linked to a legacy key.
    pub linked: usize,
    /// Rows that were already linked exactly so.
    pub already_linked: usize,
    /// Principals with no Waveform data.
    pub unmatched: Vec<String>,
    /// Problems that stopped the run (nothing was applied).
    pub conflicts: Vec<String>,
    /// Accounts whose preferences were consolidated into the `@` row.
    pub preferences_consolidated: u64,
    /// Provider keys re-sealed into `@` rows.
    pub provider_keys_resealed: u64,
    /// Provider keys left on their legacy rows because no encryption key was configured.
    pub provider_keys_skipped: u64,
    /// IAM-era Briefcase grants marked invalidated.
    pub storage_grants_invalidated: u64,
}

/// One parsed mapping line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappingRow {
    /// 1-based line number in the file.
    pub line: usize,
    /// IAM principal (public id or legacy UUID).
    pub principal: String,
    /// Silicon Accounts uuid.
    pub account: AccountUuid,
}

/// Parses `iam_principal_id,accounts_uuid` CSV (an optional header line, blank
/// lines and `#` comments allowed).
///
/// # Errors
///
/// Every malformed line, each with its number and reason.
pub fn parse_mapping(text: &str) -> Result<Vec<MappingRow>, Vec<String>> {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || (line == 1 && trimmed.replace(' ', "") == "iam_principal_id,accounts_uuid")
        {
            continue;
        }
        let fields = trimmed.split(',').map(str::trim).collect::<Vec<_>>();
        let [principal, account] = fields.as_slice() else {
            errors.push(format!(
                "line {line}: expected exactly two columns: iam_principal_id,accounts_uuid"
            ));
            continue;
        };
        if principal.is_empty() || principal.len() > 255 {
            errors.push(format!(
                "line {line}: iam_principal_id must be 1 to 255 characters"
            ));
            continue;
        }
        match AccountUuid::from_str(account) {
            Ok(account) => rows.push(MappingRow {
                line,
                principal: (*principal).to_owned(),
                account,
            }),
            Err(_) => errors.push(format!(
                "line {line}: `{account}` is not a Silicon Accounts uuid (letters and digits, case-sensitive)"
            )),
        }
    }
    let mut by_principal: BTreeMap<&str, &AccountUuid> = BTreeMap::new();
    for row in &rows {
        if let Some(previous) = by_principal.insert(&row.principal, &row.account)
            && previous != &row.account
        {
            errors.push(format!(
                "line {}: {} is mapped to two different accounts ({previous} and {})",
                row.line, row.principal, row.account
            ));
        }
    }
    if errors.is_empty() {
        Ok(rows)
    } else {
        Err(errors)
    }
}

/// Confirms every target exists and has the legacy identity's kind before an
/// operator applies or previews a mapping. Public ids may have changed and are
/// never used as ownership evidence.
///
/// # Errors
/// Database failures and unavailable or contradictory Accounts lookups.
pub async fn verify_mapping_accounts(
    pool: &PgPool,
    gateway: &crate::infrastructure::accounts::AccountsGateway,
    rows: &[MappingRow],
) -> Result<(), String> {
    use crate::infrastructure::accounts::{Lookup, LookupPurpose};
    let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;
    for row in rows {
        let legacy = legacy_key(&mut transaction, &row.principal)
            .await
            .map_err(|error| error.to_string())?;
        let Legacy::Found(_, kind, _) = legacy else {
            continue;
        };
        match gateway
            .lookup(row.account.as_str(), LookupPurpose::Authorization)
            .await
        {
            Ok(Lookup::Found(account)) if account.kind.as_str() == kind.as_str() => {}
            Ok(Lookup::Found(_)) => {
                return Err(format!(
                    "line {}: {} and account {} have different kinds; nothing changed",
                    row.line, row.principal, row.account
                ));
            }
            Ok(Lookup::Deleted | Lookup::NotFound) => {
                return Err(format!(
                    "line {}: account {} is missing or deleted; nothing changed",
                    row.line, row.account
                ));
            }
            Err(_) => {
                return Err(format!(
                    "line {}: Silicon Accounts could not verify {}; retry before applying the mapping",
                    row.line, row.account
                ));
            }
        }
    }
    transaction
        .rollback()
        .await
        .map_err(|error| error.to_string())
}

/// Applies a mapping (see the module docs).
///
/// # Errors
///
/// Database failures. Conflicts are reported in [`LinkReport::conflicts`] and
/// leave the database unchanged.
pub async fn link_identities(
    pool: &PgPool,
    vault: Option<&Vault>,
    rows: &[MappingRow],
    options: LinkOptions,
) -> Result<LinkReport, sqlx::Error> {
    let mut report = LinkReport {
        dry_run: options.dry_run,
        rows: rows.len(),
        ..LinkReport::default()
    };
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('waveform-link-identities', 0))")
        .execute(&mut *transaction)
        .await?;
    let mut linked_keys = Vec::new();
    let mut account_sources: BTreeMap<&str, (Uuid, usize)> = BTreeMap::new();
    let mut key_targets: BTreeMap<Uuid, (&str, usize)> = BTreeMap::new();
    for row in rows {
        let (storage_key, kind, public_id) = match legacy_key(&mut transaction, &row.principal)
            .await?
        {
            Legacy::Found(storage_key, kind, public_id) => (storage_key, kind, public_id),
            Legacy::NoData => {
                report.unmatched.push(row.principal.clone());
                continue;
            }
            Legacy::UnknownKind => {
                report.conflicts.push(format!(
                    "line {}: Waveform cannot tell whether {} is a Carbon or a Silicon (its rows hold settings only); map its c:/si: id instead",
                    row.line, row.principal
                ));
                continue;
            }
        };
        if let Some((previous, line)) =
            account_sources.insert(row.account.as_str(), (storage_key, row.line))
            && previous != storage_key
        {
            report.conflicts.push(format!("lines {line} and {} map different legacy identities to account {}; nothing changed", row.line, row.account));
            continue;
        }
        if let Some((previous, line)) =
            key_targets.insert(storage_key, (row.account.as_str(), row.line))
            && previous != row.account.as_str()
        {
            report.conflicts.push(format!("lines {line} and {} map the same legacy storage key to different accounts; nothing changed", row.line));
            continue;
        }
        match link_one(
            &mut transaction,
            row,
            storage_key,
            kind,
            public_id.as_deref(),
            options,
        )
        .await?
        {
            Ok(true) => report.linked += 1,
            Ok(false) => report.already_linked += 1,
            Err(conflict) => {
                report.conflicts.push(conflict);
                continue;
            }
        }
        linked_keys.push(storage_key);
    }
    if !report.conflicts.is_empty() {
        transaction.rollback().await?;
        return Ok(report);
    }
    linked_keys.sort_unstable();
    linked_keys.dedup();
    for storage_key in linked_keys {
        consolidate(&mut transaction, vault, storage_key, &mut report).await?;
    }
    if options.dry_run {
        transaction.rollback().await?;
    } else {
        transaction.commit().await?;
    }
    Ok(report)
}

enum Legacy {
    Found(Uuid, ActorKind, Option<String>),
    NoData,
    UnknownKind,
}

/// The private key, kind and public id behind an IAM-era principal.
async fn legacy_key(
    transaction: &mut Transaction<'_, Postgres>,
    principal: &str,
) -> Result<Legacy, sqlx::Error> {
    if let Some(row) = sqlx::query("SELECT storage_actor_id, iam_public_id FROM waveform_identity_links WHERE iam_principal_id=$1")
        .bind(principal)
        .fetch_optional(&mut **transaction)
        .await?
    {
        let storage_key: Uuid = row.try_get("storage_actor_id")?;
        let public_id: Option<String> = row.try_get("iam_public_id")?;
        if let Some(kind) = public_id.as_deref().and_then(public_id_kind) {
            return Ok(Legacy::Found(storage_key, kind, public_id));
        }
        return kind_from_rows(transaction, storage_key, public_id).await;
    }
    // A legacy principal UUID: accounts imported before IAM 3 kept it as their key.
    match Uuid::parse_str(principal) {
        Ok(candidate) if !candidate.is_nil() => kind_from_rows(transaction, candidate, None).await,
        _ => Ok(Legacy::NoData),
    }
}

/// The kind recorded on a key's own rows, when it owns any.
async fn kind_from_rows(
    transaction: &mut Transaction<'_, Postgres>,
    storage_key: Uuid,
    public_id: Option<String>,
) -> Result<Legacy, sqlx::Error> {
    let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM (SELECT actor_kind AS kind FROM waveform_jobs WHERE actor_id=$1 UNION ALL SELECT actor_type::text FROM waveform_idempotency_records WHERE actor_id=$1) kinds LIMIT 1")
        .bind(storage_key)
        .fetch_optional(&mut **transaction)
        .await?;
    if let Some(kind) = kind
        .as_deref()
        .and_then(|kind| ActorKind::from_str(kind).ok())
    {
        return Ok(Legacy::Found(storage_key, kind, public_id));
    }
    let owns_rows: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM waveform_account_preferences WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_provider_keys WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_bug_reports WHERE actor_id=$1)")
        .bind(storage_key)
        .fetch_one(&mut **transaction)
        .await?;
    Ok(if owns_rows {
        Legacy::UnknownKind
    } else {
        Legacy::NoData
    })
}

async fn key_has_data(
    transaction: &mut Transaction<'_, Postgres>,
    storage_key: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM waveform_jobs WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_account_preferences WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_provider_keys WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_idempotency_records WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_bug_reports WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_storage_proofs WHERE storage_actor_id=$1) OR EXISTS(SELECT 1 FROM waveform_held_audio WHERE storage_actor_id=$1)")
        .bind(storage_key)
        .fetch_one(&mut **transaction)
        .await
}

/// Links one row. `Ok(Ok(true))` linked now, `Ok(Ok(false))` already linked,
/// `Ok(Err(reason))` a conflict that stops the run.
async fn link_one(
    transaction: &mut Transaction<'_, Postgres>,
    row: &MappingRow,
    storage_key: Uuid,
    kind: ActorKind,
    public_id: Option<&str>,
    options: LinkOptions,
) -> Result<Result<bool, String>, sqlx::Error> {
    let account = row.account.as_str();
    let holder = sqlx::query(
        "SELECT account_uuid, source FROM waveform_account_keys WHERE storage_actor_id=$1",
    )
    .bind(storage_key)
    .fetch_optional(&mut **transaction)
    .await?;
    if let Some(holder) = holder {
        let other: String = holder.try_get("account_uuid")?;
        let source: String = holder.try_get("source")?;
        if other == account {
            mark_link(transaction, row, storage_key).await?;
            return Ok(Ok(false));
        }
        if !(options.relink && source == "link_identities") {
            return Ok(Err(format!(
                "line {}: the Waveform data of {} already belongs to account {other}; re-run with --relink to move it (allowed only for links this command made)",
                row.line, row.principal
            )));
        }
        let used_since_link: bool = sqlx::query_scalar("WITH linked AS (SELECT min(linked_at) AS at FROM waveform_identity_links WHERE storage_actor_id=$1 AND accounts_uuid=$2) SELECT EXISTS(SELECT 1 FROM waveform_jobs, linked WHERE actor_id=$1 AND created_at>linked.at) OR EXISTS(SELECT 1 FROM waveform_account_preferences, linked WHERE actor_id=$1 AND updated_at>linked.at) OR EXISTS(SELECT 1 FROM waveform_provider_keys, linked WHERE actor_id=$1 AND updated_at>linked.at) OR EXISTS(SELECT 1 FROM waveform_idempotency_records, linked WHERE actor_id=$1 AND updated_at>linked.at) OR EXISTS(SELECT 1 FROM waveform_storage_proofs, linked WHERE storage_actor_id=$1 AND updated_at>linked.at) OR EXISTS(SELECT 1 FROM waveform_held_audio, linked WHERE storage_actor_id=$1 AND created_at>linked.at) OR EXISTS(SELECT 1 FROM waveform_accounts, linked WHERE account_uuid=$2 AND (id_changed_at>linked.at OR custodian_changed_at>linked.at))")
            .bind(storage_key).bind(&other).fetch_one(&mut **transaction).await?;
        if used_since_link {
            return Ok(Err(format!(
                "line {}: account {other} has used its linked data; --relink is only for corrections before use and cannot transfer that activity",
                row.line
            )));
        }
        // Keep the old identity's revocation/deletion fence even though its storage
        // mapping is being corrected. A later first sighting folds it back in.
        sqlx::query("INSERT INTO waveform_account_pending_events(account_uuid, revoked_before, revoked_reason, deleted_at) SELECT account_uuid, COALESCE(revoked_before, deleted_at), COALESCE(revoked_reason, 'account_deleted'), deleted_at FROM waveform_accounts WHERE account_uuid=$1 AND (revoked_before IS NOT NULL OR deleted_at IS NOT NULL) ON CONFLICT(account_uuid) DO UPDATE SET revoked_before=GREATEST(waveform_account_pending_events.revoked_before, EXCLUDED.revoked_before), deleted_at=COALESCE(waveform_account_pending_events.deleted_at, EXCLUDED.deleted_at)")
            .bind(&other).execute(&mut **transaction).await?;
        sqlx::query("DELETE FROM waveform_accounts WHERE account_uuid=$1")
            .bind(&other)
            .execute(&mut **transaction)
            .await?;
        sqlx::query(
            "DELETE FROM waveform_account_keys WHERE account_uuid=$1 AND source='link_identities'",
        )
        .bind(&other)
        .execute(&mut **transaction)
        .await?;
    }
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT storage_actor_id FROM waveform_account_keys WHERE account_uuid=$1",
    )
    .bind(account)
    .fetch_optional(&mut **transaction)
    .await?;
    if let Some(fresh_key) = existing
        && fresh_key != storage_key
    {
        let earlier_link: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM waveform_identity_links WHERE accounts_uuid=$1 AND storage_actor_id<>$2)")
            .bind(account).bind(storage_key).fetch_one(&mut **transaction).await?;
        if earlier_link {
            return Ok(Err(format!(
                "line {}: account {account} is already linked to a different legacy identity; --merge cannot combine distinct identities",
                row.line
            )));
        }
        if !options.merge {
            return Ok(Err(format!(
                "line {}: account {account} already has its own Waveform key (it signed in after the cutover started); re-run with --merge to point it at {}'s data",
                row.line, row.principal
            )));
        }
        if key_has_data(transaction, fresh_key).await? {
            return Ok(Err(format!(
                "line {}: account {account} already has Waveform data under its new key, so {}'s data cannot be merged automatically",
                row.line, row.principal
            )));
        }
        sqlx::query("UPDATE waveform_account_keys SET storage_actor_id=$2, source='link_identities' WHERE account_uuid=$1")
            .bind(account)
            .bind(storage_key)
            .execute(&mut **transaction)
            .await?;
        mark_link(transaction, row, storage_key).await?;
        return Ok(Ok(true));
    }
    sqlx::query("INSERT INTO waveform_account_keys(account_uuid, storage_actor_id, kind, source) VALUES($1,$2,$3,'link_identities') ON CONFLICT (account_uuid) DO NOTHING")
        .bind(account)
        .bind(storage_key)
        .bind(kind.as_str())
        .execute(&mut **transaction)
        .await?;
    sqlx::query("INSERT INTO waveform_accounts(account_uuid, kind, current_id) VALUES($1,$2,$3) ON CONFLICT (account_uuid) DO NOTHING")
        .bind(account)
        .bind(kind.as_str())
        .bind(public_id.filter(|id| public_id_kind(id) == Some(kind)))
        .execute(&mut **transaction)
        .await?;
    mark_link(transaction, row, storage_key).await?;
    Ok(Ok(true))
}

async fn mark_link(
    transaction: &mut Transaction<'_, Postgres>,
    row: &MappingRow,
    storage_key: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO waveform_identity_links(iam_principal_id, iam_public_id, storage_actor_id, accounts_uuid, linked_at, source) VALUES($1, NULL, $2, $3, now(), 'mapping_file') ON CONFLICT (iam_principal_id) DO UPDATE SET accounts_uuid=EXCLUDED.accounts_uuid, linked_at=COALESCE(CASE WHEN waveform_identity_links.accounts_uuid = EXCLUDED.accounts_uuid THEN waveform_identity_links.linked_at END, now())")
        .bind(&row.principal)
        .bind(storage_key)
        .bind(row.account.as_str())
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Folds an account's per-org settings into its `@` rows (newest wins) and
/// retires its IAM-era Briefcase grants. Legacy rows are kept.
async fn consolidate(
    transaction: &mut Transaction<'_, Postgres>,
    vault: Option<&Vault>,
    storage_key: Uuid,
    report: &mut LinkReport,
) -> Result<(), sqlx::Error> {
    report.preferences_consolidated += sqlx::query("INSERT INTO waveform_account_preferences(plane_id, org_id, actor_id, tts_order, stt_order, voice_profile, updated_at) SELECT plane_id, $2, actor_id, tts_order, stt_order, voice_profile, updated_at FROM waveform_account_preferences WHERE plane_id=$1 AND actor_id=$3 AND org_id<>$2 ORDER BY updated_at DESC LIMIT 1 ON CONFLICT (plane_id, org_id, actor_id) DO NOTHING")
        .bind(Uuid::nil())
        .bind(ACCOUNT_SCOPE)
        .bind(storage_key)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
    let legacy_keys = sqlx::query("SELECT DISTINCT ON (provider) provider, org_id, secret_cipher FROM waveform_provider_keys WHERE plane_id=$1 AND actor_id=$3 AND org_id<>$2 AND provider NOT IN (SELECT provider FROM waveform_provider_keys WHERE plane_id=$1 AND actor_id=$3 AND org_id=$2) ORDER BY provider, updated_at DESC")
        .bind(Uuid::nil())
        .bind(ACCOUNT_SCOPE)
        .bind(storage_key)
        .fetch_all(&mut **transaction)
        .await?;
    for row in legacy_keys {
        let provider: String = row.try_get("provider")?;
        let org: String = row.try_get("org_id")?;
        let cipher: Vec<u8> = row.try_get("secret_cipher")?;
        let resealed = vault.and_then(|vault| {
            let secret = vault
                .open(
                    &cipher,
                    &crate::control::legacy_key_context(&org, storage_key, &provider),
                )
                .ok()?;
            vault
                .seal(
                    secrecy::ExposeSecret::expose_secret(&secret),
                    &crate::control::key_context(storage_key, &provider),
                )
                .ok()
        });
        let Some(resealed) = resealed else {
            report.provider_keys_skipped += 1;
            continue;
        };
        sqlx::query("INSERT INTO waveform_provider_keys(plane_id, org_id, actor_id, provider, secret_cipher) VALUES($1,$2,$3,$4,$5) ON CONFLICT (plane_id, org_id, actor_id, provider) DO NOTHING")
            .bind(Uuid::nil())
            .bind(ACCOUNT_SCOPE)
            .bind(storage_key)
            .bind(&provider)
            .bind(resealed)
            .execute(&mut **transaction)
            .await?;
        report.provider_keys_resealed += 1;
    }
    report.storage_grants_invalidated += sqlx::query("UPDATE waveform_storage_grants SET invalidated_at=now() WHERE plane_id=$1 AND actor_id=$2 AND invalidated_at IS NULL")
        .bind(Uuid::nil())
        .bind(storage_key)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_mapping;

    #[test]
    fn mapping_files_are_validated_line_by_line() {
        let rows = parse_mapping(
            "iam_principal_id,accounts_uuid\n# comment\nc:ada,zQo\n\nsi:scout , K1E\n",
        );
        assert!(
            matches!(rows.as_deref(), Ok([first, second]) if first.principal == "c:ada" && second.account.as_str() == "K1E" && second.line == 5)
        );
        let errors = parse_mapping("c:ada,zQo\nc:ada,zQ0\nbroken\nc:bob,not-a-uuid\n")
            .err()
            .unwrap_or_default();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors.iter().any(|error| error.starts_with("line 3:")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("two different accounts"))
        );
    }
}
