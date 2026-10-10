//! User verification proofs Waveform holds for accounts at Briefcase.
//!
//! The proof token (`sap_…`) and its rotating refresh token (`sapr_…`) are kept
//! encrypted. One account's proof is used, refreshed or replaced under a
//! per-account advisory lock, so concurrent requests never present the same
//! refresh token twice (a reused refresh token revokes the whole proof).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    accounts::{AccountsGateway, ProofError},
    vault::Vault,
};
use crate::{
    application::ports::{StorageAuthorityError, StorageAuthorityPort},
    domain::auth::{AccessToken, AuthorizedActor, BriefcaseProof, ProofToken},
};

/// Briefcase operations Waveform needs, as Briefcase's scope ids (the old OBO endpoint ids).
pub const BRIEFCASE_SCOPES: [&str; 4] = [
    "briefcase.uploads.reserve",
    "briefcase.uploads.commit",
    "briefcase.entries.list",
    "briefcase.files.read",
];

/// A proof token is replaced when it has less than this left.
const MINIMUM_REMAINING: time::Duration = time::Duration::seconds(60);

/// Why access ended when someone decided it: the account (`waveform storage revoke`, or at Silicon
/// Accounts) or its custodian. Speech never gives the access back by itself after one of these; the
/// account approves again (`waveform storage approve`). Every other end (the sign-in it stood on ended,
/// expiry, access removed and given again) is replaced on the account's next direct request.
const EXPLICIT_REVOCATIONS: [&str; 4] = [
    "revoked_by_account",
    "revoked_by_custodian",
    "revoked_at_accounts",
    "account_uuid_migrated",
];

fn is_explicit(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| EXPLICIT_REVOCATIONS.contains(&reason))
}

/// The newest proof row of an account, live or ended (no secrets).
struct Latest {
    proof_id: String,
    active: bool,
    reason: Option<String>,
}

/// What Waveform holds for one account at Briefcase (no secrets).
#[derive(Clone, Debug, serde::Serialize)]
pub struct StorageProofStatus {
    /// `active`, `revoked` or `missing`.
    pub status: &'static str,
    /// Briefcase's app id.
    pub receiving_app: String,
    /// Scopes the proof carries.
    pub scopes: Vec<String>,
    /// Proof id at Silicon Accounts (visible to the account at /v1/me/proofs).
    pub proof_id: Option<String>,
    /// When the current proof token expires.
    #[serde(with = "time::serde::rfc3339::option")]
    pub token_expires_at: Option<OffsetDateTime>,
    /// When the proof itself ends (its refresh token stops working).
    #[serde(with = "time::serde::rfc3339::option")]
    pub refresh_expires_at: Option<OffsetDateTime>,
    /// When it was last issued or refreshed.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    /// Why it was revoked, when it was.
    pub revoked_reason: Option<String>,
}

/// Encrypted Briefcase proof store backed by Silicon Accounts.
#[derive(Clone)]
pub struct StorageProofs {
    pool: PgPool,
    vault: Vault,
    gateway: Arc<AccountsGateway>,
    receiving_app: String,
}

impl StorageProofs {
    /// Builds the store for Briefcase's app id.
    #[must_use]
    pub const fn new(
        pool: PgPool,
        vault: Vault,
        gateway: Arc<AccountsGateway>,
        receiving_app: String,
    ) -> Self {
        Self {
            pool,
            vault,
            gateway,
            receiving_app,
        }
    }

    fn context(&self, storage_key: Uuid, part: &str) -> String {
        format!("storage-proof/{storage_key}/{}/{part}", self.receiving_app)
    }

