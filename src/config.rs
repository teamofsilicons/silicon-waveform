//! Typed, validated runtime configuration loaded from environment variables.

use std::{
    env, fmt,
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use url::Url;

const MAX_TEXT_CHARS: usize = 4_096;
const MAX_MEDIA_BYTES: usize = 25 * 1_024 * 1_024;
const MAX_SECRET_CHARS: usize = 16_384;
const ORCHESTRATION_HEADROOM: Duration = Duration::from_secs(30);

/// Fully validated process configuration.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Deployment environment and its safety policy.
    pub environment: RuntimeEnvironment,
    /// Public HTTP server controls.
    pub server: ServerSettings,
    /// PostgreSQL pool controls.
    pub database: DatabaseSettings,
    /// Silicon IAM integration settings.
    pub iam: IamSettings,
    /// Silicon Briefcase integration settings.
    pub briefcase: BriefcaseSettings,
    /// Speech-provider settings.
    pub providers: ProviderSettings,
    /// Audio normalization settings.
    pub audio: AudioSettings,
    /// Shared idempotency-record lifecycle settings.
    pub idempotency: IdempotencySettings,
    /// Synchronous input and media limits.
    pub limits: LimitSettings,
    /// Structured logging settings.
    pub telemetry: TelemetrySettings,
}

/// Deployment environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEnvironment {
    /// Local developer process.
    Development,
    /// Automated test process.
    Test,
    /// Deployed production process.
    Production,
}

/// HTTP listener and admission-control settings.
#[derive(Clone, Debug)]
pub struct ServerSettings {
    /// Address on which the API listens.
    pub bind_addr: SocketAddr,
    /// Maximum accepted JSON body size.
    pub json_body_limit: usize,
    /// Total deadline for a text-to-speech request.
    pub tts_deadline: Duration,
    /// Total deadline for a speech-to-text request.
    pub stt_deadline: Duration,
    /// Maximum requests admitted concurrently by one process.
    pub max_in_flight: NonZeroUsize,
    /// Time allowed for in-flight work to drain during shutdown.
    pub shutdown_timeout: Duration,
}

/// PostgreSQL pool and statement settings.
#[derive(Clone, Debug)]
pub struct DatabaseSettings {
    /// Secret-bearing PostgreSQL connection URL.
    pub url: SecretString,
    /// Maximum open connections per process.
    pub max_connections: NonZeroU32,
    /// Minimum idle connections per process.
    pub min_connections: u32,
    /// Pool acquisition deadline.
    pub acquire_timeout: Duration,
    /// Per-statement database deadline.
    pub statement_timeout: Duration,
}

/// Silicon IAM authorization settings.
#[derive(Clone, Debug)]
pub struct IamSettings {
    /// IAM service base URL.
    pub base_url: Url,
    /// Waveform's IAM application identifier.
    pub app_id: String,
    /// Waveform's IAM application secret.
    pub app_secret: SecretString,
    /// IAM access-token introspection endpoint path.
    pub token_introspection_path: String,
    /// IAM OBO proof verification endpoint path.
    pub obo_verify_path: String,
    /// Required audience for inbound OBO proofs.
    pub audience: String,
    /// IAM action required for text-to-speech.
    pub tts_action: String,
    /// IAM action required for speech-to-text.
    pub stt_action: String,
    /// Deadline for one IAM request.
    pub timeout: Duration,
}

/// Silicon Briefcase access and download settings.
#[derive(Clone, Debug)]
pub struct BriefcaseSettings {
    /// Briefcase API base URL.
    pub base_url: Url,
    /// Exact allowed origin for permanent Briefcase URLs.
    pub permanent_origin: Url,
    /// Exact allowed origin for temporary media downloads.
    pub cdn_origin: Url,
    /// Waveform's Briefcase application-folder identifier.
    pub app_id: String,
    /// Deadline for one Briefcase API request.
    pub timeout: Duration,
    /// Deadline for a bounded source-media download.
    pub download_timeout: Duration,
    /// Hard byte limit applied while downloading source media.
    pub max_download_bytes: usize,
}

/// Provider chain configuration.
#[derive(Clone, Debug)]
pub struct ProviderSettings {
    /// Google Gemini configuration.
    pub gemini: GeminiSettings,
    /// `ElevenLabs` configuration.
    pub elevenlabs: ElevenLabsSettings,
    /// `OpenAI` configuration.
    pub openai: OpenAiSettings,
    /// Deepgram configuration.
    pub deepgram: DeepgramSettings,
    /// Whether every documented fallback provider must be configured.
    pub require_full_chain: bool,
}

/// Google Gemini speech settings.
#[derive(Clone, Debug)]
pub struct GeminiSettings {
    /// Optional Gemini API key; absence disables this adapter.
    pub api_key: Option<SecretString>,
    /// Gemini API base URL.
    pub base_url: Url,
    /// Gemini text-to-speech model identifier.
    pub tts_model: String,
    /// Gemini speech-to-text model identifier.
    pub stt_model: String,
    /// Gemini prebuilt text-to-speech voice.
    pub tts_voice: String,
    /// Deadline for one Gemini text-to-speech attempt.
    pub tts_timeout: Duration,
    /// Deadline for the complete Gemini upload, poll, and transcription attempt.
    pub stt_timeout: Duration,
    /// Maximum simultaneous Gemini attempts.
    pub max_concurrency: NonZeroUsize,
}

