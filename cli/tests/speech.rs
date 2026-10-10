//! Speech, settings and Briefcase access through the CLI: request-only controls and keys,
//! custodian selection, proofs from other apps, held audio and exact retry hints.
mod common;

use common::{Home, Stubs, assert_never_shown, json_any, json_out, sign_in, text};
use serde_json::json;
use std::fs;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

fn tts_body(held: bool) -> serde_json::Value {
    if held {
        json!({"request_id":"0192b9f0-0000-7000-8000-000000000001","file_url":null,"temporary_url":null,
            "audio_url":"http://127.0.0.1:4161/api/v1/audio/0192b9f0-0000-7000-8000-000000000001",
            "storage":{"status":"held","reason":"briefcase_unavailable","message":"Briefcase did not answer.","expires_at":"2099-01-01T00:00:00Z"},
            "media_type":"audio/mpeg","provider":"gemini","duration_ms":1200})
    } else {
        json!({"request_id":"0192b9f0-0000-7000-8000-000000000002","file_url":"https://briefcase.test/aB1/apps/waveform/a.mp3",
            "storage":{"status":"stored"},"media_type":"audio/mpeg","provider":"gemini","duration_ms":1200})
    }
}

#[tokio::test]
async fn request_controls_and_keys_reach_only_the_speech_request() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-speech", "sar_speech", 1800).await;
    let key_path = home.root.join("input.key");
    fs::write(&key_path, "request-private-key\n").unwrap();
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .and(header("authorization", "Bearer eyJ-speech"))
        .and(body_json(json!({"text":"Hello","provider_order":["gemini"],"auto_fallback":false,
            "provider_options":{"gemini":{"scene":"A quiet room"}},"provider_keys":{"gemini":"request-private-key"}})))
        .respond_with(ResponseTemplate::new(502).set_body_json(json!({"error":{
            "code":"provider_failed","message":"gemini request failed: invalid credentials. Try another provider.",
            "provider":"gemini","reason":"invalid_credentials","provider_status":401,"request_id":"req-1"}})))
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    let output = stubs
        .run(
            &home,
            &[
                "tts",
                "Hello",
                "--provider",
                "gemini",
                "--provider-options",
                r#"{"gemini":{"scene":"A quiet room"}}"#,
                "--provider-key-file",
                &format!("gemini={}", key_path.display()),
                "--idempotency",
                "retry-key-0001",
                "--request-id",
                "0192b9f0-0000-7000-8000-00000000000a",
            ],
            None,
        )
        .await;
    assert_eq!(output.status.code(), Some(1));
    let shown = text(&output);
    assert!(
        shown.contains("gemini request failed: invalid credentials"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "--idempotency retry-key-0001 --request-id 0192b9f0-0000-7000-8000-00000000000a"
        ),
        "the exact retry is named: {shown}"
    );
    assert_never_shown(&output, &["request-private-key"]);
    assert!(
        !home
            .all_files_text()
            .replace("request-private-key\n", "")
            .contains("request-private-key")
    );

    Mock::given(method("POST"))
        .and(path("/api/v1/stt"))
        .and(body_json(json!({"file_url":"https://briefcase.test/audio","provider_keys":{"openai":"stdin-private-key"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"request_id":"fixture","transcript":"Hello",
            "provider":"openai","detected_language":"en","duration_ms":100})))
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    let output = stubs
        .run(
            &home,
            &[
                "stt",
                "https://briefcase.test/audio",
                "--provider-key-file",
                "openai=-",
                "--json",
            ],
            Some("stdin-private-key\n"),
        )
        .await;
    assert_eq!(json_out(&output)["transcript"], "Hello");
    assert_never_shown(&output, &["stdin-private-key"]);
}

