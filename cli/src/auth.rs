//! Signing in, the saved sign-in's lifecycle (single-flight refresh under the file lock), and
//! authenticated calls that refresh once when Waveform says the token went stale.

use std::{
    future::Future,
    io::{IsTerminal as _, Read as _},
    path::PathBuf,
    time::Duration,
};

use serde_json::{Value, json};
use silicon_waveform_client::{
    APP_ID, Auth, CARBON_SIGN_IN, Client, SILICON_SIGN_IN, SLT_MINT_COMMAND,
    signin::{DeviceProgress, SignIn, SignInError, Tokens},
};

use crate::args::{LoginArgs, StatusArgs};
use crate::output::{CliError, CliResult, EXIT_AUTH, EXIT_INVALID, Outcome, Output};
use crate::session::{Loaded, SessionStore, StoredSession, normalize_url, now};

/// Everything a command needs to know about where it runs.
pub struct Ctx {
    pub out: Output,
    pub api_url: String,
    pub accounts_url: String,
    pub profile: String,
    pub profile_dir: PathBuf,
}

impl Ctx {
    pub fn store(&self) -> SessionStore {
        SessionStore::new(self.profile_dir.clone(), &self.accounts_url)
    }

    pub fn api(&self) -> CliResult<Client> {
        Ok(Client::new(&self.api_url, Auth::Anonymous)?)
    }

    pub fn signin(&self) -> CliResult<SignIn> {
        let signin = SignIn::new(&self.accounts_url)?;
        // Test hook: scale the device flow's 5-second polls, honoured only for a Silicon
        // Accounts on this machine (the CLI's integration tests run a stub there).
        let local = url::Url::parse(&self.accounts_url)
            .is_ok_and(|url| silicon_waveform_client::is_loopback(&url));
        match std::env::var("WAVEFORM_TEST_POLL_UNIT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            Some(ms) if local => Ok(signin.with_poll_unit(Duration::from_millis(ms))),
            _ => Ok(signin),
        }
    }

    fn not_signed_in(&self) -> CliError {
        let store = self.store();
        let mut message = format!(
            "This machine is not signed in to Waveform (profile `{}`, Silicon Accounts {}).",
            self.profile, self.accounts_url
        );
        if !store.legacy_files().is_empty() {
            message.push_str(" A sign-in saved by an earlier Waveform version was found; it cannot be used any more.");
        } else if let Some(other) = store.other_origins().first() {
            message.push_str(&format!(
                " This profile is signed in at {other} instead; pass --accounts-url {other} (or ACCOUNTS_URL) to use it."
            ));
        }
        CliError::new(
            EXIT_AUTH,
            "not_signed_in",
            message,
            format!("Carbons: `{CARBON_SIGN_IN}`. Silicons: `{SILICON_SIGN_IN}`."),
        )
    }
}

fn unreadable(path: &std::path::Path, why: &str) -> CliError {
    CliError::new(
        EXIT_AUTH,
        "session_unreadable",
        format!(
            "The saved sign-in {} cannot be used: {why}.",
            path.display()
        ),
        format!(
            "Sign in again (Carbons: `{CARBON_SIGN_IN}`; Silicons: `{SILICON_SIGN_IN}`); the new sign-in replaces it."
        ),
    )
}

fn ended(session: &StoredSession, why: &str) -> CliError {
    CliError::new(
        EXIT_AUTH,
        "sign_in_ended",
        format!(
            "The Waveform sign-in of {} ended: {why}",
            session.account.label()
        ),
        format!("Sign in again: Carbons run `{CARBON_SIGN_IN}`; Silicons run `{SILICON_SIGN_IN}`."),
    )
}

/// RFC 3339 for a Unix time.
pub fn rfc3339(unix: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| unix.to_string())
}

