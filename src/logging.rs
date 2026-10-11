//! Structured process logs that avoid secret-bearing payload fields.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::config::{LogSettings, Settings};

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
    init_config(&settings.logging)
}

/// Installs the global tracing subscriber from the log settings alone.
///
/// This is useful for small operational processes that do not load the full
/// application composition.
///
/// # Errors
///
/// Returns an error when the filter directive is invalid or another global
/// subscriber has already been installed.
pub fn init_config(settings: &LogSettings) -> anyhow::Result<()> {
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

fn parse_filter(settings: &LogSettings) -> anyhow::Result<EnvFilter> {
    Ok(EnvFilter::try_new(&settings.filter)?)
}

#[cfg(test)]
mod tests {
    use super::parse_filter;
    use crate::config::LogSettings;

    #[test]
    fn accepts_targeted_filter_directives() {
        let settings = LogSettings {
            filter: "silicon_waveform=debug,tower_http=info".to_owned(),
            json: false,
        };

        assert!(parse_filter(&settings).is_ok());
    }

    #[test]
    fn rejects_malformed_filter_directives() {
        let settings = LogSettings {
            filter: "silicon_waveform[broken=trace".to_owned(),
            json: true,
        };

        assert!(parse_filter(&settings).is_err());
    }
}
