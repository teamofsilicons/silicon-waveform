//! Pure Waveform domain types and policy.
//!
//! This module deliberately contains no HTTP, persistence, or provider wire
//! representations. Adapters translate those representations at the boundary.

pub mod auth;
pub mod capabilities;
pub mod error;
pub mod idempotency;
pub mod identity;
pub mod language;
pub mod media;
pub mod provider;
pub mod speech;
