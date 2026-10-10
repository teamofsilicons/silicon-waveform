//! Silicon Accounts authentication for every Waveform route.
//!
//! - `Authorization: Bearer <JWT>`: verified locally (`EdDSA`, `aud` = Waveform,
//!   `iss` = Accounts), then refused if the account's sign-in ended after the
//!   token was issued (webhook revocation markers) or the account was deleted.
//! - `Authorization: Proof sap_…`: verified online with Waveform's credentials;
//!   it must be a valid User verification proof for Waveform carrying the
//!   route's scope, from an app allowed for that scope. Waveform then acts as
//!   the proof's account and records the issuing app.

use std::{str::FromStr as _, sync::Arc};

use async_trait::async_trait;
use silicon_accounts_client::{AccountKind, ProofKind, ProofVerification};
use time::OffsetDateTime;

use super::{
    accounts::AccountsGateway,
    directory::{AccountDirectory, AccountRecord, KeySource},
};
use crate::{
    application::ports::{AccountsError, AccountsPort},
    config::ProofIssuers,
    domain::{
        auth::{AuthFailure, AuthorizationRequest, AuthorizedActor, InboundCredentials},
        identity::{
            AccountRef, AccountUuid, Actor, ActorId, ActorKind, ApplicationId, public_id_kind,
        },
    },
};

/// Verifies credentials and resolves accounts to Waveform's private keys.
#[derive(Clone)]
pub struct AccountsAuthenticator {
    gateway: Arc<AccountsGateway>,
    directory: AccountDirectory,
    proof_issuers: ProofIssuers,
}

impl AccountsAuthenticator {
    /// Wires the gateway, the account directory and the proof allow-list.
    #[must_use]
    pub const fn new(
        gateway: Arc<AccountsGateway>,
        directory: AccountDirectory,
        proof_issuers: ProofIssuers,
    ) -> Self {
        Self {
            gateway,
            directory,
            proof_issuers,
        }
    }

    /// The account directory behind this authenticator.
    #[must_use]
    pub const fn directory(&self) -> &AccountDirectory {
        &self.directory
    }

    /// The Accounts gateway behind this authenticator.
    #[must_use]
    pub const fn gateway(&self) -> &Arc<AccountsGateway> {
        &self.gateway
    }

    async fn bearer(&self, token: &str) -> Result<AuthorizedActor, AccountsError> {
        let claims = self.gateway.verify_access_token(token).await?;
        let uuid = AccountUuid::from_str(&claims.sub)
            .map_err(|_| AccountsError::Rejected(AuthFailure::InvalidToken))?;
        self.require_current_uuid(&uuid).await?;
        let kind = match claims.kind {
            Some(AccountKind::Carbon) => ActorKind::Carbon,
            Some(AccountKind::Silicon) => ActorKind::Silicon,
            None => claims
                .id
                .as_deref()
                .and_then(public_id_kind)
                .ok_or(AccountsError::Rejected(AuthFailure::InvalidToken))?,
        };
        let issued_at = claims
            .iat
            .and_then(|iat| OffsetDateTime::from_unix_timestamp(iat).ok());
        // The token's id was the account's id when it was issued.
        let record = self
            .directory
            .sighting(
                &uuid,
                kind,
                claims.id.as_deref(),
                issued_at,
                KeySource::SignIn,
            )
            .await
            .map_err(|_| AccountsError::Unavailable)?;
        if record.deleted_at.is_some() {
            return Err(AccountsError::Rejected(AuthFailure::AccountDeleted));
        }
        if let Some(revoked_before) = record.revoked_before
            && issued_at.is_none_or(|issued_at| issued_at < revoked_before)
        {
            return Err(AccountsError::Rejected(AuthFailure::SignInEnded));
        }
        same_kind(&record, kind)?;
        let mut actor = authorized(
            &record,
            claims.id.clone(),
            None,
            OffsetDateTime::from_unix_timestamp(claims.exp).ok(),
        )?;
        actor.scopes = claims.scopes().into_iter().map(str::to_owned).collect();
        actor.token_family = claims.fid;
        Ok(actor)
    }

