//! Live IAM authentication, reusable endpoint verification and storage consent.
//! Direct sessions use the encrypted feature grant broker. Incoming OBO tokens
//! retain the same credential along a declared, user-approved dependency chain.

use std::{fmt, str::FromStr as _, time::Duration};

use async_trait::async_trait;
use http::StatusCode;
use secrecy::ExposeSecret as _;
use thiserror::Error;
use time::OffsetDateTime;
use url::Url;

use crate::{
    application::ports::{IamError, IamPort},
    config::IamSettings,
    domain::{
        auth::{
            AuthorizationRequest, AuthorizedActor, DelegatedAuthorization, DelegationRequest,
            InboundCredentials, OboCredentials, WaveformAction,
        },
        identity::{Actor, ActorId, ActorKind, ApplicationId, OrganizationId},
    },
};

#[cfg(test)]
const MAX_IAM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
use silicon_iam_client::models::TokenIntrospection;
const USER_AGENT: &str = concat!("silicon-waveform/", env!("CARGO_PKG_VERSION"));

/// Construction failure for the IAM HTTP adapter.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum IamAdapterBuildError {
    /// A configured IAM URL or endpoint path is not safe and absolute.
    #[error("invalid IAM endpoint configuration")]
    InvalidEndpoint,
    /// Application identity configuration cannot be represented safely.
    #[error("invalid IAM application identity configuration")]
    InvalidApplicationIdentity,
    /// Reqwest could not build the redirect-free client.
    #[error("failed to build IAM HTTP client")]
    HttpClient,
}

/// Redirect-free, bounded IAM authorization client.
#[derive(Clone)]
pub struct IamHttpAdapter {
    identity_pool: Option<sqlx::PgPool>,
    storage_grants: Option<crate::control::storage::StorageGrants>,
    introspection_url: Url,
    obo_verify_url: Url,
    sdk: silicon_iam_client::Client,
    storage_audience: Option<String>,
    application_id: ApplicationId,
    audience: ApplicationId,
    tts_action: String,
    stt_action: String,
    timeout: Duration,
}

