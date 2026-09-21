//! Stateful Waveform CLI. Session material lives under a private `.waveform` directory.
use clap::{Parser, Subcommand};
mod daemon;
mod updater;

use silicon_waveform_client::{
    Auth, Client, CreateTestEnvironment, ProviderKeys, SttRequest, TestEnvironmentKey,
    TtsProviderOptions, TtsRequest,
};
use std::{
    fs,
    io::{self, IsTerminal as _, Write},
    path::PathBuf,
    time::Duration,
};

#[derive(Parser, Debug)]
#[command(
    name = "waveform",
    bin_name = "waveform",
    version,
    about = "Silicon Waveform: speech for Carbons and Silicons",
    after_help = "Start: waveform iam --json → waveform login SLT → waveform tts --help\nDocs: https://docs.waveform.teamofsilicons.com\nRepository: https://github.com/teamofsilicons/silicon-waveform\nRust: https://crates.io/crates/silicon-waveform-client\nUse waveform docs TOPIC for bundled guides; every branch accepts --help.",
    after_long_help = include_str!("../README.md")
)]
struct Args {
    /// Select an IAM test app_secret (ask_…) or a previously saved sandbox UUID.
    #[arg(
        long,
        value_name = "APP_SECRET_OR_ID",
        global = true,
        env = "WAVEFORM_TEST",
        hide_env_values = true
    )]
    test: Option<String>,
    /// Read an IAM test app_secret from a file (use - for stdin).
    #[arg(long, global = true, conflicts_with = "test")]
    app_secret_file: Option<PathBuf>,
    /// Waveform backend origin; defaults to the production service.
    #[arg(
        long,
        env = "WAVEFORM_URL",
        default_value = "https://backend.waveform.teamofsilicons.com"
    )]
    url: String,
    #[arg(long, global = true)]
    json: bool,
    /// Organization identifier used when resolving a test UUID.
    #[arg(long, env = "WAVEFORM_ORG", global = true)]
    organization: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand, Debug)]
