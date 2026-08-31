//! Online, fail-closed Silicon IAM authorization adapter.
//!
//! ## Wire assumptions
//!
//! This module implements only IAM operations published in IAM's current API
//! documentation and `OpenAPI` contract:
//!
//! - bearer tokens are posted as `application/x-www-form-urlencoded` with the
//!   `token` and `token_type_hint=access_token` fields;
//! - Waveform authenticates to IAM with HTTP Basic using its application ID and
//!   secret;
//! - OBO proofs are consumed with JSON fields `access_proof`, `audience`,
//!   `action`, and an omitted resource, plus `X-Org-ID` and a stable
//!   idempotency key;
//! - introspection returns `principal_id`, `actor_type`, `org_id`, `scope`,
//!   `audience`, and an optional Unix `expires_at`; OBO verification returns the
//!   documented nested actor and RFC 3339 timestamps.
//!
//! Waveform deliberately does not implement downstream proof exchange here.
//! The application retains a verified actor, not a reusable actor subject
//! token, so neither a bearer token nor an inbound Waveform-audience proof can
//! safely be repurposed for Briefcase. `IamPort::delegate` therefore fails with
//! `IamError::ContractUnavailable` until IAM publishes a safe actor-context
//! exchange that Waveform can call without forwarding the inbound credential.

use std::{fmt, str::FromStr as _, time::Duration};

use async_trait::async_trait;
use futures::StreamExt as _;
use http::{StatusCode, header};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

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