    async fn proof(
        &self,
        token: &str,
        scope: &'static str,
    ) -> Result<AuthorizedActor, AccountsError> {
        let ProofVerification::Valid(proof) = self.gateway.verify_proof(token).await? else {
            return Err(AccountsError::Rejected(AuthFailure::InvalidProof));
        };
        if proof.kind != ProofKind::UserVerification {
            return Err(AccountsError::Rejected(AuthFailure::ProofWithoutAccount));
        }
        if proof.receiving_app.app_id != self.gateway.app_id() {
            return Err(AccountsError::Rejected(AuthFailure::InvalidProof));
        }
        if !proof.scopes.iter().any(|granted| granted == scope) {
            return Err(AccountsError::Rejected(AuthFailure::ProofScopeMissing));
        }
        if !self.proof_issuers.allows(scope, &proof.issuing_app.app_id) {
            return Err(AccountsError::Rejected(AuthFailure::ProofIssuerNotAllowed));
        }
        let issuing_app = ApplicationId::from_str(&proof.issuing_app.app_id)
            .map_err(|_| AccountsError::Rejected(AuthFailure::InvalidProof))?;
        let user = proof
            .user
            .as_ref()
            .ok_or(AccountsError::Rejected(AuthFailure::ProofWithoutAccount))?;
        let uuid = AccountUuid::from_str(&user.uuid)
            .map_err(|_| AccountsError::Rejected(AuthFailure::InvalidProof))?;
        self.require_current_uuid(&uuid).await?;
        let kind = match user.kind {
            Some(AccountKind::Carbon) => ActorKind::Carbon,
            Some(AccountKind::Silicon) => ActorKind::Silicon,
            None => public_id_kind(&user.id)
                .ok_or(AccountsError::Rejected(AuthFailure::InvalidProof))?,
        };
        // Silicon Accounts just verified the proof, so its id is the account's id now.
        let record = self
            .directory
            .sighting(
                &uuid,
                kind,
                Some(&user.id),
                Some(OffsetDateTime::now_utc()),
                KeySource::Proof,
            )
            .await
            .map_err(|_| AccountsError::Unavailable)?;
        if record.deleted_at.is_some() {
            return Err(AccountsError::Rejected(AuthFailure::AccountDeleted));
        }
        same_kind(&record, kind)?;
        let mut actor = authorized(
            &record,
            Some(user.id.clone()),
            Some(issuing_app),
            Some(proof.expires_at),
        )?;
        actor.scopes = proof.scopes;
        Ok(actor)
    }

    async fn require_current_uuid(&self, uuid: &AccountUuid) -> Result<(), AccountsError> {
        if self
            .directory
            .is_retired_uuid(uuid)
            .await
            .map_err(|_| AccountsError::Unavailable)?
        {
            return Err(AccountsError::Rejected(AuthFailure::SignInEnded));
        }
        Ok(())
    }
}

/// An account is a Carbon or a Silicon for good, so a stored record of the other
/// kind holds another account's data (a wrong cutover mapping): show neither.
fn same_kind(record: &AccountRecord, kind: ActorKind) -> Result<(), AccountsError> {
    if record.kind == kind {
        return Ok(());
    }
    tracing::error!(
        stored = record.kind.as_str(),
        credential = kind.as_str(),
        "an account's stored kind differs from its credential; its earlier data was linked to the wrong account"
    );
    Err(AccountsError::Rejected(AuthFailure::AccountRecordConflict))
}

fn authorized(
    record: &AccountRecord,
    token_id: Option<String>,
    originating_application: Option<ApplicationId>,
    expires_at: Option<OffsetDateTime>,
) -> Result<AuthorizedActor, AccountsError> {
    let storage_key = ActorId::new(record.storage_key).map_err(|_| AccountsError::Unavailable)?;
    Ok(AuthorizedActor {
        actor: Actor::new(record.kind, storage_key),
        account: AccountRef {
            uuid: record.uuid.clone(),
            id: record.current_id.clone().or(token_id).unwrap_or_default(),
        },
        originating_application,
        expires_at,
        scopes: Vec::new(),
        token_family: None,
    })
}

#[async_trait]
impl AccountsPort for AccountsAuthenticator {
    async fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizedActor, AccountsError> {
        match &request.credentials {
            InboundCredentials::Bearer(token) => self.bearer(token.expose_secret()).await,
            InboundCredentials::Proof(token) => {
                let scope = request
                    .action
                    .proof_scope()
                    .ok_or(AccountsError::Rejected(AuthFailure::ProofNotAccepted))?;
                self.proof(token.expose_secret(), scope).await
            }
        }
    }
}