    async fn lock(
        &self,
        storage_key: Uuid,
    ) -> Result<Transaction<'static, Postgres>, StorageAuthorityError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 71))")
            .bind(format!(
                "storage-proof:{storage_key}:{}",
                self.receiving_app
            ))
            .execute(&mut *transaction)
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        Ok(transaction)
    }

    /// Issues a new proof from the account's own Waveform token and stores it,
    /// replacing (and revoking) any previous one.
    ///
    /// # Errors
    ///
    /// [`StorageAuthorityError`] by category.
    pub async fn approve(
        &self,
        actor: &AuthorizedActor,
        subject_token: &AccessToken,
    ) -> Result<StorageProofStatus, StorageAuthorityError> {
        let storage_key = actor.actor.id.as_uuid();
        let mut transaction = self.lock(storage_key).await?;
        let previous = self.latest(&mut transaction, storage_key).await?;
        self.issue(
            &mut transaction,
            storage_key,
            subject_token,
            previous.as_ref().map(|latest| latest.proof_id.as_str()),
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        if let Some(previous) = previous.filter(|latest| latest.active) {
            self.revoke_remote_by_id(&previous.proof_id).await;
        }
        self.status(storage_key)
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)
    }

    /// Issues a proof from the account's token. The idempotency key names the
    /// proof being replaced, so a retry of this issuance gets the same proof while
    /// replacing a revoked or ended proof with the same token gets a new one
    /// (Silicon Accounts would otherwise answer the earlier, revoked proof again).
    async fn issue(
        &self,
        transaction: &mut Transaction<'static, Postgres>,
        storage_key: Uuid,
        subject_token: &AccessToken,
        replacing: Option<&str>,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        let subject_digest = Sha256::digest(subject_token.expose_secret().as_bytes());
        let key = format!(
            "waveform-briefcase-{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(
                [
                    storage_key.as_bytes().as_slice(),
                    &subject_digest,
                    replacing.unwrap_or("none").as_bytes(),
                ]
                .concat()
            ))
        );
        let issued = self
            .gateway
            .issue_user_verification(
                subject_token.expose_secret(),
                &self.receiving_app,
                &BRIEFCASE_SCOPES,
                &key,
            )
            .await
            .map_err(|error| map_proof_error(&error))?;
        self.save(transaction, storage_key, &issued).await
    }

    async fn latest(
        &self,
        transaction: &mut Transaction<'static, Postgres>,
        storage_key: Uuid,
    ) -> Result<Option<Latest>, StorageAuthorityError> {
        let row = sqlx::query("SELECT proof_id, invalidated_at IS NULL AS active, invalidated_reason FROM waveform_storage_proofs WHERE storage_actor_id=$1 AND receiving_app=$2")
            .bind(storage_key)
            .bind(&self.receiving_app)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        row.map(|row| {
            Ok(Latest {
                proof_id: row.try_get("proof_id")?,
                active: row.try_get("active")?,
                reason: row.try_get("invalidated_reason")?,
            })
        })
        .transpose()
        .map_err(|_: sqlx::Error| StorageAuthorityError::Unavailable)
    }

    /// Issues a replacement when the request carries the account's own token and
    /// the last proof did not end by someone's decision.
    async fn replace(
        &self,
        transaction: &mut Transaction<'static, Postgres>,
        storage_key: Uuid,
        subject_token: Option<&AccessToken>,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        let latest = self.latest(transaction, storage_key).await?;
        let explicit = latest
            .as_ref()
            .is_some_and(|latest| !latest.active && is_explicit(latest.reason.as_deref()));
        match subject_token {
            Some(subject_token) if !explicit => {
                self.issue(
                    transaction,
                    storage_key,
                    subject_token,
                    latest.as_ref().map(|latest| latest.proof_id.as_str()),
                )
                .await
            }
            _ => Err(StorageAuthorityError::NotAuthorized),
        }
    }

    async fn save(
        &self,
        transaction: &mut Transaction<'static, Postgres>,
        storage_key: Uuid,
        issued: &silicon_accounts_client::IssuedProof,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        let expires_at = issued
            .expires_at
            .ok_or(StorageAuthorityError::Unavailable)?;
        let token = ProofToken::new(issued.proof_token.expose().to_owned())
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let token_cipher = self
            .vault
            .seal(token.expose_secret(), &self.context(storage_key, "token"))
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let refresh_cipher = issued
            .proof_refresh_token
            .as_ref()
            .map(|refresh| {
                self.vault
                    .seal(refresh.expose(), &self.context(storage_key, "refresh"))
                    .map_err(|_| StorageAuthorityError::Unavailable)
            })
            .transpose()?;
        sqlx::query("INSERT INTO waveform_storage_proofs(storage_actor_id, receiving_app, proof_id, scopes, token_cipher, token_expires_at, refresh_cipher, refresh_expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (storage_actor_id, receiving_app) DO UPDATE SET proof_id=EXCLUDED.proof_id, scopes=EXCLUDED.scopes, token_cipher=EXCLUDED.token_cipher, token_expires_at=EXCLUDED.token_expires_at, refresh_cipher=EXCLUDED.refresh_cipher, refresh_expires_at=EXCLUDED.refresh_expires_at, invalidated_at=NULL, invalidated_reason=NULL, updated_at=now()")
            .bind(storage_key)
            .bind(&self.receiving_app)
            .bind(&issued.proof_id)
            .bind(&issued.scopes)
            .bind(token_cipher)
            .bind(expires_at)
            .bind(refresh_cipher)
            .bind(issued.refresh_expires_at)
            .execute(&mut **transaction)
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        Ok(BriefcaseProof { token, expires_at })
    }

    /// The account's Briefcase access, without secrets.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn status(&self, storage_key: Uuid) -> Result<StorageProofStatus, sqlx::Error> {
        let row = sqlx::query("SELECT proof_id, scopes, token_expires_at, refresh_expires_at, updated_at, invalidated_at, invalidated_reason FROM waveform_storage_proofs WHERE storage_actor_id=$1 AND receiving_app=$2")
            .bind(storage_key)
            .bind(&self.receiving_app)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(StorageProofStatus {
                status: "missing",
                receiving_app: self.receiving_app.clone(),
                scopes: Vec::new(),
                proof_id: None,
                token_expires_at: None,
                refresh_expires_at: None,
                updated_at: None,
                revoked_reason: None,
            });
        };
        let invalidated: Option<OffsetDateTime> = row.try_get("invalidated_at")?;
        let refresh_expires_at: Option<OffsetDateTime> = row.try_get("refresh_expires_at")?;
        let ended = refresh_expires_at.is_some_and(|end| end <= OffsetDateTime::now_utc());
        Ok(StorageProofStatus {
            status: if invalidated.is_some() || ended {
                "revoked"
            } else {
                "active"
            },
            receiving_app: self.receiving_app.clone(),
            scopes: row.try_get("scopes")?,
            proof_id: row.try_get("proof_id")?,
            token_expires_at: row.try_get("token_expires_at")?,
            refresh_expires_at,
            updated_at: row.try_get("updated_at")?,
            revoked_reason: if ended && invalidated.is_none() {
                Some("expired".to_owned())
            } else {
                row.try_get("invalidated_reason")?
            },
        })
    }

    /// Ends Waveform's Briefcase access for an account: revokes the proof at
    /// Accounts (best effort) and wipes the stored tokens, keeping why.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn revoke(&self, storage_key: Uuid, reason: &str) -> Result<bool, sqlx::Error> {
        let proof_id: Option<String> = sqlx::query_scalar("UPDATE waveform_storage_proofs SET token_cipher=''::bytea, refresh_cipher=NULL, invalidated_at=now(), invalidated_reason=$3, updated_at=now() WHERE storage_actor_id=$1 AND receiving_app=$2 AND invalidated_at IS NULL RETURNING proof_id")
            .bind(storage_key)
            .bind(&self.receiving_app)
            .bind(reason)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(proof_id) = &proof_id {
            self.revoke_remote_by_id(proof_id).await;
        }
        Ok(proof_id.is_some())
    }

    /// Revokes a proof at Silicon Accounts, best effort (a failure is logged; the
    /// proof also ends with the account's sign-in).
    pub async fn revoke_remote_by_id(&self, proof_id: &str) {
        if self.gateway.revoke_proof(proof_id).await.is_err() {
            tracing::warn!(
                "could not revoke a Briefcase proof at Silicon Accounts; it ends with the account's sign-in"
            );
        }
    }

    async fn stored(
        &self,
        transaction: &mut Transaction<'static, Postgres>,
        storage_key: Uuid,
    ) -> Result<Option<BriefcaseProof>, StorageAuthorityError> {
        let row = sqlx::query("SELECT token_cipher, token_expires_at, refresh_cipher, refresh_expires_at FROM waveform_storage_proofs WHERE storage_actor_id=$1 AND receiving_app=$2 AND invalidated_at IS NULL")
            .bind(storage_key)
            .bind(&self.receiving_app)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let Some(row) = row else { return Ok(None) };
        let now = OffsetDateTime::now_utc();
        let token_expires_at: OffsetDateTime = row
            .try_get("token_expires_at")
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        if token_expires_at - now > MINIMUM_REMAINING {
            let cipher: Vec<u8> = row
                .try_get("token_cipher")
                .map_err(|_| StorageAuthorityError::Unavailable)?;
            let token = self
                .vault
                .open(&cipher, &self.context(storage_key, "token"))
                .map_err(|_| StorageAuthorityError::Unavailable)?;
            return Ok(Some(BriefcaseProof {
                token: ProofToken::new(token.expose_secret().to_owned())
                    .map_err(|_| StorageAuthorityError::Unavailable)?,
                expires_at: token_expires_at,
            }));
        }
        let refresh_cipher: Option<Vec<u8>> = row
            .try_get("refresh_cipher")
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let refresh_expires_at: Option<OffsetDateTime> = row
            .try_get("refresh_expires_at")
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let (Some(refresh_cipher), true) = (
            refresh_cipher,
            refresh_expires_at.is_none_or(|end| end > now),
        ) else {
            self.invalidate(transaction, storage_key, "expired").await?;
            return Ok(None);
        };
        let refresh = self
            .vault
            .open(&refresh_cipher, &self.context(storage_key, "refresh"))
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        // The same refresh token always yields the same key, so a retry after a
        // lost answer returns the same rotated pair instead of tripping reuse.
        let key = format!(
            "waveform-refresh-{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(refresh.expose_secret().as_bytes()))
        );
        match self
            .gateway
            .refresh_proof(refresh.expose_secret(), &key)
            .await
        {
            Ok(issued) => self.save(transaction, storage_key, &issued).await.map(Some),
            Err(error @ ProofError::Refused { .. }) => {
                let reason = if error.is_explicit_revocation() {
                    "revoked_at_accounts"
                } else {
                    "refresh_refused"
                };
                self.invalidate(transaction, storage_key, reason).await?;
                Ok(None)
            }
            Err(error) => Err(map_proof_error(&error)),
        }
    }

    /// See [`StorageAuthorityPort::renew_refused_proof`].
    async fn renew(
        &self,
        transaction: &mut Transaction<'static, Postgres>,
        storage_key: Uuid,
        subject_token: Option<&AccessToken>,
        refused: &BriefcaseProof,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        let row = sqlx::query("SELECT token_cipher, token_expires_at, refresh_cipher, refresh_expires_at FROM waveform_storage_proofs WHERE storage_actor_id=$1 AND receiving_app=$2 AND invalidated_at IS NULL")
            .bind(storage_key)
            .bind(&self.receiving_app)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let Some(row) = row else {
            return self.replace(transaction, storage_key, subject_token).await;
        };
        let now = OffsetDateTime::now_utc();
        let token_expires_at: OffsetDateTime = row
            .try_get("token_expires_at")
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let cipher: Vec<u8> = row
            .try_get("token_cipher")
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let token = self
            .vault
            .open(&cipher, &self.context(storage_key, "token"))
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        if token.expose_secret() != refused.token.expose_secret()
            && token_expires_at - now > MINIMUM_REMAINING
        {
            // Another request already put a new proof in its place.
            return Ok(BriefcaseProof {
                token: ProofToken::new(token.expose_secret().to_owned())
                    .map_err(|_| StorageAuthorityError::Unavailable)?,
                expires_at: token_expires_at,
            });
        }
        let refresh_cipher: Option<Vec<u8>> = row
            .try_get("refresh_cipher")
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        let refresh_expires_at: Option<OffsetDateTime> = row
            .try_get("refresh_expires_at")
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        if let (Some(refresh_cipher), true) = (
            refresh_cipher,
            refresh_expires_at.is_none_or(|end| end > now),
        ) {
            let refresh = self
                .vault
                .open(&refresh_cipher, &self.context(storage_key, "refresh"))
                .map_err(|_| StorageAuthorityError::Unavailable)?;
            let key = format!(
                "waveform-refresh-{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(Sha256::digest(refresh.expose_secret().as_bytes()))
            );
            match self
                .gateway
                .refresh_proof(refresh.expose_secret(), &key)
                .await
            {
                // Still alive: present a fresh token.
                Ok(issued) => return self.save(transaction, storage_key, &issued).await,
                Err(error) if error.is_explicit_revocation() => {
                    self.invalidate(transaction, storage_key, "revoked_at_accounts")
                        .await?;
                    return Err(StorageAuthorityError::NotAuthorized);
                }
                // Ended with its sign-in, expired, or its refresh token was spent: replace it.
                Err(ProofError::Refused { .. }) => {}
                Err(error) => return Err(map_proof_error(&error)),
            }
        }
        self.invalidate(transaction, storage_key, "proof_ended")
            .await?;
        self.replace(transaction, storage_key, subject_token).await
    }

    async fn invalidate(
        &self,
        transaction: &mut Transaction<'static, Postgres>,
        storage_key: Uuid,
        reason: &str,
    ) -> Result<(), StorageAuthorityError> {
        sqlx::query("UPDATE waveform_storage_proofs SET token_cipher=''::bytea, refresh_cipher=NULL, invalidated_at=now(), invalidated_reason=$3, updated_at=now() WHERE storage_actor_id=$1 AND receiving_app=$2 AND invalidated_at IS NULL")
            .bind(storage_key)
            .bind(&self.receiving_app)
            .bind(reason)
            .execute(&mut **transaction)
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        Ok(())
    }
}

