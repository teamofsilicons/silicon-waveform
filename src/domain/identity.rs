//! Actor, organization, application, and request identities.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const MAX_ORGANIZATION_ID_LENGTH: usize = 64;
const MAX_APPLICATION_ID_LENGTH: usize = 80;

/// Kind of account that may perform a Waveform speech operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// Human account.
    Carbon,
    /// AI-agent account.
    Silicon,
}

/// Stable internal identity of a Carbon or Silicon.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "Uuid", into = "Uuid")]
pub struct ActorId(Uuid);

impl ActorId {
    /// Constructs a non-nil actor identifier.
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

/// A fully qualified Waveform actor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Actor {
    /// Account kind.
    pub kind: ActorKind,
    /// Stable internal account ID.
    pub id: ActorId,
}

impl Actor {
    /// Constructs an actor reference.
    #[must_use]
    pub const fn new(kind: ActorKind, id: ActorId) -> Self {
        Self { kind, id }
    }
}

/// Public organization handle used to scope all speech work.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrganizationId(String);

impl OrganizationId {
    /// Returns the normalized organization handle.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for OrganizationId {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        normalize_scoped_identifier(value, "organization ID", MAX_ORGANIZATION_ID_LENGTH).map(Self)
    }
}

impl TryFrom<String> for OrganizationId {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<OrganizationId> for String {
    fn from(value: OrganizationId) -> Self {
        value.0
    }
}

impl fmt::Display for OrganizationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Registered IAM application handle.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ApplicationId(String);

impl ApplicationId {
    /// Returns the normalized application handle.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ApplicationId {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        normalize_scoped_identifier(value, "application ID", MAX_APPLICATION_ID_LENGTH).map(Self)
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
    /// The value contained a character outside the stable identifier alphabet.
    #[error("{field} may contain only ASCII letters, digits, hyphens, and underscores")]
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

fn normalize_scoped_identifier(
    value: &str,
    field: &'static str,
    max_length: usize,
) -> Result<String, IdentityError> {
    if value != value.trim() || !(1..=max_length).contains(&value.len()) {
        return Err(IdentityError::InvalidLength { field, max_length });
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(IdentityError::InvalidCharacters { field });
    }

    Ok(value.to_ascii_lowercase())
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

    use super::{ApplicationId, IdentityError, OrganizationId, RequestId};

    #[test]
    fn scoped_identifiers_are_normalized() {
        let organization = OrganizationId::from_str("Team_Of-Silicons");
        let application = ApplicationId::from_str("Silicon-Waveform");

        assert_eq!(
            organization.map(|value| value.to_string()),
            Ok("team_of-silicons".to_owned())
        );
        assert_eq!(
            application.map(|value| value.to_string()),
            Ok("silicon-waveform".to_owned())
        );
    }

    #[test]
    fn scoped_identifiers_reject_whitespace_and_unicode() {
        assert_eq!(
            OrganizationId::from_str(" tos"),
            Err(IdentityError::InvalidLength {
                field: "organization ID",
                max_length: 64,
            })
        );
        assert_eq!(
            OrganizationId::from_str("tøs"),
            Err(IdentityError::InvalidCharacters {
                field: "organization ID"
            })
        );
    }

    #[test]
    fn identifier_kinds_enforce_their_authoritative_bounds() {
        let eighty_characters = "a".repeat(80);
        let application = ApplicationId::from_str(&eighty_characters);
        let organization = OrganizationId::from_str(&eighty_characters);

        assert!(application.is_ok());
        assert_eq!(
            organization,
            Err(IdentityError::InvalidLength {
                field: "organization ID",
                max_length: 64,
            })
        );
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
