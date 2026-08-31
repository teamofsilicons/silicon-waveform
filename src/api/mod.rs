//! Axum delivery layer for the public and operational Waveform APIs.

pub mod error;
pub mod headers;
pub mod middleware;
pub mod model;

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    middleware as axum_middleware,
    response::{IntoResponse as _, Response},
    routing::{get, post},
};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use serde::Serialize;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;

use crate::{
    application::{
        ports::{AudioNormalizer, IdempotencyStore},
        service::{ServiceResponse, SpeechRequestContext, WaveformService},
    },
    config::ServerSettings,
    domain::{
        capabilities::Capabilities,
        identity::RequestId,
        speech::{SpeechValidationError, SttResult, TtsResult},
    },
};

use self::{
    error::ApiError,
    headers::{HeaderError, SpeechHeaders},
    model::{
        CapabilitiesResponseBody, RequestBodyError, SttRequestBody, SttResponseBody,
        TtsRequestBody, TtsResponseBody,
    },
};

const REQUEST_ID_HEADER: &str = "x-request-id";
const REPLAYED_HEADER: &str = "idempotency-replayed";
const READINESS_MAX_IN_FLIGHT: usize = 8;
const PUBLIC_CONTROL_DEADLINE: Duration = Duration::from_secs(10);
const TRANSPORT_DEADLINE_ALLOWANCE: Duration = Duration::from_secs(5);

/// Dependencies and deadlines shared by request handlers.
#[derive(Clone)]
pub struct ApiState {
    service: Arc<WaveformService>,
    readiness: ReadinessChecks,
    tts_deadline: Duration,
    stt_deadline: Duration,
}

impl ApiState {
    /// Constructs immutable handler state.
    #[must_use]
    pub const fn new(
        service: Arc<WaveformService>,
        readiness: ReadinessChecks,
        tts_deadline: Duration,
        stt_deadline: Duration,
    ) -> Self {
        Self {
            service,
            readiness,
            tts_deadline,
            stt_deadline,
        }
    }
}

/// Non-billable authoritative readiness checks.
#[derive(Clone)]
pub struct ReadinessChecks {
    idempotency: Arc<dyn IdempotencyStore>,
    audio_normalizer: Arc<dyn AudioNormalizer>,
    provider_chains_configured: bool,
    dependency_contracts_available: bool,
}

impl ReadinessChecks {
    /// Constructs checks with composition-time provider and contract status.
    #[must_use]
    pub const fn new(
        idempotency: Arc<dyn IdempotencyStore>,
        audio_normalizer: Arc<dyn AudioNormalizer>,
        provider_chains_configured: bool,
        dependency_contracts_available: bool,
    ) -> Self {
        Self {
            idempotency,
            audio_normalizer,
            provider_chains_configured,
            dependency_contracts_available,
        }
    }

    async fn is_ready(&self) -> bool {
        if !self.provider_chains_configured || !self.dependency_contracts_available {
            return false;
        }
        let (database, audio) = tokio::join!(
            self.idempotency.check_ready(),
            self.audio_normalizer.check_ready()
        );
        database.is_ok() && audio.is_ok()
    }
}

/// Builds the complete public router with bounded admission and request bodies.
pub fn router(state: ApiState, server: &ServerSettings) -> Router {
    let limiters =
        middleware::AdmissionLimiters::new(server.max_in_flight.get(), READINESS_MAX_IN_FLIGHT);
    let request_deadlines = middleware::RequestDeadlines::new(
        server.tts_deadline,
        server.stt_deadline,
        PUBLIC_CONTROL_DEADLINE,
        TRANSPORT_DEADLINE_ALLOWANCE,
    );
    let sensitive_headers = [
        header::AUTHORIZATION,
        HeaderName::from_static("x-iam-obo-access-proof"),
        HeaderName::from_static("idempotency-key"),
    ];

    Router::new()
        .route("/api/v1/tts", post(synthesize))
        .route("/api/v1/stt", post(transcribe))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness))
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found)
        .with_state(state)
        .layer(DefaultBodyLimit::max(server.json_body_limit))
        .layer(axum_middleware::from_fn_with_state(
            limiters,
            middleware::admission,
        ))
        .layer(axum_middleware::from_fn_with_state(
            request_deadlines,
            middleware::enforce_request_deadline,
        ))
        .layer(axum_middleware::from_fn(middleware::catch_panic))
        .layer(axum_middleware::from_fn(middleware::trace_request))
        .layer(SetSensitiveRequestHeadersLayer::new(sensitive_headers))
        .layer(axum_middleware::from_fn(middleware::request_id))
}

async fn synthesize(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Json<TtsRequestBody>, JsonRejection>,
) -> Response {
    let candidate_request_id = headers::request_id(&headers);
    let headers = match SpeechHeaders::parse(&headers) {
        Ok(headers) => headers,
        Err(error) => return header_error(&error, candidate_request_id).into_response(),
    };
    let request = match body {
        Ok(Json(body)) => match body.into_domain() {
            Ok(request) => request,
            Err(error) => return body_error(&error, headers.request_id).into_response(),
        },
        Err(error) => return json_error(&error, headers.request_id).into_response(),
    };
    let request_id = headers.request_id;
    let context = SpeechRequestContext {
        request_id,
        organization_id: headers.organization_id,
        credentials: headers.credentials,
        idempotency_key: headers.idempotency_key,
    };

    match tokio::time::timeout(
        state.tts_deadline,
        state.service.synthesize(context, request),
    )
    .await
    {
        Ok(Ok(response)) => tts_success(response),
        Ok(Err(error)) => ApiError::from_waveform(error, request_id).into_response(),
        Err(_) => ApiError::request_timeout(request_id).into_response(),
    }
}