impl IamHttpAdapter {
    /// Builds an IAM adapter from validated process settings.
    ///
    /// The resulting client disables redirects, applies explicit connect and
    /// whole-request deadlines, bounds idle connections, and never blindly retries an
    /// OBO verification request.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if an endpoint, application identifier, or the
    /// underlying HTTP client cannot be constructed safely.
    pub fn new(settings: &IamSettings) -> Result<Self, IamAdapterBuildError> {
        if settings.token_introspection_path != "/api/v1/oauth/introspect"
            || settings.obo_verify_path != "/api/v1/obo-access/token-verifications"
        {
            return Err(IamAdapterBuildError::InvalidEndpoint);
        }
        let introspection_url = endpoint(&settings.base_url, &settings.token_introspection_path)?;
        let obo_verify_url = endpoint(&settings.base_url, &settings.obo_verify_path)?;
        let sdk = silicon_iam_client::Client::builder(settings.base_url.as_str())
            .map_err(|_| IamAdapterBuildError::InvalidEndpoint)?
            .credential(silicon_iam_client::Credential::application(
                &settings.app_id,
                settings.app_secret.expose_secret(),
            ))
            .auto_update(false)
            .timeout(settings.timeout)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| IamAdapterBuildError::HttpClient)?;
        let application_id = ApplicationId::from_str(&settings.app_id)
            .map_err(|_| IamAdapterBuildError::InvalidApplicationIdentity)?;
        let audience = ApplicationId::from_str(&settings.audience)
            .map_err(|_| IamAdapterBuildError::InvalidApplicationIdentity)?;
        if application_id.as_str() != settings.app_id.as_str()
            || audience.as_str() != settings.audience.as_str()
            || settings.app_secret.expose_secret().is_empty()
            || !valid_action(&settings.tts_action)
            || !valid_action(&settings.stt_action)
        {
            return Err(IamAdapterBuildError::InvalidApplicationIdentity);
        }
        Ok(Self {
            identity_pool: None,
            storage_grants: None,
            introspection_url,
            obo_verify_url,
            sdk,
            storage_audience: None,
            application_id,
            audience,
            tts_action: settings.tts_action.clone(),
            stt_action: settings.stt_action.clone(),
            timeout: settings.timeout,
        })
    }

    async fn authorize_bearer(
        &self,
        request: &AuthorizationRequest,
        token: &crate::domain::auth::AccessToken,
    ) -> Result<AuthorizedActor, IamError> {
        let input = silicon_iam_client::models::TokenIntrospectionRequest {
            token: token.expose_secret().to_owned(),
            token_type_hint: Some(
                silicon_iam_client::models::TokenIntrospectionRequestTokenTypeHint::AccessToken,
            ),
        };
        let response = tokio::time::timeout(
            self.timeout,
            self.sdk
                .oauth()
                .introspect(&input, Some(request.organization_id.as_str())),
        )
        .await
        .map_err(|_| IamError::Timeout)?
        .map_err(map_sdk_error)?;
        let public_id = response.public_id.clone().or_else(|| {
            response
                .authorization
                .as_ref()
                .and_then(|a| a.public_id.clone())
        });
        let mut actor = self.validate_introspection(response, request)?;
        let public_id = public_id.ok_or(IamError::InvalidResponse)?;
        if crate::infrastructure::actor_keys::canonical_kind(&public_id) != Some(actor.actor.kind) {
            return Err(IamError::InvalidResponse);
        }
        let key = if let Some(pool) = &self.identity_pool {
            crate::infrastructure::actor_keys::resolve(pool, uuid::Uuid::nil(), &public_id)
                .await
                .map_err(|_| IamError::InvalidResponse)?
        } else {
            #[cfg(not(test))]
            return Err(IamError::ContractUnavailable);
            #[cfg(test)]
            uuid::Uuid::new_v4()
        };
        actor.actor.id = ActorId::new(key).map_err(|_| IamError::InvalidResponse)?;
        Ok(actor)
    }

    /// Uses durable private row keys for canonical IAM identities.
    #[must_use]
    pub fn with_identity_store(mut self, pool: sqlx::PgPool) -> Self {
        self.identity_pool = Some(pool);
        self
    }

    async fn authorize_obo(
        &self,
        request: &AuthorizationRequest,
        credentials: &OboCredentials,
    ) -> Result<AuthorizedActor, IamError> {
        verify_obo(
            &self.sdk,
            self.identity_pool.as_ref(),
            uuid::Uuid::nil(),
            None,
            &self.application_id,
            request,
            credentials,
        )
        .await
    }

    pub(crate) fn with_storage_grants(
        mut self,
        store: Option<crate::control::storage::StorageGrants>,
    ) -> Self {
        self.storage_grants = store;
        self
    }

    /// Selects the independently configured Briefcase audience for upload delegation.
    #[must_use]
    pub fn with_storage_audience(mut self, audience: &str) -> Self {
        self.storage_audience = Some(audience.to_owned());
        self
    }

    fn validate_introspection(
        &self,
        response: TokenIntrospection,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizedActor, IamError> {
        if !response.active {
            return Err(IamError::InvalidCredential);
        }

        if response
            .authorization
            .as_ref()
            .is_some_and(|authority| authority.testing_environment_id.is_some())
        {
            return Err(IamError::InvalidCredential);
        }
        if let Some(authority) = &response.authorization
            && (response
                .public_id
                .as_ref()
                .is_some_and(|id| authority.public_id.as_ref() != Some(id))
                || authority.org_id != request.organization_id.as_str()
                || authority.audience != self.audience.as_str())
        {
            return Err(IamError::InvalidCredential);
        }
        let actor_kind = actor_kind(response.actor_type.as_ref())?;
        let organization = required_organization(response.org_id.as_deref())?;
        if organization != request.organization_id {
            return Err(IamError::OrganizationMismatch);
        }

        let audience = response.audience.ok_or(IamError::InvalidResponse)?;
        if audience != self.audience.as_str() {
            return Err(IamError::InvalidCredential);
        }
        if !response
            .scope
            .as_deref()
            .is_some_and(|scope| contains_scope(scope, self.action(request.action)))
        {
            return Err(IamError::Forbidden);
        }

        let expires_at = response
            .expires_at
            .map(|timestamp| {
                OffsetDateTime::from_unix_timestamp(timestamp)
                    .map_err(|_| IamError::InvalidResponse)
            })
            .transpose()?;
        if expires_at.is_some_and(|expiry| expiry <= OffsetDateTime::now_utc()) {
            return Err(IamError::InvalidCredential);
        }
        let actor_id = ActorId::new(uuid::Uuid::now_v7()).map_err(|_| IamError::InvalidResponse)?;

        Ok(AuthorizedActor {
            actor: Actor::new(actor_kind, actor_id),
            organization_id: organization,
            originating_application: None,
            expires_at,
        })
    }

    fn action(&self, action: WaveformAction) -> &str {
        match action {
            WaveformAction::SynthesizeSpeech => &self.tts_action,
            WaveformAction::TranscribeSpeech => &self.stt_action,
        }
    }
}