/// `ElevenLabs` text-to-speech settings.
#[derive(Clone, Debug)]
pub struct ElevenLabsSettings {
    /// Optional `ElevenLabs` API key; absence disables this adapter.
    pub api_key: Option<SecretString>,
    /// `ElevenLabs` API base URL.
    pub base_url: Url,
    /// `ElevenLabs` text-to-speech model identifier.
    pub model: String,
    /// `ElevenLabs` voice identifier.
    pub voice_id: String,
    /// Whether `ElevenLabs` request-history logging is permitted.
    pub enable_logging: bool,
    /// Deadline for one `ElevenLabs` attempt.
    pub timeout: Duration,
    /// Maximum simultaneous `ElevenLabs` attempts.
    pub max_concurrency: NonZeroUsize,
}

/// `OpenAI` speech settings.
#[derive(Clone, Debug)]
pub struct OpenAiSettings {
    /// Optional `OpenAI` API key; absence disables this adapter.
    pub api_key: Option<SecretString>,
    /// `OpenAI` API base URL.
    pub base_url: Url,
    /// `OpenAI` text-to-speech model identifier.
    pub tts_model: String,
    /// `OpenAI` speech-to-text model identifier.
    pub stt_model: String,
    /// `OpenAI` text-to-speech voice.
    pub voice: String,
    /// Deadline for one `OpenAI` text-to-speech attempt.
    pub tts_timeout: Duration,
    /// Deadline for one `OpenAI` speech-to-text attempt.
    pub stt_timeout: Duration,
    /// Maximum simultaneous `OpenAI` attempts.
    pub max_concurrency: NonZeroUsize,
}

/// Deepgram speech-to-text settings.
#[derive(Clone, Debug)]
pub struct DeepgramSettings {
    /// Optional Deepgram API key; absence disables this adapter.
    pub api_key: Option<SecretString>,
    /// Deepgram API base URL.
    pub base_url: Url,
    /// Deepgram speech-to-text model identifier.
    pub stt_model: String,
    /// Whether requests opt out of the Deepgram Model Improvement Program.
    pub mip_opt_out: bool,
    /// Deadline for one Deepgram attempt.
    pub timeout: Duration,
    /// Maximum simultaneous Deepgram attempts.
    pub max_concurrency: NonZeroUsize,
}

/// `FFmpeg` audio-normalization controls.
#[derive(Clone, Debug)]
pub struct AudioSettings {
    /// `FFmpeg` executable path. Production requires an absolute path.
    pub ffmpeg_path: PathBuf,
    /// Deadline for one audio normalization operation.
    pub timeout: Duration,
    /// Maximum provider artifact size accepted before normalization.
    pub max_input_bytes: usize,
    /// Maximum normalized output size.
    pub max_output_bytes: usize,
    /// Constant MP3 output bitrate in kilobits per second.
    pub bitrate_kbps: u16,
}

/// PostgreSQL-backed idempotency lifecycle settings.
#[derive(Clone, Debug)]
pub struct IdempotencySettings {
    /// Secret key for HMAC request digests; it must remain stable for retention.
    pub digest_key: SecretString,
    /// Lifetime of terminal success records.
    pub ttl: Duration,
    /// Maximum live ownership lease for one request.
    pub lease: Duration,
    /// Suggested delay before a caller retries an in-progress request.
    pub retry_after: Duration,
    /// Interval between expired-record cleanup passes.
    pub cleanup_interval: Duration,
    /// Maximum records deleted in one cleanup pass.
    pub cleanup_batch: NonZeroU32,
}

/// Synchronous public request limits.
#[derive(Clone, Debug)]
pub struct LimitSettings {
    /// Maximum Unicode scalar values accepted by text-to-speech.
    pub max_text_chars: usize,
    /// Maximum source-media size accepted by speech-to-text.
    pub max_media_bytes: usize,
}

/// Process tracing settings.
#[derive(Clone, Debug)]
pub struct TelemetrySettings {
    /// Tracing filter directive.
    pub filter: String,
    /// Whether events are emitted as newline-delimited JSON.
    pub json: bool,
}

