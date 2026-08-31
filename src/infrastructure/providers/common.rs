use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderMap, StatusCode};
use secrecy::SecretString;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

/// Default first-retry backoff before bounded jitter is applied.
pub const PROVIDER_RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
/// Maximum random jitter added to a transient retry delay.
pub const PROVIDER_RETRY_MAX_JITTER: Duration = Duration::from_millis(100);
/// Maximum accepted wait from a provider `Retry-After` header.
pub const PROVIDER_RETRY_AFTER_CAP: Duration = Duration::from_secs(2);
const MIN_RETRY_EXCHANGE_BUDGET: Duration = Duration::from_millis(50);

/// At most one explicitly configured retry is allowed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransientRetry {
    /// Do not retry chargeable provider requests.
    Disabled,
    /// Retry one transient failure after this base delay plus bounded jitter.
    Once {
        /// Base delay before the bounded random jitter is added.
        delay: Duration,
    },
}

/// Shared HTTP and resource bounds for one provider.
#[derive(Clone, Debug)]
pub struct ProviderHttpConfig {
    /// Provider origin or test server base URL.
    pub base_url: Url,
    /// Total timeout for the initial exchange, optional delay, and one retry.
    pub timeout: Duration,
    /// Largest successful response body retained in memory.
    pub max_response_bytes: usize,
    /// Maximum simultaneous logical provider attempts.
    pub max_concurrency: usize,
    /// Optional single transient retry.
    pub retry: TransientRetry,
}

impl ProviderHttpConfig {
    /// Creates conservative defaults for synchronous provider calls.
    #[must_use]
    pub const fn new(base_url: Url) -> Self {
        Self {
            base_url,
            timeout: Duration::from_secs(20),
            max_response_bytes: 32 * 1024 * 1024,
            max_concurrency: 8,
            retry: TransientRetry::Once {
                delay: PROVIDER_RETRY_BASE_DELAY,
            },
        }
    }
}

/// Stable, body-free provider failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderErrorKind {
    /// The provider semaphore had no immediately available permit.
    Saturated,
    /// A bounded operation timed out.
    Timeout,
    /// The network exchange failed.
    Network,
    /// The provider rate limited the attempt.
    RateLimited,
    /// The provider rejected this otherwise locally valid request.
    Rejected,
    /// Credentials, access, or provider configuration are invalid.
    Configuration,
    /// A successful response was malformed, empty, or oversized.
    InvalidResponse,
    /// The provider returned a transient server failure.
    Unavailable,
}

/// Provider failure that intentionally excludes private response bodies.
#[derive(Clone, Copy, Debug, Error)]
#[error("provider request failed: {kind:?}")]
pub struct ProviderError {
    /// Stable failure category used by fallback policy.
    pub kind: ProviderErrorKind,
}

impl ProviderError {
    pub(crate) const fn new(kind: ProviderErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn invalid_response() -> Self {
        Self::new(ProviderErrorKind::InvalidResponse)
    }

    fn is_retryable(self) -> bool {
        matches!(
            self.kind,
            ProviderErrorKind::Timeout
                | ProviderErrorKind::Network
                | ProviderErrorKind::RateLimited
                | ProviderErrorKind::Unavailable
        )
    }
}

/// HTTP response retained only after status classification and byte bounding.
pub(crate) struct BoundedResponse {
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

/// One retry token shared by every retryable exchange in a logical provider attempt.
#[derive(Debug, Default)]
pub(crate) struct TransientRetryBudget {
    consumed: bool,
}

impl TransientRetryBudget {
    pub(crate) const fn new() -> Self {
        Self { consumed: false }
    }

