//! Sign in to Waveform with Silicon Accounts, as Waveform's public client (`client_id`
//! `waveform`, no app secret: a CLI on someone's machine has nowhere to keep one).
//!
//! - **Carbons** use the device flow: [`SignIn::start_device`] returns a code to show, the
//!   Carbon approves it at the verification page in any browser, and
//!   [`SignIn::wait_for_device`] polls (honouring `interval` and `slow_down`) until the code is
//!   approved, denied or expires (10 minutes).
//! - **Silicons** never see a page: they mint a short-lived token with
//!   `silicon-accounts login --app waveform -q` and hand it over; [`SignIn::exchange_slt`]
//!   exchanges it (single use, 2 minutes, bound to Waveform).
//! - [`SignIn::refresh`] rotates the refresh token. Presenting a used refresh token ends the
//!   whole sign-in, so refresh one at a time and store the new pair before using it.
//! - [`SignIn::revoke`] ends the sign-in at Silicon Accounts.
//!
//! Nothing here reads files or the environment, and tokens are [`SecretString`]s whose `Debug`
//! output hides the value.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret as _, SecretString};
use serde_json::Value;
use silicon_accounts_client::{
    AccountsClient, DevicePoll as AccountsPoll, OAuthError, SLT_GRANT_TYPE, TokenResponse,
};
use url::Url;

use crate::types::{Account, AccountRef};
use crate::{APP_ID, CARBON_SIGN_IN, SILICON_SIGN_IN, SLT_MINT_COMMAND};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// Waveform's tokens for one account, from a sign-in or a refresh.
#[derive(Clone)]
pub struct Tokens {
    /// Silicon Accounts access token for `waveform` (EdDSA JWT, 30 minutes).
    pub access_token: SecretString,
    /// Rotating refresh token (`sar_…`); every refresh returns a new one.
    pub refresh_token: SecretString,
    /// When the access token expires (Unix seconds, by this machine's clock at receipt).
    pub expires_at: i64,
    /// When the sign-in itself ends (Unix seconds), if Silicon Accounts said.
    pub refresh_expires_at: Option<i64>,
    /// Granted scopes, space-separated.
    pub scope: String,
    /// The signed-in account.
    pub account: Account,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    fn from_response(response: TokenResponse, received_at: i64) -> Result<Self, SignInError> {
        let unexpected = |what: &str| SignInError::Accounts {
            status: None,
            code: "unexpected_response".into(),
            message: format!("Silicon Accounts answered the sign-in without {what}."),
            hint: Some("Retry; if it keeps happening, report it with `waveform report`.".into()),
        };
        let refresh_token = response
            .refresh_token
            .as_ref()
            .ok_or_else(|| unexpected("a refresh token"))?;
        let account = response
            .account
            .as_ref()
            .ok_or_else(|| unexpected("the account"))?;
        let text = |value: &str| Some(value.to_owned()).filter(|value| !value.is_empty());
        Ok(Self {
            access_token: SecretString::from(response.access_token.expose().to_owned()),
            refresh_token: SecretString::from(refresh_token.expose().to_owned()),
            expires_at: received_at
                .saturating_add(i64::try_from(response.expires_in).unwrap_or(i64::MAX)),
            refresh_expires_at: response
                .refresh_token_expires_at
                .map(|at| at.unix_timestamp()),
            scope: response.scope.clone().unwrap_or_default(),
            account: Account {
                uuid: account.uuid.clone(),
                id: text(&account.id),
                kind: account.kind.as_str().to_owned(),
                display_name: text(&account.display_name),
                pfp_url: text(&account.pfp_url),
                custodian: account.custodian.as_ref().map(|custodian| AccountRef {
                    uuid: custodian.uuid.clone(),
                    id: text(&custodian.id),
                }),
            },
        })
    }
}

