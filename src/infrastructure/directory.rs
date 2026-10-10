//! Waveform's cache of Silicon Accounts accounts: the uuid ↔ private storage key
//! mapping, display fields, a Silicon's custodian, revocation markers, and the
//! custodian circle used by authorisation.
//!
//! circle(Carbon C) = C and every Silicon C is custodian of.
//! circle(Silicon S) = S, its custodian, and the custodian's other Silicons.

use std::{
    collections::HashMap,
    str::FromStr as _,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;
use uuid::Uuid;

use super::accounts::AccountsGateway;
use crate::{
    application::ports::AccountsError,
    domain::identity::{AccountUuid, ActorKind, public_id_kind},
};

/// A Silicon's custodian is re-read from Accounts at most this often on sight.
const CUSTODIAN_RECHECK: Duration = Duration::from_secs(600);
/// A mismatching custodian is re-checked at most this often before refusing.
const CUSTODIAN_MISMATCH_RECHECK: Duration = Duration::from_secs(60);
/// The first request of a Silicon waits at most this long for its custodian lookup.
const FIRST_LOOKUP_WAIT: Duration = Duration::from_secs(2);
/// After a first lookup failed, requests stop waiting for another one for this long.
const FIRST_LOOKUP_BACKOFF: Duration = Duration::from_secs(60);
/// A webhook change this close to a lookup's start (or later) wins over the lookup's
/// answer, which may have been read before the change (allows for clock skew).
const LOOKUP_SKEW: time::Duration = time::Duration::seconds(5);
/// An account whose name and photo are unknown asks Accounts' userinfo at most this often.
const PROFILE_RECHECK: Duration = Duration::from_secs(600);

/// How an account first became known to Waveform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeySource {
    /// Signed in to Waveform (bearer token).
    SignIn,
    /// Another app called for it with a proof.
    Proof,
    /// Looked up because a custodian selected it.
    Lookup,
}

impl KeySource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SignIn => "sign_in",
            Self::Proof => "proof",
            Self::Lookup => "lookup",
        }
    }
}

/// One cached account.
#[derive(Clone, Debug)]
pub struct AccountRecord {
    /// Permanent Accounts uuid.
    pub uuid: AccountUuid,
    /// Private storage key used by every Waveform table.
    pub storage_key: Uuid,
    /// Carbon or Silicon.
    pub kind: ActorKind,
    /// Current public id, when known.
    pub current_id: Option<String>,
    /// Display name, when known.
    pub display_name: Option<String>,
    /// Profile photo URL from Accounts, when known.
    pub pfp_url: Option<String>,
    /// A Silicon's custodian (uuid, current id).
    pub custodian: Option<(String, String)>,
    /// When the custodian was last confirmed with Accounts.
    pub custodian_checked_at: Option<OffsetDateTime>,
    /// Tokens issued before this instant are refused.
    pub revoked_before: Option<OffsetDateTime>,
    /// Why `revoked_before` was set.
    pub revoked_reason: Option<String>,
    /// Set once the account was deleted (tombstone).
    pub deleted_at: Option<OffsetDateTime>,
}

/// Postgres-backed account cache fed by tokens, proofs, lookups and webhooks.
#[derive(Clone)]
pub struct AccountDirectory {
    pool: PgPool,
    gateway: Arc<AccountsGateway>,
    first_lookup_failures: Arc<Mutex<HashMap<String, Instant>>>,
    profile_checks: Arc<Mutex<HashMap<String, Instant>>>,
}

const SELECT_RECORD: &str = "SELECT k.account_uuid, k.storage_actor_id, k.kind, a.current_id, a.display_name, a.pfp_url, a.custodian_uuid, a.custodian_id, a.custodian_checked_at, a.revoked_before, a.revoked_reason, a.deleted_at FROM waveform_account_keys k LEFT JOIN waveform_accounts a ON a.account_uuid = k.account_uuid";