    fn consume(&mut self) -> bool {
        if self.consumed {
            false
        } else {
            self.consumed = true;
            true
        }
    }
}

struct AttemptFailure {
    error: ProviderError,
    retry_after: Option<Duration>,
}

impl AttemptFailure {
    const fn new(error: ProviderError) -> Self {
        Self {
            error,
            retry_after: None,
        }
    }
}

/// Shared provider runtime. A logical attempt acquires its semaphore only once.
#[derive(Clone, Debug)]
pub struct ProviderRuntime {
    pub(crate) client: reqwest::Client,
    pub(crate) api_key: SecretString,
    pub(crate) config: ProviderHttpConfig,
    semaphore: Arc<Semaphore>,
}

impl ProviderRuntime {
    /// Creates a provider runtime from an already hardened Reqwest client.
    #[must_use]
    pub fn new(client: reqwest::Client, api_key: SecretString, config: ProviderHttpConfig) -> Self {
        let semaphore = Self::shared_semaphore(config.max_concurrency);
        Self::new_with_semaphore(client, api_key, config, semaphore)
    }

    pub(crate) fn new_with_semaphore(
        client: reqwest::Client,
        api_key: SecretString,
        config: ProviderHttpConfig,
        semaphore: Arc<Semaphore>,
    ) -> Self {
        Self {
            client,
            api_key,
            config,
            semaphore,
        }
    }

    pub(crate) fn shared_semaphore(max_concurrency: usize) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(max_concurrency))
    }

    pub(crate) fn try_acquire(&self) -> Result<OwnedSemaphorePermit, ProviderError> {
        Arc::clone(&self.semaphore)
            .try_acquire_owned()
            .map_err(|_| ProviderError::new(ProviderErrorKind::Saturated))
    }

    pub(crate) fn url(&self, endpoint: &str) -> Result<Url, ProviderError> {
        let endpoint_segments = endpoint
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if endpoint_segments.is_empty()
            || endpoint_segments
                .iter()
                .any(|segment| matches!(*segment, "." | ".."))
        {
            return Err(ProviderError::new(ProviderErrorKind::Configuration));
        }

        let mut base_segments = self
            .config
            .base_url
            .path_segments()
            .ok_or_else(|| ProviderError::new(ProviderErrorKind::Configuration))?
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if endpoint_segments
            .iter()
            .any(|segment| is_api_version(segment))
            && base_segments
                .last()
                .is_some_and(|segment| is_api_version(segment))
        {
            base_segments.pop();
        }

        let mut url = self.config.base_url.clone();
        url.set_query(None);
        url.set_fragment(None);
        {
            let mut output = url
                .path_segments_mut()
                .map_err(|()| ProviderError::new(ProviderErrorKind::Configuration))?;
            output.clear();
            output.extend(base_segments);
            output.extend(endpoint_segments);
        }
        Ok(url)
    }

    pub(crate) async fn execute(
        &self,
        build: impl Fn() -> Result<reqwest::RequestBuilder, ProviderError>,
    ) -> Result<BoundedResponse, ProviderError> {
        let mut retry_budget = TransientRetryBudget::new();
        self.execute_with_retry_budget(&mut retry_budget, build)
            .await
    }

    pub(crate) async fn execute_with_retry_budget(
        &self,
        retry_budget: &mut TransientRetryBudget,
        build: impl Fn() -> Result<reqwest::RequestBuilder, ProviderError>,
    ) -> Result<BoundedResponse, ProviderError> {
        let deadline = tokio::time::Instant::now() + self.config.timeout;
        let operation = async {
            loop {
                let result = self.execute_once(build()?).await;
                match (result, self.config.retry) {
                    (Ok(response), _) => return Ok(response),
                    (Err(failure), TransientRetry::Once { delay })
                        if failure.error.is_retryable() && retry_budget.consume() =>
                    {
                        let delay = retry_delay(delay, failure.retry_after);
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        if remaining <= delay.saturating_add(MIN_RETRY_EXCHANGE_BUDGET) {
                            return Err(failure.error);
                        }
                        tokio::time::sleep(delay).await;
                    }
                    (Err(failure), _) => return Err(failure.error),
                }
            }
        };
        tokio::time::timeout_at(deadline, operation)
            .await
            .map_err(|_| ProviderError::new(ProviderErrorKind::Timeout))?
    }

