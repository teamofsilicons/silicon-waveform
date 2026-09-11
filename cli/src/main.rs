//! Stateful Waveform CLI. Session material lives under a private `.waveform` directory.
use clap::{Parser, Subcommand};
mod updater;

use silicon_waveform_client::{
    Auth, Client, CreateTestEnvironment, SttRequest, TestEnvironmentKey, TtsRequest,
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
    about = "Synchronous Silicon Waveform speech client",
    after_long_help = include_str!("../README.md")
)]
struct Args {
    /// Execute against a test environment root key or UUID. UUIDs are resolved through IAM.
    #[arg(
        long,
        value_name = "ROOT_KEY_OR_ID",
        global = true,
        env = "WAVEFORM_TEST",
        hide_env_values = true
    )]
    test: Option<String>,
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
    /// Show public IAM application metadata, including the app_id used to obtain an SLT.
    Iam,
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
        #[arg(long)]
        provider_order: Option<String>,
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
        api_key: String,
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
    /// Manage isolated Waveform test environments.
    TestEnv {
        #[command(subcommand)]
        command: TestEnvironmentCommand,
    },
}

#[derive(Subcommand, Debug)]
enum LoginCommand {
    /// Verify the saved session online and show its carbon or silicon identity.
    Status,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
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
    /// Create an isolated environment (run without --test).
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
    /// Print the current root key (authorized managers only).
    Key {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Rotate a root key (authorized managers only).
    Rotate {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Soft-delete one environment.
    Delete {
        #[arg(long)]
        org: String,
        environment_id: String,
    },
    /// Restore one environment during its 30-day recovery window.
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
    /// Remove all data from the selected environment.
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
fn write_local_test_key(base: &str, id: &str, key: &str) -> Result<(), String> {
    let dir = dirs_fallback();
    fs::create_dir_all(&dir).map_err(|_| "cannot create ~/.waveform".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot secure session directory".to_owned())?;
    }
    let path = test_environments_path(base);
    let mut map: serde_json::Map<String, serde_json::Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    map.insert(id.to_owned(), serde_json::Value::String(key.to_owned()));
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(&map)
            .map_err(|_| "cannot encode test environment state".to_owned())?,
    )
    .map_err(|_| "cannot write test environment state".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .map_err(|_| "cannot secure test environment state".to_owned())?;
    }
    fs::rename(tmp, path).map_err(|_| "cannot commit test environment state".to_owned())
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
}
fn read_session(path: &std::path::Path) -> Option<Session> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}
fn bearer(client: &Client, path: &std::path::Path) -> Result<Client, String> {
    let session = read_session(path).ok_or("not logged in for this server and environment; run waveform login with the same --url and --test options".to_owned())?;
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
    let args = Args::parse();
    let maintain = !matches!(args.command, Command::Config { .. });
    let result = run(args).await;
    if let Err(error) = &result {
        eprintln!("{error}");
    }
    if maintain && let Err(error) = updater::automatic(&dirs_fallback()).await {
        eprintln!("Update check: {error}");
    }
    if result.is_err() {
        std::process::exit(1);
    }
    Ok(())
}

async fn run(args: Args) -> Result<(), String> {
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
                    let production = bearer(&client, &session_file)?;
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
                    write_local_test_key(&args.url, selector, key)?;
                    TestEnvironmentKey::new(key.to_owned()).map_err(|e| e.to_string())?
                }
            }
            Err(error) => return Err(error.to_string()),
        };
        let encoded_key = serde_json::to_string(&root_key).map_err(|e| e.to_string())?;
        session_file = session_path(&args.url, Some(&encoded_key));
        client = client.with_test_environment(root_key);
    }
    match args.command {
        Command::Iam => print_json(&client.iam().await.map_err(|e| e.to_string())?)?,
        Command::Login {
            command: Some(LoginCommand::Status),
            ..
        } => {
            let selected = match read_session(&session_file) {
                Some(session) => client.with_bearer(session.access_token),
                None => client,
            };
            let status = selected.login_status().await.map_err(|e| e.to_string())?;
            if args.json {
                print_json(&status)?;
            } else if let Some(actor) = status.actor {
                println!(
                    "Authenticated as {} {} (principal {}), organization {}",
                    serde_json::to_value(actor.actor_type)
                        .map_err(|e| e.to_string())?
                        .as_str()
                        .ok_or("invalid actor type")?,
                    actor.public_id,
                    actor.principal_id,
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
            let tokens = client
                .login(&read_slt(slt.or(slt_flag))?)
                .await
                .map_err(|e| e.to_string())?;
            write_session(
                &session_file,
                &serde_json::to_value(&tokens).map_err(|e| e.to_string())?,
            )?;
            print_status(
                "logged in; session saved for this server and environment",
                args.json,
            )?;
        }
        Command::Refresh => {
            let session = read_session(&session_file).ok_or("not logged in; run waveform login")?;
            let refresh = session
                .refresh_token
                .ok_or("session has no refresh token; run waveform login")?;
            let tokens = client.refresh(&refresh).await.map_err(|e| e.to_string())?;
            write_session(
                &session_file,
                &serde_json::to_value(&tokens).map_err(|e| e.to_string())?,
            )?;
            print_status("session refreshed", args.json)?;
        }
        Command::Logout => {
            let session = read_session(&session_file).ok_or("not logged in; run waveform login")?;
            client
                .logout(&session.access_token)
                .await
                .map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&session_file);
            print_status("logged out", args.json)?;
        }
        Command::Me => print_json(
            &bearer(&client, &session_file)?
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
            let c = bearer(&client, &session_file)?;
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
            let c = bearer(&client, &session_file)?;
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
            &bearer(&client, &session_file)?
                .voice_profiles(&org, &actor)
                .await
                .map_err(|e| e.to_string())?,
        )?,
        Command::ProviderKeys { org, actor } => print_json(
            &bearer(&client, &session_file)?
                .provider_keys(&org, &actor)
                .await
                .map_err(|e| e.to_string())?,
        )?,
        Command::ProviderKeySet {
            org,
            actor,
            provider,
            api_key,
        } => {
            bearer(&client, &session_file)?
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
            bearer(&client, &session_file)?
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
            idempotency,
            request_id,
            org,
            actor,
        } => {
            let idempotency =
                idempotency.unwrap_or_else(|| format!("waveform-cli-{}", uuid::Uuid::new_v4()));
            let request_id = request_id.unwrap_or_else(uuid::Uuid::new_v4);
            let c = bearer(&client, &session_file)?
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
                        provider_order: list_arg(provider_order),
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
            idempotency,
            request_id,
            org,
            actor,
        } => {
            let idempotency =
                idempotency.unwrap_or_else(|| format!("waveform-cli-{}", uuid::Uuid::new_v4()));
            let request_id = request_id.unwrap_or_else(uuid::Uuid::new_v4);
            let c = bearer(&client, &session_file)?
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
                let created = bearer(&client, &session_file)?
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
                    write_local_test_key(&args.url, id, key)?;
                }
                print_json(&encoded)?;
            }
            TestEnvironmentCommand::List { org } => {
                if test_selector.is_some() {
                    return Err("test-env list must run without --test".into());
                }
                print_json(
                    &bearer(&client, &session_file)?
                        .test_environments(&org)
                        .await
                        .map_err(|e| e.to_string())?,
                )?;
            }
            TestEnvironmentCommand::Show {
                org,
                environment_id,
            } => print_json(
                &bearer(&client, &session_file)?
                    .test_environment_detail(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?,
            )?,
            TestEnvironmentCommand::Key {
                org,
                environment_id,
            } => {
                let value = bearer(&client, &session_file)?
                    .test_environment_key(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(key) = value.get("key").and_then(serde_json::Value::as_str) {
                    write_local_test_key(&args.url, &environment_id, key)?;
                }
                print_json(&value)?;
            }
            TestEnvironmentCommand::Rotate {
                org,
                environment_id,
            } => {
                let value = bearer(&client, &session_file)?
                    .rotate_test_environment_key(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(key) = value.get("key").and_then(serde_json::Value::as_str) {
                    write_local_test_key(&args.url, &environment_id, key)?;
                }
                print_json(&value)?;
            }
            TestEnvironmentCommand::Delete {
                org,
                environment_id,
            } => print_json(
                &bearer(&client, &session_file)?
                    .delete_test_environment(&org, &environment_id)
                    .await
                    .map_err(|e| e.to_string())?,
            )?,
            TestEnvironmentCommand::Restore {
                org,
                environment_id,
            } => print_json(
                &bearer(&client, &session_file)?
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