/// The saved sign-in, refreshed under the lock when it has less than 60 seconds left (or when
/// `refused` names the access token Waveform just refused as stale). Another process may have
/// refreshed while this one waited for the lock: its result is used, never refreshed twice.
pub async fn ensure_fresh(ctx: &Ctx, refused: Option<&str>) -> CliResult<StoredSession> {
    let store = ctx.store();
    let current = match store.load() {
        Loaded::Missing => return Err(ctx.not_signed_in()),
        Loaded::Unreadable { path, why } => return Err(unreadable(&path, &why)),
        Loaded::Session { session, warning } => {
            if let Some(warning) = warning {
                ctx.out.warn(&warning);
            }
            *session
        }
    };
    if current.ended(now()) {
        return Err(ended(
            &current,
            &format!(
                "it expired at {}.",
                rfc3339(current.refresh_expires_at.unwrap_or_default())
            ),
        ));
    }
    if refused.is_none() && !current.needs_refresh(now()) {
        return Ok(current);
    }
    let _lock = store.lock().await?;
    let latest = match store.load() {
        Loaded::Missing => return Err(ctx.not_signed_in()),
        Loaded::Unreadable { path, why } => return Err(unreadable(&path, &why)),
        Loaded::Session { session, .. } => *session,
    };
    if latest.account.uuid != current.account.uuid {
        return Err(CliError::new(
            EXIT_AUTH,
            "session_changed",
            format!(
                "The saved sign-in changed while this command ran: this profile is now signed in as {}.",
                latest.account.label()
            ),
            "Run the command again.",
        ));
    }
    let someone_else_refreshed = match refused {
        Some(token) => latest.access_token != token && !latest.needs_refresh(now()),
        None => !latest.needs_refresh(now()),
    };
    if someone_else_refreshed {
        return Ok(latest);
    }
    match ctx.signin()?.refresh(&latest.refresh_token).await {
        Ok(tokens) => {
            let rotated = latest.rotated(&tokens);
            store.save(&rotated)?;
            Ok(rotated)
        }
        Err(SignInError::SignInEnded { description }) => {
            if let Loaded::Session { session, .. } = store.load()
                && session.refresh_token == latest.refresh_token
            {
                store.remove()?;
            }
            Err(ended(&latest, &description))
        }
        Err(error) => Err(error.into()),
    }
}

/// Runs an authenticated call with the saved sign-in. When Waveform answers that the token is
/// stale (expired, revoked after it was issued, unknown signing key), the sign-in is refreshed
/// once and the call repeated once; Waveform refused it before doing any work.
pub async fn call<T, F, Fut>(ctx: &Ctx, op: F) -> CliResult<T>
where
    F: Fn(Client) -> Fut,
    Fut: Future<Output = silicon_waveform_client::Result<T>>,
{
    call_with_session(ctx, op).await.map(|(value, _)| value)
}

pub async fn call_with_session<T, F, Fut>(ctx: &Ctx, op: F) -> CliResult<(T, StoredSession)>
where
    F: Fn(Client) -> Fut,
    Fut: Future<Output = silicon_waveform_client::Result<T>>,
{
    let api = ctx.api()?;
    let session = ensure_fresh(ctx, None).await?;
    match op(api.with_bearer(session.access_token.clone())).await {
        Ok(value) => Ok((value, session)),
        Err(error) if error.is_stale_credential() => {
            let session = ensure_fresh(ctx, Some(&session.access_token)).await?;
            let value = op(api.with_bearer(session.access_token.clone())).await?;
            Ok((value, session))
        }
        Err(error) => Err(error.into()),
    }
}

/// The client for a command that may act for an account with a proof (`--proof-file`).
pub fn proof_client(ctx: &Ctx, proof_file: &std::path::Path) -> CliResult<Client> {
    let proof = crate::commands::read_secret_file(proof_file, "proof", 16_384)?;
    Ok(ctx.api()?.with_proof(proof)?)
}

// ---- status output ----------------------------------------------------------------------

