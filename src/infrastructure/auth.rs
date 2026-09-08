//! Online IAM authorization and exact-byte upload delegation.
//!
//! Bearer introspection is bounded and checked against the selected actor action.
//! Storage proofs use the official SDK and a separately configured Briefcase
//! audience. Uploads bind normalized bytes; reads bind immutable SDK manifests.
//! Inbound OBO proof chaining is not part of the released IAM contract.

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
    /// whole-request deadlines, bounds idle connections, and never retries the
    /// single-use OBO verification request.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if an endpoint, application identifier, or the
    /// underlying HTTP client cannot be constructed safely.
    pub fn new(settings: &IamSettings) -> Result<Self, IamAdapterBuildError> {
        if settings.token_introspection_path != "/api/v1/oauth/introspect"
            || settings.obo_verify_path != "/api/v1/obo-access/verify"
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
            || application_id.as_str().len() < 3
            || audience.as_str().len() < 3
            || settings.app_secret.expose_secret().is_empty()
            || !valid_action(&settings.tts_action)
            || !valid_action(&settings.stt_action)
        {
            return Err(IamAdapterBuildError::InvalidApplicationIdentity);
        }
        Ok(Self {
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
        self.validate_introspection(response, request)
    }

    async fn authorize_obo(
        &self,
        _request: &AuthorizationRequest,
        _credentials: &OboCredentials,
    ) -> Result<AuthorizedActor, IamError> {
        tokio::task::yield_now().await;
        Err(IamError::ContractUnavailable)
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
        let principal_id = response.principal_id.ok_or(IamError::InvalidResponse)?;
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
        let actor_id = ActorId::new(principal_id).map_err(|_| IamError::InvalidResponse)?;

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
        delegate_storage(
            &self.sdk,
            &self.application_id,
            self.storage_audience
                .as_deref()
                .ok_or(IamError::ContractUnavailable)?,
            request,
        )
        .await
    }
}

/// Storage delegation using an already selected test IAM client. It cannot
/// authenticate callers; the control plane must validate their test identity first.
pub(crate) struct TestStorageDelegator {
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
        delegate_storage(&self.sdk, &self.application_id, &self.audience, request).await
    }
}