const MAX_IAM_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_IAM_RESPONSE_BYTES_U64: u64 = 64 * 1_024;
const MAX_OBO_LIFETIME: time::Duration = time::Duration::seconds(60);
const MAX_IAM_CLOCK_SKEW: time::Duration = time::Duration::seconds(5);
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
    client: reqwest::Client,
    introspection_url: Url,
    obo_verify_url: Url,
    application_id: ApplicationId,
    application_secret: SecretString,
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
        let introspection_url = endpoint(&settings.base_url, &settings.token_introspection_path)?;
        let obo_verify_url = endpoint(&settings.base_url, &settings.obo_verify_path)?;
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
        let client = reqwest::Client::builder()
            .connect_timeout(settings.timeout.min(Duration::from_secs(5)))
            .timeout(settings.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(8)
            .tcp_keepalive(Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| IamAdapterBuildError::HttpClient)?;

        Ok(Self {
            client,
            introspection_url,
            obo_verify_url,
            application_id,
            application_secret: settings.app_secret.clone(),
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
        let form = IntrospectionForm {
            token: token.expose_secret(),
            token_type_hint: "access_token",
        };
        let outbound = self
            .authenticated(self.client.post(self.introspection_url.clone()))
            .header("X-Org-ID", request.organization_id.as_str())
            .header("X-Request-ID", request.request_id.to_string())
            .header(header::ACCEPT, "application/json")
            .form(&form);
        let response: TokenIntrospection = self.execute_json(outbound).await?;
        self.validate_introspection(response, request)
    }

    async fn authorize_obo(
        &self,
        request: &AuthorizationRequest,
        credentials: &OboCredentials,
    ) -> Result<AuthorizedActor, IamError> {
        let action = self.action(request.action);
        let body = OboVerifyBody {
            access_proof: credentials.proof.expose_secret(),
            audience: self.audience.as_str(),
            action,
            resource: None,
        };
        let idempotency_key = format!("waveform:iam:verify:{}", request.request_id);
        let outbound = self
            .authenticated(self.client.post(self.obo_verify_url.clone()))
            .header("X-Org-ID", request.organization_id.as_str())
            .header("X-Request-ID", request.request_id.to_string())
            .header("Idempotency-Key", idempotency_key)
            .header(header::ACCEPT, "application/json")
            .json(&body);
        let response: OboVerification = self.execute_json(outbound).await?;
        self.validate_obo(&response, credentials, request)
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.basic_auth(
            self.application_id.as_str(),
            Some(self.application_secret.expose_secret()),
        )
    }

    async fn execute_json<T>(&self, request: reqwest::RequestBuilder) -> Result<T, IamError>
    where
        T: DeserializeOwned,
    {
        let operation = async {
            let response = request
                .send()
                .await
                .map_err(|error| map_reqwest_error(&error))?;
            if response.status() != StatusCode::OK {
                return Err(map_status(response.status()));
            }
            if !is_json(response.headers()) {
                return Err(IamError::InvalidResponse);
            }
            let body = read_bounded(response).await?;
            serde_json::from_slice(&body).map_err(|_| IamError::InvalidResponse)
        };

        tokio::time::timeout(self.timeout, operation)
            .await
            .map_err(|_| IamError::Timeout)?
    }

    fn validate_introspection(
        &self,
        response: TokenIntrospection,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizedActor, IamError> {
        if !response.active {
            return Err(IamError::InvalidCredential);
        }

        let principal_id = response.principal_id.ok_or(IamError::InvalidResponse)?;
        let actor_kind = actor_kind(response.actor_type.as_deref())?;
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

    fn validate_obo(
        &self,
        response: &OboVerification,
        credentials: &OboCredentials,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizedActor, IamError> {
        if !response.valid || response.proof_id.is_nil() {
            return Err(IamError::InvalidCredential);
        }
        if response.issuer_app_id != credentials.application_id.as_str()
            || response.audience != self.audience.as_str()
        {
            return Err(IamError::InvalidCredential);
        }
        if response.action != self.action(request.action) || response.resource.is_some() {
            return Err(IamError::Forbidden);
        }

        let organization = required_organization(Some(&response.org_id))?;
        if organization != request.organization_id {
            return Err(IamError::OrganizationMismatch);
        }
        let now = OffsetDateTime::now_utc();
        if !valid_obo_window(response.consumed_at, response.expires_at, now) {
            return Err(IamError::InvalidCredential);
        }
        if response.actor.public_id.trim().is_empty() {
            return Err(IamError::InvalidResponse);
        }

        let actor_kind = actor_kind(Some(&response.actor.kind))?;
        let actor_id =
            ActorId::new(response.actor.principal_id).map_err(|_| IamError::InvalidResponse)?;
        Ok(AuthorizedActor {
            actor: Actor::new(actor_kind, actor_id),
            organization_id: organization,
            originating_application: Some(credentials.application_id.clone()),
            expires_at: Some(response.expires_at),
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
        _request: DelegationRequest,
    ) -> Result<DelegatedAuthorization, IamError> {
        Err(IamError::ContractUnavailable)
    }
}

#[derive(Serialize)]
struct IntrospectionForm<'a> {
    token: &'a str,
    token_type_hint: &'static str,
}

#[derive(Debug, Deserialize)]
struct TokenIntrospection {
    #[serde(default)]
    active: bool,
    principal_id: Option<Uuid>,
    actor_type: Option<String>,
    org_id: Option<String>,
    scope: Option<String>,
    audience: Option<String>,
    expires_at: Option<i64>,
}

#[derive(Serialize)]
struct OboVerifyBody<'a> {
    access_proof: &'a str,
    audience: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct OboVerification {
    valid: bool,
    proof_id: Uuid,
    issuer_app_id: String,
    audience: String,
    actor: IamActor,
    org_id: String,
    action: String,
    resource: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    consumed_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
struct IamActor {
    principal_id: Uuid,
    #[serde(rename = "type")]
    kind: String,
    public_id: String,
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

fn actor_kind(value: Option<&str>) -> Result<ActorKind, IamError> {
    match value {
        Some("carbon") => Ok(ActorKind::Carbon),
        Some("silicon") => Ok(ActorKind::Silicon),
        Some("application" | "service") => Err(IamError::Forbidden),
        Some(_) | None => Err(IamError::InvalidResponse),
    }
}

fn contains_scope(scope: &str, required: &str) -> bool {
    scope
        .split_ascii_whitespace()
        .any(|candidate| candidate == required)
}

fn valid_obo_window(
    consumed_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    now: OffsetDateTime,
) -> bool {
    let proof_lifetime = expires_at - consumed_at;
    proof_lifetime > time::Duration::ZERO
        && proof_lifetime <= MAX_OBO_LIFETIME
        && consumed_at <= now + MAX_IAM_CLOCK_SKEW
        && expires_at > now - MAX_IAM_CLOCK_SKEW
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

fn is_json(headers: &http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<mime::Mime>().ok())
        .is_some_and(|content_type| {
            content_type.essence_str() == mime::APPLICATION_JSON.essence_str()
        })
}

async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>, IamError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_IAM_RESPONSE_BYTES_U64)
    {
        return Err(IamError::InvalidResponse);
    }

    let mut body = Vec::with_capacity(8 * 1_024);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| map_reqwest_error(&error))?;
        let next_length = body
            .len()
            .checked_add(chunk.len())
            .ok_or(IamError::InvalidResponse)?;
        if next_length > MAX_IAM_RESPONSE_BYTES {
            return Err(IamError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn map_reqwest_error(error: &reqwest::Error) -> IamError {
    if error.is_timeout() {
        IamError::Timeout
    } else {
        IamError::Unavailable
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
    use time::{OffsetDateTime, macros::datetime};
    use url::Url;
    use uuid::Uuid;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string_contains, header, method, path},
    };

    use super::{IamHttpAdapter, MAX_IAM_RESPONSE_BYTES, map_status, valid_obo_window};
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

    #[test]
    fn obo_window_uses_iam_interval_and_bounded_clock_skew() {
        let now = datetime!(2026-08-31 12:00:00 UTC);

        assert!(valid_obo_window(
            now + time::Duration::seconds(3),
            now + time::Duration::seconds(60),
            now,
        ));
        assert!(!valid_obo_window(
            now,
            now + time::Duration::seconds(61),
            now,
        ));
        assert!(!valid_obo_window(
            now + time::Duration::seconds(6),
            now + time::Duration::seconds(30),
            now,
        ));
        assert!(!valid_obo_window(
            now - time::Duration::seconds(60),
            now - time::Duration::seconds(5),
            now,
        ));
    }

    fn settings(server: &MockServer) -> Result<IamSettings, url::ParseError> {
        Ok(IamSettings {
            base_url: Url::parse(&server.uri())?,
            app_id: "waveform".to_owned(),
            app_secret: SecretString::from("unit-test-app-secret".to_owned()),
            token_introspection_path: "/api/v1/auth/tokens/introspect".to_owned(),
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
            .and(path("/api/v1/auth/tokens/introspect"))
            .and(header("authorization", basic_authorization()))
            .and(header("x-org-id", "acme"))
            .and(header("x-request-id", request_id.to_string()))
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
    async fn obo_verification_binds_issuer_audience_action_and_org()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let actor_id = Uuid::new_v4();
        let proof_id = Uuid::new_v4();
        let request_id = request_id()?;
        let now = OffsetDateTime::now_utc();
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/verify"))
            .and(header("authorization", basic_authorization()))
            .and(header("x-org-id", "acme"))
            .and(header(
                "idempotency-key",
                format!("waveform:iam:verify:{request_id}"),
            ))
            .and(body_json(json!({
                "access_proof": "obo_opaque-proof",
                "audience": "waveform",
                "action": "waveform.stt"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "valid": true,
                "proof_id": proof_id,
                "issuer_app_id": "calling-app",
                "audience": "waveform",
                "actor": {
                    "principal_id": actor_id,
                    "type": "silicon",
                    "public_id": "silicon-one"
                },
                "org_id": "acme",
                "action": "waveform.stt",
                "resource": null,
                "expires_at": (now + time::Duration::seconds(30))
                    .format(&time::format_description::well_known::Rfc3339)?,
                "consumed_at": now.format(&time::format_description::well_known::Rfc3339)?
            })))
            .expect(1)
            .mount(&server)
            .await;
        let adapter = IamHttpAdapter::new(&settings(&server)?)?;
        let originating_application = ApplicationId::from_str("calling-app")?;
        let authorized = adapter
            .authorize(AuthorizationRequest {
                credentials: InboundCredentials::OnBehalfOf(OboCredentials {
                    application_id: originating_application.clone(),
                    proof: OboProof::new("obo_opaque-proof".to_owned())?,
                }),
                organization_id: organization()?,
                action: WaveformAction::TranscribeSpeech,
                request_id,
            })
            .await?;

        assert_eq!(authorized.actor.id.as_uuid(), actor_id);
        assert_eq!(
            authorized.originating_application,
            Some(originating_application)
        );
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_organization_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/tokens/introspect"))
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
            .and(path("/api/v1/auth/tokens/introspect"))
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
                request_id: request_id()?,
            })
            .await;

        assert!(matches!(result, Err(IamError::ContractUnavailable)));
        Ok(())
    }
}