/// The `login status --json` object of a signed-in profile.
pub fn status_json(ctx: &Ctx, session: &StoredSession, verified: bool) -> Value {
    json!({
        "authenticated": true,
        "app_id": APP_ID,
        "uuid": session.account.uuid,
        "id": session.account.id,
        "kind": session.account.kind,
        "display_name": session.account.display_name,
        "custodian": session.account.custodian,
        "expires_at": rfc3339(session.expires_at),
        "refresh_expires_at": session.refresh_expires_at.map(rfc3339),
        "accounts_url": session.accounts_url,
        "api_url": normalize_url(&ctx.api_url),
        "profile": ctx.profile,
        "method": session.method,
        "verified": verified,
    })
}

fn title(kind: &str) -> &'static str {
    if kind == "silicon" {
        "Silicon"
    } else {
        "Carbon"
    }
}

fn signed_in_text(ctx: &Ctx, session: &StoredSession, verified: bool) -> String {
    let account = &session.account;
    let name = account
        .display_name
        .as_deref()
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    let mut lines = vec![
        format!(
            "Signed in to Waveform as {}{name}, a {}.",
            account.label(),
            title(&account.kind)
        ),
        format!("  uuid              {}", account.uuid),
    ];
    if let Some(custodian) = &account.custodian {
        lines.push(format!(
            "  custodian         {}",
            custodian.id.as_deref().unwrap_or(&custodian.uuid)
        ));
    }
    lines.push(format!(
        "  access token      expires {} (refreshed automatically)",
        rfc3339(session.expires_at)
    ));
    if let Some(end) = session.refresh_expires_at {
        lines.push(format!("  sign-in ends      {}", rfc3339(end)));
    }
    lines.push(format!("  Silicon Accounts  {}", session.accounts_url));
    lines.push(format!(
        "  Waveform          {}",
        normalize_url(&ctx.api_url)
    ));
    if ctx.profile != "default" {
        lines.push(format!("  profile           {}", ctx.profile));
    }
    lines.push(format!(
        "  checked           {}",
        if verified {
            "yes, Waveform accepted the sign-in just now"
        } else {
            "no (saved sign-in, not checked with Waveform)"
        }
    ));
    lines.join("\n")
}

fn not_signed_in_outcome(ctx: &Ctx, json: Value, why: &str) -> Outcome {
    let text = format!("{why}\nCarbons: {CARBON_SIGN_IN}\nSilicons: {SILICON_SIGN_IN}");
    Outcome::new(json, text).exit(if ctx.out.json { 0 } else { 1 })
}

/// Asks Waveform about the session's account; refreshes once on a stale refusal.
async fn check_with_waveform(ctx: &Ctx) -> CliResult<(silicon_waveform_client::Me, StoredSession)> {
    call_with_session(ctx, |client| async move { client.me().await }).await
}

/// Keeps the saved id, name and custodian in step with what Waveform reports.
async fn remember_account(ctx: &Ctx, session: &StoredSession, me: &silicon_waveform_client::Me) {
    let account = &me.account;
    if account.uuid != session.account.uuid
        || (account.id == session.account.id
            && account.display_name == session.account.display_name
            && account.custodian == session.account.custodian)
    {
        return;
    }
    let store = ctx.store();
    let Ok(_lock) = store.lock().await else {
        return;
    };
    if let Loaded::Session {
        session: latest, ..
    } = store.load()
        && latest.account.uuid == account.uuid
    {
        let mut updated = *latest;
        updated.account.id.clone_from(&account.id);
        if account.display_name.is_some() {
            updated
                .account
                .display_name
                .clone_from(&account.display_name);
        }
        updated.account.custodian.clone_from(&account.custodian);
        let _ = store.save(&updated);
    }
}

