//! Authentication credentials and verified authorization context.

use std::fmt;

use thiserror::Error;
use time::OffsetDateTime;
use zeroize::Zeroize as _;

use super::identity::{Actor, ApplicationId, OrganizationId, RequestId};

const MAX_CREDENTIAL_LENGTH: usize = 16_384;

/// Waveform action an authenticated caller wants to perform.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WaveformAction {
    /// Synthesize text into speech.
    SynthesizeSpeech,
    /// Transcribe source media.
    TranscribeSpeech,
}

impl WaveformAction {
    /// Returns the stable IAM action name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SynthesizeSpeech => "waveform.tts",
            Self::TranscribeSpeech => "waveform.stt",
        }
    }
}

/// Purpose for which Waveform requests a new audience-bound delegation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DelegationPurpose {
    /// Read or issue a delivery URL for one Briefcase file.
    ReadBriefcaseFile,
    /// Store generated audio in the represented actor's Waveform app folder.
    StoreGeneratedAudio,
}

#[derive(Clone)]
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
            /// Validates and stores an opaque credential.
            ///
            /// # Errors
            ///
            /// Returns [`CredentialError`] when the value is empty, too long,
            /// or not representable as visible ASCII.
            pub fn new(value: String) -> Result<Self, CredentialError> {
                OpaqueCredential::new(value).map(Self)
            }

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
    /// Opaque IAM access token supplied as a bearer credential.
    AccessToken
);
credential_type!(
    /// Short-lived, audience-bound IAM OBO proof.
    OboProof
);

/// Complete inbound OBO credential pair.
#[derive(Clone, Debug)]
pub struct OboCredentials {
    /// Application that initiated the on-behalf-of request.
    pub application_id: ApplicationId,
    /// Proof bound to Waveform, the actor, organization, and action.
    pub proof: OboProof,
}

/// Exactly one supported inbound authentication mode.
#[derive(Clone, Debug)]
pub enum InboundCredentials {
    /// IAM actor access token.
    Bearer(AccessToken),
    /// Application acting on behalf of an actor.
    OnBehalfOf(OboCredentials),
}

/// Identity and organization verified online by IAM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedActor {
    /// Represented Carbon or Silicon.
    pub actor: Actor,
    /// Current organization membership under which the action is authorized.
    pub organization_id: OrganizationId,
    /// Originating app for OBO requests; absent for direct bearer requests.
    pub originating_application: Option<ApplicationId>,
    /// Credential expiry reported by IAM, when present.
    pub expires_at: Option<OffsetDateTime>,
}

/// Input to online IAM authentication and authorization.
#[derive(Clone, Debug)]
pub struct AuthorizationRequest {
    /// Inbound credential mode.
    pub credentials: InboundCredentials,
    /// Organization explicitly selected by the caller.
    pub organization_id: OrganizationId,
    /// Action that IAM must authorize.
    pub action: WaveformAction,
    /// Correlation ID propagated to IAM.
    pub request_id: RequestId,
}

/// Input to an actor-bound delegation exchange for Briefcase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationRequest {
    /// Previously verified actor and organization.
    pub authorization: AuthorizedActor,
    /// Narrow reason for the new proof.
    pub purpose: DelegationPurpose,
    /// Correlation ID propagated to IAM.
    pub request_id: RequestId,
}

/// New credential minted specifically for a downstream audience.
#[derive(Clone, Debug)]
pub struct DelegatedAuthorization {
    /// Application identity Briefcase expects alongside the proof.
    pub application_id: ApplicationId,
    /// Short-lived proof that must never be replaced by the inbound proof.
    pub proof: OboProof,
    /// Purpose to which the proof was bound.
    pub purpose: DelegationPurpose,
    /// Expiry reported by IAM.
    pub expires_at: OffsetDateTime,
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
    /// IAM credentials use a visible ASCII transport representation.
    #[error("credential contains unsupported characters")]
    InvalidCharacters,
}

#[cfg(test)]
mod tests {
    use super::{AccessToken, CredentialError, OboProof};

    #[test]
    fn debug_output_never_contains_credentials() {
        let token = AccessToken::new("secret-bearer-token".to_owned());
        let proof = OboProof::new("secret-obo-proof".to_owned());

        assert_eq!(
            token.map(|value| format!("{value:?}")),
            Ok("AccessToken([REDACTED])".to_owned())
        );
        assert_eq!(
            proof.map(|value| format!("{value:?}")),
            Ok("OboProof([REDACTED])".to_owned())
        );
    }

    #[test]
    fn credentials_reject_whitespace_and_control_characters() {
        assert!(matches!(
            AccessToken::new("token with spaces".to_owned()),
            Err(CredentialError::InvalidCharacters)
        ));
        assert!(matches!(
            AccessToken::new("token\n".to_owned()),
            Err(CredentialError::InvalidCharacters)
        ));
    }

    #[test]
    fn credentials_are_available_only_through_explicit_exposure() {
        let token = AccessToken::new("opaque-token".to_owned());
        assert_eq!(
            token.map(|value| value.expose_secret().to_owned()),
            Ok("opaque-token".to_owned())
        );
    }
}
