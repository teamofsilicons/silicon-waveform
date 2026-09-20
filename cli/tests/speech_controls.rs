//! Speech controls and BYOK must survive the CLI boundary without exposing keys.
use serde_json::json;
use std::{
    fs,
    io::Write as _,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path},
};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("waveform-controls-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    async fn run(&self, server: &MockServer, args: &[&str], stdin: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_waveform"));
        command
            .env("SILICON_HOME", &self.0)
            .env("WAVEFORM_AUTO_UPDATE", "false")
            .env_remove("WAVEFORM_TEST")
            .args(["--url", &server.uri()])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let input = stdin.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            let mut child = command.spawn().unwrap();
            if let Some(input) = input {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(input.as_bytes())
                    .unwrap();
            }
            child.wait_with_output().unwrap()
        })
        .await
        .unwrap()
    }

    async fn login(&self, server: &MockServer) {
        Mock::given(method("POST")).and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token":"oat_fixture", "refresh_token":"ort_fixture", "token_type":"Bearer",
                "expires_in":1800,"scope":"","actor":{"principal_id":"00000000-0000-0000-0000-000000000001","type":"carbon","public_id":"12345678"}
            }))).expect(1).mount(server).await;
        assert!(
            self.run(server, &["login", "oac_fixture"], None)
                .await
                .status
                .success()
        );
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn request_controls_and_keys_reach_only_the_speech_request() {
    let home = Home::new();
    let server = MockServer::start().await;
    home.login(&server).await;
    let key_path = home.0.join("input.key");
    fs::write(&key_path, "request-private-key\n").unwrap();
    Mock::given(method("POST")).and(path("/api/v1/tts"))
        .and(body_json(json!({"text":"Hello", "provider_order":["gemini"], "auto_fallback":false,
            "provider_options":{"gemini":{"scene":"A quiet room"}}, "provider_keys":{"gemini":"request-private-key"}})))
        .respond_with(ResponseTemplate::new(502).set_body_json(json!({"error":{
            "code":"provider_failed", "message":"gemini request failed: invalid credentials. Try another provider.",
            "provider":"gemini", "reason":"invalid_credentials", "provider_status":401
        }}))).expect(1).mount(&server).await;
    let output = home
        .run(
            &server,
            &[
                "tts",
                "Hello",
                "--org",
                "tos",
                "--actor",
                "actor",
                "--provider",
                "gemini",
                "--provider-options",
                r#"{"gemini":{"scene":"A quiet room"}}"#,
                "--provider-key-file",
                &format!("gemini={}", key_path.display()),
            ],
            None,
        )
        .await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gemini request failed: invalid credentials"),
        "{stderr}"
    );
    assert!(!stderr.contains("request-private-key"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("request-private-key"));
    for file in fs::read_dir(home.0.join(".waveform/dir")).unwrap() {
        let file = file.unwrap().path();
        if file.is_file() {
            assert!(
                !String::from_utf8_lossy(&fs::read(file).unwrap()).contains("request-private-key")
            );
        }
    }

    Mock::given(method("POST")).and(path("/api/v1/stt"))
        .and(body_json(json!({"file_url":"https://briefcase.test/audio", "provider_keys":{"openai":"stdin-private-key"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"request_id":"fixture", "transcript":"Hello", "provider":"openai", "detected_language":"en", "duration_ms":100})))
        .expect(1).mount(&server).await;
    let output = home
        .run(
            &server,
            &[
                "stt",
                "https://briefcase.test/audio",
                "--org",
                "tos",
                "--actor",
                "actor",
                "--provider-key-file",
                "openai=-",
            ],
            Some("stdin-private-key\n"),
        )
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("stdin-private-key"));
}

#[tokio::test]
async fn saved_provider_key_supports_stdin_without_echo() {
    let home = Home::new();
    let server = MockServer::start().await;
    home.login(&server).await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/provider-keys/gemini"))
        .and(body_json(json!({"api_key":"saved-private-key"})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let output = home
        .run(
            &server,
            &[
                "provider-key-set",
                "--org",
                "tos",
                "--actor",
                "actor",
                "gemini",
                "--key-file",
                "-",
            ],
            Some("saved-private-key\n"),
        )
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("saved-private-key"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("saved-private-key"));
}

#[tokio::test]
async fn maximum_unicode_gemini_controls_fit_file_and_stdin_inputs() {
    let home = Home::new();
    let server = MockServer::start().await;
    home.login(&server).await;
    let guidance = "😀".repeat(4096);
    let options = json!({"gemini": {
        "scene": guidance,
        "audio_profile": guidance,
        "director_notes": guidance,
        "sample_context": guidance,
    }});
    let literal = serde_json::to_string(&options).unwrap();
    let escaped = literal.replace('😀', r"\ud83d\ude00");
    assert!(literal.len() > 65_536);
    assert!(escaped.len() < 327_680);
    let controls_path = home.0.join("controls.json");
    fs::write(&controls_path, literal).unwrap();
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .and(body_json(json!({
            "text": "Hello", "provider_order": ["gemini"], "auto_fallback": false,
            "provider_options": options,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "fixture", "file_url": "https://briefcase.test/audio",
            "media_type": "audio/mpeg", "provider": "gemini", "duration_ms": 100,
        })))
        .expect(2)
        .mount(&server)
        .await;
    for (source, stdin) in [
        (controls_path.to_str().unwrap(), None),
        ("-", Some(escaped.as_str())),
    ] {
        let output = home
            .run(
                &server,
                &[
                    "tts",
                    "Hello",
                    "--org",
                    "tos",
                    "--actor",
                    "actor",
                    "--provider",
                    "gemini",
                    "--provider-options-file",
                    source,
                ],
                stdin,
            )
            .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