/// Configuration loading or validation failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum SettingsError {
    /// A required variable is absent or empty.
    #[error("required environment variable {0} is missing")]
    Missing(&'static str),
    /// A variable is not valid Unicode.
    #[error("environment variable {0} is not valid Unicode")]
    NonUnicode(&'static str),
    /// A variable cannot be parsed or violates a safety policy.
    #[error("invalid environment variable {name}: {reason}")]
    Invalid {
        /// Environment variable name.
        name: &'static str,
        /// Redacted reason that never contains the supplied value.
        reason: String,
    },
}

impl Settings {
    /// Loads and validates settings from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when a setting is absent, malformed, mutually
    /// inconsistent, or unsafe for the selected deployment environment.
    pub fn from_env() -> Result<Self, SettingsError> {
        Self::from_source(&ProcessEnvironment)
    }

    fn from_source(source: &impl EnvironmentSource) -> Result<Self, SettingsError> {
        let environment = parse_or(source, "WAVEFORM_ENVIRONMENT", "development")?;
        let limits = load_limits(source)?;
        let server = load_server(source, &limits)?;
        let database = load_database(source, environment)?;
        let iam = load_iam(source)?;
        let briefcase = load_briefcase(source, &iam.app_id, &limits)?;
        let providers = load_providers(source)?;
        let audio = load_audio(source)?;
        let idempotency = load_idempotency(source)?;
        let telemetry = load_telemetry(source)?;
        validate_idempotency_lease(&idempotency, &server)?;
        validate_workflow_deadlines(&server, &database, &iam, &briefcase, &providers, &audio)?;

        let settings = Self {
            environment,
            server,
            database,
            iam,
            briefcase,
            providers,
            audio,
            idempotency,
            limits,
            telemetry,
        };
        validate_environment_safety(&settings)?;
        Ok(settings)
    }
}

fn load_limits(source: &impl EnvironmentSource) -> Result<LimitSettings, SettingsError> {
    Ok(LimitSettings {
        max_text_chars: usize_in_range(
            source,
            "WAVEFORM_MAX_TEXT_CHARS",
            MAX_TEXT_CHARS,
            1,
            MAX_TEXT_CHARS,
        )?,
        max_media_bytes: usize_in_range(
            source,
            "WAVEFORM_MAX_MEDIA_BYTES",
            MAX_MEDIA_BYTES,
            1_024,
            MAX_MEDIA_BYTES,
        )?,
    })
}

fn load_server(
    source: &impl EnvironmentSource,
    limits: &LimitSettings,
) -> Result<ServerSettings, SettingsError> {
    let settings = ServerSettings {
        bind_addr: parse_or(source, "WAVEFORM_BIND_ADDR", "127.0.0.1:8080")?,
        json_body_limit: usize_in_range(
            source,
            "WAVEFORM_JSON_BODY_LIMIT_BYTES",
            65_536,
            1_024,
            1_048_576,
        )?,
        tts_deadline: duration_seconds(source, "WAVEFORM_TTS_DEADLINE_SECONDS", 300, 1, 900)?,
        stt_deadline: duration_seconds(source, "WAVEFORM_STT_DEADLINE_SECONDS", 600, 1, 900)?,
        max_in_flight: nonzero_usize(source, "WAVEFORM_MAX_IN_FLIGHT", 256, 10_000)?,
        shutdown_timeout: duration_seconds(
            source,
            "WAVEFORM_SHUTDOWN_TIMEOUT_SECONDS",
            30,
            1,
            300,
        )?,
    };
    if settings.bind_addr.port() == 0 {
        return Err(invalid(
            "WAVEFORM_BIND_ADDR",
            "port must be greater than zero",
        ));
    }

    let minimum_json_bytes = limits
        .max_text_chars
        .saturating_mul(12)
        .saturating_add(1_024);
    if settings.json_body_limit < minimum_json_bytes {
        return Err(invalid(
            "WAVEFORM_JSON_BODY_LIMIT_BYTES",
            "must accommodate the configured text limit",
        ));
    }
    Ok(settings)
}

fn load_database(
    source: &impl EnvironmentSource,
    environment: RuntimeEnvironment,
) -> Result<DatabaseSettings, SettingsError> {
    let max_connections = nonzero_u32(source, "WAVEFORM_DATABASE_MAX_CONNECTIONS", 16, 256)?;
    let settings = DatabaseSettings {
        url: secret_required(source, "WAVEFORM_DATABASE_URL", 1, MAX_SECRET_CHARS)?,
        max_connections,
        min_connections: u32_in_range(
            source,
            "WAVEFORM_DATABASE_MIN_CONNECTIONS",
            1,
            0,
            max_connections.get().saturating_sub(1),
        )?,
        acquire_timeout: duration_seconds(
            source,
            "WAVEFORM_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            3,
            1,
            60,
        )?,
        statement_timeout: duration_seconds(
            source,
            "WAVEFORM_DATABASE_STATEMENT_TIMEOUT_SECONDS",
            10,
            1,
            300,
        )?,
    };
    validate_database_url(environment, &settings.url)?;
    Ok(settings)
}

fn load_iam(source: &impl EnvironmentSource) -> Result<IamSettings, SettingsError> {
    let app_id = bounded_string(
        "WAVEFORM_IAM_APP_ID",
        value_or(source, "WAVEFORM_IAM_APP_ID", "waveform")?,
        1,
        80,
    )?;
    Ok(IamSettings {
        base_url: http_url_or(source, "WAVEFORM_IAM_BASE_URL", "http://127.0.0.1:8081")?,
        app_id: app_id.clone(),
        app_secret: secret_required(source, "WAVEFORM_IAM_APP_SECRET", 1, MAX_SECRET_CHARS)?,
        token_introspection_path: endpoint_path(
            "WAVEFORM_IAM_TOKEN_INTROSPECTION_PATH",
            value_or(
                source,
                "WAVEFORM_IAM_TOKEN_INTROSPECTION_PATH",
                "/api/v1/auth/tokens/introspect",
            )?,
        )?,
        obo_verify_path: endpoint_path(
            "WAVEFORM_IAM_OBO_VERIFY_PATH",
            value_or(
                source,
                "WAVEFORM_IAM_OBO_VERIFY_PATH",
                "/api/v1/obo-access/verify",
            )?,
        )?,
        audience: bounded_string(
            "WAVEFORM_IAM_AUDIENCE",
            optional(source, "WAVEFORM_IAM_AUDIENCE")?.unwrap_or_else(|| app_id.clone()),
            1,
            80,
        )?,
        tts_action: action_name(
            "WAVEFORM_IAM_TTS_ACTION",
            value_or(source, "WAVEFORM_IAM_TTS_ACTION", "waveform.tts")?,
        )?,
        stt_action: action_name(
            "WAVEFORM_IAM_STT_ACTION",
            value_or(source, "WAVEFORM_IAM_STT_ACTION", "waveform.stt")?,
        )?,
        timeout: duration_seconds(source, "WAVEFORM_IAM_TIMEOUT_SECONDS", 5, 1, 60)?,
    })
}

fn load_briefcase(
    source: &impl EnvironmentSource,
    iam_app_id: &str,
    limits: &LimitSettings,
) -> Result<BriefcaseSettings, SettingsError> {
    let base_url = http_url_or(
        source,
        "WAVEFORM_BRIEFCASE_BASE_URL",
        "http://127.0.0.1:8082",
    )?;
    let settings = BriefcaseSettings {
        permanent_origin: origin_url_or(
            source,
            "WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN",
            "https://127.0.0.1:8082",
        )?,
        cdn_origin: origin_url_or(
            source,
            "WAVEFORM_BRIEFCASE_CDN_ORIGIN",
            "https://127.0.0.1:8082",
        )?,
        base_url,
        app_id: bounded_string(
            "WAVEFORM_BRIEFCASE_APP_ID",
            optional(source, "WAVEFORM_BRIEFCASE_APP_ID")?.unwrap_or_else(|| iam_app_id.to_owned()),
            1,
            80,
        )?,
        timeout: duration_seconds(source, "WAVEFORM_BRIEFCASE_TIMEOUT_SECONDS", 10, 1, 120)?,
        download_timeout: duration_seconds(
            source,
            "WAVEFORM_BRIEFCASE_DOWNLOAD_TIMEOUT_SECONDS",
            30,
            1,
            300,
        )?,
        max_download_bytes: usize_in_range(
            source,
            "WAVEFORM_BRIEFCASE_MAX_DOWNLOAD_BYTES",
            MAX_MEDIA_BYTES,
            1_024,
            MAX_MEDIA_BYTES,
        )?,
    };
    if settings.max_download_bytes < limits.max_media_bytes {
        return Err(invalid(
            "WAVEFORM_BRIEFCASE_MAX_DOWNLOAD_BYTES",
            "must be at least WAVEFORM_MAX_MEDIA_BYTES",
        ));
    }
    Ok(settings)
}

impl FromStr for RuntimeEnvironment {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" | "prod" => Ok(Self::Production),
            _ => Err("must be development, test, or production"),
        }
    }
}