/// A device sign-in in progress. Show [`DeviceCode::user_code`] and
/// [`DeviceCode::browser_url`]; the device code itself is never shown.
#[derive(Clone)]
pub struct DeviceCode {
    /// The code the Carbon confirms, e.g. `WDJB-MJHT`.
    pub user_code: String,
    /// Where to approve, e.g. `https://accounts.teamofsilicons.com/device`.
    pub verification_uri: String,
    /// The same page with the code filled in.
    pub verification_uri_complete: Option<String>,
    /// Seconds the code stays valid (600).
    pub expires_in: u64,
    /// Seconds between polls (5).
    pub interval: u64,
    /// When the code stops working (Unix seconds).
    pub expires_at: i64,
    device_code: SecretString,
}

impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCode")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl DeviceCode {
    /// The page to open: the one with the code filled in when Silicon Accounts sent it.
    #[must_use]
    pub fn browser_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// One poll of a device sign-in that did not fail.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DevicePoll {
    /// Not approved yet; poll again after the interval.
    Pending,
    /// Polling too fast; add 5 seconds to the interval.
    SlowDown,
    /// Approved: Waveform's tokens for the Carbon.
    Approved(Box<Tokens>),
}

/// What [`SignIn::wait_for_device`] reports after each poll that did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceProgress {
    /// Not approved yet.
    Pending,
    /// Silicon Accounts asked to poll more slowly; the new interval in seconds.
    SlowDown {
        /// The new interval, in seconds.
        interval: u64,
    },
    /// A poll failed for a passing reason (network, 5xx, rate limit); polling continues.
    Retrying {
        /// What failed.
        message: String,
    },
}

/// Why Silicon Accounts refused a short-lived token. A refused token is used up either way.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SltRefusal {
    /// It was already exchanged once.
    AlreadyUsed,
    /// Its 2 minutes passed.
    Expired,
    /// It was minted for another app (named when Silicon Accounts said which).
    WrongApp {
        /// The app it was minted for.
        app: Option<String>,
    },
    /// Mistyped, never issued, or issued by another Silicon Accounts deployment.
    Unknown,
    /// Not a short-lived token at all (an access or refresh token, for example).
    NotAShortLivedToken,
    /// The account removed Waveform's access after the token was minted.
    AccessRemoved,
    /// The Silicon's sign-in that minted it ended (STK rotated, CI sign-in over, trust removed).
    SignInEnded,
    /// Another refusal; the description says why.
    Other,
}

impl SltRefusal {
    fn classify(description: &str) -> Self {
        let text = description.to_ascii_lowercase();
        if text.contains("already used") {
            Self::AlreadyUsed
        } else if text.contains("expired at") || text.contains("token expired") {
            Self::Expired
        } else if text.contains("was issued for the app") {
            Self::WrongApp {
                app: description
                    .split('\'')
                    .nth(1)
                    .filter(|app| !app.is_empty())
                    .map(str::to_owned),
            }
        } else if text.contains("is not known") {
            Self::Unknown
        } else if text.contains("must be a short-lived token") || text.contains("starts with slt_")
        {
            Self::NotAShortLivedToken
        } else if text.contains("removed the access of the app") {
            Self::AccessRemoved
        } else if text.contains("rotated its stk")
            || text.contains("trusted outside token")
            || text.contains("removed that trust")
        {
            Self::SignInEnded
        } else {
            Self::Other
        }
    }

    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::AlreadyUsed => "slt_already_used",
            Self::Expired => "slt_expired",
            Self::WrongApp { .. } => "slt_wrong_app",
            Self::Unknown => "slt_unknown",
            Self::NotAShortLivedToken => "slt_not_a_short_lived_token",
            Self::AccessRemoved => "slt_access_removed",
            Self::SignInEnded => "slt_sign_in_ended",
            Self::Other => "slt_refused",
        }
    }
}