    /// Executes one HTTP exchange without retrying a potentially non-idempotent request.
    pub(crate) async fn execute_without_retry(
        &self,
        build: impl Fn() -> Result<reqwest::RequestBuilder, ProviderError>,
    ) -> Result<BoundedResponse, ProviderError> {
        tokio::time::timeout(self.config.timeout, async {
            self.execute_once(build()?)
                .await
                .map_err(|failure| failure.error)
        })
        .await
        .map_err(|_| ProviderError::new(ProviderErrorKind::Timeout))?
    }

    async fn execute_once(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<BoundedResponse, AttemptFailure> {
        let response = request
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))
            .map_err(AttemptFailure::new)?;
        let status = response.status();
        if !status.is_success() {
            return Err(AttemptFailure {
                error: classify_status(status),
                retry_after: retry_after(response.headers()),
            });
        }
        let headers = response.headers().clone();
        let body = read_response_body(response, self.config.max_response_bytes)
            .await
            .map_err(AttemptFailure::new)?;
        Ok(BoundedResponse { headers, body })
    }
}

fn retry_delay(base: Duration, retry_after: Option<Duration>) -> Duration {
    let base = base.min(PROVIDER_RETRY_AFTER_CAP);
    let max_jitter_millis =
        u64::try_from(PROVIDER_RETRY_MAX_JITTER.as_millis()).unwrap_or(u64::MAX);
    let jitter = if max_jitter_millis == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(rand::random_range(0..=max_jitter_millis))
    };
    base.saturating_add(jitter)
        .min(PROVIDER_RETRY_AFTER_CAP)
        .max(
            retry_after
                .unwrap_or_default()
                .min(PROVIDER_RETRY_AFTER_CAP),
        )
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(PROVIDER_RETRY_AFTER_CAP));
    }
    let target =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc2822).ok()?;
    let delay =
        std::time::Duration::try_from(target - time::OffsetDateTime::now_utc()).unwrap_or_default();
    Some(delay.min(PROVIDER_RETRY_AFTER_CAP))
}

fn is_api_version(segment: &str) -> bool {
    let Some(suffix) = segment.strip_prefix('v') else {
        return false;
    };
    !suffix.is_empty()
        && suffix
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn classify_reqwest_error(error: &reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::new(ProviderErrorKind::Timeout)
    } else {
        ProviderError::new(ProviderErrorKind::Network)
    }
}

fn classify_status(status: StatusCode) -> ProviderError {
    let kind = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderErrorKind::Configuration,
        StatusCode::REQUEST_TIMEOUT => ProviderErrorKind::Timeout,
        StatusCode::TOO_MANY_REQUESTS => ProviderErrorKind::RateLimited,
        value if value.is_server_error() => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::Rejected,
    };
    ProviderError::new(kind)
}

