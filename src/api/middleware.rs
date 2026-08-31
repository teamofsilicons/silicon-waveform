//! Privacy-safe request correlation, tracing, timeouts, panic isolation, and
//! admission control.

use std::{
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use futures::FutureExt as _;
use http::HeaderValue;
use tokio::sync::Semaphore;
use tracing::Instrument as _;

use super::{error::ApiError, headers};

const REQUEST_ID_HEADER: &str = "x-request-id";
const TTS_PATH: &str = "/api/v1/tts";
const STT_PATH: &str = "/api/v1/stt";
const LIVENESS_PATH: &str = "/health/live";
const READINESS_PATH: &str = "/health/ready";

/// Whole-request limits applied outside admission control.
///
/// Speech handlers retain their configured application deadlines. A small
/// transport allowance bounds request extraction and response construction
/// without subtracting time from the provider workflow.
#[derive(Clone, Copy, Debug)]
pub struct RequestDeadlines {
    tts: Duration,
    stt: Duration,
    other: Duration,
    transport_allowance: Duration,
}

impl RequestDeadlines {
    /// Creates whole-request limits for speech and inexpensive control routes.
    #[must_use]
    pub const fn new(
        tts: Duration,
        stt: Duration,
        other: Duration,
        transport_allowance: Duration,
    ) -> Self {
        Self {
            tts,
            stt,
            other,
            transport_allowance,
        }
    }

    fn for_path(self, path: &str) -> Option<Duration> {
        match path {
            LIVENESS_PATH => None,
            TTS_PATH => Some(self.tts.saturating_add(self.transport_allowance)),
            STT_PATH => Some(self.stt.saturating_add(self.transport_allowance)),
            _ => Some(self.other),
        }
    }
}

/// Independent admission budgets keep operational readiness observable while
/// long-running speech work consumes the public budget.
#[derive(Clone)]
pub struct AdmissionLimiters {
    public: Arc<Semaphore>,
    readiness: Arc<Semaphore>,
}

impl AdmissionLimiters {
    /// Creates separate public and readiness concurrency budgets.
    #[must_use]
    pub fn new(public: usize, readiness: usize) -> Self {
        Self {
            public: Arc::new(Semaphore::new(public)),
            readiness: Arc::new(Semaphore::new(readiness)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdmissionClass {
    Bypass,
    Public,
    Readiness,
}

/// Preserves a valid caller UUID or generates a `UUIDv7`, then propagates it to
/// every response.
pub async fn request_id(mut request: Request<Body>, next: Next) -> Response {
    let request_id = headers::request_id(request.headers());
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        request.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    let mut response = next.run(request).await;
    if !response.headers().contains_key(REQUEST_ID_HEADER)
        && let Ok(value) = HeaderValue::from_str(&request_id.to_string())
    {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response.headers_mut().insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

/// Emits one privacy-safe span and completion event for every HTTP request.
///
/// Only the validated request ID, method, matched route template, response
/// status, and elapsed time are recorded. The raw URI (including its query),
/// headers, and bodies are deliberately never attached to telemetry.
pub async fn trace_request(request: Request<Body>, next: Next) -> Response {
    let request_id = headers::request_id(request.headers());
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("<unmatched>", MatchedPath::as_str)
        .to_owned();
    let started_at = Instant::now();
    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %method,
        route = route.as_str(),
    );

    async move {
        let response = next.run(request).await;
        let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            status = response.status().as_u16(),
            latency_ms,
            "HTTP request completed"
        );
        response
    }
    .instrument(span)
    .await
}

/// Converts handler panics into a content-free error envelope.
pub async fn catch_panic(request: Request<Body>, next: Next) -> Response {
    let request_id = headers::request_id(request.headers());
    if let Ok(response) = AssertUnwindSafe(next.run(request)).catch_unwind().await {
        response
    } else {
        tracing::error!(request_id = %request_id, "request handler panicked");
        ApiError::internal(request_id).into_response()
    }
}

/// Bounds complete requests, including body extraction.
///
/// This middleware must wrap [`admission`] so cancellation drops an acquired
/// semaphore permit even when a client stalls while streaming a request body
/// or an operational dependency check stalls.
pub async fn enforce_request_deadline(
    State(deadlines): State<RequestDeadlines>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(deadline) = deadlines.for_path(request.uri().path()) else {
        return next.run(request).await;
    };
    let request_id = headers::request_id(request.headers());

    match tokio::time::timeout(deadline, next.run(request)).await {
        Ok(response) => response,
        Err(_) => ApiError::request_timeout(request_id).into_response(),
    }
}

/// Rejects excess concurrent requests immediately instead of queueing them.
pub async fn admission(
    State(limiters): State<AdmissionLimiters>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let request_id = headers::request_id(request.headers());
    let limiter = match admission_class(request.uri().path()) {
        AdmissionClass::Bypass => return next.run(request).await,
        AdmissionClass::Public => limiters.public,
        AdmissionClass::Readiness => limiters.readiness,
    };
    let Ok(_permit) = limiter.try_acquire_owned() else {
        return ApiError::overloaded(request_id).into_response();
    };
    next.run(request).await
}

fn admission_class(path: &str) -> AdmissionClass {
    match path {
        LIVENESS_PATH => AdmissionClass::Bypass,
        READINESS_PATH => AdmissionClass::Readiness,
        _ => AdmissionClass::Public,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{Router, middleware as axum_middleware, routing::get};
    use http::{StatusCode, header::CACHE_CONTROL};

    use super::{
        AdmissionClass, AdmissionLimiters, REQUEST_ID_HEADER, RequestDeadlines, admission,
        admission_class, enforce_request_deadline, request_id, trace_request,
    };

    #[test]
    fn operational_routes_do_not_share_the_public_speech_budget() {
        assert_eq!(admission_class("/health/live"), AdmissionClass::Bypass);
        assert_eq!(admission_class("/health/ready"), AdmissionClass::Readiness);
        assert_eq!(admission_class("/api/v1/tts"), AdmissionClass::Public);
    }

    #[test]
    fn request_deadlines_preserve_speech_budget_and_bound_readiness() {
        let deadlines = RequestDeadlines::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            Duration::from_secs(10),
            Duration::from_secs(5),
        );

        assert_eq!(
            deadlines.for_path("/api/v1/tts"),
            Some(Duration::from_secs(305))
        );
        assert_eq!(
            deadlines.for_path("/api/v1/stt"),
            Some(Duration::from_secs(605))
        );
        assert_eq!(
            deadlines.for_path("/api/v1/capabilities"),
            Some(Duration::from_secs(10))
        );
        assert_eq!(deadlines.for_path("/health/live"), None);
        assert_eq!(
            deadlines.for_path("/health/ready"),
            Some(Duration::from_secs(10))
        );
    }

    #[tokio::test]
    async fn timeout_releases_admission_permit_and_finalizes_headers()
    -> Result<(), Box<dyn std::error::Error>> {
        let deadlines = RequestDeadlines::new(
            Duration::from_millis(20),
            Duration::from_millis(20),
            Duration::from_millis(20),
            Duration::ZERO,
        );
        let router = Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    StatusCode::OK
                }),
            )
            .route("/fast", get(|| async { StatusCode::OK }))
            .layer(axum_middleware::from_fn_with_state(
                AdmissionLimiters::new(1, 1),
                admission,
            ))
            .layer(axum_middleware::from_fn_with_state(
                deadlines,
                enforce_request_deadline,
            ))
            .layer(axum_middleware::from_fn(trace_request))
            .layer(axum_middleware::from_fn(request_id));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let client = reqwest::Client::builder().no_proxy().build()?;

        let timed_out = client.get(format!("http://{address}/slow")).send().await?;
        assert_eq!(timed_out.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            timed_out
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert!(timed_out.headers().contains_key(REQUEST_ID_HEADER));

        let next_request = client.get(format!("http://{address}/fast")).send().await?;
        assert_eq!(next_request.status(), StatusCode::OK);
        assert_eq!(
            next_request
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );

        server.abort();
        let server_result = server.await;
        assert!(matches!(server_result, Err(error) if error.is_cancelled()));
        Ok(())
    }
}
