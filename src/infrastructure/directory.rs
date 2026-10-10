//! Waveform's cache of Silicon Accounts accounts: the uuid ↔ private storage key
//! mapping, display fields, a Silicon's custodian, revocation markers, and the
//! custodian circle used by authorisation.
//!
//! circle(Carbon C) = C and every Silicon C is custodian of.
//! circle(Silicon S) = S, its custodian, and the custodian's other Silicons.
//!
//! The cache never outranks Silicon Accounts: a public id follows the newest
//! verified credential, webhook or lookup; a cached custodian stands for reads
//! only while Silicon Accounts confirmed it recently, and writes ask again.

use std::{
    collections::HashMap,
    str::FromStr as _,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;
use uuid::Uuid;

use super::accounts::{AccountsGateway, Lookup, LookupPurpose};
use crate::{
    application::ports::AccountsError,
    domain::identity::{AccountUuid, ActorKind, public_id_kind},
};

/// A cached custodian stands for reads this long after Silicon Accounts confirmed
/// it; a Silicon's own requests also re-check it in the background this often.
pub const CUSTODIAN_RECHECK: Duration = Duration::from_secs(600);
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
/// One caller may make Waveform look up at most this many accounts it named, per minute.
const SELECTOR_LOOKUPS_PER_CALLER: u32 = 30;
/// An id or uuid Silicon Accounts said no account has is answered from memory this long.
const MISSING_TTL: Duration = Duration::from_secs(60);
/// `GET /api/v1/silicons` confirms at most this many stale custodians at once.
const LIST_REFRESH_CONCURRENCY: usize = 8;
/// In-memory throttles are pruned, then cleared, rather than grown past this size.
const MEMORY_LIMIT: usize = 10_000;
const MINUTE: Duration = Duration::from_secs(60);

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

/// How sure a custodian check must be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Confirm {
    /// Reads: a custodian Silicon Accounts confirmed within [`CUSTODIAN_RECHECK`] stands.
    Recent,
    /// Writes: always ask Silicon Accounts.
    Always,
}

/// The result of a Carbon naming an account to act on as its custodian.
#[derive(Clone, Debug)]
pub enum Selection {
    /// The caller is the account's custodian (confirmed as the [`Confirm`] asked).
    Custodian(Box<AccountRecord>),
    /// The account exists, but the caller is not its custodian.
    NotCustodian,
    /// No account has this id or uuid (or it was deleted).
    NotFound,
    /// The caller made Waveform look up too many accounts this minute.
    Throttled,
}

/// What a fresh lookup of an account found.
#[derive(Clone, Debug)]
pub enum Refreshed {
    /// The account, as Silicon Accounts describes it now.
    Account(Box<AccountRecord>),
    /// Silicon Accounts says the account was deleted.
    Deleted,
    /// Silicon Accounts does not know the uuid.
    Unknown,
}

/// The Silicons a Carbon looks after, as far as Waveform could confirm.
#[derive(Clone, Debug)]
pub struct SiliconList {
    /// Silicons whose custodian Silicon Accounts confirmed recently.
    pub items: Vec<AccountRecord>,
    /// False when some cached Silicons could not be confirmed (and were left out).
    pub complete: bool,
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

impl AccountRecord {
    fn custodian_is(&self, carbon: &AccountUuid) -> bool {
        self.kind == ActorKind::Silicon
            && self.deleted_at.is_none()
            && self
                .custodian
                .as_ref()
                .is_some_and(|(uuid, _)| uuid == carbon.as_str())
    }

