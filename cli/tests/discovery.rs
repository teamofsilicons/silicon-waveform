//! The command tree, the three discovery commands Silicon Apps runs on every package, the
//! retired flags, and where state lives.
mod common;

use common::{Home, Stubs, json_any, json_out, text};
use serde_json::json;
use std::fs;

const COMMANDS: [&str; 19] = [
    "login",
    "logout",
    "accounts",
    "refresh",
    "me",
    "silicons",
    "tts",
    "stt",
    "stt-folder",
    "audio",
    "jobs",
    "preferences",
    "voice-profiles",
    "provider-keys",
    "storage",
    "report",
    "capabilities",
    "contracts",
    "docs",
];

/// Words a Carbon or Silicon must never read in help (D9).
const BANNED: [&str; 9] = [
    "organization",
    "--org",
    "IAM",
    "Honeycomb",
    "human",
    "AI agent",
    "user account",
    "Team",
    "test-env",
];

#[tokio::test]
async fn help_is_a_complete_tree_and_every_branch_explains_itself() {
    let home = Home::new();
    let top = home.run(&["--help"], None).await;
    assert!(top.status.success());
    let top_text = String::from_utf8_lossy(&top.stdout).into_owned();
    for command in COMMANDS {
        assert!(
            top_text.contains(&format!("  {command} ")),
            "{command} missing from --help"
        );
    }
    assert!(
        !top_text.contains("  iam "),
        "the transition alias stays hidden"
    );
    let mut pages = vec![top_text];
    for command in COMMANDS.into_iter().chain(["config"]) {
        let output = home.run(&[command, "--help"], None).await;
        assert!(output.status.success(), "{command} --help failed");
        let page = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            page.contains("Usage: waveform"),
            "{command} --help has no usage"
        );
        pages.push(page);
    }
    for nested in [
        ["login", "status"],
        ["provider-keys", "set"],
        ["provider-keys", "delete"],
        ["storage", "approve"],
        ["storage", "status"],
        ["storage", "revoke"],
        ["config", "show"],
        ["config", "home"],
    ] {
        let output = home.run(&[nested[0], nested[1], "--help"], None).await;
        assert!(output.status.success(), "{nested:?} --help failed");
        pages.push(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    for page in &pages {
        // "Team of Silicons" is the maker's name, not an account grouping.
        let page = page.replace("Team of Silicons", "");
        for word in BANNED {
            assert!(!page.contains(word), "help says `{word}`:\n{page}");
        }
    }
    let login = &pages[1];
    for needle in [
        "--slt-stdin",
        "silicon-accounts login --app waveform -q",
        "--open",
        "status",
    ] {
        assert!(login.contains(needle), "login --help lacks {needle}");
    }
    let tts = pages
        .iter()
        .find(|page| page.contains("Usage: waveform tts"))
        .unwrap();
    for needle in [
        "--proof-file",
        "--provider-options",
        "--idempotency",
        "Examples:",
    ] {
        assert!(tts.contains(needle), "tts --help lacks {needle}");
    }
    let jobs = pages
        .iter()
        .find(|page| page.contains("Usage: waveform jobs"))
        .unwrap();
    assert!(jobs.contains("--account") && jobs.contains("si:"));
}

#[tokio::test]
async fn discovery_answers_in_an_empty_environment_with_the_exact_shapes() {
    let home = Home::new();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_waveform"));
    command
        .env_clear()
        .env("HOME", &home.root)
        .env("SILICON_HOME", &home.root);
    let help = home.run_with(command, &["--help"], None).await;
    assert!(help.status.success() && !help.stdout.is_empty());

    let accounts = json_out(&home.run(&["accounts", "--json"], None).await);
    assert_eq!(
        accounts,
        json!({
            "app_id": "waveform",
            "accounts_url": "https://accounts.teamofsilicons.com",
            "api_url": "https://api.waveform.teamofsilicons.com",
            "docs_url": "https://docs.waveform.teamofsilicons.com",
            "version": env!("CARGO_PKG_VERSION"),
            "proof_scopes": ["waveform.tts", "waveform.stt"],
            "sign_in": {
                "carbon": "waveform login",
                "silicon": "silicon-accounts login --app waveform -q | waveform login --slt-stdin",
                "short_lived_token": "silicon-accounts login --app waveform -q",
            },
        })
    );
    // The hidden transition alias prints exactly the same object, with or without --json.
    assert_eq!(
        json_out(&home.run(&["iam", "--json"], None).await),
        accounts
    );
    assert_eq!(json_out(&home.run(&["iam"], None).await), accounts);

    let status = json_out(&home.run(&["login", "status", "--json"], None).await);
    assert_eq!(status, json!({"authenticated": false}));
    let text_status = home.run(&["login", "status"], None).await;
    assert_eq!(
        text_status.status.code(),
        Some(1),
        "signed out without --json exits 1"
    );
    assert!(text(&text_status).contains("waveform login"));

    assert!(
        fs::read_dir(&home.root).unwrap().next().is_none(),
        "discovery writes nothing"
    );
}

#[tokio::test]
async fn discovery_reports_the_configured_origins() {
    let home = Home::new();
    let mut command = home.command();
    command
        .env("WAVEFORM_URL", "http://127.0.0.1:4161/")
        .env("ACCOUNTS_URL", "http://localhost:9590/");
    let value = json_out(&home.run_with(command, &["accounts", "--json"], None).await);
    assert_eq!(value["api_url"], "http://127.0.0.1:4161");
    assert_eq!(value["accounts_url"], "http://localhost:9590");
    // Flags win over the environment, and nothing is validated or contacted offline.
    let value = json_out(
        &home
            .run(
                &["--url", "https://waveform.example", "accounts", "--json"],
                None,
            )
            .await,
    );
    assert_eq!(value["api_url"], "https://waveform.example");
}

#[tokio::test]
async fn status_json_exits_zero_even_when_nothing_can_be_read() {
    let home = Home::new();
    let invalid = home
        .run(
            &["--profile", "../escape", "login", "status", "--json"],
            None,
        )
        .await;
    let value = json_out(&invalid);
    assert_eq!(value["authenticated"], false);
    assert_eq!(value["reason"], "invalid_input");
    let failed = home
        .run(&["--profile", "../escape", "login", "status"], None)
        .await;
    assert_eq!(failed.status.code(), Some(2));
}

#[tokio::test]
async fn retired_flags_and_commands_say_what_replaced_them() {
    let home = Home::new();
    for (args, code, needle) in [
        (
            vec!["tts", "Hi", "--org", "tos", "--actor", "c:ada"],
            "organizations_removed",
            "--account si:",
        ),
        (
            vec!["--test", "ask_x", "jobs"],
            "test_environments_removed",
            "development Waveform",
        ),
        (
            vec!["tts", "Hi", "--obo-app", "ting", "--obo-token-file", "t"],
            "delegated_tokens_removed",
            "--proof-file",
        ),
        (
            vec!["jobs", "--org", "tos"],
            "organizations_removed",
            "--account",
        ),
        (
            vec!["test-env", "list"],
            "test_environments_removed",
            "waveform docs testing",
        ),
        (
            vec!["daemon", "status"],
            "updater_removed",
            "silicon-apps install waveform",
        ),
        (
            vec!["config", "auto-update", "off"],
            "updater_removed",
            "Silicon Apps",
        ),
        (
            vec!["provider-key-set", "gemini", "--key-file", "k"],
            "command_renamed",
            "provider-keys set",
        ),
        (
            vec!["storage", "start"],
            "command_renamed",
            "storage approve",
        ),
    ] {
        let output = home.run(&args, None).await;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let shown = text(&output);
        assert!(shown.contains(needle), "{args:?}: {shown}");
        let mut with_json = args.clone();
        with_json.push("--json");
        let value = json_any(&home.run(&with_json, None).await);
        assert_eq!(value["error"]["code"], code, "{args:?}");
        assert!(value["error"]["hint"].is_string());
    }
    // Usage errors under --json are JSON too.
    let value = json_any(&home.run(&["tts", "--bogus", "--json"], None).await);
    assert_eq!(value["error"]["code"], "invalid_arguments");
}

#[tokio::test]
async fn config_home_moves_state_and_the_pointer_stays_under_silicon_home() {
    let home = Home::new();
    let elsewhere = Home::new();
    let value = json_out(
        &home
            .run(
                &["config", "home", elsewhere.root.to_str().unwrap(), "--json"],
                None,
            )
            .await,
    );
    assert_eq!(
        value["state_dir"].as_str().unwrap(),
        fs::canonicalize(&elsewhere.root)
            .unwrap()
            .join(".waveform/dir")
            .to_str()
            .unwrap()
    );
    assert!(home.root.join(".waveform/config.json").is_file());
    assert!(!home.root.join(".waveform/dir").exists());
    let shown = json_out(&home.run(&["config", "show", "--json"], None).await);
    assert_eq!(shown["state_dir"], value["state_dir"]);
    assert_eq!(shown["signed_in"], serde_json::Value::Null);
    let missing = home
        .run(&["config", "home", "/definitely/not/here"], None)
        .await;
    assert_eq!(missing.status.code(), Some(2));
}

#[tokio::test]
async fn without_silicon_home_the_os_home_is_used() {
    let home = Home::new();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_waveform"));
    command.env_clear().env("HOME", &home.root);
    let shown = json_out(
        &home
            .run_with(command, &["config", "show", "--json"], None)
            .await,
    );
    assert_eq!(
        shown["state_dir"].as_str().unwrap(),
        home.root.join(".waveform/dir").to_str().unwrap()
    );
}

#[tokio::test]
async fn retired_telemetry_commands_change_nothing_and_exit_zero() {
    let home = Home::new();
    let top = text(&home.run(&["--help"], None).await);
    assert!(!top.contains("  telemetry "), "help no longer offers it");
    for args in [
        vec!["telemetry", "off"],
        vec!["telemetry", "on", "--account", "si:scout"],
        vec!["config", "telemetry", "off"],
    ] {
        let output = home.run(&args, None).await;
        assert!(output.status.success(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "Telemetry was removed from Waveform; there is nothing to turn on or off.\n",
            "{args:?}"
        );
        let mut with_json = args.clone();
        with_json.push("--json");
        let value = json_out(&home.run(&with_json, None).await);
        assert_eq!(value["telemetry"], "removed", "{args:?}");
    }
    assert!(
        !home.root.join(".waveform").exists(),
        "no state is read or written"
    );
}

#[tokio::test]
async fn earlier_sign_ins_are_never_used_and_logout_removes_them() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    fs::create_dir_all(home.state_dir()).unwrap();
    fs::write(
        home.state_dir().join("session-0f0f.json"),
        br#"{"access_token":"oat_old_private","refresh_token":"ort_old_private","identity":{"org_id":"tos"}}"#,
    )
    .unwrap();
    let status = json_out(&stubs.run(&home, &["login", "status", "--json"], None).await);
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "legacy_session");
    let refused = stubs.run(&home, &["jobs"], None).await;
    assert_eq!(refused.status.code(), Some(3));
    assert!(text(&refused).contains("earlier Waveform version"));
    assert!(
        stubs.waveform_requests().await.is_empty(),
        "nothing was sent with the old tokens"
    );
    let logout = json_out(&stubs.run(&home, &["logout", "--json"], None).await);
    assert_eq!(logout["legacy_files_removed"], 1);
    assert!(!home.state_dir().join("session-0f0f.json").exists());
    assert!(stubs.accounts_requests("/").await.is_empty());
}

#[tokio::test]
async fn a_corrupted_sign_in_is_reported_and_replaced_by_the_next_sign_in() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    common::sign_in(&home, &stubs, "carbon", "eyJ-first", "sar_first", 1800).await;
    let file = home.session_files().remove(0);
    fs::write(&file, b"{ truncated").unwrap();
    let status = json_out(&stubs.run(&home, &["login", "status", "--json"], None).await);
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "session_unreadable");
    let refused = stubs.run(&home, &["me", "--json"], None).await;
    assert_eq!(refused.status.code(), Some(3));
    assert_eq!(json_any(&refused)["error"]["code"], "session_unreadable");
    common::sign_in(&home, &stubs, "carbon", "eyJ-second", "sar_second", 1800).await;
    assert_eq!(home.session()["access_token"], "eyJ-second");
}
