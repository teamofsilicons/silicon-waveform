//! The command tree. Every command says what it is for, how it combines with others, and its
//! flags; `waveform <command> --help` goes one level deeper.

use clap::{Args as ClapArgs, Parser, Subcommand};
use std::path::PathBuf;

const AFTER_HELP: &str = "\
Start here:
  waveform login                                   Carbon: approve a code in the browser
  silicon-accounts login --app waveform -q | waveform login --slt-stdin
                                                   Silicon: sign in with a short-lived token
  waveform login status --json                     who is signed in
  waveform tts 'Hello, Carbons and Silicons.'      speak; the MP3 lands in your Briefcase files

Every command accepts --help. Guides: waveform docs [start|cli|api|client|accounts|testing|configuration]
Docs: https://docs.waveform.teamofsilicons.com   Source: https://github.com/teamofsilicons/silicon-waveform
Rust package: https://crates.io/crates/silicon-waveform-client";

/// Silicon Waveform: text to speech and speech to text for Carbons and Silicons.
///
/// Waveform turns text into speech (Gemini, then ElevenLabs, then OpenAI) and transcribes audio in
/// your Briefcase files (Gemini, then gpt-transcribe, then Deepgram). Everything you do belongs to
/// the account you signed in as: your history, settings, provider keys and audio. A Carbon can read
/// and manage the Silicons it looks after with --account si:<id>; nobody speaks as another account.
///
/// Sign in with Silicon Accounts first (`waveform login`), then use the speech commands.
#[derive(Parser, Debug)]
#[command(
    name = "waveform",
    bin_name = "waveform",
    version,
    arg_required_else_help = true,
    after_help = AFTER_HELP,
    after_long_help = include_str!("../README.md")
)]
pub struct Args {
    /// Waveform API origin (https, or http for this machine only).
    #[arg(
        long,
        global = true,
        env = "WAVEFORM_URL",
        default_value = silicon_waveform_client::DEFAULT_API_URL,
        value_name = "URL"
    )]
    pub url: String,
    /// Silicon Accounts origin used to sign in and refresh (https, or http for this machine only).
    #[arg(
        long,
        global = true,
        env = "ACCOUNTS_URL",
        default_value = silicon_waveform_client::DEFAULT_ACCOUNTS_URL,
        value_name = "URL"
    )]
    pub accounts_url: String,
    /// Which saved sign-in to use. Keep one profile per account on a shared machine.
    #[arg(
        long,
        global = true,
        env = "WAVEFORM_PROFILE",
        default_value = "default",
        value_name = "NAME"
    )]
    pub profile: String,
    /// Print results as JSON on stdout (errors become {"error":{code,message,hint}}).
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sign in to Waveform, or check the sign-in with `login status`.
    ///
    /// Carbons: `waveform login` prints a code and a page; approve it in any browser where you are
    /// signed in to Silicon Accounts, and the CLI finishes by itself (the code lasts 10 minutes).
    /// Silicons never see a page: mint a short-lived token and pipe it in:
    /// `silicon-accounts login --app waveform -q | waveform login --slt-stdin`.
    /// The sign-in is saved privately under $SILICON_HOME/.waveform/dir (or ~/.waveform/dir) and
    /// refreshed automatically; `waveform logout` ends it.
    #[command(
        args_conflicts_with_subcommands = true,
        after_help = "Examples:\n  waveform login\n  waveform login --open\n  silicon-accounts login --app waveform -q | waveform login --slt-stdin\n  waveform login --slt slt_…\n  waveform login status --json"
    )]
    Login(LoginArgs),
    /// Sign out: end this sign-in at Silicon Accounts and delete it from this machine.
    ///
    /// Other machines and the website stay signed in. Sign in again with `waveform login`.
    Logout,
    /// Print Waveform's app id and where to sign in. Offline; always exits 0.
    ///
    /// Use it to discover how to sign in: the app id is what `silicon-accounts login --app <id>`
    /// needs. With --json: {"app_id":"waveform","accounts_url":…,"api_url":…,"version":…}.
    Accounts,
    /// Refresh the saved sign-in now (normally automatic, 60 seconds before it expires).
    Refresh,
    /// Show the signed-in account as Waveform sees it (and a Silicon's custodian).
    Me,
    /// List the Silicons you look after that Waveform knows (Carbons; empty for a Silicon).
    ///
    /// Pass one of their ids to --account on jobs, preferences, provider-keys and storage to
    /// manage that Silicon's Waveform data.
    Silicons,
    /// Convert text to speech and store the MP3 in your Briefcase files.
    ///
    /// Runs one provider by default (your provider order's first, or --provider); add
    /// --auto-fallback to try the others after a failure. The answer has `file_url` (your
    /// Briefcase files) or, when Waveform could not store it there, `audio_url` and
    /// `storage.reason`: download that audio with `waveform audio <request_id>`. A request id is
    /// printed on stderr first so you can follow it with `waveform jobs --job-id <id> --wait`.
    #[command(
        after_help = "Examples:\n  waveform tts 'Hello, Carbons and Silicons.'\n  waveform tts --text-file note.txt --voice-profile puck --lang en\n  waveform tts 'Welcome home.' --provider elevenlabs --provider-options '{\"elevenlabs\":{\"stability\":0.35}}'\n  waveform tts 'Hi' --provider-key-file gemini=/private/gemini.key\n  waveform tts 'Hi' --proof-file /private/proof     (an app acting for an account)"
    )]
    Tts(TtsArgs),
    /// Transcribe an audio file in your Briefcase files.
    ///
    /// Pass the file's permanent Briefcase URL. Waveform reads it with the Briefcase access you
    /// gave it (`waveform storage approve`); upload sources to your Waveform folder
    /// (`waveform stt-folder`). Providers fall back automatically.
    #[command(
        after_help = "Examples:\n  waveform stt https://briefcase.teamofsilicons.com/<you>/apps/waveform/note.mp3\n  waveform stt FILE_URL --language en --provider-order deepgram"
    )]
    Stt(SttArgs),
    /// Show (and create) your Waveform folder in Briefcase, where STT sources are uploaded.
    SttFolder(ProofArg),
    /// Download an MP3 that Waveform holds for you (when TTS answered `storage.status: held`).
    #[command(
        after_help = "Examples:\n  waveform audio 0192b9f0-…-… --output speech.mp3\n  waveform audio 0192b9f0-…-… --output - > speech.mp3"
    )]
    Audio(AudioArgs),
    /// List your speech history, or follow one job with --job-id (and --wait).
    ///
    /// History is content-free: operation, length, first line, status, provider. A Carbon reads a
    /// Silicon's history with --account si:<id>.
    #[command(
        after_help = "Examples:\n  waveform jobs --operation tts --limit 10\n  waveform jobs --job-id 0192b9f0-… --wait\n  waveform jobs --account si:scout"
    )]
    Jobs(JobsArgs),
    /// Show or change your provider order and default voice.
    #[command(
        after_help = "Examples:\n  waveform preferences\n  waveform preferences --voice-profile puck --tts-order elevenlabs,gemini\n  waveform preferences --account si:scout --stt-order deepgram"
    )]
    Preferences(PreferencesArgs),
    /// List the voice profiles and how each maps to Gemini, ElevenLabs and OpenAI voices.
    VoiceProfiles,
    /// List, save or delete your own provider API keys (values are never shown).
    #[command(args_conflicts_with_subcommands = true)]
    ProviderKeys(ProviderKeysArgs),
    /// Give, check or revoke Waveform's access to your Briefcase files.
    ///
    /// Waveform stores your speech and reads your STT sources in your Briefcase files with a
    /// proof Silicon Accounts issues for you. Your first speech request gives it automatically;
    /// `approve` gives it now, `revoke` takes it back (speech is then held by Waveform).
    Storage(StorageArgs),
    /// Retired: Waveform records no telemetry. Accepted so older scripts keep working; does nothing.
    #[command(hide = true)]
    Telemetry(TelemetryArgs),
    /// Report a bug to the Waveform maintainers, optionally with the pull request that fixes it.
    #[command(
        after_help = "Examples:\n  waveform report 'tts --provider openai fails with 502 for any text; request id …'\n  waveform report 'Fix provider order parsing' --pr https://github.com/teamofsilicons/silicon-waveform/pull/123"
    )]
    Report(ReportArgs),
    /// Show supported languages, formats and provider controls. No sign-in needed.
    Capabilities,
    /// Show API versions, compatibility and deprecation policy. No sign-in needed.
    Contracts,
    /// Read the bundled guides.
    Docs(DocsArgs),
    /// Local CLI settings: where state is kept.
    Config(ConfigArgs),
    /// Transition alias of `accounts --json` for older Silicon runtimes.
    #[command(hide = true)]
    Iam,
}