/// Everything that can go wrong signing in. Each error says what failed and why
/// ([`SignInError::message`]) and what to do next ([`SignInError::hint`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignInError {
    /// A setting is wrong before anything was sent (URL, empty token…).
    #[error("{message}")]
    Configuration {
        /// What is wrong.
        message: String,
        /// How to fix it.
        hint: String,
    },
    /// Silicon Accounts refused a short-lived token; it is used up either way.
    #[error("{description}")]
    SltRefused {
        /// Why, classified.
        reason: SltRefusal,
        /// Silicon Accounts' exact description.
        description: String,
    },
    /// The refresh token no longer works: this sign-in ended (signed out on another machine or
    /// on the account site, Waveform's access removed, STK rotated, the refresh token presented
    /// twice, or the sign-in expired).
    #[error("{description}")]
    SignInEnded {
        /// Silicon Accounts' exact description.
        description: String,
    },
    /// The Carbon denied the device sign-in on the account site.
    #[error("The sign-in request {user_code} was denied on the account site.")]
    DeviceDenied {
        /// The code that was denied.
        user_code: String,
    },
    /// Nobody approved the device code in time.
    #[error("The code {user_code} expired before anyone approved it.")]
    DeviceExpired {
        /// The code that expired.
        user_code: String,
    },
    /// Waveform's sign-in setup at this Silicon Accounts does not allow this grant.
    #[error("{description}")]
    NotEnabled {
        /// The grant that is not allowed.
        grant: String,
        /// Silicon Accounts' exact description.
        description: String,
    },
    /// Any other answer from Silicon Accounts, with its code.
    #[error("{message}")]
    Accounts {
        /// HTTP status, when there was an answer.
        status: Option<u16>,
        /// Silicon Accounts' error code.
        code: String,
        /// What failed and why.
        message: String,
        /// What to do next, when Silicon Accounts said.
        hint: Option<String>,
    },
    /// No answer: connection refused, DNS, TLS or timeout.
    #[error("{message}")]
    Transport {
        /// What failed.
        message: String,
    },
}

impl SignInError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::Configuration { .. } => "sign_in_configuration".into(),
            Self::SltRefused { reason, .. } => reason.code().into(),
            Self::SignInEnded { .. } => "sign_in_ended".into(),
            Self::DeviceDenied { .. } => "device_denied".into(),
            Self::DeviceExpired { .. } => "device_code_expired".into(),
            Self::NotEnabled { .. } => "sign_in_not_enabled".into(),
            Self::Accounts { code, .. } => code.clone(),
            Self::Transport { .. } => "accounts_unreachable".into(),
        }
    }

    /// What failed and why.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::SignInEnded { description } => {
                format!("Your Waveform sign-in ended: {description}")
            }
            Self::SltRefused {
                reason: SltRefusal::Unknown,
                description,
            } => format!(
                "{description} (A short-lived token is only known to the Silicon Accounts that minted it.)"
            ),
            _ => self.to_string(),
        }
    }

    /// What to do next.
    #[must_use]
    pub fn hint(&self) -> String {
        let fresh = format!("Mint a fresh one and use it at once: {SILICON_SIGN_IN}");
        match self {
            Self::Configuration { hint, .. } => hint.clone(),
            Self::SltRefused { reason, .. } => match reason {
                SltRefusal::AlreadyUsed => format!("Short-lived tokens work once. {fresh}"),
                SltRefusal::Expired => format!("Short-lived tokens last 2 minutes. {fresh}"),
                SltRefusal::WrongApp { app } => format!(
                    "That token was minted for {}; Waveform needs one minted with --app waveform: {SILICON_SIGN_IN}",
                    app.as_deref().unwrap_or("another app")
                ),
                SltRefusal::Unknown => format!(
                    "Pass the whole token, and sign in at the Silicon Accounts that minted it (ACCOUNTS_URL or --accounts-url). {fresh}"
                ),
                SltRefusal::NotAShortLivedToken => format!(
                    "Pass a short-lived token (slt_…), not an access or refresh token. {fresh}"
                ),
                SltRefusal::AccessRemoved => format!(
                    "The account removed Waveform's access after that token was minted; a token minted now signs in again: {SILICON_SIGN_IN}"
                ),
                SltRefusal::SignInEnded => format!(
                    "Sign the Silicon in to Silicon Accounts again (silicon-accounts login --silicon si:<id> …), then: {SILICON_SIGN_IN}"
                ),
                SltRefusal::Other => fresh,
            },
            Self::SignInEnded { .. } => format!(
                "Sign in again: Carbons run `{CARBON_SIGN_IN}`; Silicons run `{SILICON_SIGN_IN}`."
            ),
            Self::DeviceDenied { .. } => {
                format!("If that was a mistake, run `{CARBON_SIGN_IN}` again and approve the new code.")
            }
            Self::DeviceExpired { .. } => format!(
                "Run `{CARBON_SIGN_IN}` again and approve the new code within 10 minutes."
            ),
            Self::NotEnabled { grant, .. } => format!(
                "Waveform's sign-in setup at this Silicon Accounts must allow {grant} (device_flow and public_client). Check ACCOUNTS_URL / --accounts-url: it must be the Silicon Accounts this Waveform uses."
            ),
            Self::Accounts { hint, .. } => hint.clone().unwrap_or_else(|| {
                "Retry; if it keeps failing, report it with `waveform report`.".into()
            }),
            Self::Transport { .. } => "Check the network and the Silicon Accounts URL (ACCOUNTS_URL or --accounts-url), then retry.".into(),
        }
    }

    /// A passing failure worth retrying later (network, 5xx, rate limit).
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Accounts {
                status: Some(status),
                ..
            } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

