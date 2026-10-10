//! The Waveform API client against a stub Waveform: credentials, custodian selection,
//! speech, held audio, history polling and typed errors.
use serde_json::json;
use silicon_waveform_client::{
    Auth, Capabilities, Client, Error, GeminiTtsOptions, PreferencesUpdate, ProviderKeys,
    SttRequest, TtsProviderOptions, TtsRequest, TtsResponse,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn tts_body(id: &str, held: bool) -> serde_json::Value {
    if held {
        json!({"request_id":id,"file_url":null,"temporary_url":null,
            "audio_url":format!("http://127.0.0.1/api/v1/audio/{id}"),
            "storage":{"status":"held","reason":"briefcase_unavailable","message":"Briefcase is unavailable.","expires_at":"2030-01-01T00:00:00Z"},
            "media_type":"audio/mpeg","provider":"gemini","duration_ms":1200,"voice_profile":{"id":"puck","revision":2}})
    } else {
        json!({"request_id":id,"file_url":"https://briefcase.test/zQo/apps/waveform/a.mp3","temporary_url":null,
            "audio_url":null,"storage":{"status":"stored"},"media_type":"audio/mpeg","provider":"gemini","duration_ms":1200})
    }
}

#[test]
fn urls_must_be_origins_and_plain_http_only_for_this_machine() {
    for ok in [
        "https://api.waveform.teamofsilicons.com",
        "http://127.0.0.1:4161",
        "http://localhost:4161/",
    ] {
        assert!(Client::new(ok, Auth::Anonymous).is_ok(), "{ok}");
    }
    for bad in [
        "http://api.waveform.teamofsilicons.com",
        "https://user:pass@backend.test",
        "https://backend.test/api",
        "https://backend.test/?x=1",
        "ftp://backend.test",
        "not a url",
    ] {
        let error = Client::new(bad, Auth::Anonymous).unwrap_err();
        assert_eq!(error.code(), "invalid_input", "{bad}");
    }
}

#[test]
fn proofs_and_selectors_are_validated_and_credentials_never_printed() -> TestResult {
    let client = Client::new("http://127.0.0.1:9", Auth::Anonymous)?;
    assert!(client.with_proof("sar_refresh_token").is_err());
    assert!(client.with_proof("sap_with space").is_err());
    let proof = client.with_proof("sap_private_proof")?;
    assert!(!format!("{proof:?}").contains("private"));
    let bearer = client.with_bearer("eyJprivate");
    assert!(!format!("{bearer:?}").contains("eyJprivate"));
    assert!(client.for_account("").is_err());
    assert!(client.for_account("si:has space").is_err());
    assert_eq!(
        client.for_account(" si:scout ")?.selected_account(),
        Some("si:scout")
    );
    Ok(())
}

#[tokio::test]
async fn bearer_and_proof_credentials_are_sent_without_any_organization_header() -> TestResult {
    let server = MockServer::start().await;
    let id = uuid::Uuid::new_v4().to_string();
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .and(header("authorization", "Bearer eyJ-access"))
        .and(header("idempotency-key", "retry-key-0001"))
        .and(header("x-waveform-api-version", "v1"))
        .and(header("x-waveform-protocol-version", "1"))
        .and(body_json(json!({"text":"Hi","auto_fallback":false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(tts_body(&id, false)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/stt"))
        .and(header("authorization", "Proof sap_interface_proof"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id":id,"transcript":"hello","detected_language":"en","provider":"gemini","duration_ms":900
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), Auth::Anonymous)?;
    let request = TtsRequest {
        text: "Hi".into(),
        ..TtsRequest::default()
    };
    let speech = client
        .with_bearer("eyJ-access")
        .tts(&request, "retry-key-0001")
        .await?;
    assert_eq!(speech.storage.unwrap().status, "stored");
    let text = client
        .with_proof("sap_interface_proof")?
        .stt(
            &SttRequest {
                file_url: "https://briefcase.test/zQo/apps/waveform/a.mp3".into(),
                ..SttRequest::default()
            },
            "retry-key-0002",
        )
        .await?;
    assert_eq!(text.transcript, "hello");
    for request in server.received_requests().await.unwrap() {
        for retired in [
            "x-org-id",
            "x-actor-id",
            "x-app-id",
            "x-testing-environment-key",
        ] {
            assert!(!request.headers.contains_key(retired), "{retired}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn custodian_selection_applies_to_account_routes_and_never_to_speech() -> TestResult {
    let server = MockServer::start().await;
    for route in [
        "/api/v1/jobs",
        "/api/v1/preferences",
        "/api/v1/provider-keys",
    ] {
        let body = match route {
            "/api/v1/jobs" => {
                json!({"account":{"uuid":"K1E","id":"si:scout"},"items":[],"next_cursor":null})
            }
            "/api/v1/preferences" => {
                json!({"account":{"uuid":"K1E","id":"si:scout"},"voice_profile":"puck","tts_order":["gemini"],"stt_order":["gemini"],"telemetry_enabled":true})
            }
            _ => {
                json!({"account":{"uuid":"K1E","id":"si:scout"},"items":[{"provider":"gemini","configured":true,"updated_at":"2026-10-10T00:00:00Z"}]})
            }
        };
        Mock::given(method("GET"))
            .and(path(route))
            .and(query_param("account", "si:scout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("DELETE"))
        .and(path("/api/v1/provider-keys/openai"))
        .and(query_param("account", "si:scout"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let carbon = Client::new(&server.uri(), Auth::Anonymous)?
        .with_bearer("eyJ-carbon")
        .for_account("si:scout")?;
    let page = carbon.jobs(None).await?;
    assert_eq!(page.account.unwrap().id.as_deref(), Some("si:scout"));
    assert_eq!(
        carbon.preferences().await?.voice_profile.as_deref(),
        Some("puck")
    );
    assert_eq!(carbon.provider_keys().await?.items[0].provider, "gemini");
    carbon.delete_provider_key("openai").await?;
    // Speech and Briefcase approval are refused before any request.
    let request = TtsRequest {
        text: "Hi".into(),
        ..TtsRequest::default()
    };
    let error = carbon.tts(&request, "retry-key-0003").await.unwrap_err();
    assert!(
        error.to_string().contains("never speaks as its Silicon"),
        "{error}"
    );
    assert!(carbon.approve_storage().await.is_err());
    assert!(carbon.stt_source_folder().await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
    Ok(())
}

#[tokio::test]
async fn held_audio_and_storage_results_are_typed_and_downloadable() -> TestResult {
    let server = MockServer::start().await;
    let id = uuid::Uuid::new_v4().to_string();
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tts_body(&id, true)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/audio/{id}")))
        .and(header("authorization", "Bearer eyJ-access"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .insert_header(
                    "content-disposition",
                    "attachment; filename=\"../evil.mp3\"",
                )
                .insert_header("x-waveform-audio-expires-at", "2030-01-01T00:00:00Z")
                .set_body_bytes(b"ID3fake-mp3".to_vec()),
        )
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), Auth::Anonymous)?.with_bearer("eyJ-access");
    let speech: TtsResponse = client
        .tts(
            &TtsRequest {
                text: "Hi".into(),
                ..TtsRequest::default()
            },
            "retry-key-0004",
        )
        .await?;
    let storage = speech.storage.clone().unwrap();
    assert!(storage.is_held());
    assert_eq!(storage.reason.as_deref(), Some("briefcase_unavailable"));
    assert!(speech.file_url.is_none());
    let audio = client.held_audio(&speech.request_id).await?;
    assert_eq!(audio.bytes, b"ID3fake-mp3");
    assert_eq!(audio.expires_at.as_deref(), Some("2030-01-01T00:00:00Z"));
    assert_eq!(audio.filename, None, "path separators are never accepted");
    assert!(client.held_audio("../me").await.is_err());
    Ok(())
}

#[tokio::test]
async fn login_status_reports_refusals_without_failing_and_keeps_server_errors() -> TestResult {
    let server = MockServer::start().await;
    let anonymous = Client::new(&server.uri(), Auth::Anonymous)?;
    assert!(!anonymous.login_status().await?.authenticated);
    assert!(server.received_requests().await.unwrap().is_empty());
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .and(header("authorization", "Bearer good"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authenticated":true,"app_id":"waveform","via":"bearer","expires_at":"2030-01-01T00:00:00Z",
            "account":{"uuid":"zQo","id":"si:scout","kind":"silicon","display_name":"Scout","pfp_url":null,
                "custodian":{"uuid":"aB1","id":"c:ada"}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .and(header("authorization", "Bearer ended"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error":{
            "code":"unauthenticated","message":"This sign-in ended.","hint":"Sign in again.","reason":"sign_in_ended"}})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .and(header("authorization", "Bearer broken"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "5"))
        .mount(&server)
        .await;
    let status = anonymous.with_bearer("good").login_status().await?;
    let account = status.account.unwrap();
    assert!(status.authenticated && account.is_silicon());
    assert_eq!(account.custodian.unwrap().id.as_deref(), Some("c:ada"));
    let ended = anonymous.with_bearer("ended").login_status().await?;
    assert!(!ended.authenticated);
    assert_eq!(ended.reason.as_deref(), Some("sign_in_ended"));
    let error = anonymous
        .with_bearer("broken")
        .login_status()
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(503));
    assert_eq!(error.as_api().unwrap().retry_after_seconds, Some(5));
    let error = anonymous.with_bearer("ended").me().await.unwrap_err();
    assert!(error.is_stale_credential());
    assert_eq!(error.hint(), Some("Sign in again."));
    Ok(())
}

fn job_json(id: &str, status: &str) -> serde_json::Value {
    json!({"id":id,"operation":"tts","status":status,"first_line":"hello","duration_ms":1234,
        "provider":"gemini","error_code":null,"issuing_app":"interface",
        "created_at":"2026-10-10T00:00:00Z","finished_at":null})
}

#[tokio::test]
async fn a_known_speech_id_can_be_polled_before_its_job_row_exists() -> TestResult {
    let server = MockServer::start().await;
    let id = uuid::Uuid::new_v4();
    let id_text = id.to_string();
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .and(header("x-request-id", id_text.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(40))
                .set_body_json(tts_body(&id_text, false)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let job_id = id_text.clone();
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/jobs/{id}")))
        .respond_with(
            move |_: &Request| match observed.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(404)
                    .set_body_json(json!({"error":{"code":"not_found","message":"no job"}})),
                1 => ResponseTemplate::new(200).set_body_json(job_json(&job_id, "running")),
                _ => ResponseTemplate::new(200).set_body_json(job_json(&job_id, "completed")),
            },
        )
        .expect(3)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), Auth::Anonymous)?.with_bearer("eyJ-access");
    let speech = client.with_speech_request_id(id)?;
    assert!(client.with_speech_request_id(uuid::Uuid::nil()).is_err());
    let request = TtsRequest {
        text: "hello".into(),
        ..TtsRequest::default()
    };
    let (response, job) = tokio::join!(
        speech.tts(&request, "poll-fixture-key"),
        client.wait_for_job(&id_text, Duration::from_secs(2), Duration::from_millis(1))
    );
    assert_eq!(response?.request_id, id_text);
    let job = job?;
    assert_eq!(job.id, id_text);
    assert_eq!(job.issuing_app.as_deref(), Some("interface"));
    assert_eq!(count.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn the_polling_deadline_includes_a_slow_answer() -> TestResult {
    let server = MockServer::start().await;
    let id = uuid::Uuid::new_v4().to_string();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(300))
                .set_body_json(job_json(&id, "completed")),
        )
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), Auth::Anonymous)?.with_bearer("eyJ-access");
    let error = client
        .wait_for_job(&id, Duration::from_millis(30), Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("did not finish within 30 ms"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn settings_storage_and_reports_use_the_documented_bodies() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/preferences"))
        .and(body_json(json!({"telemetry_enabled":false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account":{"uuid":"zQo","id":"c:ada"},"voice_profile":"puck","tts_order":null,"stt_order":null,"telemetry_enabled":false})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/provider-keys/gemini"))
        .and(body_json(json!({"api_key":"private-provider-key"})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/storage-authorizations"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "account":{"uuid":"zQo","id":"c:ada"},
            "briefcase":{"status":"active","receiving_app":"briefcase","scopes":["briefcase.uploads.reserve"],"proof_id":"prf_1"}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/reports"))
        .and(header("idempotency-key", "report-key-1"))
        .and(body_json(json!({"message":"It broke","pr":null,"client_version":silicon_waveform_client::VERSION})))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "id":"5b0b0c8e-0000-4000-8000-000000000001","submitted":true,"notification":"queued",
            "repository":"https://github.com/teamofsilicons/silicon-waveform"})))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), Auth::Anonymous)?.with_bearer("eyJ-access");
    assert_eq!(
        client.set_telemetry(false).await?.telemetry_enabled,
        Some(false)
    );
    assert!(
        client
            .update_preferences(&PreferencesUpdate::default())
            .await
            .is_err()
    );
    assert!(client.put_provider_key("nope", "key").await.is_err());
    assert!(
        client
            .put_provider_key("gemini", "has space")
            .await
            .is_err()
    );
    client
        .put_provider_key("gemini", "private-provider-key")
        .await?;
    assert_eq!(client.approve_storage().await?.briefcase.status, "active");
    assert!(client.report("x", None, "bad key!").await.is_err());
    let receipt = client.report("It broke", None, "report-key-1").await?;
    assert!(receipt.submitted);
    Ok(())
}

#[tokio::test]
async fn api_failures_explain_the_provider_reason_and_stay_typed() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tts"))
        .respond_with(ResponseTemplate::new(502).set_body_json(json!({"error":{
            "code":"provider_failed","message":"elevenlabs request failed: invalid credentials. Try another provider.",
            "request_id":"fixture","provider":"elevenlabs","reason":"invalid_credentials","provider_status":401,
            "raw_body":"secret-provider-response"}})))
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), Auth::Anonymous)?.with_bearer("eyJ-access");
    let error = client
        .tts(
            &TtsRequest {
                text: "Hi".into(),
                ..TtsRequest::default()
            },
            "retry-key-0005",
        )
        .await
        .unwrap_err();
    assert!(matches!(&error, Error::Api(api) if api.provider.as_deref() == Some("elevenlabs")));
    assert!(error.to_string().contains("invalid credentials"));
    assert!(!format!("{error:?}").contains("secret-provider-response"));
    assert!(client.tts(&TtsRequest::default(), "short").await.is_err());
    Ok(())
}

#[test]
fn provider_controls_and_request_keys_are_typed_and_redacted() -> TestResult {
    let options: TtsProviderOptions = serde_json::from_value(json!({
        "gemini": {"voice":"Kore", "scene":"A private rehearsal"}
    }))?;
    assert!(
        serde_json::from_value::<TtsProviderOptions>(json!({"gemini": {"unknown_control": true}}))
            .is_err()
    );
    let mut keys = ProviderKeys::default();
    keys.insert("gemini", "private-request-key")?;
    assert!(keys.insert("unknown-provider", "secret").is_err());
    let request = TtsRequest {
        text: "hello".into(),
        provider_order: Some(vec!["gemini".into()]),
        provider_options: options,
        provider_keys: keys,
        ..Default::default()
    };
    let encoded = serde_json::to_value(&request)?;
    assert_eq!(encoded["auto_fallback"], false);
    assert_eq!(encoded["provider_keys"]["gemini"], "private-request-key");
    assert_eq!(
        encoded["provider_options"]["gemini"]["scene"],
        "A private rehearsal"
    );
    let debug = format!("{request:?}");
    assert!(!debug.contains("private-request-key"));
    assert!(!debug.contains("A private rehearsal"));
    let empty = TtsProviderOptions {
        gemini: Some(GeminiTtsOptions::default()),
        ..TtsProviderOptions::default()
    };
    assert!(empty.is_empty());
    Ok(())
}

#[test]
fn capabilities_and_responses_from_older_servers_still_parse() -> TestResult {
    let mut value = json!({
        "tts":{"output_format":"mp3","languages":["en"]},
        "stt":{"languages":["en"],"accepted_media_types":["audio/mpeg"]}
    });
    let old: Capabilities = serde_json::from_value(value.clone())?;
    assert!(old.tts.auto_fallback_default.is_none() && old.byok.is_none());
    value["byok"] =
        json!({"saved":true,"per_request":true,"precedence":["request","saved","shared"]});
    let current: Capabilities = serde_json::from_value(value)?;
    assert!(current.byok.is_some_and(|byok| byok.per_request));
    let legacy: TtsResponse = serde_json::from_value(json!({"request_id":"fixture",
        "file_url":"https://example.test/audio","media_type":"audio/mpeg","provider":"gemini","duration_ms":100}))?;
    assert!(legacy.storage.is_none() && legacy.voice_profile.is_none());
    Ok(())
}

#[tokio::test]
async fn contract_discovery_sends_the_version_handshake_without_credentials() -> TestResult {
    let server = MockServer::start().await;
    let catalog =
        json!({"service":"silicon-waveform","contracts":[{"api_version":"v1","state":"active"}]});
    Mock::given(method("GET"))
        .and(path("/api/contracts"))
        .and(header("x-waveform-api-version", "v1"))
        .and(header("x-waveform-protocol-version", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&catalog))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "app_id":"waveform","accounts_url":"https://accounts.teamofsilicons.com",
            "api_url":"https://api.waveform.teamofsilicons.com","docs_url":"https://docs.waveform.teamofsilicons.com",
            "proof_scopes":["waveform.tts","waveform.stt"],"version":"0.6.0"})))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), Auth::Anonymous)?.with_bearer("eyJ-never-sent");
    assert_eq!(client.contracts().await?, catalog);
    assert_eq!(client.accounts().await?.app_id, "waveform");
    for request in server.received_requests().await.unwrap() {
        assert!(!request.headers.contains_key("authorization"));
    }
    Ok(())
}
