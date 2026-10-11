//! Every command other than signing in: speech, history, settings, provider keys, Briefcase
//! access, reports, discovery, docs and local configuration.

use std::{
    io::{self, IsTerminal as _, Read as _, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};
use silicon_waveform_client::{
    APP_ID, CARBON_SIGN_IN, Client, DOCS_URL, PROOF_SCOPES, PreferencesUpdate, ProviderKeys,
    REPOSITORY_URL, SILICON_SIGN_IN, SLT_MINT_COMMAND, SttRequest, TtsProviderOptions, TtsRequest,
};

use crate::args::{
    AccountArg, AudioArgs, Command, ConfigCommand, JobsArgs, PreferencesArgs, ProofArg,
    ProviderKeysArgs, ProviderKeysCommand, ReportArgs, StorageCommand, SttArgs, TtsArgs,
};
use crate::auth::{Ctx, call, proof_client, rfc3339};
use crate::output::{CliError, CliResult, EXIT_FAILURE, Outcome};
use crate::session::normalize_url;

// ---- input helpers ----------------------------------------------------------------------

/// Reads a bounded UTF-8 file (`-` reads stdin).
pub fn read_bounded_file(path: &Path, limit: usize, label: &str) -> CliResult<String> {
    let reader: Box<dyn io::Read> = if path.as_os_str() == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(std::fs::File::open(path).map_err(|error| {
            CliError::invalid(
                format!("Cannot read the {label} file {}: {error}.", path.display()),
                "Check the path and its permissions.",
            )
        })?)
    };
    let mut value = String::new();
    reader
        .take(limit as u64 + 1)
        .read_to_string(&mut value)
        .map_err(|error| {
            CliError::invalid(
                format!("Cannot read the {label} as UTF-8 text: {error}."),
                "Pass a text file.",
            )
        })?;
    if value.len() > limit {
        return Err(CliError::invalid(
            format!("The {label} is larger than {limit} bytes."),
            "Pass a smaller file.",
        ));
    }
    Ok(value)
}

/// Reads a one-line secret (a key or a proof): no whitespace inside, final newline allowed.
pub fn read_secret_file(path: &Path, label: &str, limit: usize) -> CliResult<String> {
    let value = read_bounded_file(path, limit + 2, label)?;
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() || value.len() > limit || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(CliError::invalid(
            format!(
                "The {label} must be 1-{limit} visible ASCII characters on one line (a final newline is fine)."
            ),
            format!("Put only the {label} in the file."),
        ));
    }
    Ok(value)
}

fn read_provider_keys(files: &[String]) -> CliResult<ProviderKeys> {
    let mut keys = ProviderKeys::default();
    let mut providers = std::collections::BTreeSet::new();
    for entry in files {
        let (provider, path) = entry
            .split_once('=')
            .filter(|(_, path)| !path.is_empty())
            .ok_or_else(|| {
                CliError::invalid(
                    format!("--provider-key-file takes PROVIDER=FILE, not `{entry}`."),
                    "For example --provider-key-file gemini=/private/gemini.key (FILE - reads stdin).",
                )
            })?;
        if !matches!(provider, "gemini" | "elevenlabs" | "openai" | "deepgram") {
            return Err(CliError::invalid(
                format!("`{provider}` is not a provider."),
                "Use gemini, elevenlabs, openai or deepgram.",
            ));
        }
        if !providers.insert(provider) {
            return Err(CliError::invalid(
                format!("--provider-key-file names {provider} twice."),
                "Pass at most one key file per provider.",
            ));
        }
        let key = read_secret_file(Path::new(path), "provider key", 16_384)?;
        keys.insert(provider, key)?;
    }
    Ok(keys)
}

fn read_provider_options(
    value: Option<&String>,
    file: Option<&PathBuf>,
) -> CliResult<TtsProviderOptions> {
    let value = match (value, file) {
        (Some(value), _) => value.clone(),
        // Match the API body bound so valid Unicode guidance and JSON escapes fit.
        (None, Some(path)) => read_bounded_file(path, 327_680, "provider options")?,
        (None, None) => return Ok(TtsProviderOptions::default()),
    };
    serde_json::from_str(&value).map_err(|error| {
        CliError::invalid(
            format!(
                "The provider options are not valid JSON for Waveform's controls (line {}, column {}): {error}.",
                error.line(),
                error.column()
            ),
            "Use {\"gemini\":{…}}, {\"elevenlabs\":{…}} or {\"openai\":{…}} with the fields in `waveform docs cli`.",
        )
    })
}