/// `waveform login status`.
pub async fn status(ctx: &Ctx, args: &StatusArgs) -> CliResult<Outcome> {
    let store = ctx.store();
    let session = match store.load() {
        Loaded::Missing => {
            let mut json = json!({ "authenticated": false });
            let mut why = format!(
                "Not signed in to Waveform (profile `{}`, Silicon Accounts {}).",
                ctx.profile, ctx.accounts_url
            );
            if !store.legacy_files().is_empty() {
                json["reason"] = json!("legacy_session");
                json["message"] = json!(
                    "A sign-in saved by an earlier Waveform version was found; it cannot be used with Silicon Accounts. Sign in again."
                );
                why.push_str(" A sign-in saved by an earlier Waveform version was found; it cannot be used any more, so sign in again.");
            } else if let Some(other) = store.other_origins().first() {
                json["reason"] = json!("signed_in_elsewhere");
                json["session_accounts_url"] = json!(other);
                why.push_str(&format!(
                    " This profile is signed in at {other}; pass --accounts-url {other} to use it."
                ));
            }
            return Ok(not_signed_in_outcome(ctx, json, &why));
        }
        Loaded::Unreadable { path, why } => {
            let message = format!(
                "The saved sign-in {} cannot be used: {why}.",
                path.display()
            );
            return Ok(not_signed_in_outcome(
                ctx,
                json!({"authenticated": false, "reason": "session_unreadable", "message": message}),
                &message,
            ));
        }
        Loaded::Session { session, warning } => {
            if let Some(warning) = warning {
                ctx.out.warn(&warning);
            }
            *session
        }
    };
    if session.ended(now()) {
        let message = format!(
            "The Waveform sign-in of {} ended at {}.",
            session.account.label(),
            rfc3339(session.refresh_expires_at.unwrap_or_default())
        );
        return Ok(not_signed_in_outcome(
            ctx,
            json!({"authenticated": false, "reason": "session_ended", "message": message}),
            &message,
        ));
    }
    if args.offline {
        return Ok(Outcome::new(
            status_json(ctx, &session, false),
            signed_in_text(ctx, &session, false),
        ));
    }
    match check_with_waveform(ctx).await {
        Ok((me, latest)) => {
            remember_account(ctx, &latest, &me).await;
            let mut shown = latest.clone();
            shown.account.id.clone_from(&me.account.id);
            shown.account.custodian.clone_from(&me.account.custodian);
            if me.account.display_name.is_some() {
                shown
                    .account
                    .display_name
                    .clone_from(&me.account.display_name);
            }
            Ok(Outcome::new(
                status_json(ctx, &shown, true),
                signed_in_text(ctx, &shown, true),
            ))
        }
        Err(error)
            if matches!(
                error.code.as_str(),
                "sign_in_ended" | "not_signed_in" | "session_unreadable"
            ) =>
        {
            Ok(not_signed_in_outcome(
                ctx,
                json!({"authenticated": false, "reason": if error.code == "sign_in_ended" { "session_ended" } else { error.code.as_str() }, "message": error.message}),
                &error.message,
            ))
        }
        Err(error) if error.exit == EXIT_AUTH => {
            // Waveform refused the (fresh) token: it does not accept this sign-in.
            let reason = error
                .details
                .as_ref()
                .and_then(|details| details["reason"].as_str())
                .unwrap_or(error.code.as_str())
                .to_owned();
            let mut json =
                json!({"authenticated": false, "reason": reason, "message": error.message});
            if let Some(hint) = &error.hint {
                json["hint"] = json!(hint);
            }
            let why = format!(
                "Waveform at {} refused the saved sign-in: {}",
                normalize_url(&ctx.api_url),
                error.message
            );
            Ok(not_signed_in_outcome(ctx, json, &why))
        }
        Err(error) => {
            // Waveform or Silicon Accounts could not be reached: report what is saved.
            let warning = format!(
                "{} Reporting the saved sign-in without checking it.",
                error.message
            );
            ctx.out.warn(&warning);
            let mut json = status_json(ctx, &session, false);
            json["warning"] = json!(error.message);
            Ok(Outcome::new(json, signed_in_text(ctx, &session, false)))
        }
    }
}

// ---- login, refresh, logout ---------------------------------------------------------------

