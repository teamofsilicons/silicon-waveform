//! Structured process telemetry that avoids secret-bearing payload fields.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::config::{Settings, TelemetrySettings};

/// Installs the global tracing subscriber from the process settings.
///
/// Development normally uses compact human-readable output, while production
/// configuration validation requires newline-delimited JSON.
///
/// # Errors
///
/// Returns an error when the filter directive is invalid or another global
/// subscriber has already been installed.
pub fn init(settings: &Settings) -> anyhow::Result<()> {
    init_config(&settings.telemetry)
}

/// Installs the global tracing subscriber from telemetry-only settings.
///
/// This is useful for small operational processes that do not load the full
/// application composition.
///
/// # Errors
///
/// Returns an error when the filter directive is invalid or another global
/// subscriber has already been installed.
pub fn init_config(settings: &TelemetrySettings) -> anyhow::Result<()> {
    let filter = parse_filter(settings)?;
    let registry = tracing_subscriber::registry().with(filter);

    if settings.json {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_ansi(false)
                    .with_current_span(true)
                    .with_span_list(false),
            )
            .try_init()?;
    } else {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_target(true)
                    .with_thread_ids(false),
            )
            .try_init()?;
    }

    Ok(())
}

fn parse_filter(settings: &TelemetrySettings) -> anyhow::Result<EnvFilter> {
    Ok(EnvFilter::try_new(&settings.filter)?)
}

use space_station::SpaceClient;
use std::time::Duration;

/// Constructs an optional Space Station event sender using deployment configuration.
/// No configured table means recording is unavailable, without breaking application work.
#[must_use]
pub fn from_environment() -> Option<SpaceClient> {
    if std::env::var("WAVEFORM_TELEMETRY")
        .is_ok_and(|v| matches!(v.as_str(), "0" | "false" | "off" | "no"))
    {
        return None;
    }
    let key = std::env::var("WAVEFORM_TELEMETRY_KEY").ok()?;
    initialize_tls();
    SpaceClient::builder(&key)
        .home(space_station::default_home())
        .url(space_station::default_url())
        .flush_timeout(Duration::from_millis(100))
        .on_error(|_| {})
        .build()
        .ok()
}

/// Queues a bounded event consisting only of known action labels and outcome metadata.
pub fn record(client: &SpaceClient, source: &str, step: &str, success: bool, elapsed_ms: u64) {
    client.record(serde_json::json!({"application":"silicon-waveform","version":env!("CARGO_PKG_VERSION"),"source":source,"step":step,"event":"operation_completed","progress":1.0,"success":success,"elapsed_ms":elapsed_ms}));
}

#[cfg(test)]
mod tests {
    use super::parse_filter;
    use crate::config::TelemetrySettings;

    #[test]
    fn accepts_targeted_filter_directives() {
        let settings = TelemetrySettings {
            filter: "silicon_waveform=debug,tower_http=info".to_owned(),
            json: false,
        };

        assert!(parse_filter(&settings).is_ok());
    }

    #[test]
    fn rejects_malformed_filter_directives() {
        let settings = TelemetrySettings {
            filter: "silicon_waveform[broken=trace".to_owned(),
            json: true,
        };

        assert!(parse_filter(&settings).is_err());
    }
}

// Reqwest and PostgreSQL can enable different Rustls providers in the same
// process. Select one before the telemetry WebSocket thread starts. Respect a
// provider already installed by the embedding application.
fn initialize_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tls_tests {
    #[test]
    fn mixed_tls_features_have_an_explicit_process_provider() {
        super::initialize_tls();
        super::initialize_tls();
        let _ = rustls::ClientConfig::builder();
    }
}
