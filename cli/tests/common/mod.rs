//! Shared harness: run the real `waveform` binary in a private, empty home against a stub
//! Silicon Accounts and a stub Waveform (wiremock).
#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    fs,
    io::Write as _,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

/// A private home directory, removed when dropped.
pub struct Home {
    pub root: PathBuf,
}

impl Home {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!("waveform-cli-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    /// `<home>/.waveform/dir`, the default profile's state directory.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(".waveform").join("dir")
    }

    /// A command with a clean environment: only this home, no colours, fast device polls.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_waveform"));
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("SILICON_HOME", &self.root)
            .env("NO_COLOR", "1")
            .env("WAVEFORM_TEST_POLL_UNIT_MS", "10");
        // Windows networking and TLS load system components through SystemRoot.
        // Retain only OS runtime paths, never inherited app credentials/configuration.
        #[cfg(windows)]
        for name in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    pub async fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        self.run_with(self.command(), args, stdin).await
    }

    pub async fn run_with(
        &self,
        mut command: Command,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Output {
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let input = stdin.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            let mut child = command.spawn().unwrap();
            let mut pipe = child.stdin.take().unwrap();
            if let Some(input) = input {
                pipe.write_all(input.as_bytes()).unwrap();
            }
            drop(pipe);
            child.wait_with_output().unwrap()
        })
        .await
        .unwrap()
    }

    /// The saved sign-in files (`accounts-session-*.json`) of the default profile.
    pub fn session_files(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.state_dir()) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("accounts-session-") && name.ends_with(".json")
                    })
            })
            .collect()
    }

    pub fn session(&self) -> Value {
        let files = self.session_files();
        assert_eq!(files.len(), 1, "exactly one saved sign-in: {files:?}");
        serde_json::from_slice(&fs::read(&files[0]).unwrap()).unwrap()
    }

    /// Every byte the CLI wrote under the home, for secret scans.
    pub fn all_files_text(&self) -> String {
        fn walk(dir: &std::path::Path, text: &mut String) {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, text);
                } else if let Ok(bytes) = fs::read(&path) {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        }
        let mut text = String::new();
        walk(&self.root, &mut text);
        text
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A stub Silicon Accounts and a stub Waveform.
pub struct Stubs {
    pub accounts: MockServer,
    pub waveform: MockServer,
}

impl Stubs {
    pub async fn start() -> Self {
        Self {
            accounts: MockServer::start().await,
            waveform: MockServer::start().await,
        }
    }

    /// `--url … --accounts-url …` followed by `args`.
    pub fn args<'a>(&'a self, args: &[&'a str]) -> Vec<String> {
        let mut all = vec![
            "--url".to_owned(),
            self.waveform.uri(),
            "--accounts-url".to_owned(),
            self.accounts.uri(),
        ];
        all.extend(args.iter().map(|arg| (*arg).to_owned()));
        all
    }

    pub async fn run(&self, home: &Home, args: &[&str], stdin: Option<&str>) -> Output {
        let all = self.args(args);
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        home.run(&refs, stdin).await
    }

    pub async fn accounts_requests(&self, path_prefix: &str) -> Vec<wiremock::Request> {
        self.accounts
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.url.path().starts_with(path_prefix))
            .collect()
    }

    pub async fn waveform_requests(&self) -> Vec<wiremock::Request> {
        self.waveform.received_requests().await.unwrap_or_default()
    }
}

pub fn account(kind: &str) -> Value {
    if kind == "silicon" {
        json!({"uuid":"K1E","membership_id":"waveform:K1E","kind":"silicon","id":"si:scout",
            "display_name":"Scout","pfp_url":"","custodian":{"uuid":"aB1","id":"c:ada"}})
    } else {
        json!({"uuid":"aB1","membership_id":"waveform:aB1","kind":"carbon","id":"c:ada",
            "display_name":"Ada","pfp_url":"https://accounts.test/pfp/aB1.png"})
    }
}

/// A Silicon Accounts token answer for `waveform`.
pub fn tokens(access: &str, refresh: &str, kind: &str, expires_in: u64) -> Value {
    json!({
        "access_token": access, "token_type": "Bearer", "expires_in": expires_in,
        "refresh_token": refresh, "refresh_token_expires_at": "2099-01-01T00:00:00.000Z",
        "scope": "profile", "membership_id": format!("waveform:{}", account(kind)["uuid"].as_str().unwrap()),
        "account": account(kind),
    })
}

pub fn oauth_error(error: &str, description: &str) -> ResponseTemplate {
    ResponseTemplate::new(400)
        .set_body_json(json!({"error": error, "error_description": description}))
}

/// Waveform's `GET /api/v1/me` answer for an account kind.
pub fn me(kind: &str) -> Value {
    let mut account = account(kind);
    let object = account.as_object_mut().unwrap();
    object.remove("membership_id");
    json!({"authenticated":true,"app_id":"waveform","account":account,"via":"bearer",
        "expires_at":"2099-01-01T00:00:00Z"})
}

/// Answers `GET /api/v1/me` for one access token.
pub async fn mount_me(stubs: &Stubs, access: &str, kind: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .and(header("authorization", format!("Bearer {access}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(me(kind)))
        .mount(&stubs.waveform)
        .await;
}

/// Signs the home in with a short-lived token (`slt_fixture`) as `kind`; the access token is
/// `access` and expires in `expires_in` seconds.
pub async fn sign_in(
    home: &Home,
    stubs: &Stubs,
    kind: &str,
    access: &str,
    refresh: &str,
    expires_in: u64,
) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("slt=slt_fixture"))
        .and(body_string_contains("client_id=waveform"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(tokens(access, refresh, kind, expires_in)),
        )
        .up_to_n_times(1)
        .mount(&stubs.accounts)
        .await;
    mount_me(stubs, access, kind).await;
    let output = stubs
        .run(home, &["login", "--slt", "slt_fixture", "--json"], None)
        .await;
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Asserts success and parses stdout as exactly one JSON document.
pub fn json_out(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is exactly one JSON document")
}

/// Parses stdout as one JSON document whatever the exit code.
pub fn json_any(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON document ({error}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Asserts none of `secrets` appears in the output or in any file under the home (the saved
/// sign-in itself is excluded by passing only values that must never be stored).
pub fn assert_never_shown(output: &Output, secrets: &[&str]) {
    let shown = text(output);
    for secret in secrets {
        assert!(!shown.contains(secret), "`{secret}` was printed:\n{shown}");
    }
}
