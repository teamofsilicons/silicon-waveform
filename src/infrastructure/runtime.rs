//! Production dependency composition and process lifecycle management.
//!
//! Composition deliberately preserves the complete provider order even when a
//! development credential is absent. A disabled adapter occupies that slot and
//! returns a redacted configuration failure, allowing the application service
//! to continue to the next documented provider without making a network call.
//! Readiness separately requires at least one configured provider for both TTS
//! and STT. It also remains false while the IAM delegation and Briefcase
//! content/folder contracts are unavailable.

use std::{future::IntoFuture as _, str::FromStr as _, sync::Arc, time::Duration};

use async_trait::async_trait;
use secrecy::ExposeSecret as _;
use sqlx::{
    ConnectOptions as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use thiserror::Error;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    api::{ApiState, ReadinessChecks},
    application::{
        ports::{
            AudioNormalizer, BriefcasePort, IamPort, IdempotencyStore, LeaseIdGenerator,
            SpeechToTextProvider, TextToSpeechProvider,
        },
        service::{ServicePolicy, WaveformService},
    },
    config::{
        DeepgramSettings, ElevenLabsSettings, GeminiSettings, OpenAiSettings, ProviderSettings,
        RuntimeEnvironment, Settings, SettingsError,
    },
    domain::{
        idempotency::{IdempotencyLeaseId, RequestDigestKey},
        identity::RequestId,
        media::{AudioArtifact, BriefcaseOrigin, MediaSizeLimit},
        provider::{ProviderError, ProviderFailureKind, ProviderName},
        speech::{SttProviderRequest, SttProviderResult, TtsProviderRequest},
    },
    shutdown, telemetry,
};

use super::{
    audio::{AudioNormalizerConfig, FfmpegAudioNormalizer},
    auth::IamHttpAdapter,
    briefcase::FailClosedBriefcaseStore,
    idempotency::PostgresIdempotencyStore,
    providers::{
        DeepgramConfig, DeepgramProvider, ElevenLabsConfig, ElevenLabsProvider, GeminiConfig,
        GeminiProvider, OpenAiConfig, OpenAiProvider, PROVIDER_RETRY_BASE_DELAY,
        ProviderHttpConfig, TransientRetry,
    },
};

const USER_AGENT: &str = concat!("silicon-waveform/", env!("CARGO_PKG_VERSION"));
const STT_RESPONSE_LIMIT_BYTES: usize = 2 * 1_024 * 1_024;
const PROVIDER_RESPONSE_OVERHEAD_BYTES: usize = 1_024 * 1_024;
const CLEANUP_STOP_TIMEOUT: Duration = Duration::from_secs(1);
const DATABASE_IDLE_TIMEOUT: Duration = Duration::from_mins(5);
const DATABASE_MAX_LIFETIME: Duration = Duration::from_mins(30);
const DEPENDENCY_CONTRACTS_AVAILABLE: bool = false;

/// Redacted process bootstrap or lifecycle failure.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// A present `.env` file could not be parsed or read.
    #[error("failed to load the local environment file")]
    EnvironmentFile,
    /// Typed environment settings were absent, malformed, or unsafe.
    #[error(transparent)]
    Configuration(#[from] SettingsError),
    /// The global tracing subscriber could not be installed.
    #[error("failed to initialize process telemetry")]
    Telemetry,
    /// The shared provider client could not be constructed.
    #[error("failed to build the provider HTTP client")]
    ProviderClient,
    /// The PostgreSQL connection URL could not be translated safely.
    #[error("invalid PostgreSQL connection configuration")]
    DatabaseConfiguration,
    /// The authoritative PostgreSQL pool could not connect.
    #[error("failed to connect to PostgreSQL")]
    DatabaseConnection,
    /// An embedded schema migration could not be applied.
    #[error("failed to migrate the PostgreSQL schema")]
    DatabaseMigration,
    /// IAM adapter settings could not be represented safely.
    #[error("failed to construct the IAM adapter")]
    IamConfiguration,
    /// Briefcase adapter settings could not be represented safely.
    #[error("failed to construct the Briefcase adapter")]
    BriefcaseConfiguration,
    /// A media size, origin, duration, or provider-chain invariant failed.
    #[error("failed to construct the Waveform application service")]
    ServiceConfiguration,
    /// The configured TCP listener could not be bound.
    #[error("failed to bind the Waveform HTTP listener")]
    Listener,
    /// The Axum server stopped because of an I/O failure.
    #[error("the Waveform HTTP server failed")]
    Server,
}