fn load_providers(source: &impl EnvironmentSource) -> Result<ProviderSettings, SettingsError> {
    Ok(ProviderSettings {
        gemini: load_gemini(source)?,
        elevenlabs: load_elevenlabs(source)?,
        openai: load_openai(source)?,
        deepgram: load_deepgram(source)?,
        require_full_chain: parse_or(source, "WAVEFORM_REQUIRE_FULL_PROVIDER_CHAIN", "false")?,
    })
}

fn load_gemini(source: &impl EnvironmentSource) -> Result<GeminiSettings, SettingsError> {
    Ok(GeminiSettings {
        api_key: optional_secret(source, "WAVEFORM_GEMINI_API_KEY", 1, MAX_SECRET_CHARS)?,
        base_url: http_url_or(
            source,
            "WAVEFORM_GEMINI_BASE_URL",
            "https://generativelanguage.googleapis.com/v1beta",
        )?,
        tts_model: bounded_value_or(
            source,
            "WAVEFORM_GEMINI_TTS_MODEL",
            "gemini-3.1-flash-tts-preview",
            1,
            256,
        )?,
        stt_model: bounded_value_or(
            source,
            "WAVEFORM_GEMINI_STT_MODEL",
            "gemini-3.5-transcribe",
            1,
            256,
        )?,
        tts_voice: bounded_value_or(source, "WAVEFORM_GEMINI_TTS_VOICE", "Kore", 1, 128)?,
        tts_timeout: duration_seconds(source, "WAVEFORM_GEMINI_TTS_TIMEOUT_SECONDS", 45, 1, 120)?,
        stt_timeout: duration_seconds(source, "WAVEFORM_GEMINI_STT_TIMEOUT_SECONDS", 180, 1, 300)?,
        max_concurrency: nonzero_usize(source, "WAVEFORM_GEMINI_MAX_CONCURRENCY", 16, 1_024)?,
    })
}

fn load_elevenlabs(source: &impl EnvironmentSource) -> Result<ElevenLabsSettings, SettingsError> {
    Ok(ElevenLabsSettings {
        api_key: optional_secret(source, "WAVEFORM_ELEVENLABS_API_KEY", 1, MAX_SECRET_CHARS)?,
        base_url: http_url_or(
            source,
            "WAVEFORM_ELEVENLABS_BASE_URL",
            "https://api.elevenlabs.io/v1",
        )?,
        model: bounded_value_or(
            source,
            "WAVEFORM_ELEVENLABS_MODEL",
            "eleven_multilingual_v2",
            1,
            256,
        )?,
        voice_id: bounded_value_or(
            source,
            "WAVEFORM_ELEVENLABS_VOICE_ID",
            "21m00Tcm4TlvDq8ikWAM",
            1,
            256,
        )?,
        enable_logging: parse_or(source, "WAVEFORM_ELEVENLABS_ENABLE_LOGGING", "false")?,
        timeout: duration_seconds(source, "WAVEFORM_ELEVENLABS_TIMEOUT_SECONDS", 45, 1, 120)?,
        max_concurrency: nonzero_usize(source, "WAVEFORM_ELEVENLABS_MAX_CONCURRENCY", 16, 1_024)?,
    })
}

fn load_openai(source: &impl EnvironmentSource) -> Result<OpenAiSettings, SettingsError> {
    Ok(OpenAiSettings {
        api_key: optional_secret(source, "WAVEFORM_OPENAI_API_KEY", 1, MAX_SECRET_CHARS)?,
        base_url: http_url_or(
            source,
            "WAVEFORM_OPENAI_BASE_URL",
            "https://api.openai.com/v1",
        )?,
        tts_model: bounded_value_or(source, "WAVEFORM_OPENAI_TTS_MODEL", "tts-1", 1, 256)?,
        stt_model: bounded_value_or(
            source,
            "WAVEFORM_OPENAI_STT_MODEL",
            "gpt-transcribe",
            1,
            256,
        )?,
        voice: bounded_value_or(source, "WAVEFORM_OPENAI_TTS_VOICE", "alloy", 1, 128)?,
        tts_timeout: duration_seconds(source, "WAVEFORM_OPENAI_TTS_TIMEOUT_SECONDS", 45, 1, 120)?,
        stt_timeout: duration_seconds(source, "WAVEFORM_OPENAI_STT_TIMEOUT_SECONDS", 120, 1, 300)?,
        max_concurrency: nonzero_usize(source, "WAVEFORM_OPENAI_MAX_CONCURRENCY", 16, 1_024)?,
    })
}