#[derive(ClapArgs, Debug)]
pub struct LoginArgs {
    #[command(subcommand)]
    pub command: Option<LoginCommand>,
    /// A short-lived token as an argument; same as --slt.
    #[arg(value_name = "SLT")]
    pub slt_argument: Option<String>,
    /// Exchange this short-lived token (slt_…, from `silicon-accounts login --app waveform -q`).
    #[arg(long, value_name = "SLT", conflicts_with_all = ["slt_stdin", "slt_argument"])]
    pub slt: Option<String>,
    /// Read the short-lived token from stdin (recommended: it never appears in the process list).
    #[arg(long, conflicts_with = "slt_argument")]
    pub slt_stdin: bool,
    /// Open the approval page in the default browser (device flow only).
    #[arg(long)]
    pub open: bool,
    /// Name for this sign-in on the account site (device flow); default "waveform CLI".
    #[arg(long, value_name = "TEXT")]
    pub label: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum LoginCommand {
    /// Show whether this machine is signed in to Waveform, and as whom.
    ///
    /// Checks the saved sign-in with Waveform (refreshing it first when needed) unless --offline.
    /// With --json it always exits 0: {"authenticated":false} when signed out, else
    /// {"authenticated":true,"uuid","id","kind","display_name","expires_at","refresh_expires_at","verified",…}.
    /// Without --json it exits 1 when signed out.
    Status(StatusArgs),
}

#[derive(ClapArgs, Debug)]
pub struct StatusArgs {
    /// Only read the saved sign-in; no network (`verified` is then false).
    #[arg(long)]
    pub offline: bool,
}

#[derive(ClapArgs, Debug)]
pub struct ProofArg {
    /// Act for an account with a User verification proof another app holds (sap_… in FILE, - for stdin).
    #[arg(long, value_name = "FILE")]
    pub proof_file: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct TtsArgs {
    /// Text to speak (1-4096 characters). Or use --text-file.
    #[arg(required_unless_present = "text_file", conflicts_with = "text_file")]
    pub text: Option<String>,
    /// Read the text from a file (- for stdin).
    #[arg(long, value_name = "FILE")]
    pub text_file: Option<PathBuf>,
    /// Voice profile for this request (see `waveform voice-profiles`); default: your default voice.
    #[arg(long, value_name = "ID")]
    pub voice_profile: Option<String>,
    /// BCP 47 language hint, passed to Gemini only (e.g. en, hi, fr).
    #[arg(long, value_name = "TAG")]
    pub lang: Option<String>,
    /// Providers to prefer, in order, comma-separated (gemini, elevenlabs, openai).
    #[arg(long, value_name = "LIST", conflicts_with = "provider")]
    pub provider_order: Option<String>,
    /// The one provider to use. Without --auto-fallback only this provider runs.
    #[arg(long, value_parser = ["gemini", "elevenlabs", "openai"])]
    pub provider: Option<String>,
    /// Try the remaining providers after a failure (off by default; not with provider controls).
    #[arg(long, conflicts_with_all = ["provider_options", "provider_options_file"])]
    pub auto_fallback: bool,
    /// Provider controls as JSON, e.g. {"gemini":{"scene":"A quiet room"}} (see `waveform docs cli`).
    #[arg(long, value_name = "JSON", conflicts_with = "provider_options_file")]
    pub provider_options: Option<String>,
    /// Read provider controls JSON from a file (- for stdin).
    #[arg(long, value_name = "FILE")]
    pub provider_options_file: Option<PathBuf>,
    /// A key for one provider, for this request only (PROVIDER=FILE, - for stdin); repeatable.
    #[arg(long, value_name = "PROVIDER=FILE")]
    pub provider_key_file: Vec<String>,
    /// Retry key: repeat a request with the same key to get the same result without paying twice.
    #[arg(long, value_name = "KEY")]
    pub idempotency: Option<String>,
    /// Job id to use, so another process can follow it with `waveform jobs --job-id`.
    #[arg(long, value_name = "UUID")]
    pub request_id: Option<uuid::Uuid>,
    #[command(flatten)]
    pub proof: ProofArg,
}

#[derive(ClapArgs, Debug)]
pub struct SttArgs {
    /// Permanent Briefcase URL of an audio file in your Briefcase files.
    pub file_url: String,
    /// BCP 47 language hint.
    #[arg(long, value_name = "TAG")]
    pub language: Option<String>,
    /// Providers to prefer, in order, comma-separated (gemini, openai, deepgram).
    #[arg(long, value_name = "LIST")]
    pub provider_order: Option<String>,
    /// A key for one provider, for this request only (PROVIDER=FILE, - for stdin); repeatable.
    #[arg(long, value_name = "PROVIDER=FILE")]
    pub provider_key_file: Vec<String>,
    /// Retry key: repeat a request with the same key to get the same result.
    #[arg(long, value_name = "KEY")]
    pub idempotency: Option<String>,
    /// Job id to use, so another process can follow it with `waveform jobs --job-id`.
    #[arg(long, value_name = "UUID")]
    pub request_id: Option<uuid::Uuid>,
    #[command(flatten)]
    pub proof: ProofArg,
}

#[derive(ClapArgs, Debug)]
pub struct AudioArgs {
    /// The request id of the speech (TTS `request_id`).
    pub request_id: String,
    /// Where to save it (- writes the MP3 to stdout); default: the name Waveform suggests.
    #[arg(long, short = 'o', value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Replace the output file if it exists.
    #[arg(long)]
    pub force: bool,
    #[command(flatten)]
    pub proof: ProofArg,
}

#[derive(ClapArgs, Debug)]
pub struct AccountArg {
    /// Act on a Silicon you look after (si:<id> or its uuid) instead of yourself.
    #[arg(long, value_name = "ID")]
    pub account: Option<String>,
}

#[derive(ClapArgs, Debug)]
pub struct JobsArgs {
    /// Only tts or stt jobs.
    #[arg(long, value_parser = ["tts", "stt"])]
    pub operation: Option<String>,
    /// Show one job instead of a page.
    #[arg(long, value_name = "UUID")]
    pub job_id: Option<String>,
    /// Jobs per page (1-100, default 50).
    #[arg(long)]
    pub limit: Option<u32>,
    /// Continue after a page: its `next_cursor`.
    #[arg(long)]
    pub cursor: Option<String>,
    /// With --job-id: poll until the job completes or fails.
    #[arg(long, requires = "job_id")]
    pub wait: bool,
    /// With --wait: give up after this many milliseconds.
    #[arg(long, default_value_t = 60_000)]
    pub timeout_ms: u64,
    /// With --wait: milliseconds between polls.
    #[arg(long, default_value_t = 500)]
    pub poll_ms: u64,
    #[command(flatten)]
    pub account: AccountArg,
}

#[derive(ClapArgs, Debug)]
pub struct PreferencesArgs {
    /// Set the default voice profile.
    #[arg(long, value_name = "ID")]
    pub voice_profile: Option<String>,
    /// Set the TTS order: preferred providers first, comma-separated (gemini, elevenlabs, openai).
    #[arg(long, value_name = "LIST")]
    pub tts_order: Option<String>,
    /// Set the STT order: preferred providers first, comma-separated (gemini, openai, deepgram).
    #[arg(long, value_name = "LIST")]
    pub stt_order: Option<String>,
    #[command(flatten)]
    pub account: AccountArg,
}

#[derive(ClapArgs, Debug)]
pub struct ProviderKeysArgs {
    #[command(subcommand)]
    pub command: Option<ProviderKeysCommand>,
    /// Act on a Silicon you look after (si:<id> or its uuid) instead of yourself.
    #[arg(long, value_name = "ID", global = true)]
    pub account: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ProviderKeysCommand {
    /// List the providers with a saved key (the default).
    List,
    /// Save (replace) the key for one provider. It is encrypted and never shown again.
    #[command(
        after_help = "Examples:\n  waveform provider-keys set gemini --key-file /private/gemini.key\n  printf %s \"$KEY\" | waveform provider-keys set openai --key-file -"
    )]
    Set {
        /// gemini, elevenlabs, openai or deepgram.
        #[arg(value_parser = ["gemini", "elevenlabs", "openai", "deepgram"])]
        provider: String,
        /// The key as an argument (visible in the process list; prefer --key-file).
        #[arg(required_unless_present = "key_file", conflicts_with = "key_file")]
        api_key: Option<String>,
        /// Read the key from a file (- for stdin).
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
    },
    /// Delete the saved key for one provider (Waveform's own key is used again).
    Delete {
        /// gemini, elevenlabs, openai or deepgram.
        #[arg(value_parser = ["gemini", "elevenlabs", "openai", "deepgram"])]
        provider: String,
    },
}

#[derive(ClapArgs, Debug)]
pub struct StorageArgs {
    #[command(subcommand)]
    pub command: StorageCommand,
}

#[derive(Subcommand, Debug)]
pub enum StorageCommand {
    /// Give Waveform Briefcase access for your own account now (only the account itself can).
    Approve,
    /// Show whether Waveform has Briefcase access (active, revoked or missing).
    Status(AccountArg),
    /// Revoke Waveform's Briefcase access (yours, or a Silicon's you look after).
    Revoke(AccountArg),
}

#[derive(ClapArgs, Debug)]
pub struct TelemetryArgs {
    /// on or off (ignored).
    #[arg(value_parser = ["on", "off"])]
    pub enabled: String,
    #[command(flatten)]
    pub account: AccountArg,
}

#[derive(ClapArgs, Debug)]
pub struct ReportArgs {
    /// What happened, what you expected, and how to reproduce it. Never include tokens or keys.
    pub message: String,
    /// The pull request that fixes it: https://github.com/teamofsilicons/silicon-waveform/pull/<n>.
    #[arg(long, value_name = "URL")]
    pub pr: Option<String>,
    /// Retry key: repeat with the same key if you are unsure the report arrived.
    #[arg(long, value_name = "KEY")]
    pub idempotency: Option<String>,
}

#[derive(ClapArgs, Debug)]
pub struct DocsArgs {
    /// Which guide.
    #[arg(
        default_value = "start",
        value_parser = ["start", "cli", "api", "client", "accounts", "testing", "configuration"]
    )]
    pub topic: String,
}