/// Loads environment configuration, initializes telemetry, and runs the API.
///
/// A missing `.env` file is normal for deployed processes. A present but
/// unreadable or malformed file fails closed without echoing its contents.
///
/// # Errors
///
/// Returns a redacted [`RuntimeError`] when configuration, composition,
/// migration, binding, or serving fails.
pub async fn run_from_env() -> Result<(), RuntimeError> {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(error) if error.not_found() => {}
        Err(_) => return Err(RuntimeError::EnvironmentFile),
    }

    let settings = Settings::from_env()?;
    telemetry::init(&settings).map_err(|_| RuntimeError::Telemetry)?;
    run(settings).await
}

async fn run(settings: Settings) -> Result<(), RuntimeError> {
    let provider_client = build_provider_client(&settings)?;
    let pool = connect_database(&settings).await?;

    if sqlx::migrate!().run(&pool).await.is_err() {
        close_pool(&pool, settings.server.shutdown_timeout).await;
        return Err(RuntimeError::DatabaseMigration);
    }

    let result = compose_and_serve(&settings, provider_client, pool.clone()).await;
    close_pool(&pool, settings.server.shutdown_timeout).await;
    result
}

fn build_provider_client(settings: &Settings) -> Result<reqwest::Client, RuntimeError> {
    let maximum_timeout = [
        settings.providers.gemini.tts_timeout,
        settings.providers.gemini.stt_timeout,
        settings.providers.elevenlabs.timeout,
        settings.providers.openai.tts_timeout,
        settings.providers.openai.stt_timeout,
        settings.providers.deepgram.timeout,
    ]
    .into_iter()
    .fold(Duration::from_secs(1), std::cmp::max);
    let maximum_idle_per_host = [
        settings.providers.gemini.max_concurrency.get(),
        settings.providers.elevenlabs.max_concurrency.get(),
        settings.providers.openai.max_concurrency.get(),
        settings.providers.deepgram.max_concurrency.get(),
    ]
    .into_iter()
    .max()
    .map_or(1, |value| value.min(64));

    reqwest::Client::builder()
        .connect_timeout(maximum_timeout.min(Duration::from_secs(5)))
        .timeout(maximum_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .no_proxy()
        .https_only(settings.environment == RuntimeEnvironment::Production)
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(maximum_idle_per_host)
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_nodelay(true)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|_| RuntimeError::ProviderClient)
}

async fn connect_database(settings: &Settings) -> Result<PgPool, RuntimeError> {
    let statement_timeout = format!("{}ms", settings.database.statement_timeout.as_millis());
    let options = PgConnectOptions::from_str(settings.database.url.expose_secret())
        .map_err(|_| RuntimeError::DatabaseConfiguration)?
        .application_name("silicon-waveform")
        .options([("statement_timeout", statement_timeout)])
        .disable_statement_logging();

    PgPoolOptions::new()
        .max_connections(settings.database.max_connections.get())
        .min_connections(settings.database.min_connections)
        .acquire_timeout(settings.database.acquire_timeout)
        .idle_timeout(Some(DATABASE_IDLE_TIMEOUT))
        .max_lifetime(Some(DATABASE_MAX_LIFETIME))
        .test_before_acquire(true)
        .connect_with(options)
        .await
        .map_err(|_| RuntimeError::DatabaseConnection)
}