/// The short-lived token of `login --slt`, `login SLT` or `login --slt-stdin`, if one was given.
fn read_slt(args: &LoginArgs) -> CliResult<Option<String>> {
    if let Some(slt) = args.slt.clone().or_else(|| args.slt_argument.clone()) {
        return Ok(Some(slt));
    }
    if !args.slt_stdin {
        return Ok(None);
    }
    let value = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("Short-lived token (from `{SLT_MINT_COMMAND}`): "))
            .map_err(|error| {
                CliError::invalid(format!("Cannot read the short-lived token: {error}."), "")
            })?
    } else {
        let mut value = String::new();
        std::io::stdin()
            .take(16 * 1024)
            .read_to_string(&mut value)
            .map_err(|error| {
                CliError::invalid(
                    format!("Cannot read the short-lived token from stdin: {error}."),
                    format!("Pipe it in as text: {SILICON_SIGN_IN}"),
                )
            })?;
        value
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(CliError::new(
            EXIT_INVALID,
            "slt_missing",
            "--slt-stdin read nothing from stdin.",
            format!("Pipe the token in: {SILICON_SIGN_IN}"),
        ));
    }
    Ok(Some(value))
}

fn default_label() -> String {
    let host = ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty() && value.len() <= 64);
    match host {
        Some(host) => format!("waveform CLI on {host}"),
        None => "waveform CLI".into(),
    }
}

fn open_browser(url: &str) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

async fn device_flow(ctx: &Ctx, signin: &SignIn, args: &LoginArgs) -> CliResult<Tokens> {
    let label = args.label.clone().unwrap_or_else(default_label);
    let device = signin.start_device(Some(&label)).await?;
    let out = ctx.out;
    if out.json {
        out.json_line(&json!({
            "event": "device_code",
            "user_code": device.user_code,
            "verification_uri": device.verification_uri,
            "verification_uri_complete": device.verification_uri_complete,
            "expires_in": device.expires_in,
            "expires_at": rfc3339(device.expires_at),
            "interval": device.interval,
        }));
    } else {
        crate::output::stderr(&format!(
            "To sign in to Waveform, open this page and confirm the code:\n\n  {}\n  code: {}\n\nWaiting for approval (the code expires in {} minutes)...\n",
            device.browser_url(),
            device.user_code,
            device.expires_in.div_ceil(60)
        ));
    }
    if args.open
        && let Err(error) = open_browser(device.browser_url())
    {
        out.warn(&format!(
            "Could not open a browser ({error}); open {} yourself.",
            device.browser_url()
        ));
    }
    let tokens = signin
        .wait_for_device(&device, |progress| match progress {
            DeviceProgress::SlowDown { interval } if out.json => {
                out.json_line(&json!({"event": "slow_down", "interval": interval}));
            }
            DeviceProgress::Retrying { message } => {
                if out.json {
                    out.json_line(&json!({"event": "retrying", "message": message}));
                } else {
                    out.warn(&format!("{message} Still waiting for approval."));
                }
            }
            _ => {}
        })
        .await?;
    Ok(tokens)
}

/// `waveform login`.
pub async fn login(ctx: &Ctx, args: &LoginArgs) -> CliResult<Outcome> {
    let slt = read_slt(args)?;
    let signin = ctx.signin()?;
    let (tokens, method) = match slt {
        Some(slt) => (signin.exchange_slt(&slt).await?, "slt"),
        None => (device_flow(ctx, &signin, args).await?, "device"),
    };
    let store = ctx.store();
    let session = StoredSession::from_tokens(&tokens, &ctx.accounts_url, method);
    let previous = {
        let _lock = store.lock().await?;
        let previous = match store.load() {
            Loaded::Session { session, .. } => Some(*session),
            _ => None,
        };
        store.save(&session)?;
        previous
    };
    // Signing in again replaces the saved sign-in; end the replaced one at Silicon Accounts.
    if let Some(previous) = previous
        && previous.refresh_token != session.refresh_token
        && let Err(error) = signin.revoke(&previous.refresh_token).await
    {
        ctx.out.warn(&format!(
            "The previous sign-in of {} on this machine could not be ended at Silicon Accounts ({}); it ends by itself when it expires.",
            previous.account.label(),
            error.message()
        ));
    }
    let (verified, warning) = match ctx
        .api()?
        .with_bearer(session.access_token.clone())
        .me()
        .await
    {
        Ok(_) => (true, None),
        Err(error) => (
            false,
            Some(format!(
                "The sign-in is saved, but Waveform at {} did not accept it yet: {}{}",
                normalize_url(&ctx.api_url),
                error.message(),
                error
                    .hint()
                    .map(|hint| format!(" Hint: {hint}"))
                    .unwrap_or_default()
            )),
        ),
    };
    if let Some(warning) = &warning {
        ctx.out.warn(warning);
    }
    let mut json = status_json(ctx, &session, verified);
    json["event"] = json!("signed_in");
    if let Some(warning) = warning {
        json["warning"] = json!(warning);
    }
    let mut outcome = Outcome::new(json, signed_in_text(ctx, &session, verified));
    outcome.compact = true;
    Ok(outcome)
}