async fn delegate_storage(
    sdk: &silicon_iam_client::Client,
    application_id: &ApplicationId,
    audience: &str,
    request: DelegationRequest,
) -> Result<DelegatedAuthorization, IamError> {
    let token = request.subject_token.ok_or(IamError::ContractUnavailable)?;
    let (endpoint_id, path, body_sha256, metadata) = match request.purpose {
        crate::domain::auth::DelegationPurpose::StoreGeneratedAudio => {
            let upload = request.upload.ok_or(IamError::ContractUnavailable)?;
            (
                "briefcase.files.create".to_owned(),
                "/api/v1/obo/files".to_owned(),
                upload.body_sha256,
                serde_json::json!({"path":"", "name":upload.filename.as_str(), "content_type":"audio/mpeg"}),
            )
        }
        crate::domain::auth::DelegationPurpose::ReadBriefcaseFile => {
            let binding = request.manifest.ok_or(IamError::ContractUnavailable)?;
            if !matches!(
                (binding.endpoint_id.as_str(), binding.path.as_str()),
                ("briefcase.entries.list", "/api/v1/obo/entries/list")
                    | ("briefcase.files.read", "/api/v1/obo/files/read")
            ) {
                return Err(IamError::Forbidden);
            }
            (
                binding.endpoint_id,
                binding.path,
                binding.body_sha256,
                serde_json::json!({}),
            )
        }
    };
    let catalog = sdk
        .obo()
        .endpoints(audience)
        .await
        .map_err(|_| IamError::Unavailable)?;
    if catalog.application.app_id != audience
        || catalog.application.org_id != request.authorization.organization_id.as_str()
    {
        return Err(IamError::OrganizationMismatch);
    }
    let selected = catalog
        .endpoints
        .iter()
        .find(|item| item.endpoint_id == endpoint_id)
        .ok_or(IamError::ContractUnavailable)?;
    if selected.path != path {
        return Err(IamError::ContractUnavailable);
    }
    let exchange = silicon_iam_client::models::OboExchangeRequest {
        subject_token: token.expose_secret().to_owned(),
        audience: audience.to_owned(),
        endpoint_id,
        metadata,
        request: silicon_iam_client::models::OboExchangeRequestBinding {
            method: "POST".to_owned(),
            body_sha256,
        },
    };
    let response = sdk
        .obo()
        .exchange_signed(&exchange, &catalog, &silicon_iam_client::Mutation::new())
        .await
        .map_err(|_| IamError::Unavailable)?;
    if response.expires_at <= OffsetDateTime::now_utc() {
        return Err(IamError::InvalidResponse);
    }
    Ok(DelegatedAuthorization {
        application_id: application_id.clone(),
        proof: crate::domain::auth::OboProof::new(response.access_proof)
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
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.:-".contains(&byte)
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
mod tests {
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
            obo_verify_path: "/api/v1/obo-access/verify".to_owned(),
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
                "principal_id": actor_id,
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

        assert_eq!(authorized.actor.id.as_uuid(), actor_id);
        assert_eq!(authorized.organization_id.as_str(), "acme");
        assert!(authorized.originating_application.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn obo_verification_is_fail_closed_until_request_bound_contract()
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
        assert_eq!(result, Err(IamError::ContractUnavailable));
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
    #[tokio::test]
    async fn upload_delegation_uses_configured_storage_audience_and_exact_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::domain::{
            auth::{AuthorizedActor, DelegatedUploadBinding},
            identity::{Actor, ActorId, ActorKind},
            media::GeneratedAudioFileName,
        };
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        let mut config = settings(&server)?;
        config.app_id = "acme>waveform".to_owned();
        config.audience = "acme>waveform".to_owned();
        let adapter = IamHttpAdapter::new(&config)?.with_storage_audience("acme>storage");
        let filename =
            GeneratedAudioFileName::for_request(time::OffsetDateTime::now_utc(), request_id()?);
        let digest = silicon_iam_client::api::obo::body_sha256(b"exact normalized bytes");
        Mock::given(method("GET")).and(path("/api/v1/obo-access/applications/acme%3Estorage/endpoints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "application":{"app_id":"acme>storage","org_id":"acme"},
                "endpoints":[{"endpoint_id":"briefcase.files.create","path":"/api/v1/obo/files","metadata":{"path":{"type":"string"},"name":{"type":"string"},"content_type":{"type":"string"}}}]
            }))).expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/api/v1/obo-access/exchanges"))
            .and(body_partial_json(json!({"subject_token":"oat_request_subject", "audience":"acme>storage", "endpoint_id":"briefcase.files.create", "request":{"method":"POST","body_sha256":digest}, "metadata":{"name":filename.as_str(),"path":"","content_type":"audio/mpeg"}})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"access_proof":"obo_exact_upload","proof_id":Uuid::new_v4(),"expires_in":60,"expires_at":"2099-01-01T00:00:00Z"})))
            .expect(1).mount(&server).await;
        let proof = adapter
            .delegate(DelegationRequest {
                authorization: AuthorizedActor {
                    actor: Actor::new(ActorKind::Carbon, ActorId::new(Uuid::new_v4())?),
                    organization_id: organization()?,
                    originating_application: None,
                    expires_at: None,
                },
                purpose: DelegationPurpose::StoreGeneratedAudio,
                request_id: request_id()?,
                subject_token: Some(AccessToken::new("oat_request_subject".to_owned())?),
                manifest: None,
                upload: Some(DelegatedUploadBinding {
                    filename,
                    body_sha256: digest,
                }),
            })
            .await?;
        assert_eq!(proof.application_id.as_str(), "acme>waveform");
        assert_eq!(proof.proof.expose_secret(), "obo_exact_upload");
        let requests = server.received_requests().await.ok_or("no requests")?;
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("x-org-id"))
        );
        let exchange = requests
            .iter()
            .find(|r| r.url.path().ends_with("exchanges"))
            .ok_or("missing exchange")?;
        assert!(exchange.headers.contains_key("x-obo-signature"));
        Ok(())
    }
    #[tokio::test]
    async fn production_rejects_a_test_realm_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let actor = Uuid::new_v4();
        let body = json!({"active":true,"principal_id":actor,"actor_type":"carbon","org_id":"acme",
            "scope":"waveform.tts","audience":"waveform","expires_at":4_102_444_800_i64,
            "authorization":{
                "principal_id":actor,"actor_type":"carbon","public_id":"12345678",
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