fn load_deepgram(source: &impl EnvironmentSource) -> Result<DeepgramSettings, SettingsError> {
    Ok(DeepgramSettings {
        api_key: optional_secret(source, "WAVEFORM_DEEPGRAM_API_KEY", 1, MAX_SECRET_CHARS)?,
        base_url: http_url_or(
            source,
            "WAVEFORM_DEEPGRAM_BASE_URL",
            "https://api.deepgram.com/v1",
        )?,
        stt_model: bounded_value_or(source, "WAVEFORM_DEEPGRAM_STT_MODEL", "nova-3", 1, 256)?,
        mip_opt_out: parse_or(source, "WAVEFORM_DEEPGRAM_MIP_OPT_OUT", "true")?,
        timeout: duration_seconds(source, "WAVEFORM_DEEPGRAM_TIMEOUT_SECONDS", 120, 1, 120)?,
        max_concurrency: nonzero_usize(source, "WAVEFORM_DEEPGRAM_MAX_CONCURRENCY", 16, 1_024)?,
    })
}

fn load_audio(source: &impl EnvironmentSource) -> Result<AudioSettings, SettingsError> {
    let settings = AudioSettings {
        ffmpeg_path: PathBuf::from(value_or(source, "WAVEFORM_FFMPEG_PATH", "ffmpeg")?),
        timeout: duration_seconds(source, "WAVEFORM_AUDIO_TIMEOUT_SECONDS", 30, 1, 300)?,
        max_input_bytes: usize_in_range(
            source,
            "WAVEFORM_AUDIO_MAX_INPUT_BYTES",
            MAX_MEDIA_BYTES,
            1_024,
            MAX_MEDIA_BYTES,
        )?,
        max_output_bytes: usize_in_range(
            source,
            "WAVEFORM_AUDIO_MAX_OUTPUT_BYTES",
            MAX_MEDIA_BYTES,
            1_024,
            MAX_MEDIA_BYTES,
        )?,
        bitrate_kbps: u16_in_range(source, "WAVEFORM_AUDIO_BITRATE_KBPS", 128, 32, 320)?,
    };
    if settings.ffmpeg_path.as_os_str().is_empty() {
        return Err(invalid("WAVEFORM_FFMPEG_PATH", "must not be empty"));
    }
    Ok(settings)
}

fn load_idempotency(source: &impl EnvironmentSource) -> Result<IdempotencySettings, SettingsError> {
    let digest_key = secret_required(source, "WAVEFORM_IDEMPOTENCY_DIGEST_KEY", 32, 1_024)?;
    if !(32..=1_024).contains(&digest_key.expose_secret().len()) {
        return Err(invalid(
            "WAVEFORM_IDEMPOTENCY_DIGEST_KEY",
            "must encode to between 32 and 1024 bytes",
        ));
    }
    let settings = IdempotencySettings {
        digest_key,
        ttl: duration_seconds(
            source,
            "WAVEFORM_IDEMPOTENCY_TTL_SECONDS",
            86_400,
            86_400,
            604_800,
        )?,
        lease: duration_seconds(source, "WAVEFORM_IDEMPOTENCY_LEASE_SECONDS", 600, 10, 3_600)?,
        retry_after: duration_seconds(
            source,
            "WAVEFORM_IDEMPOTENCY_RETRY_AFTER_SECONDS",
            2,
            1,
            300,
        )?,
        cleanup_interval: duration_seconds(
            source,
            "WAVEFORM_IDEMPOTENCY_CLEANUP_INTERVAL_SECONDS",
            300,
            10,
            86_400,
        )?,
        cleanup_batch: nonzero_u32(source, "WAVEFORM_IDEMPOTENCY_CLEANUP_BATCH", 1_000, 100_000)?,
    };
    if settings.retry_after >= settings.lease {
        return Err(invalid(
            "WAVEFORM_IDEMPOTENCY_RETRY_AFTER_SECONDS",
            "must be shorter than the ownership lease",
        ));
    }
    if settings.lease >= settings.ttl {
        return Err(invalid(
            "WAVEFORM_IDEMPOTENCY_LEASE_SECONDS",
            "must be shorter than the terminal record lifetime",
        ));
    }
    Ok(settings)
}

fn validate_idempotency_lease(
    idempotency: &IdempotencySettings,
    server: &ServerSettings,
) -> Result<(), SettingsError> {
    let longest_request = server.tts_deadline.max(server.stt_deadline);
    if idempotency.lease < longest_request {
        return Err(invalid(
            "WAVEFORM_IDEMPOTENCY_LEASE_SECONDS",
            "must cover the longest public request deadline",
        ));
    }
    Ok(())
}

fn load_telemetry(source: &impl EnvironmentSource) -> Result<TelemetrySettings, SettingsError> {
    Ok(TelemetrySettings {
        filter: bounded_string(
            "WAVEFORM_LOG_FILTER",
            value_or(
                source,
                "WAVEFORM_LOG_FILTER",
                "silicon_waveform=info,tower_http=info",
            )?,
            1,
            2_048,
        )?,
        json: parse_or(source, "WAVEFORM_LOG_JSON", "false")?,
    })
}