enum Command {
    /// Read bundled usage and integration guides. Example: waveform docs testing.
    Docs {
        #[arg(default_value="start", value_parser=["start","cli","api","client","iam","testing","configuration"])]
        topic: String,
    },
    /// Submit a detailed bug report; optionally include your pull request.
    Report {
        message: String,
        #[arg(long)]
        pr: Option<String>,
        #[arg(long)]
        idempotency: Option<String>,
    },
    /// Inspect or stop the legacy updater. Honeycomb manages installation and updates.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Enable or disable account telemetry. Local CLI telemetry: config telemetry on|off.
    Telemetry {
        #[arg(value_parser=["on","off"])]
        enabled: String,
    },
    /// Show public IAM application metadata, including the app_id used to obtain an SLT.
    Iam,
    /// Show API versions, compatibility and deprecation policy.
    Contracts,
    /// Exchange one short-lived IAm token, or inspect the current login with `login status`.
    #[command(args_conflicts_with_subcommands = true)]
    Login {
        #[command(subcommand)]
        command: Option<LoginCommand>,
        /// Optional SLT. When omitted, read it without echoing in an interactive terminal.
        #[arg(value_name = "SLT")]
        slt: Option<String>,
        /// Backwards-compatible spelling of the positional SLT.
        #[arg(long = "slt", hide = true, conflicts_with = "slt")]
        slt_flag: Option<String>,
    },
    /// Synthesize text and return the stored file URLs.
    Tts {
        text: String,
        /// Voice profile ID for this generation; omitted uses your account default.
        #[arg(long)]
        voice_profile: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        /// Comma-separated providers for this request, in preferred order.
        #[arg(long, conflicts_with = "provider")]
        provider_order: Option<String>,
        /// Provider to use for synthesis. With fallback off, only this provider runs.
        #[arg(long, value_parser = ["gemini", "elevenlabs", "openai"])]
        provider: Option<String>,
        /// Try the remaining providers after failure (off by default).
        #[arg(long, conflicts_with_all = ["provider_options", "provider_options_file"])]
        auto_fallback: bool,
        /// Provider-specific JSON controls, for example {"gemini":{"scene":"Quiet room"}}.
        #[arg(long, conflicts_with = "provider_options_file")]
        provider_options: Option<String>,
        /// Read provider-specific JSON controls from a file (use - for stdin).
        #[arg(long)]
        provider_options_file: Option<PathBuf>,
        /// Request-only provider key, in provider=path form; repeat for multiple providers.
        #[arg(long, value_name = "PROVIDER=PATH")]
        provider_key_file: Vec<String>,
        #[arg(long)]
        idempotency: Option<String>,
        /// Known job UUID for polling while this synchronous call is running.
        #[arg(long)]
        request_id: Option<uuid::Uuid>,
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
    },
    /// Transcribe an uploaded Briefcase URL.
    Stt {
        file_url: String,
        #[arg(long)]
        language: Option<String>,
        /// Comma-separated providers for this request, in preferred order.
        #[arg(long)]
        provider_order: Option<String>,
        /// Request-only provider key, in provider=path form; repeat for multiple providers.
        #[arg(long, value_name = "PROVIDER=PATH")]
        provider_key_file: Vec<String>,
        #[arg(long)]
        idempotency: Option<String>,
        /// Known job UUID for polling while this synchronous call is running.
        #[arg(long)]
        request_id: Option<uuid::Uuid>,
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
    },
    /// Refresh and save the current IAM session.
    Refresh,
    /// Revoke the saved session.
    Logout,
    /// Show the authenticated actor and organization authorization.
    Me,
    /// Show provider and media capabilities.
    Capabilities,
    /// Read or update the authenticated actor's provider order.
    Preferences {
        /// Set the default voice profile for this account.
        #[arg(long)]
        voice_profile: Option<String>,
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
        /// Comma-separated providers, in preferred order.
        #[arg(long)]
        tts_order: Option<String>,
        /// Comma-separated providers, in preferred order.
        #[arg(long)]
        stt_order: Option<String>,
    },
    /// List voice profiles and their provider mappings.
    VoiceProfiles {
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
    },
    /// List configured personal provider keys (secret values are never shown).
    ProviderKeys {
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
    },
    /// Store or replace a personal provider key.
    ProviderKeySet {
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
        provider: String,
        /// Legacy positional key; prefer --key-file to keep secrets out of process arguments.
        #[arg(required_unless_present = "key_file", conflicts_with = "key_file")]
        api_key: Option<String>,
        /// Read the provider key from a file (use - for stdin).
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Remove a personal provider key.
    ProviderKeyDelete {
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
        provider: String,
    },
    /// List speech history or fetch one job.
    Jobs {
        #[arg(long)]
        org: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        operation: Option<String>,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        cursor: Option<String>,
        /// Keep polling a specific job until it reaches a terminal state.
        #[arg(long, default_value_t = false)]
        wait: bool,
        /// Maximum time to wait when `--wait` is used.
        #[arg(long, default_value_t = 60_000)]
        timeout_ms: u64,
        /// Delay between job status requests when `--wait` is used.
        #[arg(long, default_value_t = 500)]
        poll_ms: u64,
    },
    /// Configure local CLI state.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect test environments. Lifecycle management is in Honeycomb.
    TestEnv {
        #[command(subcommand)]
        command: TestEnvironmentCommand,
    },
}

#[derive(Subcommand, Debug)]
enum DaemonCommand {
    /// Run in the foreground; normally launched by the OS service manager.
    Run,
    /// Start for this session without registering an OS service.
    Start,
    /// Show whether this home's updater is running.
    Status,
    /// Request shutdown of a manually started updater. OS services may restart it.
    Stop,
    /// Register and start a macOS launch agent or Linux user service.
    Install,
}

#[derive(Subcommand, Debug)]
enum LoginCommand {
    /// Verify the saved session online and show its carbon or silicon identity.
    Status,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Control local CLI diagnostic recording (enabled by default).
    Telemetry {
        #[arg(value_parser=["on","off"])]
        enabled: String,
    },
    /// Enable or disable hourly checks after commands complete.
    AutoUpdate {
        #[arg(value_parser = ["on", "off"])]
        enabled: String,
    },
    /// Set the existing directory used for Waveform state.
    Home {
        /// Existing directory under which `.waveform` will be stored.
        location: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum TestEnvironmentCommand {
    /// Legacy command; create shared environments in Honeycomb.
    #[command(hide = true)]
    Create {
        #[arg(long)]
        org: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        iam_environment_id: String,
        #[arg(long)]
        iam_environment_key: String,
        #[arg(long)]
        app_secret: String,
        #[arg(long)]
        briefcase_environment_key: String,
    },
    /// List environments belonging to the authenticated organization.
    List {
        #[arg(long)]
        org: String,
    },
    /// Show one environment by UUID.
    Show {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Legacy command; manage shared environments in Honeycomb.
    #[command(hide = true)]
    Key {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Legacy command; manage shared environments in Honeycomb.
    #[command(hide = true)]
    Rotate {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Legacy command; manage shared environments in Honeycomb.
    #[command(hide = true)]
    Delete {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Legacy command; manage shared environments in Honeycomb.
    #[command(hide = true)]
    Restore {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Show metadata for the selected environment.
    Current {
        #[arg(long)]
        org: String,
    },
    /// Legacy command; manage shared environments in Honeycomb.
    #[command(hide = true)]
    Clean {
        #[arg(long)]
        org: String,
    },
}
fn default_home_dir() -> PathBuf {
    std::env::var_os("SILICON_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn home_config_path() -> PathBuf {
    default_home_dir().join(".waveform").join("config.json")
}

/// Returns the configured state directory, falling back safely when an old or
/// manually edited config file cannot be decoded.
fn dirs_fallback() -> PathBuf {
    let configured = fs::read_to_string(home_config_path())
        .ok()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        .and_then(|value| value.get("home")?.as_str().map(PathBuf::from));
    configured
        .unwrap_or_else(default_home_dir)
        .join(".waveform")
        .join("dir")
}

fn scope_digest(base: &str, environment: Option<&str>) -> String {
    use sha2::Digest as _;
    let mut digest = sha2::Sha256::new();
    digest.update(base.trim_end_matches('/').as_bytes());
    digest.update([0]);
    digest.update(environment.unwrap_or("production").as_bytes());
    format!("{:x}", digest.finalize())
}
fn session_path(base: &str, environment: Option<&str>) -> PathBuf {
    dirs_fallback().join(format!("session-{}.json", scope_digest(base, environment)))
}
fn test_environments_path(base: &str) -> PathBuf {
    dirs_fallback().join(format!(
        "test-environments-{}.json",
        scope_digest(base, None)
    ))
}
fn read_local_test_key(base: &str, id: &str) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(test_environments_path(base)).ok()?).ok()?;
    value.get(id)?.as_str().map(str::to_owned)
}
async fn write_local_test_key(base: &str, id: &str, key: &str) -> Result<(), String> {
    let path = test_environments_path(base);
    let _lock = session_lock(&path).await?;
    let mut map: serde_json::Map<String, serde_json::Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default();
    map.insert(id.to_owned(), serde_json::Value::String(key.to_owned()));
    write_session(&path, &serde_json::Value::Object(map))
}

fn configure_home(location: &std::path::Path) -> Result<(), String> {
    if !location.is_dir() {
        return Err(format!("not a directory: {}", location.display()));
    }
    let location = fs::canonicalize(location)
        .map_err(|_| format!("cannot resolve directory: {}", location.display()))?;
    configure_home_at(&home_config_path(), &location)
}

fn configure_home_at(
    config_path: &std::path::Path,
    location: &std::path::Path,
) -> Result<(), String> {
    if !location.is_dir() {
        return Err(format!("not a directory: {}", location.display()));
    }
    let location = fs::canonicalize(location)
        .map_err(|_| format!("cannot resolve directory: {}", location.display()))?;
    let config_dir = config_path
        .parent()
        .ok_or_else(|| "home configuration path has no parent".to_owned())?;
    fs::create_dir_all(config_dir).map_err(|_| "cannot create ~/.waveform".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(config_dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot secure config directory".to_owned())?;
    }
    let tmp = config_path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(&serde_json::json!({"home": location}))
            .map_err(|_| "cannot encode home configuration".to_owned())?,
    )
    .map_err(|_| "cannot write home configuration".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .map_err(|_| "cannot secure home configuration".to_owned())?;
    }
    fs::rename(tmp, config_path).map_err(|_| "cannot commit home configuration".to_owned())
}
#[derive(serde::Deserialize)]
struct Session {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: u64,
    #[serde(default)]
    refresh_started_at: Option<u64>,
}
fn read_session(path: &std::path::Path) -> Option<Session> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn session_lock(path: &std::path::Path) -> Result<fs::File, String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> std::io::Result<fs::File> {
        let directory = path.parent().expect("session path has a parent");
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(path.with_extension("lock"))?;
        file.lock()?;
        Ok(file)
    })
    .await
    .map_err(|_| "cannot acquire session lock".to_owned())?
    .map_err(|_| "cannot lock session".to_owned())
}

async fn refresh_session(
    client: &Client,
    path: &std::path::Path,
    force: bool,
) -> Result<Session, silicon_waveform_client::Error> {
    use silicon_waveform_client::Error;
    let _lock = session_lock(path).await.map_err(Error::Invalid)?;
    let mut session = read_session(path).ok_or_else(|| Error::Invalid("not logged in for this server and environment; run waveform login with the same --url and --test options".to_owned()))?;
    if !force
        && session.expires_at > now().saturating_add(60)
        && session.refresh_started_at.is_none()
    {
        return Ok(session);
    }
    for _ in 0..2 {
        let started_at = *session.refresh_started_at.get_or_insert_with(now);
        // Retain all token metadata while recording the start before consuming the token.
        let mut pending: serde_json::Value =
            serde_json::from_slice(&fs::read(path).map_err(|e| Error::Invalid(e.to_string()))?)?;
        pending["refresh_started_at"] = serde_json::json!(started_at);
        write_session(path, &pending).map_err(Error::Invalid)?;
        let refresh = session.refresh_token.as_ref().ok_or_else(|| {
            Error::Invalid("session has no refresh token; run waveform login".to_owned())
        })?;
        use sha2::{Digest as _, Sha256};
        let key = format!("waveform-refresh-{:x}", Sha256::digest(refresh.as_bytes()));
        let tokens = client.refresh_with_key(refresh, &key).await?;
        let mut value = saved_tokens(&tokens).map_err(Error::Invalid)?;
        value["expires_at"] =
            serde_json::json!(started_at.saturating_add(tokens.expires_in.max(0) as u64));
        write_session(path, &value).map_err(Error::Invalid)?;
        session = serde_json::from_value(value)?;
        if session.expires_at > now().saturating_add(60) {
            return Ok(session);
        }
    }
    Err(Error::Invalid(
        "refreshed access token has no usable lifetime; retry the command".into(),
    ))
}

fn saved_tokens(
    tokens: &silicon_iam_client::models::OAuthTokenResponse,
) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value(tokens).map_err(|error| error.to_string())?;
    value["expires_at"] = serde_json::json!(now().saturating_add(tokens.expires_in.max(0) as u64));
    Ok(value)
}

async fn bearer(client: &Client, path: &std::path::Path) -> Result<Client, String> {
    let session = refresh_session(client, path, false)
        .await
        .map_err(|error| error.to_string())?;
    Ok(client.with_bearer(session.access_token))
}
fn list_arg(value: Option<String>) -> Option<Vec<String>> {
    value.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

fn read_bounded_file(path: &std::path::Path, limit: usize, label: &str) -> Result<String, String> {
    use std::io::Read as _;
    let reader: Box<dyn io::Read> = if path.as_os_str() == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(fs::File::open(path).map_err(|_| format!("cannot read {label} file"))?)
    };
    let mut value = String::new();
    reader
        .take(limit as u64 + 1)
        .read_to_string(&mut value)
        .map_err(|_| format!("cannot read {label} file as UTF-8"))?;
    if value.len() > limit {
        return Err(format!("{label} file exceeds {limit} bytes"));
    }
    Ok(value)
}

fn read_provider_key(path: &std::path::Path) -> Result<String, String> {
    let value = read_bounded_file(path, 16386, "provider key")?;
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() || value.len() > 16384 || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(
            "provider key must contain 1-16384 ASCII bytes without whitespace or control characters".into(),
        );
    }
    Ok(value)
}

fn read_provider_keys(files: &[String]) -> Result<ProviderKeys, String> {
    let mut keys = ProviderKeys::default();
    let mut providers = std::collections::BTreeSet::new();
    for entry in files {
        let (provider, path) = entry
            .split_once('=')
            .filter(|(_, path)| !path.is_empty())
            .ok_or("--provider-key-file requires PROVIDER=PATH (use - for stdin)")?;
        if !matches!(provider, "gemini" | "elevenlabs" | "openai" | "deepgram") {
            return Err("provider must be gemini, elevenlabs, openai, or deepgram".into());
        }
        if !providers.insert(provider) {
            return Err("provide at most one key file for each provider".into());
        }
        keys.insert(provider, read_provider_key(std::path::Path::new(path))?)
            .map_err(|error| error.to_string())?;
    }
    Ok(keys)
}

fn read_provider_options(
    value: Option<String>,
    file: Option<PathBuf>,
) -> Result<TtsProviderOptions, String> {
    let value = match (value, file) {
        (Some(value), _) => value,
        // Match the API body bound so valid Unicode guidance and JSON escapes fit.
        (None, Some(path)) => read_bounded_file(&path, 327_680, "provider options")?,
        (None, None) => return Ok(TtsProviderOptions::default()),
    };
    serde_json::from_str(&value).map_err(|error| {
        format!("invalid provider options JSON at line {}, column {}; check the provider control fields and types", error.line(), error.column())
    })
}

fn validate_stdin_sources(args: &Args) -> Result<(), String> {
    let is_stdin = |path: &Option<PathBuf>| {
        usize::from(path.as_ref().is_some_and(|path| path.as_os_str() == "-"))
    };
    let key_stdin = |files: &[String]| {
        files
            .iter()
            .filter(|entry| entry.split_once('=').is_some_and(|(_, path)| path == "-"))
            .count()
    };
    let sources = is_stdin(&args.app_secret_file)
        + match &args.command {
            Command::Tts {
                provider_options_file,
                provider_key_file,
                ..
            } => is_stdin(provider_options_file) + key_stdin(provider_key_file),
            Command::Stt {
                provider_key_file, ..
            } => key_stdin(provider_key_file),
            Command::ProviderKeySet { key_file, .. } => is_stdin(key_file),
            _ => 0,
        };
    if sources > 1 {
        Err("only one input may read stdin; use files for the remaining inputs".into())
    } else {
        Ok(())
    }
}
fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}
fn print_status(message: &str, json: bool) -> Result<(), String> {
    if json {
        print_json(&serde_json::json!({"message": message}))
    } else {
        println!("{message}");
        Ok(())
    }
}
fn write_session(path: &std::path::Path, value: &serde_json::Value) -> Result<(), String> {
    let dir = path.parent().ok_or("session path has no parent")?;
    fs::create_dir_all(dir).map_err(|_| "cannot create ~/.waveform".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot secure session directory".to_owned())?;
    }
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .map_err(|_| "cannot create private session file".to_owned())?;
    file.write_all(value.to_string().as_bytes())
        .map_err(|_| "cannot write session".to_owned())?;
    file.sync_all()
        .map_err(|_| "cannot sync session".to_owned())?;
    fs::rename(tmp, path).map_err(|_| "cannot commit session".to_owned())?;
    #[cfg(unix)]
    fs::File::open(dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "cannot sync session directory".to_owned())?;
    Ok(())
}
fn read_slt(value: Option<String>) -> Result<String, String> {
    if let Some(v) = value {
        return Ok(v);
    }
    if io::stdin().is_terminal() {
        return rpassword::prompt_password("Short-lived IAm token: ")
            .map_err(|_| "cannot read token".to_owned());
    }
    print!("Short-lived IAm token: ");
    io::stdout()
        .flush()
        .map_err(|_| "cannot read token".to_owned())?;
    let mut v = String::new();
    io::stdin()
        .read_line(&mut v)
        .map_err(|_| "cannot read token".to_owned())?;
    Ok(v.trim().to_owned())
}
fn command_org(command: &Command) -> Option<&str> {
    match command {
        Command::Tts { org, .. }
        | Command::Stt { org, .. }
        | Command::Jobs { org, .. }
        | Command::Preferences { org, .. }
        | Command::VoiceProfiles { org, .. }
        | Command::ProviderKeys { org, .. }
        | Command::ProviderKeySet { org, .. }
        | Command::ProviderKeyDelete { org, .. } => Some(org),
        Command::TestEnv { command } => match command {
            TestEnvironmentCommand::Create { org, .. }
            | TestEnvironmentCommand::List { org }
            | TestEnvironmentCommand::Show { org, .. }
            | TestEnvironmentCommand::Key { org, .. }
            | TestEnvironmentCommand::Rotate { org, .. }
            | TestEnvironmentCommand::Delete { org, .. }
            | TestEnvironmentCommand::Restore { org, .. }
            | TestEnvironmentCommand::Current { org }
            | TestEnvironmentCommand::Clean { org } => Some(org),
        },
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = match Args::try_parse() {
        Ok(args) => args,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            if std::env::args().any(|s| {
                s == "--test"
                    || s.starts_with("--test=")
                    || (s == "--app-secret-file" || s.starts_with("--app-secret-file="))
            }) {
                eprintln!("Test environment selected; command was not executed.");
            }
            std::process::exit(code);
        }
    };
    validate_stdin_sources(&args)?;
    if let Some(path) = args.app_secret_file.take() {
        let value = if path.as_os_str() == "-" {
            std::io::read_to_string(std::io::stdin())
        } else {
            fs::read_to_string(path)
        };
        match value {
            Ok(value) => args.test = Some(value.trim().to_owned()),
            Err(_) => {
                eprintln!(
                    "Cannot read test app_secret file. Test environment selection failed; no command executed."
                );
                std::process::exit(1);
            }
        }
    }
    let mut banner = args
        .test
        .as_ref()
        .map(|_| "Test environment: unresolved (validation required)".to_owned());
    #[cfg(unix)]
    let local_enabled = fs::read(dirs_fallback().join("telemetry.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["enabled"].as_bool())
        .unwrap_or(true);
    #[cfg(unix)]
    let telemetry = if local_enabled
        && args.test.is_none()
        && !matches!(
            args.command,
            Command::Config { .. } | Command::Daemon { .. }
        ) {
        silicon_waveform_client::telemetry::from_environment()
    } else {
        None
    };
    #[cfg(unix)]
    let step = match &args.command {
        Command::Tts { .. } => "tts",
        Command::Stt { .. } => "stt",
        Command::Login { .. } => "login",
        Command::Refresh => "refresh",
        Command::Logout => "logout",
        Command::Report { .. } => "report",
        Command::Jobs { .. } => "jobs",
        Command::Preferences { .. } => "preferences",
        Command::Telemetry { .. } => "telemetry_preference",
        Command::ProviderKeySet { .. } | Command::ProviderKeyDelete { .. } => "provider_key_change",
        Command::TestEnv { .. } => "legacy_environment_administration",
        _ => "read_configuration",
    };
    #[cfg(unix)]
    let started = std::time::Instant::now();
    let result = run(args, &mut banner).await;
    #[cfg(unix)]
    if let Some(station) = &telemetry {
        silicon_waveform_client::telemetry::record(
            station,
            "cli",
            step,
            result.is_ok(),
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
    }
    #[cfg(unix)]
    drop(telemetry);
    if let Err(error) = &result {
        eprintln!("{error}");
    }
    if let Some(banner) = banner {
        eprintln!("{banner}");
    }
    if result.is_err() {
        std::process::exit(1);
    }
    Ok(())
}

async fn run(args: Args, banner: &mut Option<String>) -> Result<(), String> {
    if let Command::Docs { topic } = &args.command {
        print!(
            "{}",
            match topic.as_str() {
                "cli" => include_str!("../README.md"),
                "api" => include_str!("../docs/api.md"),
                "client" => include_str!("../docs/client.md"),
                "iam" => include_str!("../docs/iam.md"),
                "testing" => include_str!("../docs/testing.md"),
                "configuration" => include_str!("../docs/configuration.md"),
                _ => include_str!("../docs/README.md"),
            }
        );
        return Ok(());
    }
    if let Command::Daemon { command } = &args.command {
        if args.test.is_some() {
            return Err("The local updater has no test environment; omit --test".into());
        }
        match command {
            DaemonCommand::Run => daemon::run(&dirs_fallback()).await?,
            DaemonCommand::Start => daemon::start(&dirs_fallback())?,
            DaemonCommand::Stop => daemon::stop(&dirs_fallback())?,
            DaemonCommand::Install => daemon::install()?,
            DaemonCommand::Status => print_json(&daemon::status(&dirs_fallback())?)?,
        };
        return Ok(());
    }
    if let Command::Config {
        command: ConfigCommand::Telemetry { enabled },
    } = &args.command
    {
        write_session(
            &dirs_fallback().join("telemetry.json"),
            &serde_json::json!({"enabled":enabled=="on"}),
        )?;
        return print_status(&format!("Local telemetry {enabled}"), args.json);
    }
    if let Command::Config {
        command: ConfigCommand::AutoUpdate { enabled },
    } = &args.command
    {
        updater::set_enabled(&dirs_fallback(), enabled == "on")?;
        print_status(&format!("Automatic updates {enabled}"), args.json)?;
        return Ok(());
    }
    if let Command::Config {
        command: ConfigCommand::Home { location },
    } = &args.command
    {
        configure_home(location)?;
        print_status(
            &format!(
                "Waveform state directory set to {}",
                dirs_fallback().display()
            ),
            args.json,
        )?;
        return Ok(());
    }
    let mut client = Client::new(&args.url, Auth::Anonymous)
        .map_err(|e| e.to_string())?
        .with_auto_update(false);
    let mut session_file = session_path(&args.url, None);
    let test_selector = args.test.clone();
    if let Some(selector) = test_selector.as_deref() {
        // Root keys are accepted directly. For ergonomic CLI use, UUIDs are
        // resolved through the production management endpoint using the saved
        // IAM session, then only the resulting root key is sent to the sandbox.
        let root_key = match TestEnvironmentKey::new(selector.to_owned()) {
            Ok(key) => key,
            Err(_) if uuid::Uuid::parse_str(selector).is_ok() => {
                if let Some(key) = read_local_test_key(&args.url, selector) {
                    TestEnvironmentKey::new(key).map_err(|e| e.to_string())?
                } else {
                    let production = bearer(&client, &session_file).await?;
                    let organization = args
                        .organization
                        .as_deref()
                        .or_else(|| command_org(&args.command))
                        .ok_or("an organization is required when --test is an environment UUID")?;
                    let value = production
                        .test_environment_key(organization, selector)
                        .await
                        .map_err(|e| e.to_string())?;
                    let key = value
                        .get("key")
                        .and_then(serde_json::Value::as_str)
                        .ok_or("environment key response did not contain key")?;
                    write_local_test_key(&args.url, selector, key).await?;
                    TestEnvironmentKey::new(key.to_owned()).map_err(|e| e.to_string())?
                }
            }
            Err(error) => return Err(error.to_string()),
        };
        let encoded_key = serde_json::to_string(&root_key).map_err(|e| e.to_string())?;
        session_file = session_path(&args.url, Some(&encoded_key));
        client = client.with_test_environment(root_key);
        let metadata = client
            .current_test_environment()
            .await
            .map_err(|e| e.to_string())?;
        if let Some(id) = metadata.get("id").and_then(serde_json::Value::as_str) {
            let value: String = serde_json::from_str(&encoded_key).map_err(|e| e.to_string())?;
            write_local_test_key(&args.url, id, &value).await?;
        }
        *banner = Some(format!(
            "Test environment: {} ({})",
            metadata["name"].as_str().unwrap_or("unnamed"),
            metadata["id"].as_str().unwrap_or("unknown")
        ));
    }
    match args.command {
        Command::Docs { .. } | Command::Daemon { .. } => {
            unreachable!("local commands return early")
        }
        Command::Report {
            message,
            pr,
            idempotency,
        } => {
            let c = bearer(&client, &session_file).await?;
            let key = idempotency.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            eprintln!("Report retry key: {key}");
            print_json(
                &c.report(&message, pr.as_deref(), &key)
                    .await
                    .map_err(|e| e.to_string())?,
            )?;
            if pr.is_none() {
                eprintln!(
                    "You can also submit a fix: https://github.com/teamofsilicons/silicon-waveform"
                );
            }
        }
        Command::Telemetry { enabled } => {
            let c = bearer(&client, &session_file).await?;
            let me = c.me().await.map_err(|e| e.to_string())?;
            print_json(
                &c.set_telemetry(
                    me["org_id"].as_str().ok_or("IAM organization missing")?,
                    me["public_id"].as_str().ok_or("IAM identity missing")?,
                    enabled == "on",
                )
                .await
                .map_err(|e| e.to_string())?,
            )?;
        }
        Command::Contracts => print_json(&client.contracts().await.map_err(|e| e.to_string())?)?,
        Command::Iam => print_json(&client.iam().await.map_err(|e| e.to_string())?)?,
        Command::Login {
            command: Some(LoginCommand::Status),
            ..
        } => {
            let selected = match read_session(&session_file) {
                Some(_) => match refresh_session(&client, &session_file, false).await {
                    Ok(session) => client.with_bearer(session.access_token),
                    Err(silicon_waveform_client::Error::Api {
                        status: 401 | 403, ..
                    }) => client,
                    Err(error) => return Err(error.to_string()),
                },
                None => client,
            };
            let status = selected.login_status().await.map_err(|e| e.to_string())?;
            if args.json {
                print_json(&status)?;
            } else if let Some(actor) = status.actor {
                println!(
                    "Authenticated as {} {}, organization {}",
                    serde_json::to_value(actor.actor_type)
                        .map_err(|e| e.to_string())?
                        .as_str()
                        .ok_or("invalid actor type")?,
                    actor.public_id,
                    status.org_id.as_deref().unwrap_or("unknown")
                );
            } else {
                println!(
                    "Not authenticated; run waveform login with the same --url and --test options."
                );
            }
        }
        Command::Login {
            slt,
            slt_flag,
            command: None,
        } => {
            let slt = read_slt(slt.or(slt_flag))?;
            let _lock = session_lock(&session_file).await?;
            let tokens = client.login(&slt).await.map_err(|e| e.to_string())?;
            write_session(&session_file, &saved_tokens(&tokens)?)?;
            print_status(
                "logged in; session saved for this server and environment",
                args.json,
            )?;
        }
        Command::Refresh => {
            refresh_session(&client, &session_file, true)
                .await
                .map_err(|error| error.to_string())?;
            print_status("session refreshed", args.json)?;
        }
        Command::Logout => {
            let _lock = session_lock(&session_file).await?;
            let session = read_session(&session_file).ok_or("not logged in; run waveform login")?;
            client
                .logout(
                    session
                        .refresh_token
                        .as_deref()
                        .unwrap_or(&session.access_token),
                )
                .await
                .map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&session_file);
            print_status("logged out", args.json)?;
        }
        Command::Me => print_json(
            &bearer(&client, &session_file)
                .await?
                .me()
                .await
                .map_err(|e| e.to_string())?,
        )?,
        Command::Capabilities => {
            print_json(&client.capabilities().await.map_err(|e| e.to_string())?)?
        }
        Command::Jobs {
            org,
            actor,
            operation,
            job_id,
            limit,
            cursor,
            wait,
            timeout_ms,
            poll_ms,
        } => {
            let c = bearer(&client, &session_file).await?;
            if let Some(job_id) = job_id {
                let value = if wait {
                    c.wait_for_job(
                        &job_id,
                        &org,
                        &actor,
                        Duration::from_millis(timeout_ms),
                        Duration::from_millis(poll_ms),
                    )
                    .await
                } else {
                    c.job(&job_id, &org, &actor).await
                }
                .map_err(|e| e.to_string())?;
                print_json(&value)?;
            } else {
                print_json(
                    &c.jobs_page(&org, &actor, operation.as_deref(), limit, cursor.as_deref())
                        .await
                        .map_err(|e| e.to_string())?,
                )?;
            }
        }
        Command::Config { .. } => unreachable!("config commands return before API setup"),
        Command::Preferences {
            voice_profile,
            org,
            actor,
            tts_order,
            stt_order,
        } => {
            let c = bearer(&client, &session_file).await?;
            let value = if tts_order.is_none() && stt_order.is_none() && voice_profile.is_none() {
                c.preferences(&org, &actor).await
            } else {
                c.update_preferences_with_voice(
                    &org,
                    &actor,
                    list_arg(tts_order),
                    list_arg(stt_order),
                    voice_profile,
                )
                .await
            };
            print_json(&value.map_err(|e| e.to_string())?)?;
        }
        Command::VoiceProfiles { org, actor } => print_json(
            &bearer(&client, &session_file)
                .await?
                .voice_profiles(&org, &actor)
                .await
                .map_err(|e| e.to_string())?,
        )?,
        Command::ProviderKeys { org, actor } => print_json(
            &bearer(&client, &session_file)
                .await?
                .provider_keys(&org, &actor)
                .await
                .map_err(|e| e.to_string())?,
        )?,
        Command::ProviderKeySet {
            org,
            actor,
            provider,
            api_key,
            key_file,
        } => {
            let api_key = match (api_key, key_file) {
                (Some(key), _) => key,
                (None, Some(path)) => read_provider_key(&path)?,
                (None, None) => return Err("provide --key-file or a provider key".into()),
            };
            bearer(&client, &session_file)
                .await?
                .put_provider_key(&org, &actor, &provider, &api_key)
                .await
                .map_err(|e| e.to_string())?;
            print_status("provider key saved", args.json)?;
        }
        Command::ProviderKeyDelete {
            org,
            actor,
            provider,
        } => {
            bearer(&client, &session_file)
                .await?
                .delete_provider_key(&org, &actor, &provider)
                .await
                .map_err(|e| e.to_string())?;
            print_status("provider key deleted", args.json)?;
        }
        Command::Tts {
            text,
            voice_profile,
            lang,
            provider_order,
            provider,
            auto_fallback,
            provider_options,
            provider_options_file,
            provider_key_file,
            idempotency,
            request_id,
            org,
            actor,
        } => {
            let provider_options = read_provider_options(provider_options, provider_options_file)?;
            let provider_keys = read_provider_keys(&provider_key_file)?;
            let idempotency =
                idempotency.unwrap_or_else(|| format!("waveform-cli-{}", uuid::Uuid::new_v4()));
            let request_id = request_id.unwrap_or_else(uuid::Uuid::new_v4);
            let c = bearer(&client, &session_file)
                .await?
                .with_speech_request_id(request_id)
                .map_err(|e| e.to_string())?;
            eprintln!(
                "Speech request {request_id}. Poll with jobs --org {org} --actor {actor} --job-id {request_id} --wait using the same server and test options."
            );
            let value = c
                .tts(
                    &org,
                    &actor,
                    &TtsRequest {
                        text,
                        voice_profile,
                        lang,
                        provider_order: provider
                            .map(|provider| vec![provider])
                            .or_else(|| list_arg(provider_order)),
                        auto_fallback,
                        provider_options,
                        provider_keys,
                    },
                    &idempotency,
                )
                .await
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
            );
        }
        Command::Stt {
            file_url,
            language,
            provider_order,
            provider_key_file,
            idempotency,
            request_id,
            org,
            actor,
        } => {
            let provider_keys = read_provider_keys(&provider_key_file)?;
            let idempotency =
                idempotency.unwrap_or_else(|| format!("waveform-cli-{}", uuid::Uuid::new_v4()));
            let request_id = request_id.unwrap_or_else(uuid::Uuid::new_v4);
            let c = bearer(&client, &session_file)
                .await?
                .with_speech_request_id(request_id)
                .map_err(|e| e.to_string())?;
            eprintln!(
                "Speech request {request_id}. Poll with jobs --org {org} --actor {actor} --job-id {request_id} --wait using the same server and test options."
            );
            let value = c
                .stt(
                    &org,
                    &actor,
                    &SttRequest {
                        file_url,
                        language,
                        provider_order: list_arg(provider_order),
                        provider_keys,
                    },
                    &idempotency,
                )
                .await
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
            );
        }
        Command::TestEnv { command } => match command {
            TestEnvironmentCommand::Create {
                org,
                name,
                description,
                iam_environment_id,
                iam_environment_key,
                app_secret,
                briefcase_environment_key,
            } => {
                if test_selector.is_some() {
                    return Err("test-env create must run without --test".into());
                }
                let created = bearer(&client, &session_file)
                    .await?
                    .create_test_environment(
                        &org,
                        &CreateTestEnvironment {
                            name,
                            description,
                            iam_environment_id,
                            iam_environment_key,
                            app_secret,
                            briefcase_environment_key,
                        },
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let encoded = serde_json::to_value(&created).map_err(|e| e.to_string())?;
                if let (Some(id), Some(key)) = (
                    encoded.get("id").and_then(serde_json::Value::as_str),
                    encoded.get("key").and_then(serde_json::Value::as_str),
                ) {
                    write_local_test_key(&args.url, id, key).await?;
                }
                print_json(&encoded)?;
            }
            TestEnvironmentCommand::List { org } => {
                if test_selector.is_some() {
                    return Err("test-env list must run without --test".into());
                }
                print_json(
                    &bearer(&client, &session_file)
                        .await?
                        .test_environments(&org)
                        .await
                        .map_err(|e| e.to_string())?,
                )?;
            }
            TestEnvironmentCommand::Show {
                org,
                environment_id,
            } => print_json(
                &bearer(&client, &session_file)
                    .await?
                    .test_environment_detail(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?,
            )?,
            TestEnvironmentCommand::Key {
                org,
                environment_id,
            } => {
                let value = bearer(&client, &session_file)
                    .await?
                    .test_environment_key(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(key) = value.get("key").and_then(serde_json::Value::as_str) {
                    write_local_test_key(&args.url, &environment_id, key).await?;
                }
                print_json(&value)?;
            }
            TestEnvironmentCommand::Rotate {
                org,
                environment_id,
            } => {
                let value = bearer(&client, &session_file)
                    .await?
                    .rotate_test_environment_key(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(key) = value.get("key").and_then(serde_json::Value::as_str) {
                    write_local_test_key(&args.url, &environment_id, key).await?;
                }
                print_json(&value)?;
            }
            TestEnvironmentCommand::Delete {
                org,
                environment_id,
            } => print_json(
                &bearer(&client, &session_file)
                    .await?
                    .delete_test_environment(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?,
            )?,
            TestEnvironmentCommand::Restore {
                org,
                environment_id,
            } => print_json(
                &bearer(&client, &session_file)
                    .await?
                    .restore_test_environment(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?,
            )?,
            TestEnvironmentCommand::Current { org } => {
                if test_selector.is_none() {
                    return Err("test-env current requires --test ROOT_KEY_OR_ID".into());
                }
                print_json(
                    &client
                        .test_environment(&org)
                        .await
                        .map_err(|e| e.to_string())?,
                )?;
            }
            TestEnvironmentCommand::Clean { org } => {
                if test_selector.is_none() {
                    return Err("test-env clean requires --test ROOT_KEY_OR_ID".into());
                }
                client
                    .clean_test_environment(&org)
                    .await
                    .map_err(|e| e.to_string())?;
                print_status("test environment cleaned", args.json)?;
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Command, configure_home_at, read_slt};
    use clap::Parser as _;
    use std::fs;

    #[test]
    fn tts_defaults_to_one_provider_and_controls_conflict_with_fallback() {
        let base = [
            "waveform", "tts", "Hello", "--org", "tos", "--actor", "actor",
        ];
        let args = super::Args::try_parse_from(base).expect("default synthesis");
        assert!(matches!(
            args.command,
            Command::Tts {
                auto_fallback: false,
                ..
            }
        ));
        let with = |extra: &[&str]| {
            super::Args::try_parse_from(base.iter().copied().chain(extra.iter().copied()))
        };
        assert!(
            with(&[
                "--provider",
                "elevenlabs",
                "--provider-options",
                r#"{"elevenlabs":{"stability":0.3}}"#
            ])
            .is_ok()
        );
        assert!(with(&["--provider", "gemini", "--provider-order", "openai"]).is_err());
        assert!(with(&["--auto-fallback", "--provider-options", "{}"]).is_err());
        assert!(
            with(&[
                "--auto-fallback",
                "--provider-options-file",
                "controls.json"
            ])
            .is_err()
        );
        assert!(
            with(&[
                "--provider-options",
                "{}",
                "--provider-options-file",
                "controls.json"
            ])
            .is_err()
        );
        assert!(with(&["--provider-key", "private-key"]).is_err());
    }

    #[test]
    fn provider_keys_are_read_from_files_without_appearing_in_diagnostics() {
        let root = std::env::temp_dir().join(format!("waveform-keys-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("temporary directory");
        let path = root.join("gemini.key");
        fs::write(&path, "private-request-key\n").expect("key fixture");
        let input = format!("gemini={}", path.display());
        let keys = super::read_provider_keys(std::slice::from_ref(&input)).expect("read key");
        assert_eq!(
            serde_json::to_value(&keys).unwrap()["gemini"],
            "private-request-key"
        );
        assert!(!format!("{keys:?}").contains("private-request-key"));
        assert!(super::read_provider_keys(&[input.clone(), input]).is_err());
        fs::write(&path, "private-request-key\ninvalid").expect("invalid fixture");
        let error = super::read_provider_key(&path).unwrap_err();
        assert!(!error.contains("private-request-key"));
        assert!(super::read_provider_keys(&["gemini".into()]).is_err());
        fs::remove_dir_all(root).expect("remove key fixture");
    }

    #[test]
    fn only_one_secret_or_controls_input_may_use_stdin() {
        let args = super::Args::try_parse_from([
            "waveform",
            "tts",
            "Hello",
            "--org",
            "tos",
            "--actor",
            "actor",
            "--provider-options-file",
            "-",
            "--provider-key-file",
            "gemini=-",
        ])
        .expect("stdin flags");
        assert!(super::validate_stdin_sources(&args).is_err());
        let args = super::Args::try_parse_from([
            "waveform",
            "provider-key-set",
            "--org",
            "tos",
            "--actor",
            "actor",
            "gemini",
            "--key-file",
            "-",
        ])
        .expect("saved key stdin support");
        assert!(super::validate_stdin_sources(&args).is_ok());
        assert!(
            super::Args::try_parse_from([
                "waveform",
                "provider-key-set",
                "--org",
                "tos",
                "--actor",
                "actor",
                "gemini",
                "legacy-key",
            ])
            .is_ok()
        );
    }

    #[test]
    fn scoped_sessions_keep_production_other_servers_and_test_planes_separate() {
        let root = std::env::temp_dir().join(format!("waveform-sessions-{}", uuid::Uuid::new_v4()));
        let scopes = [
            ("https://one.test", None),
            ("https://two.test", None),
            ("https://one.test", Some("test-a")),
            ("https://one.test", Some("test-b")),
        ];
        let paths: Vec<_> = scopes
            .iter()
            .map(|(base, key)| root.join(format!("{}.json", super::scope_digest(base, *key))))
            .collect();
        for (index, path) in paths.iter().enumerate() {
            super::write_session(path, &serde_json::json!({"access_token":format!("token-{index}"),"refresh_token":format!("refresh-{index}")})).expect("write scoped session");
        }
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(
                super::read_session(path)
                    .expect("read scoped session")
                    .access_token,
                format!("token-{index}")
            );
        }
        fs::remove_file(&paths[2]).expect("log out of one test plane");
        assert!(super::read_session(&paths[2]).is_none());
        assert_eq!(
            super::read_session(&paths[0])
                .expect("production survives")
                .access_token,
            "token-0"
        );
        assert_eq!(
            super::scope_digest("https://one.test/", None),
            super::scope_digest("https://one.test", None)
        );
        fs::remove_dir_all(root).expect("remove test state");
    }

    #[test]
    fn explicit_slt_does_not_touch_stdin() {
        assert_eq!(
            read_slt(Some("slt-test-value".to_owned())).unwrap(),
            "slt-test-value"
        );
    }

    #[test]
    fn login_accepts_required_positional_slt_and_legacy_flag() {
        let positional = super::Args::try_parse_from(["waveform", "login", "oac_value"])
            .expect("positional SLT should parse");
        assert!(matches!(
            positional.command,
            Command::Login { slt: Some(_), .. }
        ));
        let flag = super::Args::try_parse_from(["waveform", "login", "--slt", "oac_value"])
            .expect("legacy SLT flag should parse");
        assert!(matches!(
            flag.command,
            Command::Login {
                slt_flag: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn config_home_requires_a_directory_and_persists_the_canonical_path() {
        let root = std::env::temp_dir().join(format!("waveform-cli-{}", uuid::Uuid::new_v4()));
        let state = root.join("state");
        fs::create_dir_all(&state).expect("create temporary state directory");
        let config = root.join("config").join("home.json");
        let missing = root.join("missing");
        assert!(configure_home_at(&config, &missing).is_err());
        configure_home_at(&config, &state).expect("persist configured home");
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config).expect("read config"))
                .expect("decode config");
        assert_eq!(
            value["home"].as_str(),
            fs::canonicalize(&state).expect("canonical state").to_str()
        );
        let _ = fs::remove_dir_all(root);
    }
}
