//! Exercise the public CLI grammar, filesystem selection and online status.
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("waveform-discovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    async fn run(&self, url: &str, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_waveform"));
        command
            .env("SILICON_HOME", &self.0)
            .env("WAVEFORM_AUTO_UPDATE", "false")
            .env_remove("WAVEFORM_TEST")
            .args(["--url", url])
            .args(args);
        tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn output_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout contains exactly one JSON document")
}
fn authority(kind: &str) -> Value {
    json!({"principal_id":"00000000-0000-0000-0000-000000000001",
        "actor_type":kind,"public_id":"12345678","organization_id":"00000000-0000-0000-0000-000000000002",
        "org_id":"tos","membership_id":"00000000-0000-0000-0000-000000000003",
        "membership_version":1,"authorization_epoch":1,"audience":"tos>waveform",
        "testing_environment_id":null,"scopes":[],"org_role":"member","tags":[]})
}
async fn login(home: &Home, server: &MockServer, selection: &[&str]) {
    Mock::given(method("POST")).and(path("/api/v1/auth/login"))
        .and(body_json(json!({"slt":"oac_fixture"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token":"oat_private","refresh_token":"ort_private","token_type":"Bearer",
            "expires_in":1800,"scope":"","actor":{"principal_id":"00000000-0000-0000-0000-000000000001","type":"carbon","public_id":"12345678"}
        }))).expect(1).mount(server).await;
    let args: Vec<_> = selection
        .iter()
        .copied()
        .chain(["login", "oac_fixture", "--json"])
        .collect();
    output_json(home.run(&server.uri(), &args).await);
}

#[tokio::test]
async fn help_is_complete_and_login_status_is_a_subcommand() {
    let home = Home::new();
    for flag in ["-h", "--help"] {
        let result = home.run("http://127.0.0.1:1", &[flag]).await;
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(help.contains("iam"));
        assert!(help.contains("test-env"));
        if flag == "--help" {
            assert!(help.contains("SILICON_HOME"));
        }
    }
    let result = home.run("http://127.0.0.1:1", &["login", "--help"]).await;
    assert!(String::from_utf8(result.stdout).unwrap().contains("status"));
    let result = home
        .run(
            "http://127.0.0.1:1",
            &["login", "status", "--slt", "oac_fixture"],
        )
        .await;
    assert!(!result.status.success());
}

