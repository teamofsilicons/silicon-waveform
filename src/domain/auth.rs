//! Silicon Accounts credentials and the verified identity a request acts for.

use std::fmt;

use thiserror::Error;
use time::OffsetDateTime;
use zeroize::Zeroize as _;

use super::identity::{AccountRef, Actor, ApplicationId, RequestId};

const MAX_CREDENTIAL_LENGTH: usize = 16_384;

/// Proof-token prefix issued by Silicon Accounts.
pub const PROOF_TOKEN_PREFIX: &str = "sap_";

/// Waveform action an authenticated caller wants to perform.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WaveformAction {
    /// Synthesize text into speech (and fetch audio Waveform holds).
    SynthesizeSpeech,
    /// Transcribe source media (and prepare the source folder).
    TranscribeSpeech,
    /// Account APIs (history, settings, keys). Never reachable with a proof.
    ManageAccount,
}

impl WaveformAction {
    /// The proof scope another app needs for this action, or `None` when
    /// proofs are not accepted for it.
    #[must_use]
    pub const fn proof_scope(self) -> Option<&'static str> {
        match self {
            Self::SynthesizeSpeech => Some("waveform.tts"),
            Self::TranscribeSpeech => Some("waveform.stt"),
            Self::ManageAccount => None,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
struct OpaqueCredential(Box<[u8]>);

impl OpaqueCredential {
    fn new(value: String) -> Result<Self, CredentialError> {
        let bytes = value.into_bytes();
        if bytes.is_empty() {
            return Err(CredentialError::Empty);
        }
        if bytes.len() > MAX_CREDENTIAL_LENGTH {
            return Err(CredentialError::TooLong);
        }
        if !bytes.iter().all(u8::is_ascii_graphic) {
            return Err(CredentialError::InvalidCharacters);
        }
        Ok(Self(bytes.into_boxed_slice()))
    }

    fn expose(&self) -> &str {
        // Construction accepts bytes obtained from a valid Rust `String`, and
        // the backing allocation is never mutated before this borrow.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl Drop for OpaqueCredential {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for OpaqueCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

macro_rules! credential_type {
    ($(#[$attribute:meta])* $name:ident) => {
        $(#[$attribute])*
        #[derive(Clone)]
        pub struct $name(OpaqueCredential);

        impl $name {
            /// Exposes the credential only at the outbound adapter boundary.
            #[must_use]
            pub fn expose_secret(&self) -> &str {
                self.0.expose()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }
    };
}

credential_type!(
    /// Silicon Accounts access token (an `EdDSA` JWT issued to Waveform).
    AccessToken
);
credential_type!(
    /// Silicon Accounts proof token (`sap_…`).
    ProofToken
);

impl AccessToken {
    /// Validates and stores an opaque bearer credential.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] when the value is empty, too long, or not
    /// visible ASCII.
    pub fn new(value: String) -> Result<Self, CredentialError> {
        OpaqueCredential::new(value).map(Self)
    }
}

impl ProofToken {
    /// Validates and stores a proof token.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] when the value is not a visible-ASCII
    /// `sap_…` token of bounded length.
    pub fn new(value: String) -> Result<Self, CredentialError> {
        if !value.starts_with(PROOF_TOKEN_PREFIX) || value.len() <= PROOF_TOKEN_PREFIX.len() {
            return Err(CredentialError::WrongKind);
        }
        OpaqueCredential::new(value).map(Self)
    }
}

/// Exactly one supported inbound authentication mode.
#[derive(Clone, Debug)]
pub enum InboundCredentials {
    /// `Authorization: Bearer <access token>` for the account itself.
    Bearer(AccessToken),
    /// `Authorization: Proof sap_…` from another app acting for an account.
    Proof(ProofToken),
}

impl InboundCredentials {
    /// The caller's own Waveform access token, which can be exchanged for a
    /// User verification proof at another app. Proof callers have none.
    #[must_use]
    pub const fn subject_token(&self) -> Option<&AccessToken> {
        match self {
            Self::Bearer(token) => Some(token),
            Self::Proof(_) => None,
        }
    }
}

/// Identity verified for one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedActor {
    /// Account kind and private storage key.
    pub actor: Actor,
    /// Permanent account uuid and current public id.
    pub account: AccountRef,
    /// The app that called with a proof; absent for the account's own bearer calls.
    pub originating_application: Option<ApplicationId>,
    /// Credential expiry, when known.
    pub expires_at: Option<OffsetDateTime>,
    /// Scopes the credential carries: the access token's `scope` claim, or the proof's scopes.
    pub scopes: Vec<String>,
    /// The access token's family (`fid`), which names one sign-in; absent for proofs.
    pub token_family: Option<String>,
}

impl AuthorizedActor {
    /// True when another app is acting for the account with a proof.
    #[must_use]
    pub const fn is_delegated(&self) -> bool {
        self.originating_application.is_some()
    }
}

/// Input to Silicon Accounts authentication.
#[derive(Clone, Debug)]
pub struct AuthorizationRequest {
    /// Inbound credential.
    pub credentials: InboundCredentials,
    /// Action being performed (selects the proof scope).
    pub action: WaveformAction,
    /// Correlation ID for logs.
    pub request_id: RequestId,
}

/// A User verification proof Waveform holds for an account at Briefcase.
#[derive(Clone, Debug)]
pub struct BriefcaseProof {
    /// `sap_…` token presented to Briefcase as `Authorization: Proof`.
    pub token: ProofToken,
    /// When this proof token stops working.
    pub expires_at: OffsetDateTime,
}

/// Why a credential was refused. Every variant has an exact message and hint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AuthFailure {
    /// No `Authorization` header.
    MissingCredentials,
    /// The header is not exactly `Bearer <token>` or `Proof sap_…`.
    MalformedAuthorization,
    /// An IAM-era header was sent.
    RetiredCredentials,
    /// The access token is not a valid Silicon Accounts JWT (format or signature).
    InvalidToken,
    /// The access token expired.
    TokenExpired,
    /// The access token was issued to another app.
    WrongAudience,
    /// The access token came from another Accounts deployment.
    WrongIssuer,
    /// The token's signing key is not published by Silicon Accounts.
    UnknownSigningKey,
    /// The account's sign-in ended (signed out, access removed) after this token was issued.
    SignInEnded,
    /// The account was deleted.
    AccountDeleted,
    /// Introspection reports the token is no longer active.
    TokenInactive,
    /// Silicon Accounts says the proof is not valid for Waveform.
    InvalidProof,
    /// The proof is an App verification proof; Waveform needs a User verification proof.
    ProofWithoutAccount,
    /// The proof lacks the scope this route needs.
    ProofScopeMissing,
    /// The issuing app is not allowed to use this scope at Waveform.
    ProofIssuerNotAllowed,
    /// The route does not accept proofs.
    ProofNotAccepted,
    /// Waveform's stored record of the account is of the other kind (Carbon or
    /// Silicon) than its credential: its earlier data was linked to the wrong account.
    AccountRecordConflict,
}

impl AuthFailure {
    /// Stable machine-readable reason.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::MissingCredentials => "missing_credentials",
            Self::MalformedAuthorization => "malformed_authorization",
            Self::RetiredCredentials => "retired_credentials",
            Self::InvalidToken => "invalid_token",
            Self::TokenExpired => "token_expired",
            Self::WrongAudience => "wrong_audience",
            Self::WrongIssuer => "wrong_issuer",
            Self::UnknownSigningKey => "unknown_signing_key",
            Self::SignInEnded => "sign_in_ended",
            Self::AccountDeleted => "account_deleted",
            Self::TokenInactive => "token_inactive",
            Self::InvalidProof => "invalid_proof",
            Self::ProofWithoutAccount => "proof_without_account",
            Self::ProofScopeMissing => "proof_scope_missing",
            Self::ProofIssuerNotAllowed => "proof_issuer_not_allowed",
            Self::ProofNotAccepted => "proof_not_accepted",
            Self::AccountRecordConflict => "account_record_conflict",
        }
    }

    /// True when the caller is authenticated but not allowed (HTTP 403).
    #[must_use]
    pub const fn is_forbidden(self) -> bool {
        matches!(
            self,
            Self::ProofWithoutAccount
                | Self::ProofScopeMissing
                | Self::ProofIssuerNotAllowed
                | Self::ProofNotAccepted
                | Self::AccountRecordConflict
        )
    }

    /// Exact explanation, safe to show: it never contains token material.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::MissingCredentials => {
                "No credentials: send Authorization: Bearer <Silicon Accounts access token for waveform>, or Authorization: Proof sap_… when another app calls for an account."
            }
            Self::MalformedAuthorization => {
                "The Authorization header must be exactly `Bearer <access token>` or `Proof sap_…`, sent once."
            }
            Self::RetiredCredentials => {
                "X-IAM-OBO-Access-Token, X-IAM-OBO-Access-Proof and X-App-ID are no longer accepted. Waveform uses Silicon Accounts: send Authorization: Bearer <access token> or Authorization: Proof sap_…."
            }
            Self::InvalidToken => {
                "The access token is not a valid Silicon Accounts token for Waveform (bad format or signature)."
            }
            Self::TokenExpired => "The access token expired.",
            Self::WrongAudience => {
                "The access token was issued to another app; Waveform accepts only tokens issued to waveform."
            }
            Self::WrongIssuer => {
                "The access token was issued by a different Silicon Accounts deployment than the one Waveform trusts."
            }
            Self::UnknownSigningKey => {
                "The access token is signed with a key Silicon Accounts does not publish."
            }
            Self::SignInEnded => {
                "This sign-in ended (signed out, or Waveform's access was removed) after the token was issued."
            }
            Self::AccountDeleted => "This Silicon Accounts account was deleted.",
            Self::TokenInactive => {
                "Silicon Accounts reports this access token is no longer active (its sign-in was revoked)."
            }
            Self::InvalidProof => {
                "Silicon Accounts says this proof is not valid for Waveform: unknown, expired, revoked, issued for another app, or its sign-in ended."
            }
            Self::ProofWithoutAccount => {
                "This is an App verification proof. Waveform only acts for an account, so it needs a User verification proof."
            }
            Self::ProofScopeMissing => "The proof does not carry the scope this route needs.",
            Self::ProofIssuerNotAllowed => {
                "The app that issued this proof is not allowed to use this scope at Waveform."
            }
            Self::ProofNotAccepted => {
                "This route does not accept proofs; the account must call it with its own access token."
            }
            Self::AccountRecordConflict => {
                "Waveform's record of this account names the other kind of account (Carbon or Silicon) than its credential, so it holds another account's earlier data; Waveform shows none of it."
            }
        }
    }

