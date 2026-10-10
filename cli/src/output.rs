//! Printing and errors. Results go to stdout (text, or one JSON document with `--json`);
//! notices, progress and warnings go to stderr. Errors are `error:`/`hint:` lines on stderr,
//! or `{"error":{"code","message","hint"}}` on stdout with `--json`, so a script parsing stdout
//! always reads exactly one JSON document. Exit codes follow the Silicon Accounts CLI.

use std::io::{IsTerminal as _, Write as _};

use serde_json::{Value, json};

/// Success.
pub const EXIT_OK: i32 = 0;
/// A failure that is not one of the classes below.
pub const EXIT_FAILURE: i32 = 1;
/// Invalid arguments or input (usage errors, retired flags, a malformed file).
pub const EXIT_INVALID: i32 = 2;
/// Not signed in, sign-in ended, or the credential was refused (401/403).
pub const EXIT_AUTH: i32 = 3;
/// Not found (404).
pub const EXIT_NOT_FOUND: i32 = 4;
/// Conflict (409), e.g. an idempotency key reused for another request.
pub const EXIT_CONFLICT: i32 = 5;
/// Rate limited (429) or asked to wait.
pub const EXIT_RATE_LIMITED: i32 = 6;

/// A failure with what went wrong, why, and what to do next.
#[derive(Debug)]
pub struct CliError {
    pub exit: i32,
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub details: Option<Value>,
}

pub type CliResult<T> = Result<T, CliError>;

impl CliError {
    pub fn new(
        exit: i32,
        code: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        let hint = hint.into();
        Self {
            exit,
            code: code.into(),
            message: message.into(),
            hint: (!hint.is_empty()).then_some(hint),
            details: None,
        }
    }

    pub fn invalid(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(EXIT_INVALID, "invalid_input", message, hint)
    }

    pub fn local(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(EXIT_FAILURE, "local_state", message, hint)
    }

    pub fn to_json(&self) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(hint) = &self.hint {
            error["hint"] = json!(hint);
        }
        if let Some(details) = &self.details {
            error["details"] = details.clone();
        }
        json!({ "error": error })
    }
}

impl From<silicon_waveform_client::Error> for CliError {
    fn from(error: silicon_waveform_client::Error) -> Self {
        let exit = match error.status() {
            Some(400 | 406 | 410 | 413 | 415 | 422) => EXIT_INVALID,
            Some(401 | 403) => EXIT_AUTH,
            Some(404) => EXIT_NOT_FOUND,
            Some(409) => EXIT_CONFLICT,
            Some(423 | 429) => EXIT_RATE_LIMITED,
            Some(_) => EXIT_FAILURE,
            None if error.is_code("invalid_input") => EXIT_INVALID,
            None => EXIT_FAILURE,
        };
        let mut details = serde_json::Map::new();
        if let Some(api) = error.as_api() {
            details.insert("status".into(), json!(api.status));
            for (key, value) in [
                ("reason", &api.reason),
                ("request_id", &api.request_id),
                ("provider", &api.provider),
            ] {
                if let Some(value) = value {
                    details.insert(key.into(), json!(value));
                }
            }
            if let Some(status) = api.provider_status {
                details.insert("provider_status".into(), json!(status));
            }
            if let Some(seconds) = api.retry_after_seconds {
                details.insert("retry_after_seconds".into(), json!(seconds));
            }
        }
        Self {
            exit,
            code: error.code().to_owned(),
            message: error.message(),
            hint: error.hint().map(str::to_owned),
            details: (!details.is_empty()).then_some(Value::Object(details)),
        }
    }
}