fn validate_workflow_deadlines(
    server: &ServerSettings,
    database: &DatabaseSettings,
    iam: &IamSettings,
    briefcase: &BriefcaseSettings,
    providers: &ProviderSettings,
    audio: &AudioSettings,
) -> Result<(), SettingsError> {
    let tts_attempt_budget = providers
        .gemini
        .tts_timeout
        .saturating_add(providers.elevenlabs.timeout)
        .saturating_add(providers.openai.tts_timeout)
        .saturating_add(audio.timeout.saturating_mul(3))
        .saturating_add(iam.timeout.saturating_mul(2))
        .saturating_add(briefcase.timeout)
        .saturating_add(database.statement_timeout.saturating_mul(2))
        .saturating_add(ORCHESTRATION_HEADROOM);
    if server.tts_deadline < tts_attempt_budget {
        return Err(invalid(
            "WAVEFORM_TTS_DEADLINE_SECONDS",
            "must cover provider, codec, IAM, Briefcase, storage, and orchestration budgets",
        ));
    }

    let stt_attempt_budget = providers
        .gemini
        .stt_timeout
        .saturating_add(providers.openai.stt_timeout)
        .saturating_add(providers.deepgram.timeout)
        .saturating_add(iam.timeout.saturating_mul(2))
        .saturating_add(briefcase.timeout)
        .saturating_add(briefcase.download_timeout)
        .saturating_add(database.statement_timeout.saturating_mul(2))
        .saturating_add(ORCHESTRATION_HEADROOM);
    if server.stt_deadline < stt_attempt_budget {
        return Err(invalid(
            "WAVEFORM_STT_DEADLINE_SECONDS",
            "must cover provider, IAM, Briefcase, storage, and orchestration budgets",
        ));
    }
    Ok(())
}

fn validate_environment_safety(settings: &Settings) -> Result<(), SettingsError> {
    if settings.providers.require_full_chain {
        validate_full_provider_chain(&settings.providers)?;
    }

    if settings.environment != RuntimeEnvironment::Production {
        return Ok(());
    }

    if !settings.providers.require_full_chain {
        return Err(invalid(
            "WAVEFORM_REQUIRE_FULL_PROVIDER_CHAIN",
            "must be true in production",
        ));
    }
    if !settings.telemetry.json {
        return Err(invalid("WAVEFORM_LOG_JSON", "must be true in production"));
    }
    if !settings.audio.ffmpeg_path.is_absolute() {
        return Err(invalid(
            "WAVEFORM_FFMPEG_PATH",
            "must be an absolute path in production",
        ));
    }
    if settings.iam.app_secret.expose_secret().chars().count() < 32 {
        return Err(invalid(
            "WAVEFORM_IAM_APP_SECRET",
            "must contain at least 32 characters in production",
        ));
    }

    for (name, url) in [
        ("WAVEFORM_IAM_BASE_URL", &settings.iam.base_url),
        ("WAVEFORM_BRIEFCASE_BASE_URL", &settings.briefcase.base_url),
        (
            "WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN",
            &settings.briefcase.permanent_origin,
        ),
        (
            "WAVEFORM_BRIEFCASE_CDN_ORIGIN",
            &settings.briefcase.cdn_origin,
        ),
        (
            "WAVEFORM_GEMINI_BASE_URL",
            &settings.providers.gemini.base_url,
        ),
        (
            "WAVEFORM_ELEVENLABS_BASE_URL",
            &settings.providers.elevenlabs.base_url,
        ),
        (
            "WAVEFORM_OPENAI_BASE_URL",
            &settings.providers.openai.base_url,
        ),
        (
            "WAVEFORM_DEEPGRAM_BASE_URL",
            &settings.providers.deepgram.base_url,
        ),
    ] {
        if url.scheme() != "https" {
            return Err(invalid(name, "must use https in production"));
        }
    }
    Ok(())
}

fn validate_full_provider_chain(providers: &ProviderSettings) -> Result<(), SettingsError> {
    for (name, configured) in [
        (
            "WAVEFORM_GEMINI_API_KEY",
            providers.gemini.api_key.is_some(),
        ),
        (
            "WAVEFORM_ELEVENLABS_API_KEY",
            providers.elevenlabs.api_key.is_some(),
        ),
        (
            "WAVEFORM_OPENAI_API_KEY",
            providers.openai.api_key.is_some(),
        ),
        (
            "WAVEFORM_DEEPGRAM_API_KEY",
            providers.deepgram.api_key.is_some(),
        ),
    ] {
        if !configured {
            return Err(SettingsError::Missing(name));
        }
    }
    Ok(())
}

fn validate_database_url(
    environment: RuntimeEnvironment,
    value: &SecretString,
) -> Result<(), SettingsError> {
    let url = Url::parse(value.expose_secret()).map_err(|_| {
        invalid(
            "WAVEFORM_DATABASE_URL",
            "must be a valid PostgreSQL connection URL",
        )
    })?;
    if !matches!(url.scheme(), "postgres" | "postgresql") || url.host_str().is_none() {
        return Err(invalid(
            "WAVEFORM_DATABASE_URL",
            "must use postgres:// or postgresql:// and include a host",
        ));
    }
    if environment == RuntimeEnvironment::Production {
        let ssl_mode = url
            .query_pairs()
            .find_map(|(key, value)| (key == "sslmode").then_some(value.into_owned()));
        if ssl_mode.as_deref() != Some("verify-full") {
            return Err(invalid(
                "WAVEFORM_DATABASE_URL",
                "must set sslmode=verify-full in production",
            ));
        }
    }
    Ok(())
}

fn http_url_or(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: &str,
) -> Result<Url, SettingsError> {
    let value = value_or(source, name, default)?;
    let url = Url::parse(&value).map_err(|_| invalid(name, "must be a valid absolute URL"))?;
    validate_http_url(name, &url)?;
    Ok(url)
}