#[async_trait]
impl StorageAuthorityPort for StorageProofs {
    async fn briefcase_proof(
        &self,
        actor: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        let storage_key = actor.actor.id.as_uuid();
        let mut transaction = self.lock(storage_key).await?;
        let proof = match self.stored(&mut transaction, storage_key).await? {
            Some(proof) => Ok(proof),
            None => {
                self.replace(&mut transaction, storage_key, subject_token)
                    .await
            }
        };
        // Commit what was learnt (an invalidation) even when no proof came out of it.
        transaction
            .commit()
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        proof
    }

    async fn renew_refused_proof(
        &self,
        actor: &AuthorizedActor,
        subject_token: Option<&AccessToken>,
        refused: &BriefcaseProof,
    ) -> Result<BriefcaseProof, StorageAuthorityError> {
        let storage_key = actor.actor.id.as_uuid();
        let mut transaction = self.lock(storage_key).await?;
        let renewed = self
            .renew(&mut transaction, storage_key, subject_token, refused)
            .await;
        transaction
            .commit()
            .await
            .map_err(|_| StorageAuthorityError::Unavailable)?;
        renewed
    }
}

const fn map_proof_error(error: &ProofError) -> StorageAuthorityError {
    match error {
        ProofError::Refused { .. } => StorageAuthorityError::Refused,
        ProofError::Unavailable => StorageAuthorityError::Unavailable,
        ProofError::Timeout => StorageAuthorityError::Timeout,
    }
}