impl From<silicon_waveform_client::signin::SignInError> for CliError {
    fn from(error: silicon_waveform_client::signin::SignInError) -> Self {
        use silicon_waveform_client::signin::SignInError as E;
        let exit = match &error {
            E::Configuration { .. } => EXIT_INVALID,
            E::SltRefused { .. }
            | E::SignInEnded { .. }
            | E::DeviceDenied { .. }
            | E::DeviceExpired { .. }
            | E::NotEnabled { .. } => EXIT_AUTH,
            E::Accounts {
                status: Some(429), ..
            } => EXIT_RATE_LIMITED,
            _ => EXIT_FAILURE,
        };
        Self::new(exit, error.code(), error.message(), error.hint())
    }
}

/// What a command produced.
pub struct Outcome {
    /// Printed with `--json`.
    pub json: Value,
    /// Printed in text mode (stdout).
    pub text: String,
    /// Exit code (0 unless the command reports a negative result).
    pub exit: i32,
    /// With `--json`, print one compact line (the last event of `login --json`'s stream).
    pub compact: bool,
    /// Print nothing on stdout (the command already wrote its result there, e.g. MP3 bytes).
    pub silent: bool,
}

impl Outcome {
    pub fn new(json: Value, text: impl Into<String>) -> Self {
        Self {
            json,
            text: text.into(),
            exit: EXIT_OK,
            compact: false,
            silent: false,
        }
    }

    /// A JSON result whose text form is the same JSON, pretty-printed.
    pub fn data(json: Value) -> Self {
        let text = serde_json::to_string_pretty(&json).unwrap_or_default();
        Self::new(json, text)
    }

    pub fn exit(mut self, code: i32) -> Self {
        self.exit = code;
        self
    }
}

/// Output settings.
#[derive(Clone, Copy, Debug)]
pub struct Output {
    pub json: bool,
    /// Print JSON errors on one line (inside `login --json`'s line stream).
    pub compact: bool,
}

impl Output {
    fn color(self) -> bool {
        std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
    }

    pub fn outcome(self, outcome: &Outcome) {
        if outcome.silent {
            return;
        }
        if self.json && outcome.compact {
            print_stdout(&outcome.json.to_string());
        } else if self.json {
            print_stdout(&serde_json::to_string_pretty(&outcome.json).unwrap_or_default());
        } else if !outcome.text.is_empty() {
            print_stdout(outcome.text.trim_end_matches('\n'));
        }
    }

    /// One compact JSON line on stdout (progress events of `login --json`).
    pub fn json_line(self, value: &Value) {
        print_stdout(&value.to_string());
    }

    pub fn error(self, error: &CliError) {
        if self.json && self.compact {
            print_stdout(&error.to_json().to_string());
            return;
        }
        if self.json {
            print_stdout(&serde_json::to_string_pretty(&error.to_json()).unwrap_or_default());
            return;
        }
        let (error_label, hint_label) = if self.color() {
            ("\x1b[1;31merror:\x1b[0m", "\x1b[1mhint:\x1b[0m")
        } else {
            ("error:", "hint:")
        };
        let mut text = format!("{error_label} {}\n", error.message);
        if let Some(hint) = &error.hint {
            text.push_str(&format!("{hint_label} {hint}\n"));
        }
        if let Some(id) = error
            .details
            .as_ref()
            .and_then(|details| details.get("request_id"))
            .and_then(Value::as_str)
        {
            text.push_str(&format!("request id: {id}\n"));
        }
        stderr(&text);
    }

    /// A notice on stderr (text mode only).
    pub fn notice(self, text: &str) {
        if !self.json {
            stderr(&format!("{text}\n"));
        }
    }

    /// A warning on stderr (as a JSON line in JSON mode).
    pub fn warn(self, text: &str) {
        if self.json {
            stderr(&format!("{}\n", json!({ "warning": text })));
        } else {
            let label = if self.color() {
                "\x1b[1;33mwarning:\x1b[0m"
            } else {
                "warning:"
            };
            stderr(&format!("{label} {text}\n"));
        }
    }
}

/// Writes to stdout; a closed pipe (e.g. `| head`) is not an error worth a panic.
pub fn print_stdout(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

pub fn stderr(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}