fn origin_url_or(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: &str,
) -> Result<Url, SettingsError> {
    let url = http_url_or(source, name, default)?;
    if !matches!(url.path(), "" | "/") {
        return Err(invalid(name, "must be an origin without a non-root path"));
    }
    Ok(url)
}

fn validate_http_url(name: &'static str, url: &Url) -> Result<(), SettingsError> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(invalid(name, "must use http or https and include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(name, "must not contain embedded credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid(name, "must not contain a query or fragment"));
    }
    Ok(())
}

fn endpoint_path(name: &'static str, value: String) -> Result<String, SettingsError> {
    let value = bounded_string(name, value, 2, 512)?;
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.contains('?')
        || value.contains('#')
    {
        return Err(invalid(
            name,
            "must be an absolute HTTP path without query or fragment",
        ));
    }
    Ok(value)
}

fn action_name(name: &'static str, value: String) -> Result<String, SettingsError> {
    let value = bounded_string(name, value, 1, 128)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(invalid(
            name,
            "must contain only letters, numbers, period, underscore, hyphen, or colon",
        ));
    }
    Ok(value)
}

fn bounded_value_or(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: &str,
    minimum: usize,
    maximum: usize,
) -> Result<String, SettingsError> {
    bounded_string(name, value_or(source, name, default)?, minimum, maximum)
}

fn bounded_string(
    name: &'static str,
    value: String,
    minimum: usize,
    maximum: usize,
) -> Result<String, SettingsError> {
    let length = value.chars().count();
    if !(minimum..=maximum).contains(&length) || value.chars().any(char::is_control) {
        return Err(invalid(
            name,
            format!("must contain between {minimum} and {maximum} non-control characters"),
        ));
    }
    Ok(value)
}

fn secret_required(
    source: &impl EnvironmentSource,
    name: &'static str,
    minimum: usize,
    maximum: usize,
) -> Result<SecretString, SettingsError> {
    let value = required(source, name)?;
    validate_secret_length(name, &value, minimum, maximum)?;
    Ok(SecretString::from(value))
}

fn optional_secret(
    source: &impl EnvironmentSource,
    name: &'static str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<SecretString>, SettingsError> {
    optional(source, name)?
        .map(|value| {
            validate_secret_length(name, &value, minimum, maximum)?;
            Ok(SecretString::from(value))
        })
        .transpose()
}

fn validate_secret_length(
    name: &'static str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), SettingsError> {
    let length = value.chars().count();
    if !(minimum..=maximum).contains(&length) {
        return Err(invalid(
            name,
            format!("must contain between {minimum} and {maximum} characters"),
        ));
    }
    Ok(())
}

fn duration_seconds(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<Duration, SettingsError> {
    Ok(Duration::from_secs(u64_in_range(
        source, name, default, minimum, maximum,
    )?))
}

fn nonzero_usize(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: usize,
    maximum: usize,
) -> Result<NonZeroUsize, SettingsError> {
    NonZeroUsize::new(usize_in_range(source, name, default, 1, maximum)?)
        .ok_or_else(|| invalid(name, "must be greater than zero"))
}

fn nonzero_u32(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: u32,
    maximum: u32,
) -> Result<NonZeroU32, SettingsError> {
    NonZeroU32::new(u32_in_range(source, name, default, 1, maximum)?)
        .ok_or_else(|| invalid(name, "must be greater than zero"))
}

fn usize_in_range(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> Result<usize, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    validate_range(name, value, minimum, maximum)
}

fn u64_in_range(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    validate_range(name, value, minimum, maximum)
}

fn u32_in_range(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: u32,
    minimum: u32,
    maximum: u32,
) -> Result<u32, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    validate_range(name, value, minimum, maximum)
}

fn u16_in_range(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: u16,
    minimum: u16,
    maximum: u16,
) -> Result<u16, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    validate_range(name, value, minimum, maximum)
}

fn validate_range<T>(
    name: &'static str,
    value: T,
    minimum: T,
    maximum: T,
) -> Result<T, SettingsError>
where
    T: Copy + fmt::Display + PartialOrd,
{
    if value < minimum || value > maximum {
        return Err(invalid(
            name,
            format!("must be between {minimum} and {maximum}"),
        ));
    }
    Ok(value)
}

fn parse_or<T>(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: &str,
) -> Result<T, SettingsError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    let value = value_or(source, name, default)?;
    value
        .parse::<T>()
        .map_err(|error| invalid(name, error.to_string()))
}

fn required(source: &impl EnvironmentSource, name: &'static str) -> Result<String, SettingsError> {
    optional(source, name)?.ok_or(SettingsError::Missing(name))
}

fn value_or(
    source: &impl EnvironmentSource,
    name: &'static str,
    default: &str,
) -> Result<String, SettingsError> {
    Ok(optional(source, name)?.unwrap_or_else(|| default.to_owned()))
}

fn optional(
    source: &impl EnvironmentSource,
    name: &'static str,
) -> Result<Option<String>, SettingsError> {
    Ok(source
        .read(name)?
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty()))
}

fn invalid(name: &'static str, reason: impl Into<String>) -> SettingsError {
    SettingsError::Invalid {
        name,
        reason: reason.into(),
    }
}