impl fmt::Debug for IamHttpAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IamHttpAdapter")
            .field("introspection_url", &self.introspection_url)
            .field("obo_verify_url", &self.obo_verify_url)
            .field("application_id", &self.application_id)
            .field("audience", &self.audience)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl IamPort for IamHttpAdapter {
    async fn authorize(&self, request: AuthorizationRequest) -> Result<AuthorizedActor, IamError> {
        match &request.credentials {
            InboundCredentials::Bearer(token) => self.authorize_bearer(&request, token).await,
            InboundCredentials::OnBehalfOf(credentials) => {
                self.authorize_obo(&request, credentials).await
            }
        }
    }

    async fn delegate(
        &self,
        request: DelegationRequest,
    ) -> Result<DelegatedAuthorization, IamError> {
        let delegated = delegate_storage(
            &self.sdk,
            self.storage_grants.as_ref(),
            uuid::Uuid::nil(),
            &self.application_id,
            self.storage_audience
                .as_deref()
                .ok_or(IamError::ContractUnavailable)?,
            request,
        )
        .await?;
        if delegated.testing_secret.is_some() {
            return Err(IamError::InvalidResponse);
        }
        Ok(delegated)
    }
}

/// Storage delegation using an already selected test IAM client. It cannot
/// authenticate callers; the control plane must validate their test identity first.
pub(crate) struct TestStorageDelegator {
    pub(crate) store: crate::control::storage::StorageGrants,
    pub(crate) plane: uuid::Uuid,
    pub(crate) sdk: silicon_iam_client::Client,
    pub(crate) application_id: ApplicationId,
    pub(crate) audience: String,
}

#[async_trait]
impl IamPort for TestStorageDelegator {
    async fn authorize(&self, _request: AuthorizationRequest) -> Result<AuthorizedActor, IamError> {
        Err(IamError::InvalidCredential)
    }

    async fn delegate(
        &self,
        request: DelegationRequest,
    ) -> Result<DelegatedAuthorization, IamError> {
        let delegated = delegate_storage(
            &self.sdk,
            Some(&self.store),
            self.plane,
            &self.application_id,
            &self.audience,
            request,
        )
        .await?;
        if delegated.testing_secret.is_none() {
            return Err(IamError::InvalidResponse);
        }
        Ok(delegated)
    }
}

/// Delegates storage for an identity already verified by the source-target route.
/// The selected SDK client retains its production or testing plane credentials.
pub(crate) struct SourceStorageDelegator {
    pub(crate) store: crate::control::storage::StorageGrants,
    pub(crate) plane: uuid::Uuid,
    pub(crate) sdk: silicon_iam_client::Client,
    pub(crate) application_id: ApplicationId,
    pub(crate) audience: String,
    pub(crate) testing: bool,
}

#[async_trait]
impl IamPort for SourceStorageDelegator {
    async fn authorize(&self, _request: AuthorizationRequest) -> Result<AuthorizedActor, IamError> {
        Err(IamError::InvalidCredential)
    }

    async fn delegate(
        &self,
        request: DelegationRequest,
    ) -> Result<DelegatedAuthorization, IamError> {
        let delegated = delegate_storage(
            &self.sdk,
            Some(&self.store),
            self.plane,
            &self.application_id,
            &self.audience,
            request,
        )
        .await?;
        if delegated.testing_secret.is_some() != self.testing {
            return Err(IamError::InvalidResponse);
        }
        Ok(delegated)
    }
}