async fn read_response_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, ProviderError> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(limit).unwrap_or(u64::MAX))
    {
        return Err(ProviderError::invalid_response());
    }

    let mut stream = response.bytes_stream();
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| classify_reqwest_error(&error))?;
        let next_len = output
            .len()
            .checked_add(chunk.len())
            .ok_or_else(ProviderError::invalid_response)?;
        if next_len > limit {
            return Err(ProviderError::invalid_response());
        }
        output.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(output))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use secrecy::SecretString;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    use super::*;

    #[test]
    fn logical_retry_budget_can_be_consumed_only_once() {
        let mut budget = TransientRetryBudget::new();

        assert!(budget.consume());
        assert!(!budget.consume());
    }

    #[tokio::test]
    async fn bounds_streamed_success_bodies() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0_u8; 9]))
            .mount(&server)
            .await;
        let Some(base_url) = Url::parse(&server.uri()).ok() else {
            return;
        };
        let mut config = ProviderHttpConfig::new(base_url);
        config.max_response_bytes = 8;
        let runtime = ProviderRuntime::new(
            reqwest::Client::new(),
            SecretString::from("test-key".to_owned()),
            config,
        );
        let url = runtime.url("health").ok();

        let result = match url {
            Some(url) => {
                runtime
                    .execute(|| Ok(runtime.client.get(url.clone())))
                    .await
            }
            None => return,
        };

        assert!(matches!(
            result,
            Err(ProviderError {
                kind: ProviderErrorKind::InvalidResponse
            })
        ));
    }

    #[tokio::test]
    async fn retries_at_most_once_when_explicitly_enabled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;
        let parsed = Url::parse(&server.uri());
        let Some(base_url) = parsed.ok() else {
            return;
        };
        let mut config = ProviderHttpConfig::new(base_url);
        config.retry = TransientRetry::Once {
            delay: Duration::ZERO,
        };
        let runtime = Arc::new(ProviderRuntime::new(
            reqwest::Client::new(),
            SecretString::from("test-key".to_owned()),
            config,
        ));
        let Some(url) = runtime.url("health").ok() else {
            return;
        };

        let result = runtime
            .execute(|| Ok(runtime.client.get(url.clone())))
            .await;

        assert!(matches!(
            result,
            Err(ProviderError {
                kind: ProviderErrorKind::Unavailable
            })
        ));
    }

    #[tokio::test]
    async fn skips_retry_when_the_total_budget_cannot_fit_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let Ok(base_url) = Url::parse(&server.uri()) else {
            return;
        };
        let mut config = ProviderHttpConfig::new(base_url);
        config.timeout = Duration::from_millis(20);
        config.retry = TransientRetry::Once {
            delay: Duration::from_millis(100),
        };
        let runtime = ProviderRuntime::new(
            reqwest::Client::new(),
            SecretString::from("test-key".to_owned()),
            config,
        );
        let Ok(url) = runtime.url("health") else {
            return;
        };

        let result = runtime
            .execute(|| Ok(runtime.client.get(url.clone())))
            .await;

        assert!(matches!(
            result,
            Err(ProviderError {
                kind: ProviderErrorKind::Unavailable
            })
        ));
    }

    #[test]
    fn caps_retry_after_and_jitter() {
        assert_eq!(
            retry_delay(Duration::ZERO, Some(Duration::from_secs(60))),
            PROVIDER_RETRY_AFTER_CAP
        );
        assert!(retry_delay(Duration::ZERO, None) <= PROVIDER_RETRY_MAX_JITTER);
    }

    #[test]
    fn parses_both_retry_after_wire_formats_with_the_same_cap() {
        for value in ["60", "Sun, 06 Nov 2094 08:49:37 GMT"] {
            let mut headers = HeaderMap::new();
            let Ok(value) = value.parse() else {
                continue;
            };
            headers.insert(http::header::RETRY_AFTER, value);

            assert_eq!(retry_after(&headers), Some(PROVIDER_RETRY_AFTER_CAP));
        }
    }

    #[test]
    fn endpoint_does_not_duplicate_a_versioned_base_path() {
        for base in [
            "https://api.example.test/v1",
            "https://api.example.test/v1/",
        ] {
            let Ok(base_url) = Url::parse(base) else {
                continue;
            };
            let runtime = ProviderRuntime::new(
                reqwest::Client::new(),
                SecretString::from("test-key".to_owned()),
                ProviderHttpConfig::new(base_url),
            );

            assert_eq!(
                runtime
                    .url("v1/audio/speech")
                    .ok()
                    .map(|url| url.path().to_owned()),
                Some("/v1/audio/speech".to_owned())
            );
        }
    }

    #[test]
    fn endpoint_replaces_the_version_prefix_for_gemini_uploads() {
        let Ok(base_url) = Url::parse("https://api.example.test/v1beta") else {
            return;
        };
        let runtime = ProviderRuntime::new(
            reqwest::Client::new(),
            SecretString::from("test-key".to_owned()),
            ProviderHttpConfig::new(base_url),
        );

        assert_eq!(
            runtime
                .url("upload/v1beta/files")
                .ok()
                .map(|url| url.path().to_owned()),
            Some("/upload/v1beta/files".to_owned())
        );
    }
}
