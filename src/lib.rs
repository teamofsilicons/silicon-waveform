//! Silicon Waveform backend library.
//!
//! Waveform exposes provider-independent text-to-speech and speech-to-text
//! workflows. Domain and application policy are isolated from HTTP, database,
//! identity, storage, codec, and speech-provider adapters.

#![forbid(unsafe_code)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::unwrap_used)]

pub mod api;
pub mod application;
pub mod config;
pub mod control;
pub mod domain;
pub mod infrastructure;
pub mod shutdown;
pub mod telemetry;