pub(crate) async fn verify_obo(
    sdk: &silicon_iam_client::Client,
    pool: Option<&sqlx::PgPool>,
    plane: uuid::Uuid,
    expected_testing: Option<uuid::Uuid>,
    application_id: &ApplicationId,
    request: &AuthorizationRequest,
    credentials: &OboCredentials,
) -> Result<AuthorizedActor, IamError> {
    if !credentials.proof.expose_secret().starts_with("oba_") {
        return Err(IamError::InvalidCredential);
    }
    let (endpoint, path) = match request.action {
        WaveformAction::SynthesizeSpeech => ("waveform.tts", "/api/v1/tts"),
        WaveformAction::TranscribeSpeech => ("waveform.stt", "/api/v1/stt"),
    };
    let verified = sdk
        .obo()
        .verify(&silicon_iam_client::models::OboTokenVerificationRequest {
            access_token: credentials.proof.expose_secret().to_owned(),
            endpoint_id: endpoint.to_owned(),
            request: silicon_iam_client::models::OboTokenRequestBinding {
                method: "POST".to_owned(),
                path: path.to_owned(),
            },
        })
        .await
        .map_err(map_sdk_error)?;
    if !verified.active
        || verified.issuer_app_id != credentials.application_id.as_str()
        || verified.endpoint.app_id != application_id.as_str()
        || verified.endpoint.endpoint_id != endpoint
        || verified.endpoint.path != path
        || verified.org_id != request.organization_id.as_str()
        || verified.authorization.org_id != verified.org_id
        || verified.authorization.audience != application_id.as_str()
        || verified.authorization.testing_environment_id != expected_testing
        || verified
            .authorization
            .public_id
            .as_ref()
            .is_some_and(|id| id != &verified.actor.public_id)
        || verified.expires_at <= OffsetDateTime::now_utc()
    {
        return Err(IamError::InvalidCredential);
    }
    let kind = match verified.actor.type_field {
        silicon_iam_client::models::ActorRefType::Carbon => ActorKind::Carbon,
        silicon_iam_client::models::ActorRefType::Silicon => ActorKind::Silicon,
        _ => return Err(IamError::InvalidResponse),
    };
    if crate::infrastructure::actor_keys::canonical_kind(&verified.actor.public_id) != Some(kind) {
        return Err(IamError::InvalidResponse);
    }
    let private_id = if let Some(pool) = pool {
        crate::infrastructure::actor_keys::resolve(pool, plane, &verified.actor.public_id)
            .await
            .map_err(|_| IamError::Unavailable)?
    } else {
        #[cfg(not(test))]
        return Err(IamError::ContractUnavailable);
        #[cfg(test)]
        uuid::Uuid::new_v4()
    };
    Ok(AuthorizedActor {
        actor: Actor::new(
            kind,
            ActorId::new(private_id).map_err(|_| IamError::InvalidResponse)?,
        ),
        organization_id: request.organization_id.clone(),
        originating_application: Some(
            verified
                .originating_app_id
                .parse()
                .map_err(|_| IamError::InvalidResponse)?,
        ),
        expires_at: Some(verified.expires_at),
    })
}

