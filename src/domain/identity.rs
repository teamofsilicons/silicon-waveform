//! Account, application and request identities.
//!
//! Silicon Accounts gives every Carbon and Silicon a permanent `uuid` (a short,
//! case-sensitive base62 string such as `zQo`, not an RFC 4122 UUID) and a
//! changeable public id (`c:handle` or `si:handle`). Waveform keys everything
//! on the uuid and only displays the id. Rows keep a private storage UUID
//! ([`ActorId`]) that the account uuid maps to.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const MAX_ACCOUNT_UUID_LENGTH: usize = 64;
const MAX_APPLICATION_ID_LENGTH: usize = 80;
const MAX_PUBLIC_ID_HANDLE_LENGTH: usize = 64;

/// Kind of account that may perform a Waveform speech operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A Carbon account (`c:` ids).
    Carbon,
    /// A Silicon account (`si:` ids).
    Silicon,
}

impl ActorKind {
    /// Stable storage and wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Carbon => "carbon",
            Self::Silicon => "silicon",
        }
    }
}

impl FromStr for ActorKind {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "carbon" => Ok(Self::Carbon),
            "silicon" => Ok(Self::Silicon),
            _ => Err(IdentityError::InvalidCharacters {
                field: "account kind",
            }),
        }
    }
}

/// Private, stable storage key of a Carbon or Silicon inside Waveform.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "Uuid", into = "Uuid")]
pub struct ActorId(Uuid);

