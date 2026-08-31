//! Silicon Waveform API process entry point.

#![forbid(unsafe_code)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::unwrap_used)]

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match silicon_waveform::infrastructure::runtime::run_from_env().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("waveform-api terminated: {error}");
            ExitCode::FAILURE
        }
    }
}