async fn compose_and_serve(
    settings: &Settings,
    provider_client: reqwest::Client,
    pool: PgPool,
) -> Result<(), RuntimeError> {
    let provider_chains = build_provider_chains(
        &provider_client,
        &settings.providers,
        settings.audio.max_input_bytes,
    );
    let configured_tts_providers = provider_chains.availability.tts_count();
    let configured_stt_providers = provider_chains.availability.stt_count();
    let provider_chains_configured = configured_tts_providers > 0 && configured_stt_providers > 0;

    let iam: Arc<dyn IamPort> =
        Arc::new(IamHttpAdapter::new(&settings.iam).map_err(|_| RuntimeError::IamConfiguration)?);
    let briefcase: Arc<dyn BriefcasePort> = Arc::new(
        FailClosedBriefcaseStore::new(&settings.briefcase)
            .map_err(|_| RuntimeError::BriefcaseConfiguration)?,
    );

    let audio = Arc::new(FfmpegAudioNormalizer::new(AudioNormalizerConfig {
        ffmpeg_path: settings.audio.ffmpeg_path.clone(),
        max_input_bytes: settings.audio.max_input_bytes,
        max_output_bytes: settings.audio.max_output_bytes,
        bitrate_kbps: settings.audio.bitrate_kbps,
        timeout: settings.audio.timeout,
    }));
    let audio_port: Arc<dyn AudioNormalizer> = audio.clone();

    let idempotency = Arc::new(PostgresIdempotencyStore::new(
        pool,
        settings.database.statement_timeout,
        settings.idempotency.retry_after,
        settings.idempotency.ttl,
    ));
    let idempotency_port: Arc<dyn IdempotencyStore> = idempotency.clone();

    let media_limit = u64::try_from(settings.limits.max_media_bytes)
        .ok()
        .and_then(|bytes| MediaSizeLimit::new(bytes).ok())
        .ok_or(RuntimeError::ServiceConfiguration)?;
    let briefcase_origin = BriefcaseOrigin::new(settings.briefcase.permanent_origin.clone())
        .map_err(|_| RuntimeError::ServiceConfiguration)?;
    let policy = ServicePolicy::new(
        settings.idempotency.lease,
        media_limit,
        briefcase_origin,
        settings.limits.max_text_chars,
        RequestDigestKey::new(settings.idempotency.digest_key.expose_secret().as_bytes())
            .map_err(|_| RuntimeError::ServiceConfiguration)?,
    )
    .map_err(|_| RuntimeError::ServiceConfiguration)?;

    let service = Arc::new(
        WaveformService::new(
            iam,
            briefcase,
            idempotency_port.clone(),
            audio_port.clone(),
            Arc::new(UuidLeaseIdGenerator),
            provider_chains.tts,
            provider_chains.stt,
            policy,
        )
        .map_err(|_| RuntimeError::ServiceConfiguration)?,
    );
    let readiness = ReadinessChecks::new(
        idempotency_port,
        audio_port,
        provider_chains_configured,
        DEPENDENCY_CONTRACTS_AVAILABLE,
    );
    let state = ApiState::new(
        service,
        readiness,
        settings.server.tts_deadline,
        settings.server.stt_deadline,
    );
    let application = crate::api::router(state, &settings.server);
    let listener = TcpListener::bind(settings.server.bind_addr)
        .await
        .map_err(|_| RuntimeError::Listener)?;

    let cleanup_shutdown = CancellationToken::new();
    let cleanup_task = spawn_cleanup(
        idempotency,
        cleanup_shutdown.clone(),
        settings.idempotency.cleanup_interval,
        settings.idempotency.cleanup_batch.get(),
    );

    tracing::info!(
        bind_address = %settings.server.bind_addr,
        configured_tts_providers,
        configured_stt_providers,
        "Waveform API is listening"
    );
    tracing::warn!(
        "readiness is fail-closed while IAM delegation and Briefcase content contracts are unavailable"
    );

    serve_until_shutdown(
        listener,
        application,
        settings.server.shutdown_timeout,
        cleanup_shutdown,
        cleanup_task,
    )
    .await
}