    /// True when Silicon Accounts confirmed the custodian less than `limit` ago.
    fn custodian_checked_within(&self, limit: Duration) -> bool {
        let limit = time::Duration::try_from(limit).unwrap_or(time::Duration::MAX);
        self.custodian_checked_at
            .is_some_and(|checked| OffsetDateTime::now_utc() - checked < limit)
    }
}

/// Postgres-backed account cache fed by tokens, proofs, lookups and webhooks.
#[derive(Clone)]
pub struct AccountDirectory {
    pool: PgPool,
    gateway: Arc<AccountsGateway>,
    first_lookup_failures: Arc<Mutex<HashMap<String, Instant>>>,
    profile_checks: Arc<Mutex<HashMap<String, Instant>>>,
    selector_lookups: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
    missing: Arc<Mutex<HashMap<String, Instant>>>,
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
            selector_lookups: Arc::new(Mutex::new(HashMap::new())),
            missing: Arc::new(Mutex::new(HashMap::new())),
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
    /// its storage key on first sight. `id_hint` is the public id the credential
    /// carried and `as_of` when that was true (a token's `iat`, or now for a proof
    /// Silicon Accounts just verified): it replaces the cached id unless Waveform
    /// learnt a newer one since. The first time Waveform sees a Silicon, the
    /// request waits (briefly) for its custodian lookup so the answer already
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
        as_of: Option<OffsetDateTime>,
        source: KeySource,
    ) -> Result<AccountRecord, sqlx::Error> {
        let id_hint = id_hint.filter(|id| public_id_kind(id) == Some(kind));
        let mut record = match self.load(uuid).await? {
            // Another kind of account's record is left untouched; the caller refuses it.
            Some(record) if record.kind != kind => return Ok(record),
            Some(record) if record.current_id.is_some() || id_hint.is_none() => record,
            _ => self.ensure(uuid, kind, id_hint, as_of, source).await?,
        };
        if let (Some(id), Some(as_of)) = (id_hint, as_of)
            && record.deleted_at.is_none()
            && record.current_id.as_deref() != Some(id)
            && let Some(adopted) = self.adopt_id(uuid, id, as_of).await?
        {
            record = adopted;
        }
        if record.kind != ActorKind::Silicon || record.deleted_at.is_some() {
            return Ok(record);
        }
        if record.custodian_checked_at.is_none() {
            if !self.may_wait_for_first_lookup(uuid) {
                self.refresh_in_background(uuid);
                return Ok(record);
            }
            match tokio::time::timeout(FIRST_LOOKUP_WAIT, self.refresh_from_accounts(uuid)).await {
                Ok(Ok(Refreshed::Account(fresh))) => return Ok(*fresh),
                Ok(Ok(Refreshed::Deleted | Refreshed::Unknown)) => {}
                Ok(Err(_)) | Err(_) => {
                    tracing::debug!(
                        "custodian lookup failed; later requests look it up in the background"
                    );
                    self.first_lookup_failed(uuid);
                }
            }
        } else if !record.custodian_checked_within(CUSTODIAN_RECHECK) {
            self.refresh_in_background(uuid);
        }
        Ok(record)
    }

