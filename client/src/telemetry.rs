//! Opt-out diagnostic recording. Secrets and caller-supplied text are never events.
use space_station::SpaceClient;
use std::time::Duration;

/// Constructs an optional Space Station event sender using deployment configuration.
/// No configured table means recording is unavailable, without breaking application work.
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