/// Which request an error answers, to word `invalid_grant` precisely.
#[derive(Clone, Copy)]
enum Grant {
    Device,
    Slt,
    Refresh,
    Revoke,
}

fn from_accounts(error: silicon_accounts_client::Error, grant: Grant) -> SignInError {
    use silicon_accounts_client::Error as E;
    if let E::OAuth(oauth) = &error {
        let description = oauth.message();
        match (oauth.error.as_str(), grant) {
            ("invalid_grant", Grant::Slt) => {
                return SignInError::SltRefused {
                    reason: SltRefusal::classify(&description),
                    description,
                };
            }
            ("invalid_grant", Grant::Refresh) => return SignInError::SignInEnded { description },
            ("unauthorized_client" | "invalid_client", _) => {
                return SignInError::NotEnabled {
                    grant: match grant {
                        Grant::Device => "the device flow",
                        Grant::Slt => "short-lived token exchange without a secret",
                        Grant::Refresh | Grant::Revoke => "public clients",
                    }
                    .into(),
                    description,
                };
            }
            _ => {}
        }
    }
    if let E::Http { message, hint, .. } = &error {
        return SignInError::Transport {
            message: format!("{message} {hint}"),
        };
    }
    SignInError::Accounts {
        status: error.status(),
        code: error.code().to_owned(),
        message: error.message(),
        hint: error.hint(),
    }
}

/// Decodes an error answer of the token or revoke endpoint (OAuth or standard body).
fn from_body(status: u16, body: &[u8], grant: Grant) -> SignInError {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        let description = value
            .get("error_description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        return from_accounts(
            silicon_accounts_client::Error::OAuth(Box::new(OAuthError::new(
                status,
                error,
                description,
            ))),
            grant,
        );
    }
    if let Some(error) = value.get("error").filter(|error| error.is_object()) {
        let text = |key: &str| error.get(key).and_then(Value::as_str).map(str::to_owned);
        return SignInError::Accounts {
            status: Some(status),
            code: text("code").unwrap_or_else(|| "accounts_error".into()),
            message: text("message")
                .unwrap_or_else(|| format!("Silicon Accounts answered HTTP {status}.")),
            hint: text("hint"),
        };
    }
    SignInError::Accounts {
        status: Some(status),
        code: "unexpected_response".into(),
        message: format!(
            "Silicon Accounts answered HTTP {status} without an error body this client understands."
        ),
        hint: Some("Check that ACCOUNTS_URL points at Silicon Accounts, then retry.".into()),
    }
}