impl AccountDirectory {
    /// Wraps the pool and the Accounts gateway.
    #[must_use]
    pub fn new(pool: PgPool, gateway: Arc<AccountsGateway>) -> Self {
        Self {
            pool,
            gateway,
            first_lookup_failures: Arc::new(Mutex::new(HashMap::new())),
            profile_checks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Fills a missing display name and photo from Accounts' userinfo, read with
    /// the account's own access token (app lookups carry neither; later changes
    /// arrive as `account.updated`). At most once per [`PROFILE_RECHECK`] per
    /// account; a profile older than the cached version is ignored. Best effort:
    /// on any failure the record is answered as it was.
    pub async fn complete_profile(&self, record: AccountRecord, token: &str) -> AccountRecord {
        if record.deleted_at.is_some()
            || (record.display_name.is_some() && record.pfp_url.is_some())
        {
            return record;
        }
        if let Ok(mut checks) = self.profile_checks.lock() {
            checks.retain(|_, checked| checked.elapsed() < PROFILE_RECHECK);
            if checks.contains_key(record.uuid.as_str()) {
                return record;
            }
            checks.insert(record.uuid.as_str().to_owned(), Instant::now());
        }
        let Ok(info) = self.gateway.userinfo(token).await else {
            tracing::debug!("userinfo unavailable; the profile is filled on a later request");
            return record;
        };
        if info.account.uuid != record.uuid.as_str() {
            return record;
        }
        let updated = sqlx::query("UPDATE waveform_accounts SET display_name = COALESCE(NULLIF($2, ''), display_name), pfp_url = COALESCE(NULLIF($3, ''), pfp_url), profile_version = GREATEST(profile_version, $4), updated_at = now() WHERE account_uuid = $1 AND deleted_at IS NULL AND profile_version <= $4")
            .bind(record.uuid.as_str())
            .bind(&info.account.display_name)
            .bind(&info.account.pfp_url)
            .bind(info.account.version)
            .execute(&self.pool)
            .await;
        match updated {
            Ok(_) => self
                .load(&record.uuid)
                .await
                .ok()
                .flatten()
                .unwrap_or(record),
            Err(_) => record,
        }
    }

    /// The gateway used for lookups.
    #[must_use]
    pub fn gateway(&self) -> &Arc<AccountsGateway> {
        &self.gateway
    }

    /// Reads one account.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn load(&self, uuid: &AccountUuid) -> Result<Option<AccountRecord>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SELECT_RECORD} WHERE k.account_uuid = $1"
        )))
        .bind(uuid.as_str())
        .fetch_optional(&self.pool)
        .await?
        .map(|row| record(&row))
        .transpose()
    }

    /// Reads the account behind a private storage key.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn by_storage_key(&self, key: Uuid) -> Result<Option<AccountRecord>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SELECT_RECORD} WHERE k.storage_actor_id = $1"
        )))
        .bind(key)
        .fetch_optional(&self.pool)
        .await?
        .map(|row| record(&row))
        .transpose()
    }

    /// Records that an account was seen with a verified credential, creating
    /// its storage key on first sight. The first time Waveform sees a Silicon,
    /// the request waits (briefly) for its custodian lookup so the answer already
    /// names its custodian; later re-checks run in the background.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn sighting(
        &self,
        uuid: &AccountUuid,
        kind: ActorKind,
        id_hint: Option<&str>,
        source: KeySource,
    ) -> Result<AccountRecord, sqlx::Error> {
        let record = match self.load(uuid).await? {
            Some(record) if record.current_id.is_some() || id_hint.is_none() => record,
            _ => self.ensure(uuid, kind, id_hint, source).await?,
        };
        if record.kind != ActorKind::Silicon || record.deleted_at.is_some() {
            return Ok(record);
        }
        match record.custodian_checked_at {
            None if self.may_wait_for_first_lookup(uuid) => {
                match tokio::time::timeout(FIRST_LOOKUP_WAIT, self.refresh_from_accounts(uuid))
                    .await
                {
                    Ok(Ok(Some(fresh))) => return Ok(fresh),
                    Ok(Ok(None)) => {}
                    Ok(Err(_)) | Err(_) => {
                        tracing::debug!(
                            "custodian lookup failed; later requests look it up in the background"
                        );
                        self.first_lookup_failed(uuid);
                    }
                }
            }
            None => self.refresh_in_background(uuid),
            Some(checked)
                if OffsetDateTime::now_utc() - checked
                    > time::Duration::try_from(CUSTODIAN_RECHECK)
                        .unwrap_or(time::Duration::MAX) =>
            {
                self.refresh_in_background(uuid);
            }
            Some(_) => {}
        }
        Ok(record)
    }

    fn refresh_in_background(&self, uuid: &AccountUuid) {
        let directory = self.clone();
        let uuid = uuid.clone();
        tokio::spawn(async move {
            if directory.refresh_from_accounts(&uuid).await.is_err() {
                tracing::debug!("custodian refresh will be retried on a later sighting");
            }
        });
    }

    fn may_wait_for_first_lookup(&self, uuid: &AccountUuid) -> bool {
        let Ok(mut failures) = self.first_lookup_failures.lock() else {
            return false;
        };
        failures.retain(|_, failed_at| failed_at.elapsed() < FIRST_LOOKUP_BACKOFF);
        !failures.contains_key(uuid.as_str())
    }

    fn first_lookup_failed(&self, uuid: &AccountUuid) {
        if let Ok(mut failures) = self.first_lookup_failures.lock() {
            failures.insert(uuid.as_str().to_owned(), Instant::now());
        }
    }

    /// Creates the storage key and cache row when missing (idempotent), and fills
    /// a missing public id from `id_hint`.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn ensure(
        &self,
        uuid: &AccountUuid,
        kind: ActorKind,
        id_hint: Option<&str>,
        source: KeySource,
    ) -> Result<AccountRecord, sqlx::Error> {
        let id_hint = id_hint.filter(|id| public_id_kind(id) == Some(kind));
        let mut transaction = self.pool.begin().await?;
        sqlx::query("INSERT INTO waveform_account_keys(account_uuid, storage_actor_id, kind, source) VALUES($1, $2, $3, $4) ON CONFLICT (account_uuid) DO NOTHING")
            .bind(uuid.as_str())
            .bind(Uuid::now_v7())
            .bind(kind.as_str())
            .bind(source.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO waveform_accounts(account_uuid, kind, current_id) SELECT account_uuid, kind, $2 FROM waveform_account_keys WHERE account_uuid = $1 ON CONFLICT (account_uuid) DO UPDATE SET current_id = COALESCE(waveform_accounts.current_id, EXCLUDED.current_id), updated_at = now() WHERE waveform_accounts.current_id IS NULL AND waveform_accounts.deleted_at IS NULL")
            .bind(uuid.as_str())
            .bind(id_hint)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        self.load(uuid).await?.ok_or(sqlx::Error::RowNotFound)
    }

    /// Re-reads one account from Accounts (kind, id, name, photo, custodian).
    /// `Ok(None)` when Accounts has no such account.
    ///
    /// # Errors
    ///
    /// Accounts or database failures (database failures map to `Unavailable`).
    pub async fn refresh_from_accounts(
        &self,
        uuid: &AccountUuid,
    ) -> Result<Option<AccountRecord>, AccountsError> {
        let started = OffsetDateTime::now_utc();
        let Some(summary) = self.gateway.lookup(uuid.as_str()).await? else {
            return Ok(None);
        };
        self.store_summary(&summary, started).await.map(Some)
    }

    /// Stores what a lookup that started at `started` returned. Ids and custodians
    /// a webhook changed since then (or within the skew allowance) are kept: the
    /// lookup may have been answered before that change.
    async fn store_summary(
        &self,
        summary: &silicon_accounts_client::AccountSummary,
        started: OffsetDateTime,
    ) -> Result<AccountRecord, AccountsError> {
        let uuid = AccountUuid::from_str(&summary.uuid).map_err(|_| AccountsError::Unavailable)?;
        let kind = match summary.kind {
            silicon_accounts_client::AccountKind::Carbon => ActorKind::Carbon,
            silicon_accounts_client::AccountKind::Silicon => ActorKind::Silicon,
        };
        let id = (!summary.id.is_empty()).then_some(summary.id.as_str());
        self.ensure(&uuid, kind, id, KeySource::Lookup)
            .await
            .map_err(|_| AccountsError::Unavailable)?;
        let custodian = summary
            .custodian
            .as_ref()
            .filter(|_| kind == ActorKind::Silicon)
            .filter(|custodian| AccountUuid::from_str(&custodian.uuid).is_ok());
        sqlx::query("UPDATE waveform_accounts a SET current_id = CASE WHEN a.id_changed_at >= $7 THEN a.current_id ELSE COALESCE($2, a.current_id) END, display_name = COALESCE(NULLIF($3, ''), a.display_name), pfp_url = COALESCE(NULLIF($4, ''), a.pfp_url), custodian_uuid = CASE WHEN a.kind <> 'silicon' THEN NULL WHEN a.custodian_changed_at >= $7 THEN a.custodian_uuid ELSE COALESCE($5, a.custodian_uuid) END, custodian_id = CASE WHEN a.kind <> 'silicon' THEN NULL WHEN a.custodian_changed_at >= $7 THEN a.custodian_id ELSE COALESCE((SELECT c.current_id FROM waveform_accounts c WHERE c.account_uuid = $5 AND c.id_changed_at >= $7), $6, a.custodian_id) END, custodian_checked_at = CASE WHEN a.kind = 'silicon' THEN now() ELSE a.custodian_checked_at END, updated_at = now() WHERE a.account_uuid = $1 AND a.deleted_at IS NULL")
            .bind(uuid.as_str())
            .bind(id)
            .bind(&summary.display_name)
            .bind(&summary.pfp_url)
            .bind(custodian.map(|custodian| custodian.uuid.as_str()))
            .bind(custodian.map(|custodian| custodian.id.as_str()))
            .bind(started - LOOKUP_SKEW)
            .execute(&self.pool)
            .await
            .map_err(|_| AccountsError::Unavailable)?;
        self.load(&uuid)
            .await
            .map_err(|_| AccountsError::Unavailable)?
            .ok_or(AccountsError::Unavailable)
    }

    /// Resolves what a caller typed (`c:…`/`si:…` id or a uuid) to an account,
    /// asking Accounts for ids (ids can change hands, so they are never trusted
    /// from the cache). `Ok(None)` when no such account exists.
    ///
    /// # Errors
    ///
    /// Accounts failures.
    pub async fn resolve_selector(
        &self,
        selector: &str,
    ) -> Result<Option<AccountRecord>, AccountsError> {
        if public_id_kind(selector).is_some() {
            let started = OffsetDateTime::now_utc();
            let Some(summary) = self.gateway.lookup_by_id(selector).await? else {
                return Ok(None);
            };
            if summary.status == "deleted" {
                return Ok(None);
            }
            return self.store_summary(&summary, started).await.map(Some);
        }
        let Ok(uuid) = AccountUuid::from_str(selector) else {
            return Ok(None);
        };
        match self
            .load(&uuid)
            .await
            .map_err(|_| AccountsError::Unavailable)?
        {
            Some(record) => Ok(Some(record)),
            None => self.refresh_from_accounts(&uuid).await,
        }
    }

    /// True when `carbon` is the custodian of `silicon`. A mismatch is confirmed
    /// with Accounts (at most once a minute per Silicon) before refusing, so a
    /// missed `silicon.custodian_changed` webhook cannot lock a new custodian out.
    ///
    /// # Errors
    ///
    /// Accounts failures while confirming.
    pub async fn is_custodian(
        &self,
        carbon: &AccountUuid,
        silicon: &AccountRecord,
    ) -> Result<bool, AccountsError> {
        if silicon.kind != ActorKind::Silicon || silicon.deleted_at.is_some() {
            return Ok(false);
        }
        if silicon
            .custodian
            .as_ref()
            .is_some_and(|(uuid, _)| uuid == carbon.as_str())
        {
            return Ok(true);
        }
        let recently_checked = silicon.custodian_checked_at.is_some_and(|checked| {
            OffsetDateTime::now_utc() - checked
                < time::Duration::try_from(CUSTODIAN_MISMATCH_RECHECK)
                    .unwrap_or(time::Duration::MAX)
        });
        if recently_checked {
            return Ok(false);
        }
        Ok(self
            .refresh_from_accounts(&silicon.uuid)
            .await?
            .and_then(|fresh| fresh.custodian)
            .is_some_and(|(uuid, _)| uuid == carbon.as_str()))
    }

    /// The Silicons Waveform knows that `carbon` is custodian of.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn silicons_of(
        &self,
        carbon: &AccountUuid,
    ) -> Result<Vec<AccountRecord>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT_RECORD} WHERE a.custodian_uuid = $1 AND a.kind = 'silicon' AND a.deleted_at IS NULL ORDER BY a.current_id NULLS LAST, k.account_uuid")))
            .bind(carbon.as_str())
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(record)
            .collect()
    }

    /// The custodian circle of an account (always includes the account itself).
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn circle(&self, account: &AccountRecord) -> Result<Vec<AccountUuid>, sqlx::Error> {
        let custodian = match account.kind {
            ActorKind::Carbon => Some(account.uuid.as_str().to_owned()),
            ActorKind::Silicon => account.custodian.as_ref().map(|(uuid, _)| uuid.clone()),
        };
        let mut members = vec![account.uuid.clone()];
        if let Some(custodian) = custodian {
            let rows: Vec<String> = sqlx::query_scalar("SELECT account_uuid FROM waveform_accounts WHERE custodian_uuid = $1 AND kind = 'silicon' AND deleted_at IS NULL ORDER BY account_uuid")
                .bind(&custodian)
                .fetch_all(&self.pool)
                .await?;
            for uuid in std::iter::once(custodian).chain(rows) {
                if let Ok(uuid) = AccountUuid::from_str(&uuid)
                    && !members.contains(&uuid)
                {
                    members.push(uuid);
                }
            }
        }
        Ok(members)
    }
}

fn record(row: &sqlx::postgres::PgRow) -> Result<AccountRecord, sqlx::Error> {
    let decode =
        |error: crate::domain::identity::IdentityError| sqlx::Error::Decode(Box::new(error));
    let uuid: String = row.try_get("account_uuid")?;
    let kind: String = row.try_get("kind")?;
    let custodian_uuid: Option<String> = row.try_get("custodian_uuid")?;
    let custodian_id: Option<String> = row.try_get("custodian_id")?;
    Ok(AccountRecord {
        uuid: AccountUuid::from_str(&uuid).map_err(decode)?,
        storage_key: row.try_get("storage_actor_id")?,
        kind: ActorKind::from_str(&kind).map_err(decode)?,
        current_id: row.try_get("current_id")?,
        display_name: row.try_get("display_name")?,
        pfp_url: row.try_get("pfp_url")?,
        custodian: custodian_uuid.map(|uuid| (uuid, custodian_id.unwrap_or_default())),
        custodian_checked_at: row.try_get("custodian_checked_at")?,
        revoked_before: row.try_get("revoked_before")?,
        revoked_reason: row.try_get("revoked_reason")?,
        deleted_at: row.try_get("deleted_at")?,
    })
}