impl ActorId {
    /// Constructs a non-nil storage key.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::NilUuid`] for the nil UUID.
    pub fn new(value: Uuid) -> Result<Self, IdentityError> {
        ensure_non_nil(value, "actor ID").map(Self)
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl TryFrom<Uuid> for ActorId {
    type Error = IdentityError;

    fn try_from(value: Uuid) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ActorId> for Uuid {
    fn from(value: ActorId) -> Self {
        value.0
    }
}

impl fmt::Display for ActorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A fully qualified Waveform actor: account kind plus private storage key.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Actor {
    /// Account kind.
    pub kind: ActorKind,
    /// Private storage key.
    pub id: ActorId,
}

impl Actor {
    /// Constructs an actor reference.
    #[must_use]
    pub const fn new(kind: ActorKind, id: ActorId) -> Self {
        Self { kind, id }
    }
}

/// Permanent Silicon Accounts identifier (the JWT `sub`). Case-sensitive; compared exactly.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountUuid(String);

impl AccountUuid {
    /// Returns the uuid exactly as Silicon Accounts issued it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AccountUuid {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !(1..=MAX_ACCOUNT_UUID_LENGTH).contains(&value.len()) {
            return Err(IdentityError::InvalidLength {
                field: "account uuid",
                max_length: MAX_ACCOUNT_UUID_LENGTH,
            });
        }
        if !value.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(IdentityError::InvalidCharacters {
                field: "account uuid",
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for AccountUuid {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<AccountUuid> for String {
    fn from(value: AccountUuid) -> Self {
        value.0
    }
}

impl fmt::Display for AccountUuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Returns the account kind named by a public id prefix (`c:` or `si:`), or
/// `None` when the value is not a well-formed public id.
#[must_use]
pub fn public_id_kind(value: &str) -> Option<ActorKind> {
    let (kind, handle) = if let Some(handle) = value.strip_prefix("c:") {
        (ActorKind::Carbon, handle)
    } else {
        (ActorKind::Silicon, value.strip_prefix("si:")?)
    };
    ((1..=MAX_PUBLIC_ID_HANDLE_LENGTH).contains(&handle.len())
        && handle.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        }))
    .then_some(kind)
}

/// The account a request acts for: its permanent uuid and its current public id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountRef {
    /// Permanent identifier.
    pub uuid: AccountUuid,
    /// Current public id (`c:…`/`si:…`) for display; may be empty when unknown.
    pub id: String,
}

/// Registered Silicon Accounts application id (for example `waveform`).
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ApplicationId(String);

impl ApplicationId {
    /// Returns the bare application id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ApplicationId {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !(1..=MAX_APPLICATION_ID_LENGTH).contains(&value.len()) {
            return Err(IdentityError::InvalidLength {
                field: "application ID",
                max_length: MAX_APPLICATION_ID_LENGTH,
            });
        }
        if !value.as_bytes()[0].is_ascii_lowercase()
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            return Err(IdentityError::InvalidCharacters {
                field: "application ID",
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for ApplicationId {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ApplicationId> for String {
    fn from(value: ApplicationId) -> Self {
        value.0
    }
}

impl fmt::Display for ApplicationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Correlation identifier returned with every Waveform response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "Uuid", into = "Uuid")]
pub struct RequestId(Uuid);

impl RequestId {
    /// Constructs a non-nil request identifier.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::NilUuid`] for the nil UUID.
    pub fn new(value: Uuid) -> Result<Self, IdentityError> {
        ensure_non_nil(value, "request ID").map(Self)
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl TryFrom<Uuid> for RequestId {
    type Error = IdentityError;

    fn try_from(value: Uuid) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RequestId> for Uuid {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

impl FromStr for RequestId {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value)
            .map_err(|_| IdentityError::InvalidUuid {
                field: "request ID",
            })
            .and_then(Self::new)
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Validation failure for an identity value.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IdentityError {
    /// The value was empty or too long.
    #[error("{field} must contain between 1 and {max_length} characters")]
    InvalidLength {
        /// Name of the invalid field.
        field: &'static str,
        /// Maximum length published for this identifier kind.
        max_length: usize,
    },
    /// The value contained a character outside the identifier alphabet.
    #[error("{field} contains a character outside its alphabet")]
    InvalidCharacters {
        /// Name of the invalid field.
        field: &'static str,
    },
    /// A UUID string could not be parsed.
    #[error("{field} must be a valid UUID")]
    InvalidUuid {
        /// Name of the invalid field.
        field: &'static str,
    },
    /// A nil UUID cannot identify a real entity.
    #[error("{field} must not be the nil UUID")]
    NilUuid {
        /// Name of the invalid field.
        field: &'static str,
    },
}

fn ensure_non_nil(value: Uuid, field: &'static str) -> Result<Uuid, IdentityError> {
    if value.is_nil() {
        Err(IdentityError::NilUuid { field })
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use uuid::Uuid;

    use super::{AccountUuid, ActorKind, ApplicationId, IdentityError, RequestId, public_id_kind};

    #[test]
    fn account_uuids_are_case_sensitive_base62_text() {
        let upper = AccountUuid::from_str("zQo");
        let lower = AccountUuid::from_str("zqo");
        assert!(upper.is_ok());
        assert_ne!(upper, lower);
        assert_eq!(
            AccountUuid::from_str("zQo").map(|value| value.to_string()),
            Ok("zQo".to_owned())
        );
        for invalid in ["", "has space", "dash-ed", "c:ada", &"a".repeat(65)] {
            assert!(AccountUuid::from_str(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn public_ids_name_their_kind_by_prefix() {
        assert_eq!(public_id_kind("c:ada"), Some(ActorKind::Carbon));
        assert_eq!(public_id_kind("si:scout-1"), Some(ActorKind::Silicon));
        for invalid in ["ada", "c:", "si:UPPER", "si:a:b", "x:ada", "c:a b"] {
            assert_eq!(public_id_kind(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn application_ids_require_bare_handles() {
        assert_eq!(
            ApplicationId::from_str("waveform").map(|value| value.to_string()),
            Ok("waveform".to_owned())
        );
        assert!(ApplicationId::from_str("Waveform").is_err());
        assert!(ApplicationId::from_str("9waveform").is_err());
        assert!(ApplicationId::from_str("tos>waveform").is_err());
        assert_eq!(
            ApplicationId::from_str(&"a".repeat(81)),
            Err(IdentityError::InvalidLength {
                field: "application ID",
                max_length: 80,
            })
        );
    }

    #[test]
    fn request_ids_reject_nil_and_malformed_uuids() {
        assert_eq!(
            RequestId::new(Uuid::nil()),
            Err(IdentityError::NilUuid {
                field: "request ID"
            })
        );
        assert_eq!(
            RequestId::from_str("not-a-uuid"),
            Err(IdentityError::InvalidUuid {
                field: "request ID"
            })
        );
    }
}