#[tokio::test]
async fn discovery_is_public_and_preserves_test_selection() {
    let home = Home::new();
    let server = MockServer::start().await;
    let key = "A".repeat(32);
    Mock::given(method("GET"))
        .and(path("/api/v1/iam"))
        .and(header("x-testing-environment-key", key.as_str()))
        .and(|r: &wiremock::Request| !r.headers.contains_key("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "app_id":"tos>waveform", "iam_base_url":"https://iam.example/",
            "testing_environment_id":"00000000-0000-0000-0000-000000000004"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let value = output_json(
        home.run(&server.uri(), &["--test", &key, "iam", "--json"])
            .await,
    );
    assert_eq!(value["app_id"], "tos>waveform");
    assert!(!value.to_string().contains(&key));
}

#[tokio::test]
async fn status_without_a_session_is_false_without_contacting_server() {
    let home = Home::new();
    let value = output_json(
        home.run("http://127.0.0.1:1", &["login", "status", "--json"])
            .await,
    );
    assert_eq!(value["authenticated"], false);
    assert!(value["actor"].is_null());
    assert!(!home.0.join(".waveform").exists());
}

#[tokio::test]
async fn status_verifies_both_actor_types_and_hides_session_tokens() {
    for kind in ["carbon", "silicon"] {
        let home = Home::new();
        let server = MockServer::start().await;
        login(&home, &server, &[]).await;
        assert!(home.0.join(".waveform/dir").is_dir());
        Mock::given(method("GET"))
            .and(path("/api/v1/auth/me"))
            .and(header("authorization", "Bearer oat_private"))
            .respond_with(ResponseTemplate::new(200).set_body_json(authority(kind)))
            .expect(2)
            .mount(&server)
            .await;
        let value = output_json(
            home.run(&server.uri(), &["login", "status", "--json"])
                .await,
        );
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["actor"]["actor_type"], kind);
        assert_eq!(value["actor"]["public_id"], "12345678");
        assert_eq!(value["org_id"], "tos");
        assert!(!value.to_string().contains("private"));
        let result = home.run(&server.uri(), &["login", "status"]).await;
        assert!(result.status.success());
        assert!(
            String::from_utf8(result.stdout)
                .unwrap()
                .contains(&format!("{kind} 12345678"))
        );
    }
}

#[tokio::test]
async fn status_distinguishes_rejected_tokens_from_server_failure() {
    for code in [401, 403, 503] {
        let home = Home::new();
        let server = MockServer::start().await;
        login(&home, &server, &[]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/auth/me"))
            .respond_with(
                ResponseTemplate::new(code)
                    .set_body_json(json!({"error":{"code":"fixture_error"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let result = home
            .run(&server.uri(), &["login", "status", "--json"])
            .await;
        if code == 503 {
            assert!(!result.status.success());
            assert!(result.stdout.is_empty());
        } else {
            assert_eq!(output_json(result)["authenticated"], false);
        }
    }
}

#[tokio::test]
async fn status_keeps_production_and_test_sessions_separate() {
    let home = Home::new();
    let server = MockServer::start().await;
    let key = "A".repeat(32);
    login(&home, &server, &["--test", &key]).await;
    let mut identity = authority("silicon");
    identity["testing_environment_id"] = json!("00000000-0000-0000-0000-000000000004");
    Mock::given(method("GET"))
        .and(path("/api/v1/auth/me"))
        .and(header("authorization", "Bearer oat_private"))
        .and(header("x-testing-environment-key", key.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(identity))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        output_json(
            home.run(&server.uri(), &["login", "status", "--json"])
                .await
        )["authenticated"],
        false
    );
    let value = output_json(
        home.run(
            &server.uri(),
            &["--test", &key, "login", "status", "--json"],
        )
        .await,
    );
    assert_eq!(value["authenticated"], true);
    assert_eq!(
        value["testing_environment_id"],
        "00000000-0000-0000-0000-000000000004"
    );
}

#[tokio::test]
async fn explicit_home_overrides_silicon_default_and_pointer_stays_under_silicon_home() {
    let home = Home::new();
    let selected = Home::new();
    let server = MockServer::start().await;
    output_json(
        home.run(
            &server.uri(),
            &["config", "home", selected.0.to_str().unwrap(), "--json"],
        )
        .await,
    );
    assert!(home.0.join(".waveform/config.json").is_file());
    login(&home, &server, &[]).await;
    assert!(selected.0.join(".waveform/dir").is_dir());
    assert!(!home.0.join(".waveform/dir").exists());
}

#[test]
fn missing_silicon_home_uses_the_os_home() {
    let home = Home::new();
    let result = Command::new(env!("CARGO_BIN_EXE_waveform"))
        .env_remove("SILICON_HOME")
        .env("HOME", &home.0)
        .args(["config", "auto-update", "off", "--json"])
        .output()
        .unwrap();
    output_json(result);
    assert!(home.0.join(".waveform/dir/auto-update.json").is_file());
}

#[tokio::test]
async fn environment_selects_test_requests_and_explicit_flag_overrides_it() {
    let home = Home::new();
    let server = MockServer::start().await;
    let env_key = "abcdefghijklmnopqrstuvwxyz123456";
    let explicit_key = "123456abcdefghijklmnopqrstuvwxyz";
    for explicit in [false, true] {
        Mock::given(method("GET")).and(path("/api/v1/iam"))
            .and(header("x-testing-environment-key", if explicit { explicit_key } else { env_key }))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "app_id":"tos>waveform", "iam_base_url":"https://iam.example/", "testing_environment_id":null
            }))).expect(1).mount(&server).await;
        let mut command = Command::new(env!("CARGO_BIN_EXE_waveform"));
        command
            .env("SILICON_HOME", &home.0)
            .env("WAVEFORM_AUTO_UPDATE", "false")
            .env("WAVEFORM_TEST", env_key)
            .args(["--url", &server.uri()]);
        if explicit {
            command.args(["--test", explicit_key]);
        }
        command.args(["iam", "--json"]);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_eq!(output_json(output)["app_id"], "tos>waveform");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_waveform"))
        .env("WAVEFORM_TEST", env_key)
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(env_key));
}