trait EnvironmentSource {
    fn read(&self, name: &'static str) -> Result<Option<String>, SettingsError>;
}

struct ProcessEnvironment;

impl EnvironmentSource for ProcessEnvironment {
    fn read(&self, name: &'static str) -> Result<Option<String>, SettingsError> {
        match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(SettingsError::NonUnicode(name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use super::{EnvironmentSource, RuntimeEnvironment, Settings, SettingsError};

    #[derive(Default)]
    struct TestEnvironment(BTreeMap<&'static str, String>);

    impl TestEnvironment {
        fn valid() -> Self {
            Self(BTreeMap::from([
                (
                    "WAVEFORM_DATABASE_URL",
                    "postgres://waveform:password@localhost/waveform".to_owned(),
                ),
                (
                    "WAVEFORM_IAM_APP_SECRET",
                    "a-distinctive-secret-that-must-be-redacted".to_owned(),
                ),
                (
                    "WAVEFORM_IDEMPOTENCY_DIGEST_KEY",
                    "a-distinctive-digest-key-that-must-be-redacted".to_owned(),
                ),
            ]))
        }

        fn set(&mut self, name: &'static str, value: impl Into<String>) {
            self.0.insert(name, value.into());
        }
    }

    impl EnvironmentSource for TestEnvironment {
        fn read(&self, name: &'static str) -> Result<Option<String>, SettingsError> {
            Ok(self.0.get(name).cloned())
        }
    }

    #[test]
    fn defaults_form_a_bounded_development_configuration() -> Result<(), SettingsError> {
        let settings = Settings::from_source(&TestEnvironment::valid())?;

        assert_eq!(settings.environment, RuntimeEnvironment::Development);
        assert_eq!(settings.limits.max_text_chars, 4_096);
        assert_eq!(settings.limits.max_media_bytes, 25 * 1_024 * 1_024);
        assert_eq!(settings.idempotency.ttl, Duration::from_hours(24));
        assert_eq!(settings.server.json_body_limit, 65_536);
        assert_eq!(settings.providers.gemini.max_concurrency.get(), 16);
        assert!(!settings.providers.elevenlabs.enable_logging);
        assert!(settings.providers.deepgram.mip_opt_out);
        assert_eq!(settings.audio.max_input_bytes, 25 * 1_024 * 1_024);
        assert_eq!(settings.briefcase.permanent_origin.scheme(), "https");
        assert_eq!(settings.briefcase.cdn_origin.scheme(), "https");
        assert!(!settings.providers.require_full_chain);
        Ok(())
    }

    #[test]
    fn debug_output_redacts_secret_values() -> Result<(), SettingsError> {
        let settings = Settings::from_source(&TestEnvironment::valid())?;
        let debug = format!("{settings:?}");

        assert!(!debug.contains("a-distinctive-secret-that-must-be-redacted"));
        assert!(!debug.contains("a-distinctive-digest-key-that-must-be-redacted"));
        assert!(!debug.contains("postgres://waveform:password"));
        Ok(())
    }

    #[test]
    fn deadline_must_exceed_complete_provider_budget() {
        let mut environment = TestEnvironment::valid();
        environment.set("WAVEFORM_TTS_DEADLINE_SECONDS", "60");

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_TTS_DEADLINE_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn media_limit_cannot_exceed_synchronous_contract_cap() {
        let mut environment = TestEnvironment::valid();
        environment.set(
            "WAVEFORM_MAX_MEDIA_BYTES",
            (25 * 1_024 * 1_024 + 1).to_string(),
        );

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_MAX_MEDIA_BYTES",
                ..
            })
        ));
    }

    #[test]
    fn json_limit_covers_escaped_non_bmp_text() {
        let mut environment = TestEnvironment::valid();
        environment.set(
            "WAVEFORM_JSON_BODY_LIMIT_BYTES",
            (4_096 * 12 + 1_023).to_string(),
        );

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_JSON_BODY_LIMIT_BYTES",
                ..
            })
        ));
    }

    #[test]
    fn idempotency_lease_covers_the_longest_public_deadline() {
        let mut environment = TestEnvironment::valid();
        environment.set("WAVEFORM_IDEMPOTENCY_LEASE_SECONDS", "599");

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_IDEMPOTENCY_LEASE_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn idempotency_ttl_preserves_the_public_day_minimum() {
        let mut environment = TestEnvironment::valid();
        environment.set("WAVEFORM_IDEMPOTENCY_TTL_SECONDS", "86399");

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_IDEMPOTENCY_TTL_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn idempotency_digest_key_uses_the_domain_byte_bound() {
        let mut environment = TestEnvironment::valid();
        environment.set("WAVEFORM_IDEMPOTENCY_DIGEST_KEY", "é".repeat(1_024));

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_IDEMPOTENCY_DIGEST_KEY",
                ..
            })
        ));
    }

    #[test]
    fn full_chain_requires_every_provider_key() {
        let mut environment = TestEnvironment::valid();
        environment.set("WAVEFORM_REQUIRE_FULL_PROVIDER_CHAIN", "true");

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Missing("WAVEFORM_GEMINI_API_KEY"))
        ));
    }

    #[test]
    fn production_rejects_insecure_database_transport() {
        let mut environment = TestEnvironment::valid();
        environment.set("WAVEFORM_ENVIRONMENT", "production");

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_DATABASE_URL",
                ..
            })
        ));
    }

    #[test]
    fn origin_rejects_paths_that_break_exact_origin_checks() {
        let mut environment = TestEnvironment::valid();
        environment.set(
            "WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN",
            "https://briefcase.example.test/files",
        );

        assert!(matches!(
            Settings::from_source(&environment),
            Err(SettingsError::Invalid {
                name: "WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN",
                ..
            })
        ));
    }

    #[test]
    fn runtime_environment_alias_is_case_insensitive() {
        assert_eq!(
            "PrOd".parse::<RuntimeEnvironment>(),
            Ok(RuntimeEnvironment::Production)
        );
    }
}