#[derive(ClapArgs, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show the effective settings: state directory, URLs, profile.
    Show,
    /// Keep Waveform's state under another existing directory (<DIR>/.waveform/dir).
    Home {
        /// An existing directory.
        location: PathBuf,
    },
    /// Retired: Waveform records no telemetry. Accepted so older scripts keep working; does nothing.
    #[command(hide = true)]
    Telemetry {
        /// on or off (ignored).
        #[arg(value_parser = ["on", "off"])]
        enabled: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{Args, Command, LoginCommand, ProviderKeysCommand, StorageCommand};
    use clap::{CommandFactory as _, Parser as _};

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("waveform").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_tree_is_well_formed() {
        Args::command().debug_assert();
    }

    #[test]
    fn tts_runs_one_provider_by_default_and_controls_exclude_fallback() {
        let Command::Tts(tts) = parse(&["tts", "Hello"]).unwrap().command else {
            panic!("tts")
        };
        assert!(!tts.auto_fallback && tts.proof.proof_file.is_none());
        assert!(
            parse(&[
                "tts",
                "Hi",
                "--provider",
                "elevenlabs",
                "--provider-options",
                "{}"
            ])
            .is_ok()
        );
        assert!(
            parse(&[
                "tts",
                "Hi",
                "--provider",
                "gemini",
                "--provider-order",
                "openai"
            ])
            .is_err()
        );
        assert!(parse(&["tts", "Hi", "--auto-fallback", "--provider-options", "{}"]).is_err());
        assert!(
            parse(&[
                "tts",
                "Hi",
                "--auto-fallback",
                "--provider-options-file",
                "c.json"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "tts",
                "Hi",
                "--provider-options",
                "{}",
                "--provider-options-file",
                "c.json"
            ])
            .is_err()
        );
        assert!(
            parse(&["tts", "Hi", "--provider", "deepgram"]).is_err(),
            "deepgram does not speak"
        );
        assert!(
            parse(&["tts", "Hi", "--text-file", "t.txt"]).is_err(),
            "text or a file, not both"
        );
        assert!(parse(&["tts"]).is_err(), "text is required");
        assert!(parse(&["tts", "--text-file", "-"]).is_ok());
    }

    #[test]
    fn login_takes_one_short_lived_token_source() {
        let login = |args: &[&str]| match parse(args).map(|args| args.command) {
            Ok(Command::Login(login)) => Ok(login),
            Ok(_) => panic!("login"),
            Err(error) => Err(error),
        };
        assert!(login(&["login"]).unwrap().slt.is_none());
        assert_eq!(
            login(&["login", "slt_x"]).unwrap().slt_argument.as_deref(),
            Some("slt_x")
        );
        assert_eq!(
            login(&["login", "--slt", "slt_x"]).unwrap().slt.as_deref(),
            Some("slt_x")
        );
        assert!(login(&["login", "--slt-stdin"]).unwrap().slt_stdin);
        assert!(login(&["login", "--slt", "a", "--slt-stdin"]).is_err());
        assert!(login(&["login", "slt_a", "--slt", "slt_b"]).is_err());
        assert!(matches!(
            login(&["login", "status", "--offline"]).unwrap().command,
            Some(LoginCommand::Status(status)) if status.offline
        ));
    }

    #[test]
    fn account_selection_and_subcommands_parse_where_they_belong() {
        assert!(matches!(
            parse(&["provider-keys", "--account", "si:scout"]).unwrap().command,
            Command::ProviderKeys(keys) if keys.command.is_none() && keys.account.as_deref() == Some("si:scout")
        ));
        assert!(matches!(
            parse(&["provider-keys", "set", "gemini", "--key-file", "-", "--account", "si:scout"]).unwrap().command,
            Command::ProviderKeys(keys) if matches!(keys.command, Some(ProviderKeysCommand::Set { .. }))
        ));
        assert!(
            parse(&["provider-keys", "set", "gemini"]).is_err(),
            "a key or --key-file"
        );
        assert!(parse(&["provider-keys", "set", "nope", "--key-file", "k"]).is_err());
        assert!(matches!(
            parse(&["storage", "revoke", "--account", "si:scout"]).unwrap().command,
            Command::Storage(storage) if matches!(storage.command, StorageCommand::Revoke(_))
        ));
        assert!(parse(&["storage", "approve", "--account", "si:scout"]).is_err());
        assert!(
            parse(&["jobs", "--wait"]).is_err(),
            "--wait follows one job"
        );
        assert!(parse(&["jobs", "--job-id", "x", "--wait", "--account", "si:scout"]).is_ok());
        assert!(parse(&["telemetry", "maybe"]).is_err());
        assert!(
            parse(&["docs", "iam"]).is_err(),
            "the earlier guide is gone"
        );
        assert!(parse(&["--json", "--profile", "work", "me"]).is_ok());
        assert!(matches!(parse(&["iam"]).unwrap().command, Command::Iam));
    }
}