async fn serve_until_shutdown(
    listener: TcpListener,
    application: axum::Router,
    shutdown_timeout: Duration,
    cleanup_shutdown: CancellationToken,
    cleanup_task: JoinHandle<()>,
) -> Result<(), RuntimeError> {
    let server_shutdown = CancellationToken::new();
    let graceful_shutdown = server_shutdown.clone();

    let server_result = {
        let server = axum::serve(listener, application)
            .with_graceful_shutdown(graceful_shutdown.cancelled_owned())
            .into_future();
        tokio::pin!(server);

        tokio::select! {
            result = &mut server => result.map_err(|_| RuntimeError::Server),
            () = shutdown::signal() => {
                cleanup_shutdown.cancel();
                server_shutdown.cancel();
                if let Ok(result) = tokio::time::timeout(shutdown_timeout, &mut server).await {
                    result.map_err(|_| RuntimeError::Server)
                } else {
                    tracing::warn!("HTTP graceful-shutdown deadline elapsed; dropping remaining connections");
                    Ok(())
                }
            }
        }
    };

    cleanup_shutdown.cancel();
    stop_cleanup(cleanup_task).await;
    server_result
}

fn spawn_cleanup(
    store: Arc<PostgresIdempotencyStore>,
    shutdown: CancellationToken,
    interval: Duration,
    batch_size: u32,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let first_run = tokio::time::Instant::now() + interval;
        let mut ticker = tokio::time::interval_at(first_run, interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                _ = ticker.tick() => {
                    let cleanup = store.delete_expired(batch_size);
                    tokio::select! {
                        biased;
                        () = shutdown.cancelled() => break,
                        result = cleanup => if let Ok(deleted) = result {
                            tracing::debug!(deleted_records = deleted, "expired idempotency records cleaned");
                        } else {
                            tracing::warn!("bounded idempotency cleanup failed");
                        }
                    }
                }
            }
        }
    })
}

async fn stop_cleanup(mut task: JoinHandle<()>) {
    match tokio::time::timeout(CLEANUP_STOP_TIMEOUT, &mut task).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => tracing::error!("idempotency cleanup task terminated unexpectedly"),
        Err(_) => {
            task.abort();
            let _join_result = task.await;
            tracing::warn!("idempotency cleanup task exceeded its shutdown deadline");
        }
    }
}

async fn close_pool(pool: &PgPool, timeout: Duration) {
    if tokio::time::timeout(timeout, pool.close()).await.is_err() {
        tracing::warn!("PostgreSQL pool close deadline elapsed");
    }
}

struct ProviderChains {
    tts: Vec<Arc<dyn TextToSpeechProvider>>,
    stt: Vec<Arc<dyn SpeechToTextProvider>>,
    availability: ProviderAvailability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProviderAvailability {
    tts_count: usize,
    stt_count: usize,
}

impl ProviderAvailability {
    fn from_settings(settings: &ProviderSettings) -> Self {
        let gemini = usize::from(settings.gemini.api_key.is_some());
        let openai = usize::from(settings.openai.api_key.is_some());
        Self {
            tts_count: gemini + usize::from(settings.elevenlabs.api_key.is_some()) + openai,
            stt_count: gemini + openai + usize::from(settings.deepgram.api_key.is_some()),
        }
    }

    const fn tts_count(self) -> usize {
        self.tts_count
    }