    /// What to do next.
    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::MissingCredentials | Self::MalformedAuthorization | Self::RetiredCredentials => {
                "Carbons: `waveform login`. Silicons: `silicon-accounts login --app waveform -q | waveform login --slt-stdin`. Apps acting for an account: issue a User verification proof for receiving_app waveform."
            }
            Self::InvalidToken
            | Self::WrongAudience
            | Self::WrongIssuer
            | Self::UnknownSigningKey => {
                "Get a token issued to app waveform by the Silicon Accounts deployment configured in ACCOUNTS_URL, then retry."
            }
            Self::TokenExpired => {
                "Refresh the sign-in with its refresh token (once, single-flight) or sign in again."
            }
            Self::SignInEnded | Self::TokenInactive => {
                "Sign in to Waveform again; tokens issued before the sign-in ended are refused."
            }
            Self::AccountDeleted => "A deleted account cannot use Waveform.",
            Self::InvalidProof => {
                "Issue a fresh User verification proof for receiving_app waveform (POST /v1/proofs/user-verification) or refresh it."
            }
            Self::ProofWithoutAccount => {
                "Issue a User verification proof (POST /v1/proofs/user-verification) with the account's access token at your app."
            }
            Self::ProofScopeMissing => {
                "Request the scope this route needs: waveform.tts for /api/v1/tts and /api/v1/audio, waveform.stt for /api/v1/stt and /api/v1/stt/source-target."
            }
            Self::ProofIssuerNotAllowed => {
                "Waveform's operator must add the app to WAVEFORM_PROOF_ISSUERS for this scope."
            }
            Self::ProofNotAccepted => {
                "Call with Authorization: Bearer <the account's access token>."
            }
            Self::AccountRecordConflict => {
                "Waveform's operator must correct the mapping that linked earlier data to this account (`waveform-api link-identities --relink`)."
            }
        }
    }
}

