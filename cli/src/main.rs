//! The `waveform` CLI: text to speech and speech to text for Carbons and Silicons, signed in with
//! Silicon Accounts. State lives in a private `.waveform/dir` under `$SILICON_HOME` (or `~`).

mod args;
mod auth;
mod commands;
mod home;
mod output;
mod retired;
mod session;

use std::{ffi::OsString, process::ExitCode};

use clap::{Parser as _, error::ErrorKind};
use serde_json::json;

use args::{Args, Command, LoginCommand};
use auth::Ctx;
use output::{CliError, EXIT_INVALID, Outcome, Output};

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// True when `--json` appears among the options (before a `--` separator).
fn json_requested(argv: &[OsString]) -> bool {
    argv.iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json")
}

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let out = Output {
        json: json_requested(&argv),
        compact: false,
    };
    if let Some(error) = retired::check(&argv) {
        out.error(&error);
        return exit_code(error.exit);
    }
    let args = match Args::try_parse_from(&argv) {
        Ok(args) => args,
        Err(error) => return parse_failure(&error, out),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            out.error(&CliError::local(
                format!("Cannot start the async runtime: {error}."),
                "The machine is out of threads or memory; retry.",
            ));
            return exit_code(1);
        }
    };
    exit_code(runtime.block_on(run(args)))
}

/// Help and version go to stdout with exit 0; usage errors exit 2 (as JSON with `--json`).
fn parse_failure(error: &clap::Error, out: Output) -> ExitCode {
    let usage_error = !matches!(
        error.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );
    if usage_error && out.json {
        let rendered = error.render().to_string();
        let message = rendered
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("The arguments are not valid.")
            .trim_start_matches("error: ")
            .to_owned();
        out.error(&CliError::new(
            EXIT_INVALID,
            "invalid_arguments",
            message,
            "Run `waveform --help`, or `waveform <command> --help`, for the commands and their flags.",
        ));
        return exit_code(EXIT_INVALID);
    }
    let _ = error.print();
    exit_code(error.exit_code())
}

/// Environment variables of earlier versions that no longer do anything.
fn warn_retired_environment(out: Output) {
    if std::env::var_os("WAVEFORM_TEST").is_some_and(|value| !value.is_empty()) {
        out.warn("WAVEFORM_TEST is set but ignored: test environments were retired, so this command runs against the Waveform at --url / WAVEFORM_URL. Use a development Waveform for tests (waveform docs testing).");
    }
}

/// The telemetry label of a command: a fixed action name, never user input.
fn step(command: &Command) -> &'static str {
    match command {
        Command::Login(args) if args.command.is_some() => "login_status",
        Command::Login(_) => "login",
        Command::Logout => "logout",
        Command::Refresh => "refresh",
        Command::Tts(_) => "tts",
        Command::Stt(_) => "stt",
        Command::SttFolder(_) => "stt_folder",
        Command::Audio(_) => "held_audio",
        Command::Jobs(_) => "jobs",
        Command::Preferences(_) => "preferences",
        Command::ProviderKeys(_) => "provider_keys",
        Command::Storage(_) => "storage",
        Command::Telemetry(_) => "telemetry_preference",
        Command::Report(_) => "report",
        _ => "read_configuration",
    }
}

async fn run(args: Args) -> i32 {
    let mut out = Output {
        json: args.json,
        compact: false,
    };
    // Discovery and guides: offline, no state, always exit 0.
    match &args.command {
        Command::Accounts => {
            out.outcome(&commands::accounts(&args.url, &args.accounts_url));
            return 0;
        }
        Command::Iam => {
            // Transition alias for Silicon runtimes that still run `waveform iam --json`.
            let json = commands::accounts_json(&args.url, &args.accounts_url);
            output::print_stdout(&serde_json::to_string_pretty(&json).unwrap_or_default());
            return 0;
        }
        Command::Docs(docs) => {
            out.outcome(&commands::docs(&docs.topic));
            return 0;
        }
        _ => {}
    }
    warn_retired_environment(out);
    let status_json = matches!(
        &args.command,
        Command::Login(login) if login.command.is_some()
    ) && out.json;
    if matches!(&args.command, Command::Login(login) if login.command.is_none()) {
        // `login --json` streams progress lines; its result and errors are lines too.
        out.compact = true;
    }
    let profile_dir = match home::profile_dir(&args.profile) {
        Ok(dir) => dir,
        Err(error) => return finish(out, Err(error), status_json),
    };
    let ctx = Ctx {
        out,
        api_url: args.url.clone(),
        accounts_url: args.accounts_url.clone(),
        profile: args.profile.clone(),
        profile_dir,
    };
    #[cfg(unix)]
    let telemetry = (home::local_telemetry_enabled()
        && !matches!(args.command, Command::Config(_)))
    .then(silicon_waveform_client::telemetry::from_environment)
    .flatten();
    #[cfg(unix)]
    let started = std::time::Instant::now();
    let result = dispatch(&ctx, &args.command).await;
    #[cfg(unix)]
    if let Some(station) = &telemetry {
        silicon_waveform_client::telemetry::record(
            station,
            "cli",
            step(&args.command),
            result.is_ok(),
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
    }
    #[cfg(not(unix))]
    let _ = step(&args.command);
    finish(out, result, status_json)
}

/// Prints the result or the error and returns the exit code. `login status --json` always
/// exits 0: a failure is reported as `{"authenticated":false,…}` with the reason.
fn finish(out: Output, result: Result<Outcome, CliError>, status_json: bool) -> i32 {
    match result {
        Ok(outcome) => {
            out.outcome(&outcome);
            outcome.exit
        }
        Err(error) if status_json => {
            let mut json = json!({
                "authenticated": false,
                "reason": error.code,
                "message": error.message,
            });
            if let Some(hint) = &error.hint {
                json["hint"] = json!(hint);
            }
            out.outcome(&Outcome::new(json, ""));
            0
        }
        Err(error) => {
            out.error(&error);
            error.exit
        }
    }
}

async fn dispatch(ctx: &Ctx, command: &Command) -> Result<Outcome, CliError> {
    commands::check_stdin_sources(command)?;
    match command {
        Command::Login(login) => match &login.command {
            Some(LoginCommand::Status(status)) => auth::status(ctx, status).await,
            None => auth::login(ctx, login).await,
        },
        Command::Logout => auth::logout(ctx).await,
        Command::Refresh => auth::refresh(ctx).await,
        Command::Me => commands::me(ctx).await,
        Command::Silicons => commands::silicons(ctx).await,
        Command::Tts(args) => commands::tts(ctx, args).await,
        Command::Stt(args) => commands::stt(ctx, args).await,
        Command::SttFolder(args) => commands::stt_folder(ctx, args).await,
        Command::Audio(args) => commands::audio(ctx, args).await,
        Command::Jobs(args) => commands::jobs(ctx, args).await,
        Command::Preferences(args) => commands::preferences(ctx, args).await,
        Command::VoiceProfiles => commands::voice_profiles(ctx).await,
        Command::ProviderKeys(args) => commands::provider_keys(ctx, args).await,
        Command::Storage(args) => commands::storage(ctx, &args.command).await,
        Command::Telemetry(args) => commands::telemetry(ctx, args).await,
        Command::Report(args) => commands::report(ctx, args).await,
        Command::Capabilities => commands::capabilities(ctx).await,
        Command::Contracts => commands::contracts(ctx).await,
        Command::Config(args) => commands::config(ctx, &args.command),
        Command::Accounts | Command::Iam | Command::Docs(_) => {
            unreachable!("answered before any state is read")
        }
    }
}