async fn storage_token(
    sdk: &silicon_iam_client::Client,
    store: Option<&crate::control::storage::StorageGrants>,
    plane: uuid::Uuid,
    audience: &str,
    request: &DelegationRequest,
    endpoint: &str,
) -> Result<silicon_iam_client::models::OboAccessToken, IamError> {
    let token = request
        .subject_token
        .as_ref()
        .ok_or(IamError::StorageAuthorizationRequired)?;
    if request.authorization.originating_application.is_some() {
        if !token.expose_secret().starts_with("oba_") {
            return Err(IamError::InvalidCredential);
        }
        let response = sdk
            .obo()
            .delegate(
                &silicon_iam_client::models::OboDelegationRequest {
                    access_token: token.expose_secret().to_owned(),
                    audience: audience.to_owned(),
                    endpoint_id: endpoint.to_owned(),
                },
                &silicon_iam_client::Mutation::new(),
            )
            .await
            .map_err(map_sdk_error)?;
        if response.access_token != token.expose_secret() {
            return Err(IamError::InvalidResponse);
        }
        return Ok(response);
    }
    let pair = store
        .ok_or(IamError::ContractUnavailable)?
        .token(
            sdk,
            plane,
            request.authorization.organization_id.as_str(),
            request.authorization.actor.id.as_uuid(),
            audience,
            endpoint,
        )
        .await?;
    // Same fields, excluding the encrypted refresh credential retained by the broker.
    serde_json::from_value(serde_json::to_value(&pair).map_err(|_| IamError::InvalidResponse)?)
        .map_err(|_| IamError::InvalidResponse)
}
#[allow(
    clippy::too_many_lines,
    reason = "keeps provider token and selected-context checks together"
)]
async fn delegate_storage(
    sdk: &silicon_iam_client::Client,
    store: Option<&crate::control::storage::StorageGrants>,
    plane: uuid::Uuid,
    application_id: &ApplicationId,
    audience: &str,
    request: DelegationRequest,
) -> Result<DelegatedAuthorization, IamError> {
    let endpoint = match request.purpose {
        crate::domain::auth::DelegationPurpose::StoreGeneratedAudio => "briefcase.uploads.reserve",
        crate::domain::auth::DelegationPurpose::ReadBriefcaseFile => {
            let manifest = request
                .manifest
                .as_ref()
                .ok_or(IamError::ContractUnavailable)?;
            if !matches!(
                (manifest.endpoint_id.as_str(), manifest.path.as_str()),
                ("briefcase.entries.list", "/api/v1/obo/entries/list")
                    | ("briefcase.files.read", "/api/v1/obo/files/read")
            ) {
                return Err(IamError::Forbidden);
            }
            &manifest.endpoint_id
        }
    };
    let response = storage_token(sdk, store, plane, audience, &request, endpoint).await?;
    if response.audience != audience
        || response.endpoint_id != endpoint
        || response.expires_at <= OffsetDateTime::now_utc()
        || response.testing_context.is_some() == plane.is_nil()
    {
        return Err(IamError::InvalidResponse);
    }
    let commit = if request.purpose == crate::domain::auth::DelegationPurpose::StoreGeneratedAudio {
        let commit = storage_token(
            sdk,
            store,
            plane,
            audience,
            &request,
            "briefcase.uploads.commit",
        )
        .await?;
        if commit.audience != audience
            || commit.endpoint_id != "briefcase.uploads.commit"
            || commit.org_id != response.org_id
            || commit.actor.as_ref().map(|a| &a.public_id)
                != response.actor.as_ref().map(|a| &a.public_id)
            || commit.expires_at <= OffsetDateTime::now_utc()
            || commit
                .testing_context
                .as_ref()
                .map(|c| (&c.app_id, &c.app_secret))
                != response
                    .testing_context
                    .as_ref()
                    .map(|c| (&c.app_id, &c.app_secret))
        {
            return Err(IamError::InvalidResponse);
        }
        Some(
            crate::domain::auth::OboProof::new(commit.access_token)
                .map_err(|_| IamError::InvalidResponse)?,
        )
    } else {
        None
    };
    let actor_id = response.actor.as_ref().map(|actor| actor.public_id.clone());
    let list = if request.purpose == crate::domain::auth::DelegationPurpose::StoreGeneratedAudio {
        let list = storage_token(
            sdk,
            store,
            plane,
            audience,
            &request,
            "briefcase.entries.list",
        )
        .await?;
        if list.audience != audience
            || list.endpoint_id != "briefcase.entries.list"
            || list.org_id != response.org_id
            || list.actor.as_ref().map(|a| &a.public_id)
                != response.actor.as_ref().map(|a| &a.public_id)
            || list
                .testing_context
                .as_ref()
                .map(|c| (&c.app_id, &c.app_secret))
                != response
                    .testing_context
                    .as_ref()
                    .map(|c| (&c.app_id, &c.app_secret))
            || list.expires_at <= OffsetDateTime::now_utc()
        {
            return Err(IamError::InvalidResponse);
        }
        Some(
            crate::domain::auth::OboProof::new(list.access_token)
                .map_err(|_| IamError::InvalidResponse)?,
        )
    } else {
        None
    };
    Ok(DelegatedAuthorization {
        list_proof: list,
        actor_id,
        organization_id: response
            .org_id
            .parse()
            .map_err(|_| IamError::InvalidResponse)?,
        commit_proof: commit,
        testing_secret: response
            .testing_context
            .map(|context| {
                if context.app_id != audience {
                    return Err(IamError::InvalidResponse);
                }
                briefcase_client::EnvironmentKey::new(&context.app_secret)
                    .map_err(|_| IamError::InvalidResponse)?;
                Ok(secrecy::SecretString::from(context.app_secret))
            })
            .transpose()?,
        application_id: application_id.clone(),
        proof: crate::domain::auth::OboProof::new(response.access_token)
            .map_err(|_| IamError::InvalidResponse)?,
        purpose: request.purpose,
        expires_at: response.expires_at,
    })
}

fn endpoint(base_url: &Url, path: &str) -> Result<Url, IamAdapterBuildError> {
    let endpoint = base_url
        .join(path)
        .map_err(|_| IamAdapterBuildError::InvalidEndpoint)?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.origin() != base_url.origin()
    {
        return Err(IamAdapterBuildError::InvalidEndpoint);
    }
    Ok(endpoint)
}

fn required_organization(value: Option<&str>) -> Result<OrganizationId, IamError> {
    let value = value.ok_or(IamError::InvalidResponse)?;
    let organization: OrganizationId = value.parse().map_err(|_| IamError::InvalidResponse)?;
    if organization.as_str() != value {
        return Err(IamError::InvalidResponse);
    }
    Ok(organization)
}

fn actor_kind(
    value: Option<&silicon_iam_client::models::TokenIntrospectionActorType>,
) -> Result<ActorKind, IamError> {
    use silicon_iam_client::models::TokenIntrospectionActorType as Kind;
    match value {
        Some(Kind::Carbon) => Ok(ActorKind::Carbon),
        Some(Kind::Silicon) => Ok(ActorKind::Silicon),
        Some(Kind::Application) => Err(IamError::Forbidden),
        Some(Kind::Other(_)) | None => Err(IamError::InvalidResponse),
    }
}