#[tokio::test]
async fn saved_provider_keys_are_read_from_stdin_and_never_echoed() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-keys", "sar_keys", 1800).await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/provider-keys/gemini"))
        .and(body_json(json!({"api_key":"saved-private-key"})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    let output = stubs
        .run(
            &home,
            &[
                "provider-keys",
                "set",
                "gemini",
                "--key-file",
                "-",
                "--json",
            ],
            Some("saved-private-key\n"),
        )
        .await;
    assert_eq!(
        json_out(&output),
        json!({"saved":true,"provider":"gemini","account":null})
    );
    assert_never_shown(&output, &["saved-private-key"]);
    let two_stdins = stubs
        .run(
            &home,
            &[
                "tts",
                "Hi",
                "--text-file",
                "-",
                "--provider-key-file",
                "gemini=-",
            ],
            None,
        )
        .await;
    assert_eq!(
        two_stdins.status.code(),
        Some(2),
        "only one input may read stdin"
    );
}

#[tokio::test]
async fn maximum_unicode_gemini_controls_fit_file_and_stdin_inputs() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-unicode", "sar_unicode", 1800).await;
    let guidance = "😀".repeat(4096);
    let options = json!({"gemini": {"scene": guidance, "audio_profile": guidance,
        "director_notes": guidance, "sample_context": guidance}});
    let literal = serde_json::to_string(&options).unwrap();
    let escaped = literal.replace('😀', r"\ud83d\ude00");
    assert!(literal.len() > 65_536 && escaped.len() < 327_680);
    let controls = home.root.join("controls.json");
    fs::write(&controls, literal).unwrap();
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .and(body_json(json!({"text":"Hello","provider_order":["gemini"],"auto_fallback":false,"provider_options":options})))
        .respond_with(ResponseTemplate::new(200).set_body_json(tts_body(false)))
        .expect(2)
        .mount(&stubs.waveform)
        .await;
    for (source, stdin) in [
        (controls.to_str().unwrap(), None),
        ("-", Some(escaped.as_str())),
    ] {
        let output = stubs
            .run(
                &home,
                &[
                    "tts",
                    "Hello",
                    "--provider",
                    "gemini",
                    "--provider-options-file",
                    source,
                    "--json",
                ],
                stdin,
            )
            .await;
        assert_eq!(json_out(&output)["storage"]["status"], "stored");
    }
}