    /// Replaces the cached public id with one known to be right as of `as_of`,
    /// unless a newer id arrived since (a webhook, a newer token or a lookup).
    /// The account's Silicons show their custodian's new id too.
    async fn adopt_id(
        &self,
        uuid: &AccountUuid,
        id: &str,
        as_of: OffsetDateTime,
    ) -> Result<Option<AccountRecord>, sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        let adopted = sqlx::query("UPDATE waveform_accounts SET current_id = $2, id_changed_at = $3, updated_at = now() WHERE account_uuid = $1 AND deleted_at IS NULL AND current_id IS DISTINCT FROM $2 AND (id_changed_at IS NULL OR id_changed_at < $3)")
            .bind(uuid.as_str())
            .bind(id)
            .bind(as_of)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        if adopted {
            sqlx::query("UPDATE waveform_accounts SET custodian_id = $2, updated_at = now() WHERE custodian_uuid = $1 AND custodian_id IS DISTINCT FROM $2")
                .bind(uuid.as_str())
                .bind(id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        if adopted {
            self.load(uuid).await
        } else {
            Ok(None)
        }
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
    /// a missing public id from `id_hint` (known to be right as of `as_of`).
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn ensure(
        &self,
        uuid: &AccountUuid,
        kind: ActorKind,
        id_hint: Option<&str>,
        as_of: Option<OffsetDateTime>,
        source: KeySource,
    ) -> Result<AccountRecord, sqlx::Error> {
        let id_hint = id_hint.filter(|id| public_id_kind(id) == Some(kind));
        let as_of = as_of.filter(|_| id_hint.is_some());
        let mut transaction = self.pool.begin().await?;
        sqlx::query("INSERT INTO waveform_account_keys(account_uuid, storage_actor_id, kind, source) VALUES($1, $2, $3, $4) ON CONFLICT (account_uuid) DO NOTHING")
            .bind(uuid.as_str())
            .bind(Uuid::now_v7())
            .bind(kind.as_str())
            .bind(source.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO waveform_accounts(account_uuid, kind, current_id, id_changed_at) SELECT account_uuid, kind, $2, $3 FROM waveform_account_keys WHERE account_uuid = $1 ON CONFLICT (account_uuid) DO UPDATE SET current_id = EXCLUDED.current_id, id_changed_at = COALESCE(EXCLUDED.id_changed_at, waveform_accounts.id_changed_at), updated_at = now() WHERE waveform_accounts.current_id IS NULL AND waveform_accounts.deleted_at IS NULL AND EXCLUDED.current_id IS NOT NULL")
            .bind(uuid.as_str())
            .bind(id_hint)
            .bind(as_of)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        self.load(uuid).await?.ok_or(sqlx::Error::RowNotFound)
    }

    /// Re-reads one account from Accounts (kind, id, custodian) and caches it.
    ///
    /// # Errors
    ///
    /// Accounts or database failures (database failures map to `Unavailable`).
    pub async fn refresh_from_accounts(
        &self,
        uuid: &AccountUuid,
    ) -> Result<Refreshed, AccountsError> {
        let started = OffsetDateTime::now_utc();
        match self
            .gateway
            .lookup(uuid.as_str(), LookupPurpose::Authorization)
            .await?
        {
            Lookup::Found(summary) => self
                .store_summary(&summary, started)
                .await
                .map(|record| Refreshed::Account(Box::new(record))),
            Lookup::Deleted => Ok(Refreshed::Deleted),
            Lookup::NotFound => {
                tracing::warn!(
                    "Silicon Accounts does not know an account Waveform has cached; check that ACCOUNTS_API_URL is the deployment that issues Waveform's tokens"
                );
                Ok(Refreshed::Unknown)
            }
        }
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
        self.ensure(&uuid, kind, id, Some(started), KeySource::Lookup)
            .await
            .map_err(|_| AccountsError::Unavailable)?;
        let custodian = summary
            .custodian
            .as_ref()
            .filter(|_| kind == ActorKind::Silicon)
            .filter(|custodian| AccountUuid::from_str(&custodian.uuid).is_ok());
        sqlx::query("UPDATE waveform_accounts a SET current_id = CASE WHEN a.id_changed_at >= $7 THEN a.current_id ELSE COALESCE($2, a.current_id) END, id_changed_at = CASE WHEN a.id_changed_at >= $7 OR $2::text IS NULL THEN a.id_changed_at ELSE $8 END, display_name = COALESCE(NULLIF($3, ''), a.display_name), pfp_url = COALESCE(NULLIF($4, ''), a.pfp_url), custodian_uuid = CASE WHEN a.kind <> 'silicon' THEN NULL WHEN a.custodian_changed_at >= $7 THEN a.custodian_uuid ELSE COALESCE($5, a.custodian_uuid) END, custodian_id = CASE WHEN a.kind <> 'silicon' THEN NULL WHEN a.custodian_changed_at >= $7 THEN a.custodian_id ELSE COALESCE((SELECT c.current_id FROM waveform_accounts c WHERE c.account_uuid = $5 AND c.id_changed_at >= $7), $6, a.custodian_id) END, custodian_checked_at = CASE WHEN a.kind = 'silicon' THEN now() ELSE a.custodian_checked_at END, updated_at = now() WHERE a.account_uuid = $1 AND a.deleted_at IS NULL")
            .bind(uuid.as_str())
            .bind(id)
            .bind(&summary.display_name)
            .bind(&summary.pfp_url)
            .bind(custodian.map(|custodian| custodian.uuid.as_str()))
            .bind(custodian.map(|custodian| custodian.id.as_str()))
            .bind(started - LOOKUP_SKEW)
            .bind(started)
            .execute(&self.pool)
            .await
            .map_err(|_| AccountsError::Unavailable)?;
        self.load(&uuid)
            .await
            .map_err(|_| AccountsError::Unavailable)?
            .ok_or(AccountsError::Unavailable)
    }

    /// Resolves what a Carbon typed (`c:…`/`si:…` id or a uuid) to an account it
    /// is the custodian of. Ids are always resolved by Silicon Accounts (they can
    /// change hands); a cached uuid is checked as `confirm` asks. An account the
    /// caller merely named, and is not the custodian of, is never cached.
    ///
    /// # Errors
    ///
    /// Accounts failures (including an exhausted lookup budget).
    pub async fn select_for_custodian(
        &self,
        carbon: &AccountUuid,
        selector: &str,
        confirm: Confirm,
    ) -> Result<Selection, AccountsError> {
        if public_id_kind(selector).is_some() {
            return self.select_by_lookup(carbon, selector).await;
        }
        let Ok(uuid) = AccountUuid::from_str(selector) else {
            return Ok(Selection::NotFound);
        };
        match self
            .load(&uuid)
            .await
            .map_err(|_| AccountsError::Unavailable)?
        {
            Some(record) => self.custody(carbon, record, confirm).await,
            None => self.select_by_lookup(carbon, selector).await,
        }
    }

    /// The custodian check for a cached account: a match Silicon Accounts
    /// confirmed recently stands for reads; anything else asks Silicon Accounts
    /// (a mismatch at most once a minute per Silicon, so a missed
    /// `silicon.custodian_changed` cannot lock a new custodian out).
    async fn custody(
        &self,
        carbon: &AccountUuid,
        record: AccountRecord,
        confirm: Confirm,
    ) -> Result<Selection, AccountsError> {
        if record.deleted_at.is_some() {
            return Ok(Selection::NotFound);
        }
        if record.kind != ActorKind::Silicon {
            return Ok(Selection::NotCustodian);
        }
        if record.custodian_is(carbon) {
            if confirm == Confirm::Recent && record.custodian_checked_within(CUSTODIAN_RECHECK) {
                return Ok(Selection::Custodian(Box::new(record)));
            }
        } else if record.custodian_checked_within(CUSTODIAN_MISMATCH_RECHECK) {
            return Ok(Selection::NotCustodian);
        }
        Ok(match self.refresh_from_accounts(&record.uuid).await? {
            Refreshed::Account(fresh) if fresh.custodian_is(carbon) => Selection::Custodian(fresh),
            Refreshed::Account(_) | Refreshed::Unknown => Selection::NotCustodian,
            Refreshed::Deleted => Selection::NotFound,
        })
    }

    /// Asks Silicon Accounts about an account Waveform must not answer from its
    /// cache, within the caller's per-minute allowance.
    async fn select_by_lookup(
        &self,
        carbon: &AccountUuid,
        selector: &str,
    ) -> Result<Selection, AccountsError> {
        let by_id = public_id_kind(selector).is_some();
        let key = if by_id {
            selector.to_ascii_lowercase()
        } else {
            selector.to_owned()
        };
        if self.recently_missing(&key) {
            return Ok(Selection::NotFound);
        }
        if !self.may_look_up_for(carbon) {
            return Ok(Selection::Throttled);
        }
        let started = OffsetDateTime::now_utc();
        let answer = if by_id {
            self.gateway
                .lookup_by_id(selector, LookupPurpose::Selector)
                .await?
        } else {
            self.gateway
                .lookup(selector, LookupPurpose::Selector)
                .await?
        };
        let summary = match answer {
            Lookup::Found(summary) => summary,
            Lookup::NotFound | Lookup::Deleted => {
                self.remember_missing(key);
                return Ok(Selection::NotFound);
            }
        };
        let Ok(uuid) = AccountUuid::from_str(&summary.uuid) else {
            return Ok(Selection::NotFound);
        };
        let names_caller = summary.kind == silicon_accounts_client::AccountKind::Silicon
            && summary
                .custodian
                .as_ref()
                .is_some_and(|custodian| custodian.uuid == carbon.as_str());
        let known = self
            .load(&uuid)
            .await
            .map_err(|_| AccountsError::Unavailable)?
            .is_some();
        if !names_caller && !known {
            return Ok(Selection::NotCustodian);
        }
        // What is stored, not the raw answer, decides: a webhook newer than this
        // lookup wins over it.
        let record = self.store_summary(&summary, started).await?;
        Ok(if record.custodian_is(carbon) {
            Selection::Custodian(Box::new(record))
        } else {
            Selection::NotCustodian
        })
    }

    fn may_look_up_for(&self, caller: &AccountUuid) -> bool {
        let Ok(mut counts) = self.selector_lookups.lock() else {
            return false;
        };
        if counts.len() >= MEMORY_LIMIT {
            counts.retain(|_, (started, _)| started.elapsed() < MINUTE);
            if counts.len() >= MEMORY_LIMIT {
                counts.clear();
            }
        }
        let entry = counts
            .entry(caller.as_str().to_owned())
            .or_insert_with(|| (Instant::now(), 0));
        if entry.0.elapsed() >= MINUTE {
            *entry = (Instant::now(), 0);
        }
        if entry.1 >= SELECTOR_LOOKUPS_PER_CALLER {
            return false;
        }
        entry.1 += 1;
        true
    }

    fn recently_missing(&self, key: &str) -> bool {
        self.missing.lock().is_ok_and(|missing| {
            missing
                .get(key)
                .is_some_and(|noted| noted.elapsed() < MISSING_TTL)
        })
    }

    fn remember_missing(&self, key: String) {
        if let Ok(mut missing) = self.missing.lock() {
            if missing.len() >= MEMORY_LIMIT {
                missing.retain(|_, noted| noted.elapsed() < MISSING_TTL);
                if missing.len() >= MEMORY_LIMIT {
                    missing.clear();
                }
            }
            missing.insert(key, Instant::now());
        }
    }

    /// The Silicons `carbon` looks after, as cached: each one Silicon Accounts did
    /// not confirm within [`CUSTODIAN_RECHECK`] is asked about again (a few at a
    /// time), and left out when it is no longer the caller's or cannot be asked.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn silicons_of(&self, carbon: &AccountUuid) -> Result<SiliconList, sqlx::Error> {
        let cached = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT_RECORD} WHERE a.custodian_uuid = $1 AND a.kind = 'silicon' AND a.deleted_at IS NULL")))
            .bind(carbon.as_str())
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(record)
            .collect::<Result<Vec<_>, _>>()?;
        let (mut items, stale): (Vec<_>, Vec<_>) = cached
            .into_iter()
            .partition(|silicon| silicon.custodian_checked_within(CUSTODIAN_RECHECK));
        let mut complete = true;
        let confirmed = futures::stream::iter(stale)
            .map(|silicon| async move { self.refresh_from_accounts(&silicon.uuid).await })
            .buffer_unordered(LIST_REFRESH_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        for answer in confirmed {
            match answer {
                Ok(Refreshed::Account(fresh)) if fresh.custodian_is(carbon) => items.push(*fresh),
                Ok(_) => {}
                Err(_) => complete = false,
            }
        }
        items.sort_by(|left, right| {
            (
                left.current_id.is_none(),
                &left.current_id,
                left.uuid.as_str(),
            )
                .cmp(&(
                    right.current_id.is_none(),
                    &right.current_id,
                    right.uuid.as_str(),
                ))
        });
        Ok(SiliconList { items, complete })
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