/// `waveform refresh`.
pub async fn refresh(ctx: &Ctx) -> CliResult<Outcome> {
    let current = match ctx.store().load() {
        Loaded::Session { session, .. } => session.access_token.clone(),
        Loaded::Missing => return Err(ctx.not_signed_in()),
        Loaded::Unreadable { path, why } => return Err(unreadable(&path, &why)),
    };
    let session = ensure_fresh(ctx, Some(&current)).await?;
    Ok(Outcome::new(
        json!({
            "refreshed": true,
            "uuid": session.account.uuid,
            "id": session.account.id,
            "expires_at": rfc3339(session.expires_at),
            "refresh_expires_at": session.refresh_expires_at.map(rfc3339),
        }),
        format!(
            "Refreshed the sign-in of {}; the access token now expires {}.",
            session.account.label(),
            rfc3339(session.expires_at)
        ),
    ))
}

fn remove_legacy(store: &SessionStore) -> usize {
    store
        .legacy_state()
        .iter()
        .filter(|path| std::fs::remove_file(path).is_ok())
        .count()
}

/// `waveform logout`.
pub async fn logout(ctx: &Ctx) -> CliResult<Outcome> {
    let store = ctx.store();
    if matches!(store.load(), Loaded::Missing) {
        let legacy = remove_legacy(&store);
        if legacy == 0 {
            return Ok(Outcome::new(
                json!({"signed_out": false, "reason": "not_signed_in"}),
                "This machine was not signed in to Waveform; nothing to do.",
            ));
        }
        return Ok(Outcome::new(
            json!({"signed_out": true, "revoked": false, "legacy_files_removed": legacy}),
            format!(
                "Deleted {legacy} file(s) left by an earlier Waveform version (its saved sign-ins and cached test secrets). Those sign-ins could not be ended online; that sign-in system is retired."
            ),
        ));
    }
    let _lock = store.lock().await?;
    let (account, revoked) = match store.load() {
        Loaded::Session { session, .. } => {
            let revoked = match ctx.signin()?.revoke(&session.refresh_token).await {
                Ok(()) => true,
                Err(error) => {
                    ctx.out.warn(&format!(
                        "Could not end the sign-in at Silicon Accounts ({}); it is deleted from this machine anyway. End it from your account's sign-ins on the account site, or let it expire.",
                        error.message()
                    ));
                    false
                }
            };
            (Some(session.account.clone()), revoked)
        }
        Loaded::Unreadable { .. } | Loaded::Missing => (None, false),
    };
    store.remove()?;
    let legacy = remove_legacy(&store);
    let who = account.as_ref().map_or_else(
        || "this machine".to_owned(),
        |account| account.label().to_owned(),
    );
    Ok(Outcome::new(
        json!({
            "signed_out": true,
            "uuid": account.as_ref().map(|account| account.uuid.clone()),
            "id": account.as_ref().and_then(|account| account.id.clone()),
            "revoked": revoked,
            "legacy_files_removed": legacy,
        }),
        format!(
            "Signed out {who} from Waveform{}.",
            if revoked {
                " and ended the sign-in at Silicon Accounts"
            } else {
                ""
            }
        ),
    ))
}
