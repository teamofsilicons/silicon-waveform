//! Stateless, typed Rust client for Silicon Waveform: text to speech and speech to text for
//! Carbons and Silicons.
//!
//! Waveform signs accounts in with [Silicon Accounts](https://accounts.teamofsilicons.com).
//! This package never asks for a password and never stores anything; you decide where tokens
//! live (the `waveform` CLI keeps them in a private file under `$SILICON_HOME/.waveform/dir`).
//!
//! - [`signin::SignIn`] gets Waveform's tokens for an account as Waveform's public client (no
//!   app secret): the device flow for Carbons ([`signin::SignIn::start_device`]), a
//!   short-lived token for Silicons ([`signin::SignIn::exchange_slt`]), refresh and sign-out.
//! - [`Client`] calls the Waveform API with the account's access token
//!   ([`Client::with_bearer`]), or, for an app acting for an account, with a Silicon Accounts
//!   User verification proof ([`Client::with_proof`], scopes `waveform.tts` / `waveform.stt`).
//! - A Carbon reads and manages the Waveform history, settings, provider keys and Briefcase
//!   access of the Silicons it is custodian of with [`Client::for_account`]. Nobody runs speech
//!   as another account.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use silicon_waveform_client::{Auth, Client, ExposeSecret as _, TtsRequest, signin::SignIn};
//!
//! // A Silicon mints a short-lived token with `silicon-accounts login --app waveform -q`.
//! let sign_in = SignIn::new(silicon_waveform_client::DEFAULT_ACCOUNTS_URL)?;
//! let tokens = sign_in.exchange_slt("slt_…").await?;
//! println!("signed in as {}", tokens.account.label());
//!
//! let waveform = Client::new(silicon_waveform_client::DEFAULT_API_URL, Auth::Anonymous)?
//!     .with_bearer(tokens.access_token.expose_secret());
//! let request = TtsRequest { text: "Hello, Carbons and Silicons.".into(), ..Default::default() };
//! let speech = waveform.tts(&request, "greeting-0001").await?;
//! println!("{}", speech.file_url.or(speech.audio_url).unwrap_or_default());
//! # Ok(()) }
//! ```
//!
//! Errors are typed ([`Error`]): API failures keep Waveform's
//! `{"error":{"code","message","hint","reason"}}` body ([`ApiError`]), sign-in failures say
//! exactly why Silicon Accounts refused ([`signin::SignInError`]).

mod client;
mod error;
pub mod signin;
/// Retired no-ops: Waveform records no telemetry.
#[deprecated(since = "0.7.0", note = "Waveform records no telemetry")]
pub mod telemetry;
mod tts_options;
mod types;

pub use client::{Auth, Client};
pub use error::{ApiError, Error, Result};
pub use secrecy::{ExposeSecret, SecretString};
pub use tts_options::{
    ElevenLabsTtsOptions, GeminiTtsOptions, OpenAiTtsOptions, TtsProviderOptions,
};
pub use types::*;

/// Waveform's app id at Silicon Accounts. It is also the `client_id` of Waveform's public
/// client (this package and the CLI) and the `receiving_app` of proofs sent to Waveform.
pub const APP_ID: &str = "waveform";
/// The production Waveform API origin.
pub const DEFAULT_API_URL: &str = "https://api.waveform.teamofsilicons.com";
/// The production Silicon Accounts origin.
pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
/// Waveform's documentation.
pub const DOCS_URL: &str = "https://docs.waveform.teamofsilicons.com";
/// Waveform's source repository (open for issues and pull requests).
pub const REPOSITORY_URL: &str = "https://github.com/teamofsilicons/silicon-waveform";
/// How a Carbon signs the CLI in (device flow in the browser).
pub const CARBON_SIGN_IN: &str = "waveform login";
/// How a Silicon mints a short-lived token for Waveform.
pub const SLT_MINT_COMMAND: &str = "silicon-accounts login --app waveform -q";
/// How a Silicon signs the CLI in.
pub const SILICON_SIGN_IN: &str =
    "silicon-accounts login --app waveform -q | waveform login --slt-stdin";
/// Proof scopes Waveform honours from apps acting for an account.
pub const PROOF_SCOPES: [&str; 2] = ["waveform.tts", "waveform.stt"];
/// This package's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The README's examples compile as doctests, so the guide follows the API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// True for a host that is this machine (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`):
/// the only hosts plain `http://` is accepted for, because tokens would otherwise travel
/// unencrypted.
#[must_use]
pub fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod lib_tests {
    #[test]
    fn plain_http_is_recognised_only_for_this_machine() {
        for (url, loopback) in [
            ("http://localhost:4161", true),
            ("http://waveform.localhost", true),
            ("http://127.0.0.1:4161", true),
            ("http://127.8.0.1", true),
            ("http://[::1]:4161", true),
            ("http://10.0.0.2", false),
            ("http://api.waveform.teamofsilicons.com", false),
            ("http://localhost.example.com", false),
        ] {
            assert_eq!(
                super::is_loopback(&url::Url::parse(url).unwrap()),
                loopback,
                "{url}"
            );
        }
    }
}