fn list_arg(value: Option<&String>) -> Option<Vec<String>> {
    value.map(|text| {
        text.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

/// Only one input can read stdin in one command.
pub fn check_stdin_sources(command: &Command) -> CliResult<()> {
    let dash =
        |path: &Option<PathBuf>| usize::from(path.as_ref().is_some_and(|p| p.as_os_str() == "-"));
    let keys = |files: &[String]| {
        files
            .iter()
            .filter(|entry| entry.split_once('=').is_some_and(|(_, path)| path == "-"))
            .count()
    };
    let count = match command {
        Command::Login(args) => usize::from(args.slt_stdin),
        Command::Tts(args) => {
            dash(&args.text_file)
                + dash(&args.provider_options_file)
                + keys(&args.provider_key_file)
                + dash(&args.proof.proof_file)
        }
        Command::Stt(args) => keys(&args.provider_key_file) + dash(&args.proof.proof_file),
        Command::SttFolder(args) => dash(&args.proof_file),
        Command::Audio(args) => dash(&args.proof.proof_file),
        Command::ProviderKeys(ProviderKeysArgs {
            command: Some(ProviderKeysCommand::Set { key_file, .. }),
            ..
        }) => dash(key_file),
        _ => 0,
    };
    if count > 1 {
        return Err(CliError::invalid(
            "Only one input can read stdin (-) in one command.",
            "Use files for the other inputs.",
        ));
    }
    Ok(())
}

fn with_account(
    client: Client,
    account: Option<&String>,
) -> silicon_waveform_client::Result<Client> {
    match account {
        Some(account) => client.for_account(account.as_str()),
        None => Ok(client),
    }
}

/// Adds what to do after a failed speech request: the exact retry, or Briefcase approval.
fn speech_error(error: CliError, key: &str, request_id: uuid::Uuid) -> CliError {
    let retry = format!("--idempotency {key} --request-id {request_id}");
    if error.code == "storage_authorization_required" {
        let hint = format!(
            "Give Waveform Briefcase access with `waveform storage approve` (signed in as the account itself), then repeat this command with {retry}."
        );
        return CliError {
            hint: Some(hint),
            ..error
        };
    }
    if error.exit == EXIT_FAILURE {
        let hint = format!(
            "{} Repeating the command with {retry} returns the finished result without running a provider twice.",
            error.hint.clone().unwrap_or_default()
        );
        return CliError {
            hint: Some(hint.trim().to_owned()),
            ..error
        };
    }
    error
}

// ---- speech -----------------------------------------------------------------------------

pub async fn tts(ctx: &Ctx, args: &TtsArgs) -> CliResult<Outcome> {
    let text = match (&args.text, &args.text_file) {
        (Some(text), _) => text.clone(),
        (None, Some(path)) => read_bounded_file(path, 64 * 1024, "text")?
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
        (None, None) => {
            return Err(CliError::invalid(
                "Give the text to speak, or --text-file FILE.",
                "waveform tts 'Hello, Carbons and Silicons.'",
            ));
        }
    };
    let request = TtsRequest {
        text,
        voice_profile: args.voice_profile.clone(),
        lang: args.lang.clone(),
        provider_order: args
            .provider
            .clone()
            .map(|provider| vec![provider])
            .or_else(|| list_arg(args.provider_order.as_ref())),
        auto_fallback: args.auto_fallback,
        provider_options: read_provider_options(
            args.provider_options.as_ref(),
            args.provider_options_file.as_ref(),
        )?,
        provider_keys: read_provider_keys(&args.provider_key_file)?,
    };
    let key = args
        .idempotency
        .clone()
        .unwrap_or_else(|| format!("waveform-cli-{}", uuid::Uuid::new_v4()));
    let request_id = args.request_id.unwrap_or_else(uuid::Uuid::new_v4);
    ctx.out.notice(&format!(
        "Speech request {request_id}. Follow it: waveform jobs --job-id {request_id} --wait"
    ));
    let result = if let Some(proof) = &args.proof.proof_file {
        let client = proof_client(ctx, proof)?.with_speech_request_id(request_id)?;
        client.tts(&request, &key).await.map_err(CliError::from)
    } else {
        let (request, key) = (&request, key.as_str());
        call(ctx, move |client| async move {
            client
                .with_speech_request_id(request_id)?
                .tts(request, key)
                .await
        })
        .await
    };
    let response = result.map_err(|error| speech_error(error, &key, request_id))?;
    if let Some(storage) = response
        .storage
        .as_ref()
        .filter(|storage| storage.is_held())
    {
        ctx.out.notice(&format!(
            "Waveform holds this audio{} ({}: {}). Download it: waveform audio {} --output speech.mp3",
            storage
                .expires_at
                .as_deref()
                .map(|at| format!(" until {at}"))
                .unwrap_or_default(),
            storage.reason.as_deref().unwrap_or("held"),
            storage.message.as_deref().unwrap_or("it is not in Briefcase"),
            response.request_id
        ));
    }
    Ok(Outcome::data(
        serde_json::to_value(&response).unwrap_or_default(),
    ))
}

pub async fn stt(ctx: &Ctx, args: &SttArgs) -> CliResult<Outcome> {
    let request = SttRequest {
        file_url: args.file_url.clone(),
        language: args.language.clone(),
        provider_order: list_arg(args.provider_order.as_ref()),
        provider_keys: read_provider_keys(&args.provider_key_file)?,
    };
    let key = args
        .idempotency
        .clone()
        .unwrap_or_else(|| format!("waveform-cli-{}", uuid::Uuid::new_v4()));
    let request_id = args.request_id.unwrap_or_else(uuid::Uuid::new_v4);
    ctx.out.notice(&format!(
        "Speech request {request_id}. Follow it: waveform jobs --job-id {request_id} --wait"
    ));
    let result = if let Some(proof) = &args.proof.proof_file {
        let client = proof_client(ctx, proof)?.with_speech_request_id(request_id)?;
        client.stt(&request, &key).await.map_err(CliError::from)
    } else {
        let (request, key) = (&request, key.as_str());
        call(ctx, move |client| async move {
            client
                .with_speech_request_id(request_id)?
                .stt(request, key)
                .await
        })
        .await
    };
    let response = result.map_err(|error| speech_error(error, &key, request_id))?;
    Ok(Outcome::data(
        serde_json::to_value(&response).unwrap_or_default(),
    ))
}

pub async fn stt_folder(ctx: &Ctx, args: &ProofArg) -> CliResult<Outcome> {
    let folder = if let Some(proof) = &args.proof_file {
        proof_client(ctx, proof)?.stt_source_folder().await?
    } else {
        call(
            ctx,
            |client| async move { client.stt_source_folder().await },
        )
        .await?
    };
    Ok(Outcome::data(
        serde_json::to_value(&folder).unwrap_or_default(),
    ))
}

/// A file name Waveform suggested, kept only when it is a plain name.
fn safe_file_name(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    (!name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')))
    .then(|| name.to_owned())
}

pub async fn audio(ctx: &Ctx, args: &AudioArgs) -> CliResult<Outcome> {
    let to_stdout = args
        .output
        .as_ref()
        .is_some_and(|path| path.as_os_str() == "-");
    if to_stdout && io::stdout().is_terminal() {
        return Err(CliError::invalid(
            "Refusing to write MP3 bytes to a terminal.",
            "Redirect stdout (waveform audio ID --output - > speech.mp3) or pass --output FILE.",
        ));
    }
    let exists = |path: &Path| {
        CliError::invalid(
            format!("{} already exists.", path.display()),
            "Pass --force to replace it, or choose another --output.",
        )
    };
    // An explicit file is checked before downloading anything.
    if let Some(path) = args.output.as_ref().filter(|_| !to_stdout)
        && path.exists()
        && !args.force
    {
        return Err(exists(path));
    }
    let request_id = args.request_id.as_str();
    let held = if let Some(proof) = &args.proof.proof_file {
        proof_client(ctx, proof)?.held_audio(request_id).await?
    } else {
        call(ctx, move |client| async move {
            client.held_audio(request_id).await
        })
        .await?
    };
    if to_stdout {
        let mut out = io::stdout().lock();
        out.write_all(&held.bytes)
            .and_then(|()| out.flush())
            .map_err(|error| {
                CliError::local(format!("Cannot write the MP3 to stdout: {error}."), "")
            })?;
        ctx.out.notice(&format!(
            "Wrote {} bytes of {}.",
            held.bytes.len(),
            held.media_type
        ));
        let mut outcome = Outcome::new(Value::Null, "");
        outcome.silent = true;
        return Ok(outcome);
    }
    let path = args.output.clone().unwrap_or_else(|| {
        PathBuf::from(
            safe_file_name(held.filename.as_deref())
                .unwrap_or_else(|| format!("{}.mp3", args.request_id)),
        )
    });
    if path.exists() && !args.force {
        return Err(exists(&path));
    }
    std::fs::write(&path, &held.bytes).map_err(|error| {
        CliError::local(
            format!("Cannot write {}: {error}.", path.display()),
            "Check the directory exists and is writable.",
        )
    })?;
    Ok(Outcome::new(
        json!({
            "saved_to": path,
            "bytes": held.bytes.len(),
            "media_type": held.media_type,
            "expires_at": held.expires_at,
        }),
        format!(
            "Saved {} bytes to {}{}.",
            held.bytes.len(),
            path.display(),
            held.expires_at
                .as_deref()
                .map(|at| format!(" (Waveform deletes its copy at {at})"))
                .unwrap_or_default()
        ),
    ))
}

// ---- history and settings ---------------------------------------------------------------

pub async fn jobs(ctx: &Ctx, args: &JobsArgs) -> CliResult<Outcome> {
    let account = args.account.account.as_ref();
    let value = if let Some(job_id) = &args.job_id {
        let (job_id, wait) = (job_id.as_str(), args.wait);
        let (timeout, poll) = (
            Duration::from_millis(args.timeout_ms),
            Duration::from_millis(args.poll_ms),
        );
        let job = call(ctx, move |client| async move {
            let client = with_account(client, account)?;
            if wait {
                client.wait_for_job(job_id, timeout, poll).await
            } else {
                client.job(job_id).await
            }
        })
        .await?;
        serde_json::to_value(job).unwrap_or_default()
    } else {
        let (operation, limit, cursor) = (
            args.operation.as_deref(),
            args.limit,
            args.cursor.as_deref(),
        );
        let page = call(ctx, move |client| async move {
            with_account(client, account)?
                .jobs_page(operation, limit, cursor)
                .await
        })
        .await?;
        serde_json::to_value(page).unwrap_or_default()
    };
    Ok(Outcome::data(value))
}

pub async fn preferences(ctx: &Ctx, args: &PreferencesArgs) -> CliResult<Outcome> {
    let account = args.account.account.as_ref();
    let update = PreferencesUpdate {
        voice_profile: args.voice_profile.clone(),
        tts_order: list_arg(args.tts_order.as_ref()),
        stt_order: list_arg(args.stt_order.as_ref()),
        ..PreferencesUpdate::default()
    };
    let update = &update;
    let preferences = call(ctx, move |client| async move {
        let client = with_account(client, account)?;
        if update.is_empty() {
            client.preferences().await
        } else {
            client.update_preferences(update).await
        }
    })
    .await?;
    Ok(Outcome::data(
        serde_json::to_value(preferences).unwrap_or_default(),
    ))
}

pub async fn voice_profiles(ctx: &Ctx) -> CliResult<Outcome> {
    let profiles = call(ctx, |client| async move { client.voice_profiles().await }).await?;
    Ok(Outcome::data(
        serde_json::to_value(profiles).unwrap_or_default(),
    ))
}

pub async fn provider_keys(ctx: &Ctx, args: &ProviderKeysArgs) -> CliResult<Outcome> {
    let account = args.account.as_ref();
    match &args.command {
        None | Some(ProviderKeysCommand::List) => {
            let keys = call(ctx, move |client| async move {
                with_account(client, account)?.provider_keys().await
            })
            .await?;
            Ok(Outcome::data(
                serde_json::to_value(keys).unwrap_or_default(),
            ))
        }
        Some(ProviderKeysCommand::Set {
            provider,
            api_key,
            key_file,
        }) => {
            let key = match (api_key, key_file) {
                (Some(key), _) => key.clone(),
                (None, Some(path)) => read_secret_file(path, "provider key", 16_384)?,
                (None, None) => {
                    return Err(CliError::invalid(
                        "Give the key with --key-file FILE (or - for stdin).",
                        "waveform provider-keys set gemini --key-file /private/gemini.key",
                    ));
                }
            };
            let (provider, key) = (provider.as_str(), key.as_str());
            call(ctx, move |client| async move {
                with_account(client, account)?
                    .put_provider_key(provider, key)
                    .await
            })
            .await?;
            Ok(Outcome::new(
                json!({"saved": true, "provider": provider, "account": account}),
                format!(
                    "Saved the {provider} key{}; it is encrypted and never shown again.",
                    account.map(|id| format!(" for {id}")).unwrap_or_default()
                ),
            ))
        }
        Some(ProviderKeysCommand::Delete { provider }) => {
            let provider = provider.as_str();
            call(ctx, move |client| async move {
                with_account(client, account)?
                    .delete_provider_key(provider)
                    .await
            })
            .await?;
            Ok(Outcome::new(
                json!({"deleted": true, "provider": provider, "account": account}),
                format!(
                    "Deleted the saved {provider} key{}; Waveform's own key is used again.",
                    account.map(|id| format!(" of {id}")).unwrap_or_default()
                ),
            ))
        }
    }
}

pub async fn storage(ctx: &Ctx, command: &StorageCommand) -> CliResult<Outcome> {
    let access = match command {
        StorageCommand::Approve => {
            call(ctx, |client| async move { client.approve_storage().await }).await?
        }
        StorageCommand::Status(AccountArg { account }) => {
            let account = account.as_ref();
            call(ctx, move |client| async move {
                with_account(client, account)?.storage().await
            })
            .await?
        }
        StorageCommand::Revoke(AccountArg { account }) => {
            let account = account.as_ref();
            call(ctx, move |client| async move {
                with_account(client, account)?.revoke_storage().await
            })
            .await?
        }
    };
    Ok(Outcome::data(
        serde_json::to_value(access).unwrap_or_default(),
    ))
}

/// `waveform telemetry on|off` and `waveform config telemetry on|off`: kept so older scripts
/// keep working. Waveform records no telemetry, so they change nothing and read no state.
pub fn telemetry_removed() -> Outcome {
    const MESSAGE: &str =
        "Telemetry was removed from Waveform; there is nothing to turn on or off.";
    let mut outcome = Outcome::new(json!({"telemetry": "removed", "message": MESSAGE}), MESSAGE);
    outcome.compact = true;
    outcome
}

pub async fn report(ctx: &Ctx, args: &ReportArgs) -> CliResult<Outcome> {
    let key = args
        .idempotency
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ctx.out.notice(&format!(
        "Report retry key: {key} (repeat with --idempotency {key} if unsure it arrived)"
    ));
    let (message, pr, key_ref) = (args.message.as_str(), args.pr.as_deref(), key.as_str());
    let receipt = call(ctx, move |client| async move {
        client.report(message, pr, key_ref).await
    })
    .await?;
    if args.pr.is_none() {
        ctx.out.notice(&format!(
            "Found the fix too? Waveform is open source: send a pull request to {REPOSITORY_URL} and report again with --pr <link>."
        ));
    }
    Ok(Outcome::data(
        serde_json::to_value(receipt).unwrap_or_default(),
    ))
}

pub async fn me(ctx: &Ctx) -> CliResult<Outcome> {
    let me = call(ctx, |client| async move { client.me().await }).await?;
    Ok(Outcome::data(serde_json::to_value(me).unwrap_or_default()))
}

pub async fn silicons(ctx: &Ctx) -> CliResult<Outcome> {
    let silicons = call(ctx, |client| async move { client.silicons().await }).await?;
    let text = if silicons.is_empty() {
        "Waveform knows no Silicons that you look after. (It learns about a Silicon when the Silicon first uses Waveform.)".to_owned()
    } else {
        silicons
            .iter()
            .map(|silicon| {
                format!(
                    "{}{}  uuid {}",
                    silicon.label(),
                    silicon
                        .display_name
                        .as_deref()
                        .map(|name| format!(" ({name})"))
                        .unwrap_or_default(),
                    silicon.uuid
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Outcome::new(json!({ "items": silicons }), text))
}

pub async fn capabilities(ctx: &Ctx) -> CliResult<Outcome> {
    let capabilities = ctx.api()?.capabilities().await?;
    Ok(Outcome::data(
        serde_json::to_value(capabilities).unwrap_or_default(),
    ))
}

pub async fn contracts(ctx: &Ctx) -> CliResult<Outcome> {
    Ok(Outcome::data(ctx.api()?.contracts().await?))
}

// ---- discovery, docs and local configuration ------------------------------------------------

/// The `accounts --json` object: Waveform's app id and where to sign in. Static and offline:
/// it reads no file and sends no request, so it answers in any environment.
pub fn accounts_json(api_url: &str, accounts_url: &str) -> Value {
    json!({
        "app_id": APP_ID,
        "accounts_url": normalize_url(accounts_url),
        "api_url": normalize_url(api_url),
        "docs_url": DOCS_URL,
        "version": env!("CARGO_PKG_VERSION"),
        "proof_scopes": PROOF_SCOPES,
        "sign_in": {
            "carbon": CARBON_SIGN_IN,
            "silicon": SILICON_SIGN_IN,
            "short_lived_token": SLT_MINT_COMMAND,
        },
    })
}

/// `waveform accounts`.
pub fn accounts(api_url: &str, accounts_url: &str) -> Outcome {
    let json = accounts_json(api_url, accounts_url);
    let text = format!(
        "Waveform's app id at Silicon Accounts is {APP_ID}.\n  Silicon Accounts  {}\n  Waveform API      {}\n  Docs              {DOCS_URL}\n  Version           {}\nSign in:\n  Carbon   {CARBON_SIGN_IN}\n  Silicon  {SILICON_SIGN_IN}",
        normalize_url(accounts_url),
        normalize_url(api_url),
        env!("CARGO_PKG_VERSION"),
    );
    Outcome::new(json, text)
}

/// The bundled guide for a `waveform docs` topic.
pub fn guide(topic: &str) -> &'static str {
    match topic {
        "cli" => include_str!("../README.md"),
        "api" => include_str!("../docs/api.md"),
        "client" => include_str!("../docs/client.md"),
        "accounts" => include_str!("../docs/accounts.md"),
        "testing" => include_str!("../docs/testing.md"),
        "configuration" => include_str!("../docs/configuration.md"),
        _ => include_str!("../docs/README.md"),
    }
}

/// `waveform docs TOPIC`.
pub fn docs(topic: &str) -> Outcome {
    let text = guide(topic);
    Outcome::new(json!({"topic": topic, "markdown": text}), text)
}

/// `waveform config …`.
pub fn config(ctx: &Ctx, command: &ConfigCommand) -> CliResult<Outcome> {
    match command {
        ConfigCommand::Show => {
            let store = ctx.store();
            let signed_in = match store.load() {
                crate::session::Loaded::Session { session, .. } => json!({
                    "uuid": session.account.uuid,
                    "id": session.account.id,
                    "kind": session.account.kind,
                    "expires_at": rfc3339(session.expires_at),
                }),
                crate::session::Loaded::Missing => Value::Null,
                crate::session::Loaded::Unreadable { why, .. } => json!({ "unreadable": why }),
            };
            let json = json!({
                "state_dir": crate::home::state_dir(),
                "config_file": crate::home::config_path(),
                "profile": ctx.profile,
                "profile_dir": ctx.profile_dir,
                "session_file": store.path(),
                "signed_in": signed_in,
                "api_url": normalize_url(&ctx.api_url),
                "accounts_url": normalize_url(&ctx.accounts_url),
            });
            let who = json["signed_in"]["id"]
                .as_str()
                .or_else(|| json["signed_in"]["uuid"].as_str())
                .map_or_else(
                    || "not signed in".to_owned(),
                    |id| format!("signed in as {id}"),
                );
            let text = format!(
                "State directory   {}\nProfile           {} ({})\nSession file      {} ({who})\nWaveform API      {}\nSilicon Accounts  {}",
                crate::home::state_dir().display(),
                ctx.profile,
                ctx.profile_dir.display(),
                store.path().display(),
                normalize_url(&ctx.api_url),
                normalize_url(&ctx.accounts_url),
            );
            Ok(Outcome::new(json, text))
        }
        ConfigCommand::Home { location } => {
            let home = crate::home::configure_home(location)?;
            let state = crate::home::state_dir();
            Ok(Outcome::new(
                json!({"home": home, "state_dir": state}),
                format!(
                    "Waveform keeps its state in {} from now on. Sign in again there: saved sign-ins stay where they were.",
                    state.display()
                ),
            ))
        }
        ConfigCommand::Telemetry { .. } => Ok(telemetry_removed()),
    }
}