/// Credential construction failure. The rejected value is never retained.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CredentialError {
    /// Empty credentials are never meaningful.
    #[error("credential must not be empty")]
    Empty,
    /// A bounded credential prevents unbounded header and log processing.
    #[error("credential exceeds the maximum supported length")]
    TooLong,
    /// Credentials use a visible ASCII transport representation.
    #[error("credential contains unsupported characters")]
    InvalidCharacters,
    /// The value is not the expected token class.
    #[error("credential is not the expected token class")]
    WrongKind,
}

#[cfg(test)]
mod tests {
    use super::{AccessToken, AuthFailure, CredentialError, ProofToken, WaveformAction};

    #[test]
    fn debug_output_never_contains_credentials() {
        let token = AccessToken::new("secret-bearer-token".to_owned());
        let proof = ProofToken::new("sap_secret-proof".to_owned());

        assert_eq!(
            token.map(|value| format!("{value:?}")),
            Ok("AccessToken([REDACTED])".to_owned())
        );
        assert_eq!(
            proof.map(|value| format!("{value:?}")),
            Ok("ProofToken([REDACTED])".to_owned())
        );
    }

    #[test]
    fn credentials_reject_whitespace_control_characters_and_wrong_classes() {
        assert!(matches!(
            AccessToken::new("token with spaces".to_owned()),
            Err(CredentialError::InvalidCharacters)
        ));
        assert!(matches!(
            AccessToken::new("token\n".to_owned()),
            Err(CredentialError::InvalidCharacters)
        ));
        for wrong in ["sapr_refresh", "oba_legacy", "sap_"] {
            assert!(matches!(
                ProofToken::new(wrong.to_owned()),
                Err(CredentialError::WrongKind)
            ));
        }
    }