#[tokio::test]
async fn a_custodian_selects_a_silicon_for_its_data_and_never_for_speech() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(
        &home,
        &stubs,
        "carbon",
        "eyJ-custodian",
        "sar_custodian",
        1800,
    )
    .await;
    for (route, body) in [
        (
            "/api/v1/jobs",
            json!({"account":{"uuid":"K1E","id":"si:scout"},"items":[],"next_cursor":null}),
        ),
        (
            "/api/v1/preferences",
            json!({"account":{"uuid":"K1E","id":"si:scout"},"voice_profile":"puck"}),
        ),
        (
            "/api/v1/provider-keys",
            json!({"account":{"uuid":"K1E","id":"si:scout"},"items":[]}),
        ),
        (
            "/api/v1/storage-authorizations",
            json!({"account":{"uuid":"K1E","id":"si:scout"},"briefcase":{"status":"missing"}}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(query_param("account", "si:scout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&stubs.waveform)
            .await;
    }
    for args in [
        vec!["jobs", "--account", "si:scout", "--json"],
        vec!["preferences", "--account", "si:scout", "--json"],
        vec!["provider-keys", "--account", "si:scout", "--json"],
        vec!["storage", "status", "--account", "si:scout", "--json"],
    ] {
        let value = json_out(&stubs.run(&home, &args, None).await);
        assert_eq!(value["account"]["id"], "si:scout", "{args:?}");
    }
    // Speech always runs as the signed-in account.
    let refused = stubs
        .run(&home, &["tts", "Hi", "--account", "si:scout"], None)
        .await;
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        text(&refused).contains("never speaks for its Silicon"),
        "{}",
        text(&refused)
    );
    let approve = stubs
        .run(
            &home,
            &["storage", "approve", "--account", "si:scout"],
            None,
        )
        .await;
    assert_eq!(
        approve.status.code(),
        Some(2),
        "only the account itself approves Briefcase access"
    );
    assert!(text(&approve).contains("Only the account itself"));
}

#[tokio::test]
async fn an_app_acting_for_an_account_sends_its_proof_and_needs_no_sign_in() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    let proof = home.root.join("proof");
    fs::write(&proof, "sap_interface_private_proof\n").unwrap();
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .and(header("authorization", "Proof sap_interface_private_proof"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tts_body(true)))
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/audio/0192b9f0-0000-7000-8000-000000000001"))
        .and(header("authorization", "Proof sap_interface_private_proof"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .insert_header("x-waveform-audio-expires-at", "2099-01-01T00:00:00Z")
                .set_body_bytes(b"ID3fake".to_vec()),
        )
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    let output = stubs
        .run(
            &home,
            &["tts", "Hi", "--proof-file", proof.to_str().unwrap()],
            None,
        )
        .await;
    // Text mode prints the result as JSON on stdout and the notice on stderr.
    let value = json_out(&output);
    assert_eq!(value["storage"]["reason"], "briefcase_unavailable");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("waveform audio 0192b9f0-0000-7000-8000-000000000001")
    );
    assert_never_shown(&output, &["sap_interface_private_proof"]);
    let saved = home.root.join("speech.mp3");
    let output = stubs
        .run(
            &home,
            &[
                "audio",
                "0192b9f0-0000-7000-8000-000000000001",
                "--output",
                saved.to_str().unwrap(),
                "--proof-file",
                proof.to_str().unwrap(),
                "--json",
            ],
            None,
        )
        .await;
    assert_eq!(json_out(&output)["bytes"], 7);
    assert_eq!(fs::read(&saved).unwrap(), b"ID3fake");
    let again = stubs
        .run(
            &home,
            &[
                "audio",
                "0192b9f0-0000-7000-8000-000000000001",
                "--output",
                saved.to_str().unwrap(),
                "--proof-file",
                proof.to_str().unwrap(),
            ],
            None,
        )
        .await;
    assert_eq!(
        again.status.code(),
        Some(2),
        "an existing file is kept without --force"
    );
    assert!(home.session_files().is_empty(), "a proof is not a sign-in");
    let bad = home.root.join("bad-proof");
    fs::write(&bad, "sar_not_a_proof").unwrap();
    let refused = stubs
        .run(
            &home,
            &["tts", "Hi", "--proof-file", bad.to_str().unwrap()],
            None,
        )
        .await;
    assert_eq!(refused.status.code(), Some(2));
}

#[tokio::test]
async fn missing_briefcase_access_names_the_command_and_the_exact_retry() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "silicon", "eyJ-stt", "sar_stt", 1800).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/stt"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error":{
            "code":"storage_authorization_required",
            "message":"Waveform has no Briefcase access for si:scout, so it cannot read the source."}})))
        .mount(&stubs.waveform)
        .await;
    let output = stubs
        .run(
            &home,
            &[
                "stt",
                "https://briefcase.test/a.mp3",
                "--idempotency",
                "stt-key-0001",
                "--json",
            ],
            None,
        )
        .await;
    assert_eq!(output.status.code(), Some(3));
    let error = &json_any(&output)["error"];
    assert_eq!(error["code"], "storage_authorization_required");
    let hint = error["hint"].as_str().unwrap();
    assert!(
        hint.contains("waveform storage approve") && hint.contains("--idempotency stt-key-0001"),
        "{hint}"
    );
    Mock::given(method("POST"))
        .and(path("/api/v1/storage-authorizations"))
        .and(header("authorization", "Bearer eyJ-stt"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"account":{"uuid":"K1E","id":"si:scout"},
            "briefcase":{"status":"active","receiving_app":"briefcase","scopes":["briefcase.files.read"]}})))
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    let approved = json_out(
        &stubs
            .run(&home, &["storage", "approve", "--json"], None)
            .await,
    );
    assert_eq!(approved["briefcase"]["status"], "active");
}
