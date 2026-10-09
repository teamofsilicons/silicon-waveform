//! Silicon Waveform API process entry point and operator commands.

#![forbid(unsafe_code)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::unwrap_used)]

use std::process::ExitCode;

use silicon_waveform::{config::Settings, infrastructure::runtime, operator};

const HELP: &str = "waveform-api: the Waveform speech service and its operator commands.

USAGE:
    waveform-api                      Serve the API (configuration from the environment and .env)
    waveform-api migrate              Apply the database migrations and exit
    waveform-api link-identities --file <mapping.csv> [--dry-run] [--merge] [--relink]
                                      Re-key IAM-era data to Silicon Accounts at cutover.
                                      The file has rows `iam_principal_id,accounts_uuid`.
                                      Prints a JSON report; exits 2 when a conflict stopped the run.
    waveform-api contracts deprecate <version> --successor <version>
                                      Deprecate an API version (it sunsets after 7 quiet days)
    waveform-api --help               Show this help

Operator commands need the same environment as the service (WAVEFORM_DATABASE_URL,
WAVEFORM_ENCRYPTION_KEY to re-seal saved provider keys, …).";

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        None => match runtime::run_from_env().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("waveform-api terminated: {error}");
                ExitCode::FAILURE
            }
        },
        Some("--help" | "-h" | "help") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("migrate") => with_pool(|_settings, _pool| async { Ok(ExitCode::SUCCESS) }).await,
        Some("link-identities") => link_identities(&arguments[1..]).await,
        Some("contracts") => contracts(&arguments[1..]).await,
        Some(other) => {
            eprintln!("waveform-api: unknown command `{other}`. Run `waveform-api --help`.");
            ExitCode::from(64)
        }
    }
}

async fn with_pool<F, Fut>(action: F) -> ExitCode
where
    F: FnOnce(Settings, sqlx::PgPool) -> Fut,
    Fut: Future<Output = Result<ExitCode, String>>,
{
    let _ = dotenvy::dotenv();
    let settings = match Settings::from_env() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("waveform-api: {error}");
            return ExitCode::FAILURE;
        }
    };
    let pool = match runtime::connect_and_migrate(&settings).await {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("waveform-api: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = action(settings, pool.clone()).await;
    pool.close().await;
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("waveform-api: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn link_identities(arguments: &[String]) -> ExitCode {
    let mut file = None;
    let mut options = operator::LinkOptions::default();
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--file" => file = iter.next().cloned(),
            "--dry-run" => options.dry_run = true,
            "--merge" => options.merge = true,
            "--relink" => options.relink = true,
            other => {
                eprintln!(
                    "waveform-api link-identities: unknown argument `{other}`. Run `waveform-api --help`."
                );
                return ExitCode::from(64);
            }
        }
    }
    let Some(file) = file else {
        eprintln!("waveform-api link-identities: --file <mapping.csv> is required.");
        return ExitCode::from(64);
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("waveform-api link-identities: cannot read {file}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let rows = match operator::parse_mapping(&text) {
        Ok(rows) => rows,
        Err(errors) => {
            for error in errors {
                eprintln!("waveform-api link-identities: {file} {error}");
            }
            return ExitCode::from(65);
        }
    };
    with_pool(|settings, pool| async move {
        let vault = settings
            .control
            .encryption_key
            .as_ref()
            .map(silicon_waveform::infrastructure::vault::Vault::new)
            .transpose()
            .map_err(|error| format!("WAVEFORM_ENCRYPTION_KEY: {error}"))?;
        let report = operator::link_identities(&pool, vault.as_ref(), &rows, options)
            .await
            .map_err(|error| format!("database error: {error}"))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
        Ok(if report.conflicts.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        })
    })
    .await
}

async fn contracts(arguments: &[String]) -> ExitCode {
    let (Some("deprecate"), Some(version), Some("--successor"), Some(successor), None) = (
        arguments.first().map(String::as_str),
        arguments.get(1),
        arguments.get(2).map(String::as_str),
        arguments.get(3),
        arguments.get(4),
    ) else {
        eprintln!(
            "waveform-api contracts: usage: waveform-api contracts deprecate <version> --successor <version>"
        );
        return ExitCode::from(64);
    };
    let (version, successor) = (version.clone(), successor.clone());
    with_pool(|_settings, pool| async move {
        silicon_waveform::control::contracts::deprecate(&pool, &version, &successor).await?;
        println!("{version} is deprecated; it sunsets after 7 days without requests now that {successor} is active.");
        Ok(ExitCode::SUCCESS)
    })
    .await
}