fn contains_scope(scope: &str, required: &str) -> bool {
    scope
        .split_ascii_whitespace()
        .any(|candidate| candidate == required)
}

fn valid_action(value: &str) -> bool {
    (3..=128).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value.bytes().skip(1).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.:->".contains(&byte)
        })
}

fn map_sdk_error(error: silicon_iam_client::Error) -> IamError {
    match error {
        silicon_iam_client::Error::Api(error) => {
            StatusCode::from_u16(error.status).map_or(IamError::Unavailable, map_status)
        }
        silicon_iam_client::Error::Transport(error) if error.is_timeout() => IamError::Timeout,
        silicon_iam_client::Error::Decode(_)
        | silicon_iam_client::Error::ResponseTooLarge { .. } => IamError::InvalidResponse,
        _ => IamError::Unavailable,
    }
}

fn map_status(status: StatusCode) -> IamError {
    match status {
        // Both IAM endpoints authenticate Waveform itself with HTTP Basic.
        // Actor-token invalidity is represented in a successful introspection
        // body, while proof-validation failures use the explicit 4xx statuses
        // below. A 401 therefore indicates dependency configuration, not a
        // caller credential failure.
        StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS => IamError::Unavailable,
        StatusCode::BAD_REQUEST
        | StatusCode::CONFLICT
        | StatusCode::GONE
        | StatusCode::UNPROCESSABLE_ENTITY => IamError::InvalidCredential,
        StatusCode::FORBIDDEN => IamError::Forbidden,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => IamError::Timeout,
        value if value.is_server_error() => IamError::Unavailable,
        _ => IamError::InvalidResponse,
    }
}

#[cfg(test)]
pub(crate) fn test_digest(value: &[u8]) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(value))
}

#[cfg(test)]
mod tests {
    use super::{Actor, ActorId, ActorKind, AuthorizedActor};
    use std::{str::FromStr as _, time::Duration};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use serde_json::json;
    use time::OffsetDateTime;
    use url::Url;
    use uuid::Uuid;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    use super::{IamHttpAdapter, MAX_IAM_RESPONSE_BYTES, map_status};
    use crate::{
        application::ports::{IamError, IamPort},
        config::IamSettings,
        domain::{
            auth::{
                AccessToken, AuthorizationRequest, DelegationPurpose, DelegationRequest,
                InboundCredentials, OboCredentials, OboProof, WaveformAction,
            },
            identity::{ApplicationId, OrganizationId, RequestId},
        },
    };

    #[test]
    fn upstream_application_auth_failure_is_not_blame_assigned_to_the_caller() {
        assert_eq!(
            map_status(http::StatusCode::UNAUTHORIZED),
            IamError::Unavailable
        );
        assert_eq!(
            map_status(http::StatusCode::UNPROCESSABLE_ENTITY),
            IamError::InvalidCredential
        );
    }

    fn settings(server: &MockServer) -> Result<IamSettings, url::ParseError> {
        Ok(IamSettings {
            base_url: Url::parse(&server.uri())?,
            app_id: "waveform".to_owned(),
            app_secret: SecretString::from("unit-test-app-secret".to_owned()),
            token_introspection_path: "/api/v1/oauth/introspect".to_owned(),
            obo_verify_path: "/api/v1/obo-access/token-verifications".to_owned(),
            audience: "waveform".to_owned(),
            tts_action: "waveform.tts".to_owned(),
            stt_action: "waveform.stt".to_owned(),
            timeout: Duration::from_secs(2),
        })
    }

    fn request_id() -> Result<RequestId, crate::domain::identity::IdentityError> {
        RequestId::new(Uuid::new_v4())
    }

    fn organization() -> Result<OrganizationId, crate::domain::identity::IdentityError> {
        OrganizationId::from_str("acme")
    }

    fn basic_authorization() -> String {
        format!("Basic {}", STANDARD.encode("waveform:unit-test-app-secret"))
    }