    #[test]
    fn proof_scopes_are_the_old_endpoint_ids_and_account_routes_have_none() {
        assert_eq!(
            WaveformAction::SynthesizeSpeech.proof_scope(),
            Some("waveform.tts")
        );
        assert_eq!(
            WaveformAction::TranscribeSpeech.proof_scope(),
            Some("waveform.stt")
        );
        assert_eq!(WaveformAction::ManageAccount.proof_scope(), None);
    }

    #[test]
    fn failures_have_distinct_reasons_and_statuses() {
        let all = [
            AuthFailure::MissingCredentials,
            AuthFailure::MalformedAuthorization,
            AuthFailure::RetiredCredentials,
            AuthFailure::InvalidToken,
            AuthFailure::TokenExpired,
            AuthFailure::WrongAudience,
            AuthFailure::WrongIssuer,
            AuthFailure::UnknownSigningKey,
            AuthFailure::SignInEnded,
            AuthFailure::AccountDeleted,
            AuthFailure::TokenInactive,
            AuthFailure::InvalidProof,
            AuthFailure::ProofWithoutAccount,
            AuthFailure::ProofScopeMissing,
            AuthFailure::ProofIssuerNotAllowed,
            AuthFailure::ProofNotAccepted,
            AuthFailure::AccountRecordConflict,
        ];
        let reasons = all
            .iter()
            .map(|failure| failure.reason())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(reasons.len(), all.len());
        assert!(AuthFailure::ProofScopeMissing.is_forbidden());
        assert!(!AuthFailure::TokenExpired.is_forbidden());
        assert!(
            all.iter()
                .all(|failure| !failure.message().is_empty() && !failure.hint().is_empty())
        );
    }
}
