//! Flags and commands of earlier versions that were removed. They are refused before anything
//! runs, with what replaced them, instead of clap's generic "unexpected argument".

use std::ffi::OsString;

use crate::output::{CliError, EXIT_INVALID};

/// Global options of this version that take a value in the next argument.
const VALUE_OPTIONS: [&str; 3] = ["--url", "--accounts-url", "--profile"];

fn refuse(code: &str, message: &str, hint: &str) -> CliError {
    CliError::new(EXIT_INVALID, code, message, hint)
}

fn flag_error(name: &str) -> Option<CliError> {
    let error = match name {
        "--org" | "--organization" | "--actor" => refuse(
            "organizations_removed",
            &format!(
                "{name} was removed: Waveform has no organizations, and everything you do belongs to the account you signed in as."
            ),
            "Drop the flag. A Carbon reads and manages a Silicon it looks after with --account si:<id> (jobs, preferences, provider-keys, storage, telemetry); nobody runs speech as another account.",
        ),
        "--test" | "--app-secret-file" => refuse(
            "test_environments_removed",
            &format!(
                "{name} was removed: test environments were retired, so there is no sandbox to select."
            ),
            "Test against a development Waveform: --url http://127.0.0.1:<port> with ACCOUNTS_URL set to its Silicon Accounts (see `waveform docs testing`).",
        ),
        "--obo-app" | "--obo-token-file" => refuse(
            "delegated_tokens_removed",
            &format!(
                "{name} was removed: apps acting for an account now send a Silicon Accounts proof instead of a delegated access token."
            ),
            "Pass a User verification proof issued for receiving_app waveform with --proof-file FILE (tts, stt, stt-folder, audio).",
        ),
        _ => return None,
    };
    Some(error)
}

fn command_error(command: &str, next: Option<&str>) -> Option<CliError> {
    let error = match (command, next) {
        ("test-env", _) => refuse(
            "test_environments_removed",
            "`waveform test-env` was removed: test environments were retired.",
            "Test against a development Waveform: --url http://127.0.0.1:<port> with ACCOUNTS_URL set to its Silicon Accounts (see `waveform docs testing`).",
        ),
        ("daemon", _) => refuse(
            "updater_removed",
            "`waveform daemon` was removed: Silicon Apps installs and updates waveform, so the CLI runs no updater of its own.",
            "Nothing to start or stop. Install or update with `silicon-apps install waveform`; Silicon Apps checks for updates every minute.",
        ),
        ("config", Some("auto-update")) => refuse(
            "updater_removed",
            "`waveform config auto-update` was removed: Silicon Apps keeps waveform up to date.",
            "Nothing to configure. Silicon Apps checks for updates every minute.",
        ),
        ("provider-key-set", _) => refuse(
            "command_renamed",
            "`waveform provider-key-set` is now `waveform provider-keys set`.",
            "waveform provider-keys set PROVIDER --key-file FILE (add --account si:<id> for a Silicon you look after).",
        ),
        ("provider-key-delete", _) => refuse(
            "command_renamed",
            "`waveform provider-key-delete` is now `waveform provider-keys delete`.",
            "waveform provider-keys delete PROVIDER (add --account si:<id> for a Silicon you look after).",
        ),
        ("storage", Some("start" | "complete")) => refuse(
            "command_renamed",
            "`waveform storage start` and `storage complete` were the earlier approval flow; there is no link or code any more.",
            "Run `waveform storage approve` signed in as the account itself; `waveform storage status` shows the result.",
        ),
        _ => return None,
    };
    Some(error)
}

/// Returns the refusal for the first retired flag or command in `argv`, if any.
pub fn check(argv: &[OsString]) -> Option<CliError> {
    let mut command: Option<String> = None;
    let mut subcommand_checked = false;
    let mut skip_value = false;
    for arg in argv.iter().skip(1) {
        let Some(arg) = arg.to_str() else {
            continue;
        };
        if arg == "--" {
            break;
        }
        if skip_value {
            skip_value = false;
            continue;
        }
        if let Some(name) = arg
            .strip_prefix("--")
            .map(|rest| rest.split('=').next().unwrap_or(rest))
        {
            let name = format!("--{name}");
            if let Some(error) = flag_error(&name) {
                return Some(error);
            }
            // Only this version's global value options consume the next argument (they may
            // appear anywhere); an unknown flag is left to clap.
            if VALUE_OPTIONS.contains(&name.as_str()) && !arg.contains('=') {
                skip_value = true;
            }
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        match &command {
            None => {
                if let Some(error) = command_error(arg, None) {
                    return Some(error);
                }
                command = Some(arg.to_owned());
            }
            // Only the word right after the command can be a retired subcommand; later
            // words are values, but the flags after them are still checked.
            Some(first) if !subcommand_checked => {
                subcommand_checked = true;
                if let Some(error) = command_error(first, Some(arg)) {
                    return Some(error);
                }
            }
            Some(_) => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::check;

    fn run(args: &[&str]) -> Option<String> {
        let argv: Vec<std::ffi::OsString> = std::iter::once("waveform")
            .chain(args.iter().copied())
            .map(Into::into)
            .collect();
        check(&argv).map(|error| error.code)
    }

    #[test]
    fn retired_flags_are_refused_with_what_replaced_them() {
        assert_eq!(
            run(&["tts", "Hi", "--org", "tos", "--actor", "si:x"]).as_deref(),
            Some("organizations_removed")
        );
        assert_eq!(
            run(&["--organization=tos", "jobs"]).as_deref(),
            Some("organizations_removed")
        );
        assert_eq!(
            run(&["--test", "ask_x", "tts", "Hi"]).as_deref(),
            Some("test_environments_removed")
        );
        assert_eq!(
            run(&["tts", "Hi", "--app-secret-file", "-"]).as_deref(),
            Some("test_environments_removed")
        );
        assert_eq!(
            run(&["tts", "Hi", "--obo-app", "ting", "--obo-token-file", "f"]).as_deref(),
            Some("delegated_tokens_removed")
        );
    }

    #[test]
    fn retired_commands_are_refused_but_the_same_words_as_values_are_not() {
        assert_eq!(
            run(&["test-env", "list"]).as_deref(),
            Some("test_environments_removed")
        );
        assert_eq!(
            run(&["--url", "http://127.0.0.1:1", "daemon", "status"]).as_deref(),
            Some("updater_removed")
        );
        assert_eq!(
            run(&["config", "auto-update", "off"]).as_deref(),
            Some("updater_removed")
        );
        assert_eq!(
            run(&["provider-key-set", "gemini"]).as_deref(),
            Some("command_renamed")
        );
        assert_eq!(
            run(&["storage", "start"]).as_deref(),
            Some("command_renamed")
        );
        // Text that happens to be a retired command's name is just text.
        assert_eq!(run(&["tts", "daemon"]), None);
        assert_eq!(run(&["tts", "test-env", "--voice-profile", "puck"]), None);
        assert_eq!(run(&["--profile", "daemon", "jobs"]), None);
        assert_eq!(run(&["jobs", "--profile", "daemon"]), None);
        assert_eq!(
            run(&[
                "config",
                "--url",
                "http://127.0.0.1:1",
                "auto-update",
                "off"
            ])
            .as_deref(),
            Some("updater_removed")
        );
        assert_eq!(run(&["config", "home", "auto-update"]), None);
        assert_eq!(run(&["tts", "--", "--org"]), None);
        assert_eq!(run(&["login", "status", "--json"]), None);
        assert_eq!(run(&["storage", "approve"]), None);
    }
}