    #[tokio::test]
    async fn bearer_introspection_is_online_form_encoded_and_strict()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let actor_id = Uuid::new_v4();
        let request_id = request_id()?;
        let expires_at = OffsetDateTime::now_utc().unix_timestamp() + 300;
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .and(header("authorization", basic_authorization()))
            .and(header("x-org-id", "acme"))
            .and(body_string_contains("token=opaque-access-token"))
            .and(body_string_contains("token_type_hint=access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "active": true,
                "principal_id": actor_id, "public_id":"c:test-carbon",
                "actor_type": "carbon",
                "org_id": "acme",
                "scope": "waveform.tts waveform.stt",
                "audience": "waveform",
                "expires_at": expires_at
            })))
            .expect(1)
            .mount(&server)
            .await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?;
        let authorized = adapter
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::Bearer(AccessToken::new(
                    "opaque-access-token".to_owned(),
                )?),
                organization_id: organization()?,
                action: WaveformAction::SynthesizeSpeech,
                request_id,
            })
            .await?;

        assert!(!authorized.actor.id.as_uuid().is_nil());
        assert_eq!(authorized.organization_id.as_str(), "acme");
        assert!(authorized.originating_application.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn retired_proof_is_rejected_without_an_upstream_call()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?;
        let result = adapter
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::OnBehalfOf(OboCredentials {
                    application_id: ApplicationId::from_str("calling-app")?,
                    proof: OboProof::new("obo_opaque-proof".to_owned())?,
                }),
                organization_id: organization()?,
                action: WaveformAction::TranscribeSpeech,
                request_id: request_id()?,
            })
            .await;
        assert_eq!(result, Err(IamError::InvalidCredential));
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_organization_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "active": true,
                "principal_id": Uuid::new_v4(),
                "actor_type": "carbon",
                "org_id": "other",
                "scope": "waveform.tts",
                "audience": "waveform"
            })))
            .mount(&server)
            .await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?;
        let result = adapter
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::Bearer(AccessToken::new(
                    "opaque-access-token".to_owned(),
                )?),
                organization_id: organization()?,
                action: WaveformAction::SynthesizeSpeech,
                request_id: request_id()?,
            })
            .await;

        assert_eq!(result, Err(IamError::OrganizationMismatch));
        Ok(())
    }

    #[tokio::test]
    async fn oversized_success_body_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("x".repeat(MAX_IAM_RESPONSE_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?;
        let result = adapter
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::Bearer(AccessToken::new(
                    "opaque-access-token".to_owned(),
                )?),
                organization_id: organization()?,
                action: WaveformAction::SynthesizeSpeech,
                request_id: request_id()?,
            })
            .await;

        assert_eq!(result, Err(IamError::InvalidResponse));
        Ok(())
    }

    #[tokio::test]
    async fn downstream_delegation_is_explicitly_unavailable()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?;
        let authorized = crate::domain::auth::AuthorizedActor {
            actor: crate::domain::identity::Actor::new(
                crate::domain::identity::ActorKind::Carbon,
                crate::domain::identity::ActorId::new(Uuid::new_v4())?,
            ),
            organization_id: organization()?,
            originating_application: None,
            expires_at: None,
        };
        let result = adapter
            .delegate(DelegationRequest {
                authorization: authorized,
                purpose: DelegationPurpose::ReadBriefcaseFile,
                subject_token: None,
                upload: None,
                manifest: None,
                request_id: request_id()?,
            })
            .await;

        assert!(matches!(result, Err(IamError::ContractUnavailable)));
        Ok(())
    }
    fn obo_snapshot() -> serde_json::Value {
        json!({"active":true,"token_id":Uuid::new_v4(),"grant_id":Uuid::new_v4(),"actor":{"type":"carbon","public_id":"c:alice"},"org_id":"acme","issuer_app_id":"calling-app","originating_app_id":"calling-app","endpoint":{"app_id":"waveform","endpoint_id":"waveform.tts","path":"/api/v1/tts"},"chain":[{"app_id":"calling-app","audience":"waveform","endpoint_id":"waveform.tts"}],"authorization":{"organization_id":Uuid::new_v4(),"membership_id":"c:alice[acme]","membership_version":1,"authorization_epoch":1,"org_role":null,"tags":null,"audience":"waveform","org_id":"acme","testing_environment_id":null,"public_id":"c:alice","actor_type":"carbon","scopes":["obo:waveform:waveform.tts"]},"expires_at":"2099-01-01T00:00:00Z"})
    }
    fn obo_request() -> Result<AuthorizationRequest, Box<dyn std::error::Error>> {
        Ok(AuthorizationRequest {
            credentials: InboundCredentials::OnBehalfOf(OboCredentials {
                application_id: "calling-app".parse()?,
                proof: OboProof::new("oba_shared-chain".to_owned())?,
            }),
            organization_id: organization()?,
            action: WaveformAction::SynthesizeSpeech,
            request_id: request_id()?,
        })
    }
    #[tokio::test]
    async fn shared_token_is_verified_each_time_and_rejects_misbound_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        for fault in [
            None,
            Some("issuer"),
            Some("endpoint"),
            Some("org"),
            Some("plane"),
            Some("actor"),
            Some("expiry"),
        ] {
            let server = MockServer::start().await;
            let mut body = obo_snapshot();
            match fault {
                Some("issuer") => body["issuer_app_id"] = json!("other-app"),
                Some("endpoint") => body["endpoint"]["path"] = json!("/api/v1/stt"),
                Some("org") => body["org_id"] = json!("other"),
                Some("plane") => {
                    body["authorization"]["testing_environment_id"] = json!(Uuid::new_v4());
                }
                Some("actor") => body["actor"]["public_id"] = json!("si:alice"),
                Some("expiry") => body["expires_at"] = json!("2000-01-01T00:00:00Z"),
                _ => {}
            }
            Mock::given(method("POST"))
                .and(path("/api/v1/obo-access/token-verifications"))
                .and(header("authorization", basic_authorization()))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(2)
                .mount(&server)
                .await;
            let adapter = IamHttpAdapter::new(&settings(&server)?)?;
            for _ in 0..2 {
                let result = adapter.authorize(obo_request()?).await;
                if fault.is_none() {
                    let actor = result?;
                    assert_eq!(actor.organization_id.as_str(), "acme");
                    assert_eq!(
                        actor.originating_application.map(|a| a.to_string()),
                        Some("calling-app".to_owned())
                    );
                } else {
                    assert!(result.is_err(), "{fault:?}");
                }
            }
        }
        Ok(())
    }
    #[tokio::test]
    #[allow(clippy::expect_used, reason = "mock request assertion")]
    async fn downstream_keeps_shared_token_and_uses_selected_provider_subject()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/api/v1/obo-access/delegations")).and(header("authorization",basic_authorization())).respond_with(|r:&wiremock::Request|{
            let body:serde_json::Value=serde_json::from_slice(&r.body).expect("mock JSON");
            assert_eq!(body["access_token"],"oba_shared-chain");assert_eq!(body["audience"],"briefcase");
            ResponseTemplate::new(200).set_body_json(json!({"grant_id":Uuid::new_v4(),"token_id":Uuid::new_v4(),"access_token":"oba_shared-chain","token_type":"Bearer","expires_in":1800,"expires_at":"2099-01-01T00:00:00Z","audience":"briefcase","endpoint_id":body["endpoint_id"],"org_id":"other-storage-org","actor":{"type":"silicon","public_id":"si:storage-agent"},"scope":""}))
        }).expect(3).mount(&server).await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?.with_storage_audience("briefcase");
        let result = adapter
            .delegate(DelegationRequest {
                authorization: AuthorizedActor {
                    actor: Actor::new(ActorKind::Carbon, ActorId::new(Uuid::new_v4())?),
                    organization_id: organization()?,
                    originating_application: Some("calling-app".parse()?),
                    expires_at: None,
                },
                purpose: DelegationPurpose::StoreGeneratedAudio,
                request_id: request_id()?,
                subject_token: Some(AccessToken::new("oba_shared-chain".to_owned())?),
                upload: None,
                manifest: None,
            })
            .await?;
        assert_eq!(result.proof.expose_secret(), "oba_shared-chain");
        assert_eq!(
            result.commit_proof.ok_or("commit")?.expose_secret(),
            "oba_shared-chain"
        );
        assert_eq!(result.organization_id.as_str(), "other-storage-org");
        assert_eq!(result.actor_id.as_deref(), Some("si:storage-agent"));
        Ok(())
    }
    #[tokio::test]
    async fn production_rejects_a_test_realm_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let actor = Uuid::new_v4();
        let body = json!({"active":true,"principal_id":actor,"actor_type":"carbon","org_id":"acme",
            "scope":"waveform.tts","audience":"waveform","expires_at":4_102_444_800_i64,
            "authorization":{
                "principal_id":actor,"actor_type":"carbon","public_id":"c:12345678",
                "organization_id":Uuid::new_v4(),"org_id":"acme","membership_id":Uuid::new_v4(),
                "membership_version":1,"authorization_epoch":1,"audience":"waveform",
                "testing_environment_id":Uuid::new_v4(),"scopes":["waveform.tts"],"org_role":"member","tags":[]
            }
        });
        Mock::given(path("/api/v1/oauth/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?;
        let result = adapter
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::Bearer(AccessToken::new("oat_test".to_owned())?),
                organization_id: organization()?,
                action: WaveformAction::SynthesizeSpeech,
                request_id: request_id()?,
            })
            .await;
        assert_eq!(result, Err(IamError::InvalidCredential));
        Ok(())
    }
}
