//! Retired: Waveform records no telemetry. These no-ops keep code written against 0.6 compiling.
#![allow(deprecated)]

/// Never constructed: Waveform has no telemetry recorder.
#[deprecated(since = "0.7.0", note = "Waveform records no telemetry")]
#[derive(Debug)]
pub enum Recorder {}

/// Always `None`: Waveform records no telemetry.
#[deprecated(since = "0.7.0", note = "Waveform records no telemetry")]
#[must_use]
pub fn from_environment() -> Option<Recorder> {
    None
}

/// Does nothing: Waveform records no telemetry.
#[deprecated(since = "0.7.0", note = "Waveform records no telemetry")]
pub fn record(_recorder: &Recorder, _source: &str, _step: &str, _success: bool, _elapsed_ms: u64) {}