/// Silicon Accounts as Waveform's public client: device flow, short-lived token exchange,
/// refresh and sign-out, with Waveform's app id as `client_id` and no secret.
#[derive(Clone)]
pub struct SignIn {
    accounts: AccountsClient,
    http: reqwest::Client,
    base: Url,
    app_id: String,
    poll_unit: Duration,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("accounts_url", &self.accounts_url())
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl SignIn {
    /// Signs in to Waveform (`client_id` `waveform`) at a Silicon Accounts origin
    /// (`https://…`, or `http://` for this machine only).
    ///
    /// # Errors
    /// An invalid Silicon Accounts URL.
    pub fn new(accounts_url: &str) -> Result<Self, SignInError> {
        Self::for_app(accounts_url, APP_ID)
    }

    /// Like [`SignIn::new`] for another app id (a Waveform deployment registered under
    /// another id).
    ///
    /// # Errors
    /// An invalid Silicon Accounts URL or an empty app id.
    pub fn for_app(accounts_url: &str, app_id: &str) -> Result<Self, SignInError> {
        let user_agent = format!("silicon-waveform-client/{}", crate::VERSION);
        let accounts = AccountsClient::builder()
            .base_url(accounts_url)
            .user_agent(user_agent.clone())
            // Waveform records no telemetry and asks Silicon Accounts to record none for its calls.
            .telemetry(false)
            .build()
            .map_err(|error| SignInError::Configuration {
                message: error.message(),
                hint: error.hint().unwrap_or_default(),
            })?;
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return Err(SignInError::Configuration {
                message: "The app id to sign in to is empty.".into(),
                hint: format!("Use Waveform's app id, `{APP_ID}`."),
            });
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(user_agent)
            .build()
            .map_err(|error| SignInError::Configuration {
                message: format!("Could not create the HTTP client: {error}."),
                hint: "The TLS backend failed to start; check the system certificate store.".into(),
            })?;
        Ok(Self {
            base: accounts.base_url().clone(),
            accounts,
            http,
            app_id: app_id.to_owned(),
            poll_unit: Duration::from_secs(1),
        })
    }

    /// The Silicon Accounts origin.
    #[must_use]
    pub fn accounts_url(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    /// The app id used as `client_id`.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Scales the device flow's seconds (tests only; Silicon Accounts asks for 5-second polls).
    #[doc(hidden)]
    #[must_use]
    pub fn with_poll_unit(mut self, unit: Duration) -> Self {
        self.poll_unit = unit.max(Duration::from_millis(1));
        self
    }

    /// `POST /v1/device/authorize`: starts a Carbon's device sign-in. Show the user code and
    /// [`DeviceCode::browser_url`], then call [`SignIn::wait_for_device`].
    ///
    /// # Errors
    /// The device flow is not enabled for Waveform at this Silicon Accounts, or it cannot be
    /// reached.
    pub async fn start_device(
        &self,
        client_label: Option<&str>,
    ) -> Result<DeviceCode, SignInError> {
        let started = now();
        let authorization = self
            .accounts
            .app_device_authorize(&self.app_id, None, client_label)
            .await
            .map_err(|error| from_accounts(error, Grant::Device))?;
        Ok(DeviceCode {
            user_code: authorization.user_code.clone(),
            verification_uri: authorization.verification_uri.clone(),
            verification_uri_complete: authorization.verification_uri_complete.clone(),
            expires_in: authorization.expires_in,
            interval: authorization.interval.max(1),
            expires_at: started
                .saturating_add(i64::try_from(authorization.expires_in).unwrap_or(i64::MAX)),
            device_code: SecretString::from(authorization.device_code.expose().to_owned()),
        })
    }

    /// Polls the device sign-in once.
    ///
    /// # Errors
    /// [`SignInError::DeviceDenied`], [`SignInError::DeviceExpired`], or a failure to reach
    /// Silicon Accounts.
    pub async fn poll_device(&self, device: &DeviceCode) -> Result<DevicePoll, SignInError> {
        let poll = self
            .accounts
            .app_device_poll(&self.app_id, device.device_code.expose_secret())
            .await
            .map_err(|error| from_accounts(error, Grant::Device))?;
        match poll {
            AccountsPoll::Pending => Ok(DevicePoll::Pending),
            AccountsPoll::SlowDown => Ok(DevicePoll::SlowDown),
            AccountsPoll::Denied => Err(SignInError::DeviceDenied {
                user_code: device.user_code.clone(),
            }),
            AccountsPoll::Expired => Err(SignInError::DeviceExpired {
                user_code: device.user_code.clone(),
            }),
            AccountsPoll::Tokens(tokens) => Ok(DevicePoll::Approved(Box::new(
                Tokens::from_response(*tokens, now())?,
            ))),
            _ => Err(SignInError::Accounts {
                status: None,
                code: "unexpected_response".into(),
                message:
                    "Silicon Accounts answered the device poll in a way this client does not know."
                        .into(),
                hint: Some(
                    "Update the waveform CLI; Silicon Apps installs updates within minutes.".into(),
                ),
            }),
        }
    }

    /// Polls until the Carbon approves, denies, or the code expires, waiting `interval` seconds
    /// between polls and 5 seconds longer after each `slow_down`. Passing failures (network,
    /// 5xx, rate limits) are reported to `progress` and polling continues until the code expires.
    ///
    /// # Errors
    /// Denied, expired, or a permanent failure.
    pub async fn wait_for_device(
        &self,
        device: &DeviceCode,
        mut progress: impl FnMut(&DeviceProgress),
    ) -> Result<Tokens, SignInError> {
        let mut interval = device.interval.max(1);
        loop {
            if now() >= device.expires_at {
                return Err(SignInError::DeviceExpired {
                    user_code: device.user_code.clone(),
                });
            }
            let wait = u32::try_from(interval).unwrap_or(u32::MAX);
            tokio::time::sleep(self.poll_unit.saturating_mul(wait)).await;
            match self.poll_device(device).await {
                Ok(DevicePoll::Approved(tokens)) => return Ok(*tokens),
                Ok(DevicePoll::Pending) => progress(&DeviceProgress::Pending),
                Ok(DevicePoll::SlowDown) => {
                    interval = interval.saturating_add(5);
                    progress(&DeviceProgress::SlowDown { interval });
                }
                Err(error) if error.is_transient() => {
                    if matches!(
                        error,
                        SignInError::Accounts {
                            status: Some(429),
                            ..
                        }
                    ) {
                        interval = interval.saturating_add(5);
                    }
                    progress(&DeviceProgress::Retrying {
                        message: error.message(),
                    });
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Exchanges a Silicon's short-lived token (`slt_…`, from `silicon-accounts login --app
    /// waveform -q`) for Waveform's tokens, with `client_id` alone.
    ///
    /// # Errors
    /// [`SignInError::SltRefused`] with the exact reason (already used, expired, minted for
    /// another app…), or a failure to reach Silicon Accounts. A value that is not a short-lived
    /// token is refused before anything is sent.
    pub async fn exchange_slt(&self, slt: &str) -> Result<Tokens, SignInError> {
        let slt = slt.trim();
        if slt.is_empty() {
            return Err(SignInError::Configuration {
                message: "The short-lived token is empty.".into(),
                hint: format!(
                    "Mint one with `{SLT_MINT_COMMAND}` and pass it at once: {SILICON_SIGN_IN}"
                ),
            });
        }
        if !slt.starts_with("slt_")
            || slt.len() > 4096
            || !slt.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(SignInError::SltRefused {
                reason: SltRefusal::NotAShortLivedToken,
                description: "That is not a Silicon Accounts short-lived token: they start with slt_ and contain no spaces. It was not sent anywhere.".into(),
            });
        }
        let bytes = self
            .post_form(
                &["v1", "oauth", "token"],
                &[
                    ("grant_type", SLT_GRANT_TYPE),
                    ("slt", slt),
                    ("client_id", &self.app_id),
                ],
                Grant::Slt,
            )
            .await?;
        let received = now();
        let response: TokenResponse =
            serde_json::from_slice(&bytes).map_err(|error| SignInError::Accounts {
                status: None,
                code: "unexpected_response".into(),
                message: format!("Silicon Accounts answered the exchange with a body this client does not understand: {error}."),
                hint: Some("Check that ACCOUNTS_URL points at Silicon Accounts, then sign in again with a fresh token.".into()),
            })?;
        Tokens::from_response(response, received)
    }

    /// Rotates a refresh token. The old one stops working at once; store the new pair before
    /// using it, and never refresh the same token twice (a reused refresh token ends the whole
    /// sign-in).
    ///
    /// # Errors
    /// [`SignInError::SignInEnded`] when the sign-in is over, or a failure to reach Silicon
    /// Accounts (the old refresh token may or may not have been used then).
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, SignInError> {
        let response = self
            .accounts
            .refresh_app_public_client(&self.app_id, refresh_token)
            .await
            .map_err(|error| from_accounts(error, Grant::Refresh))?;
        Tokens::from_response(response, now())
    }

    /// Ends the sign-in at Silicon Accounts (`POST /v1/oauth/revoke` with `client_id` alone).
    /// Unknown or already revoked tokens succeed (RFC 7009).
    ///
    /// # Errors
    /// An empty token, or a failure to reach Silicon Accounts.
    pub async fn revoke(&self, refresh_token: &str) -> Result<(), SignInError> {
        let token = refresh_token.trim();
        if token.is_empty() {
            return Err(SignInError::Configuration {
                message: "There is no refresh token to revoke.".into(),
                hint: "Nothing to sign out of.".into(),
            });
        }
        let hint = if token.starts_with("sar_") {
            "refresh_token"
        } else {
            "access_token"
        };
        self.post_form(
            &["v1", "oauth", "revoke"],
            &[
                ("token", token),
                ("token_type_hint", hint),
                ("client_id", &self.app_id),
            ],
            Grant::Revoke,
        )
        .await
        .map(|_| ())
    }

    async fn post_form(
        &self,
        path: &[&str],
        form: &[(&str, &str)],
        grant: Grant,
    ) -> Result<Vec<u8>, SignInError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| SignInError::Configuration {
                message: "The Silicon Accounts URL cannot carry a path.".into(),
                hint: "Use an origin such as https://accounts.teamofsilicons.com.".into(),
            })?
            .pop_if_empty()
            .extend(path);
        let what = format!("POST {}", url.path());
        let mut attempt = 0;
        let response = loop {
            match self
                .http
                .post(url.clone())
                .header(reqwest::header::ACCEPT, "application/json")
                .header(silicon_accounts_client::TELEMETRY_HEADER, "off")
                .form(form)
                .send()
                .await
            {
                Ok(response) => break response,
                // Only a connection that was never established is certainly not applied.
                Err(error) if error.is_connect() && attempt < 2 => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
                }
                Err(error) => return Err(self.transport(&error, &what)),
            }
        };
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| self.transport(&error, &what))?;
        if (200..300).contains(&status) {
            Ok(bytes.to_vec())
        } else {
            Err(from_body(status, &bytes, grant))
        }
    }

    fn transport(&self, error: &reqwest::Error, what: &str) -> SignInError {
        let base = self.accounts_url();
        let mut cause = String::new();
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(inner) = source {
            cause = inner.to_string();
            source = inner.source();
        }
        SignInError::Transport {
            message: if error.is_timeout() {
                format!("Silicon Accounts at {base} did not answer {what} in time.")
            } else {
                format!("Could not reach Silicon Accounts at {base} ({what}): {cause}.")
            },
        }
    }
}