    const fn stt_count(self) -> usize {
        self.stt_count
    }
}

fn build_provider_chains(
    client: &reqwest::Client,
    settings: &ProviderSettings,
    max_audio_bytes: usize,
) -> ProviderChains {
    let availability = ProviderAvailability::from_settings(settings);
    let (gemini_tts, gemini_stt) = build_gemini(client, &settings.gemini, max_audio_bytes);
    let elevenlabs = build_elevenlabs(client, &settings.elevenlabs, max_audio_bytes);
    let (openai_tts, openai_stt) = build_openai(client, &settings.openai, max_audio_bytes);
    let deepgram = build_deepgram(client, &settings.deepgram);

    ProviderChains {
        tts: vec![gemini_tts, elevenlabs, openai_tts],
        stt: vec![gemini_stt, openai_stt, deepgram],
        availability,
    }
}

fn build_gemini(
    client: &reqwest::Client,
    settings: &GeminiSettings,
    max_audio_bytes: usize,
) -> (Arc<dyn TextToSpeechProvider>, Arc<dyn SpeechToTextProvider>) {
    let Some(api_key) = settings.api_key.as_ref() else {
        let provider = Arc::new(DisabledProvider::new(ProviderName::Gemini));
        return (provider.clone(), provider);
    };
    let provider = Arc::new(GeminiProvider::new(
        client.clone(),
        api_key.clone(),
        GeminiConfig {
            tts_http: provider_http_config(
                settings.base_url.clone(),
                settings.tts_timeout,
                encoded_audio_response_limit(max_audio_bytes),
                settings.max_concurrency.get(),
            ),
            stt_http: provider_http_config(
                settings.base_url.clone(),
                settings.stt_timeout,
                STT_RESPONSE_LIMIT_BYTES,
                settings.max_concurrency.get(),
            ),
            tts_model: settings.tts_model.clone(),
            stt_model: settings.stt_model.clone(),
            voice: settings.tts_voice.clone(),
        },
    ));
    (provider.clone(), provider)
}

fn build_elevenlabs(
    client: &reqwest::Client,
    settings: &ElevenLabsSettings,
    max_audio_bytes: usize,
) -> Arc<dyn TextToSpeechProvider> {
    let Some(api_key) = settings.api_key.as_ref() else {
        return Arc::new(DisabledProvider::new(ProviderName::ElevenLabs));
    };
    Arc::new(ElevenLabsProvider::new(
        client.clone(),
        api_key.clone(),
        ElevenLabsConfig {
            http: provider_http_config(
                settings.base_url.clone(),
                settings.timeout,
                max_audio_bytes,
                settings.max_concurrency.get(),
            ),
            model: settings.model.clone(),
            voice_id: settings.voice_id.clone(),
            enable_logging: settings.enable_logging,
        },
    ))
}

fn build_openai(
    client: &reqwest::Client,
    settings: &OpenAiSettings,
    max_audio_bytes: usize,
) -> (Arc<dyn TextToSpeechProvider>, Arc<dyn SpeechToTextProvider>) {
    let Some(api_key) = settings.api_key.as_ref() else {
        let provider = Arc::new(DisabledProvider::new(ProviderName::OpenAi));
        return (provider.clone(), provider);
    };
    let provider = Arc::new(OpenAiProvider::new(
        client.clone(),
        api_key.clone(),
        OpenAiConfig {
            tts_http: provider_http_config(
                settings.base_url.clone(),
                settings.tts_timeout,
                max_audio_bytes,
                settings.max_concurrency.get(),
            ),
            stt_http: provider_http_config(
                settings.base_url.clone(),
                settings.stt_timeout,
                STT_RESPONSE_LIMIT_BYTES,
                settings.max_concurrency.get(),
            ),
            tts_model: settings.tts_model.clone(),
            stt_model: settings.stt_model.clone(),
            voice: settings.voice.clone(),
        },
    ));
    (provider.clone(), provider)
}

fn build_deepgram(
    client: &reqwest::Client,
    settings: &DeepgramSettings,
) -> Arc<dyn SpeechToTextProvider> {
    let Some(api_key) = settings.api_key.as_ref() else {
        return Arc::new(DisabledProvider::new(ProviderName::Deepgram));
    };
    Arc::new(DeepgramProvider::new(
        client.clone(),
        api_key.clone(),
        DeepgramConfig {
            http: provider_http_config(
                settings.base_url.clone(),
                settings.timeout,
                STT_RESPONSE_LIMIT_BYTES,
                settings.max_concurrency.get(),
            ),
            model: settings.stt_model.clone(),
            mip_opt_out: settings.mip_opt_out,
        },
    ))
}

fn provider_http_config(
    base_url: url::Url,
    timeout: Duration,
    max_response_bytes: usize,
    max_concurrency: usize,
) -> ProviderHttpConfig {
    let mut config = ProviderHttpConfig::new(base_url);
    config.timeout = timeout;
    config.max_response_bytes = max_response_bytes;
    config.max_concurrency = max_concurrency;
    config.retry = TransientRetry::Once {
        delay: PROVIDER_RETRY_BASE_DELAY,
    };
    config
}

const fn encoded_audio_response_limit(max_audio_bytes: usize) -> usize {
    max_audio_bytes
        .saturating_add(2)
        .saturating_div(3)
        .saturating_mul(4)
        .saturating_add(PROVIDER_RESPONSE_OVERHEAD_BYTES)
}

#[derive(Clone, Copy, Debug)]
struct DisabledProvider {
    name: ProviderName,
}

impl DisabledProvider {
    const fn new(name: ProviderName) -> Self {
        Self { name }
    }