async fn transcribe(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Json<SttRequestBody>, JsonRejection>,
) -> Response {
    let candidate_request_id = headers::request_id(&headers);
    let headers = match SpeechHeaders::parse(&headers) {
        Ok(headers) => headers,
        Err(error) => return header_error(&error, candidate_request_id).into_response(),
    };
    let request = match body {
        Ok(Json(body)) => match body.into_domain() {
            Ok(request) => request,
            Err(error) => return body_error(&error, headers.request_id).into_response(),
        },
        Err(error) => return json_error(&error, headers.request_id).into_response(),
    };
    let request_id = headers.request_id;
    let context = SpeechRequestContext {
        request_id,
        organization_id: headers.organization_id,
        credentials: headers.credentials,
        idempotency_key: headers.idempotency_key,
    };

    match tokio::time::timeout(
        state.stt_deadline,
        state.service.transcribe(context, request),
    )
    .await
    {
        Ok(Ok(response)) => stt_success(response),
        Ok(Err(error)) => ApiError::from_waveform(error, request_id).into_response(),
        Err(_) => ApiError::request_timeout(request_id).into_response(),
    }
}

async fn capabilities(headers: HeaderMap) -> Response {
    let request_id = headers::request_id(&headers);
    let mut response =
        Json(CapabilitiesResponseBody::from(Capabilities::current())).into_response();
    insert_request_id(&mut response, request_id);
    response
}

async fn liveness(headers: HeaderMap) -> Response {
    let request_id = headers::request_id(&headers);
    health_response(StatusCode::OK, "live", request_id)
}

async fn readiness(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let request_id = headers::request_id(&headers);
    if state.readiness.is_ready().await {
        health_response(StatusCode::OK, "ready", request_id)
    } else {
        ApiError::not_ready(request_id).into_response()
    }
}

async fn not_found(headers: HeaderMap) -> Response {
    ApiError::not_found(headers::request_id(&headers)).into_response()
}

async fn method_not_allowed(headers: HeaderMap) -> Response {
    ApiError::method_not_allowed(headers::request_id(&headers)).into_response()
}

fn tts_success(response: ServiceResponse<TtsResult>) -> Response {
    let request_id = response.result.request_id;
    success_response(
        TtsResponseBody::from(response.result),
        request_id,
        response.replayed,
    )
}

fn stt_success(response: ServiceResponse<SttResult>) -> Response {
    let request_id = response.result.request_id;
    success_response(
        SttResponseBody::from(response.result),
        request_id,
        response.replayed,
    )
}

fn success_response<T: Serialize>(body: T, request_id: RequestId, replayed: bool) -> Response {
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        REPLAYED_HEADER,
        HeaderValue::from_static(if replayed { "true" } else { "false" }),
    );
    insert_request_id(&mut response, request_id);
    response
}

fn health_response(status: StatusCode, value: &'static str, request_id: RequestId) -> Response {
    let mut response = (status, Json(HealthBody { status: value })).into_response();
    insert_request_id(&mut response, request_id);
    response
}

fn insert_request_id(response: &mut Response, request_id: RequestId) {
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
}

fn header_error(error: &HeaderError, request_id: RequestId) -> ApiError {
    match error {
        HeaderError::MissingCredentials
        | HeaderError::InvalidAuthorization
        | HeaderError::InvalidCredential(_) => ApiError::unauthenticated(request_id),
        HeaderError::MissingHeader(_)
        | HeaderError::InvalidHeader(_)
        | HeaderError::DuplicateHeader(_)
        | HeaderError::IncompleteOboCredentials
        | HeaderError::AmbiguousCredentials
        | HeaderError::InvalidIdentity(_)
        | HeaderError::InvalidIdempotencyKey => ApiError::invalid_request(request_id),
    }
}

fn body_error(error: &RequestBodyError, request_id: RequestId) -> ApiError {
    match error {
        RequestBodyError::Speech(SpeechValidationError::TextTooLong { .. }) => {
            ApiError::payload_too_large(request_id)
        }
        RequestBodyError::Speech(_)
        | RequestBodyError::Language(_)
        | RequestBodyError::Media(_)
        | RequestBodyError::InvalidFileUrl => ApiError::invalid_request(request_id),
    }
}

fn json_error(error: &JsonRejection, request_id: RequestId) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::payload_too_large(request_id)
    } else {
        ApiError::invalid_request(request_id)
    }
}

#[derive(Debug, Serialize)]
struct HealthBody {
    status: &'static str,
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use http::HeaderValue;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{REPLAYED_HEADER, REQUEST_ID_HEADER, success_response};
    use crate::domain::identity::RequestId;

    #[tokio::test]
    async fn successful_replay_uses_the_canonical_id_in_header_and_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let canonical_request_id = RequestId::new(Uuid::from_u128(42))?;
        let response = success_response(
            json!({ "request_id": canonical_request_id }),
            canonical_request_id,
            true,
        );
        let canonical_request_id_text = canonical_request_id.to_string();
        let canonical_request_id_header = HeaderValue::from_str(&canonical_request_id_text)?;

        assert_eq!(
            response.headers().get(REQUEST_ID_HEADER),
            Some(&canonical_request_id_header)
        );
        assert_eq!(
            response.headers().get(REPLAYED_HEADER),
            Some(&HeaderValue::from_static("true"))
        );
        let body = to_bytes(response.into_body(), 1_024).await?;
        let body = serde_json::from_slice::<Value>(&body)?;
        assert_eq!(
            body.get("request_id").and_then(Value::as_str),
            Some(canonical_request_id_text.as_str())
        );
        Ok(())
    }
}