    const fn error(self) -> ProviderError {
        ProviderError::new(self.name, ProviderFailureKind::Authentication)
    }
}

#[async_trait]
impl TextToSpeechProvider for DisabledProvider {
    fn name(&self) -> ProviderName {
        self.name
    }

    async fn synthesize(
        &self,
        _request: TtsProviderRequest,
        _request_id: RequestId,
    ) -> Result<AudioArtifact, ProviderError> {
        Err(self.error())
    }
}

#[async_trait]
impl SpeechToTextProvider for DisabledProvider {
    fn name(&self) -> ProviderName {
        self.name
    }

    async fn transcribe(
        &self,
        _request: SttProviderRequest,
        _request_id: RequestId,
    ) -> Result<SttProviderResult, ProviderError> {
        Err(self.error())
    }
}

#[derive(Clone, Copy, Debug)]
struct UuidLeaseIdGenerator;

impl LeaseIdGenerator for UuidLeaseIdGenerator {
    fn new_idempotency_lease_id(&self) -> IdempotencyLeaseId {
        lease_id_from(Uuid::new_v4)
    }
}

fn lease_id_from(mut next: impl FnMut() -> Uuid) -> IdempotencyLeaseId {
    loop {
        if let Ok(value) = IdempotencyLeaseId::new(next()) {
            return value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::provider::{STT_PROVIDER_CHAIN, TTS_PROVIDER_CHAIN};

    #[test]
    fn disabled_adapters_preserve_their_provider_identity() {
        let gemini = DisabledProvider::new(ProviderName::Gemini);
        let deepgram = DisabledProvider::new(ProviderName::Deepgram);

        assert_eq!(TextToSpeechProvider::name(&gemini), ProviderName::Gemini);
        assert_eq!(
            SpeechToTextProvider::name(&deepgram),
            ProviderName::Deepgram
        );
        assert_eq!(gemini.error().kind, ProviderFailureKind::Authentication);
    }

    #[test]
    fn readiness_requires_at_least_one_provider_for_each_operation() {
        let tts_only = ProviderAvailability {
            tts_count: 1,
            stt_count: 0,
        };
        let split_capabilities = ProviderAvailability {
            tts_count: 1,
            stt_count: 1,
        };

        assert_eq!(tts_only.tts_count(), 1);
        assert_eq!(tts_only.stt_count(), 0);
        assert!(split_capabilities.tts_count() > 0 && split_capabilities.stt_count() > 0);
    }

    #[test]
    fn base64_response_limit_includes_encoding_and_envelope_overhead() {
        assert_eq!(
            encoded_audio_response_limit(3),
            4 + PROVIDER_RESPONSE_OVERHEAD_BYTES
        );
        assert!(encoded_audio_response_limit(25 * 1_024 * 1_024) > 25 * 1_024 * 1_024);
    }

    #[test]
    fn chain_constants_remain_the_composition_contract() {
        assert_eq!(
            TTS_PROVIDER_CHAIN,
            [
                ProviderName::Gemini,
                ProviderName::ElevenLabs,
                ProviderName::OpenAi,
            ]
        );
        assert_eq!(
            STT_PROVIDER_CHAIN,
            [
                ProviderName::Gemini,
                ProviderName::OpenAi,
                ProviderName::Deepgram,
            ]
        );
    }
}
