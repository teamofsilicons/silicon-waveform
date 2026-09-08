//! Full HTTP-to-PostgreSQL-to-SDK upload regression with paired mock upstreams.
use super::*;
use crate::{
    api::{ApiState, ReadinessChecks},
    application::{
        ports::LeaseIdGenerator,
        service::{ServicePolicy, WaveformService},
    },
    domain::{
        idempotency::{IdempotencyLeaseId, RequestDigestKey},
        media::{BriefcaseOrigin, MediaSizeLimit},
    },
    infrastructure::{
        idempotency::PostgresIdempotencyStore,
        testing::{FixtureBriefcase, FixtureIam, FixtureUploadLedger},
    },
};
use std::{sync::Mutex, time::Duration};

struct Leases;
impl LeaseIdGenerator for Leases {
    fn new_idempotency_lease_id(&self) -> IdempotencyLeaseId {
        IdempotencyLeaseId::new(Uuid::new_v4()).unwrap_or_else(|_| panic!("non-nil generated UUID"))
    }
}

fn speech_app(state: Arc<ControlState>) -> Result<Router, Box<dyn std::error::Error>> {
    let duration = Duration::from_secs(5);
    let store = Arc::new(PostgresIdempotencyStore::new(
        state.pool.clone(),
        duration,
        Duration::from_secs(1),
        Duration::from_secs(3600),
    ));
    let audio = Arc::new(crate::infrastructure::audio::FfmpegAudioNormalizer::new(
        crate::infrastructure::audio::AudioNormalizerConfig::new("ffmpeg"),
    ));
    let app_id: crate::domain::identity::ApplicationId = "tos>waveform".parse()?;
    let iam = Arc::new(FixtureIam::new(
        ActorKind::Carbon,
        Uuid::new_v4(),
        app_id.clone(),
    )?);
    let briefcase = Arc::new(FixtureBriefcase::new(
        app_id,
        state.briefcase_settings.permanent_origin.clone(),
        state.briefcase_settings.cdn_origin.clone(),
        FixtureUploadLedger::default(),
    )?);
    let (tts, stt) = crate::infrastructure::testing::database_provider_chains(state.pool.clone());
    let policy = ServicePolicy::new(
        Duration::from_secs(60),
        MediaSizeLimit::default(),
        BriefcaseOrigin::new(state.briefcase_settings.permanent_origin.clone())?,
        crate::domain::speech::MAX_TTS_TEXT_CHARACTERS,
        RequestDigestKey::new(b"test-key-123456789012345678901234567890")?,
    )?;
    let service = Arc::new(WaveformService::new(
        iam,
        briefcase,
        store.clone(),
        audio.clone(),
        Arc::new(Leases),
        tts,
        stt,
        policy,
    )?);
    let readiness = ReadinessChecks::new(store, audio, true, false);
    let api = ApiState::new(service.clone(), readiness, duration, duration)
        .with_control(state)
        .with_fixture_service(service);
    Ok(crate::api::router(
        api,
        &crate::config::ServerSettings {
            bind_addr: "127.0.0.1:0".parse()?,
            json_body_limit: 65536,
            tts_deadline: duration,
            stt_deadline: duration,
            max_in_flight: 8.try_into()?,
            shutdown_timeout: duration,
        },
    ))
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn test_tts_uploads_exact_fixture_with_both_paired_keys() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let storage = MockServer::start().await;
    let mut state = fixture(pool.clone(), &iam)?;
    let settings = &mut Arc::get_mut(&mut state)
        .ok_or("shared test state")?
        .briefcase_settings;
    settings.base_url = storage.uri().parse()?;
    settings.permanent_origin = "https://briefcase.example.test".parse()?;
    settings.cdn_origin = settings.permanent_origin.clone();
    crate::infrastructure::testing::seed_audio(&pool).await?;
    let app = speech_app(state)?;
    let actor = Uuid::new_v4();
    let iam_plane = Uuid::new_v4();
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(actor)))
        .mount(&iam)
        .await;
    let (status, created) = call(&app, "POST", "/api/v1/testing-environments", Some("oat_fixture"), json!({
        "name":"paired-storage", "iam_environment_id":iam_plane, "iam_environment_key":"I".repeat(32),
        "app_secret":"test-environment-app-secret", "briefcase_environment_key":"B".repeat(32)
    })).await?;
    assert_eq!(status, StatusCode::OK);
    let root = created["key"].as_str().ok_or("missing root")?;
    let mut test_snapshot = snapshot(actor);
    test_snapshot["authorization"]["testing_environment_id"] = json!(iam_plane);
    Mock::given(path("/api/v1/oauth/introspect"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_snapshot))
        .with_priority(1)
        .mount(&iam)
        .await;
    Mock::given(method("GET")).and(path("/api/v1/obo-access/applications/tos%3Ebriefcase/endpoints"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "application":{"app_id":"tos>briefcase","org_id":"tos"},
            "endpoints":[{"endpoint_id":"briefcase.files.create","path":"/api/v1/obo/files","metadata":{"path":{"type":"string"},"name":{"type":"string"},"content_type":{"type":"string"}}}, {"endpoint_id":"briefcase.entries.list","path":"/api/v1/obo/entries/list","metadata":{}}, {"endpoint_id":"briefcase.files.read","path":"/api/v1/obo/files/read","metadata":{}}]
        }))).expect(4).mount(&iam).await;
    let read_ledger = Arc::new(Mutex::new(ReadLedger::default()));
    let minted_reads = read_ledger.clone();
    let uploads = Arc::new(Mutex::new(Vec::<String>::new()));
    let exchange_names = uploads.clone();
    let expected_bytes = include_bytes!("../infrastructure/test-fixture.mp3");
    Mock::given(method("POST")).and(path("/api/v1/obo-access/exchanges"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or_else(|_| panic!("exchange JSON"));
            assert_eq!(body["subject_token"], "oat_fixture");
            assert_eq!(body["audience"], "tos>briefcase");
            if body["endpoint_id"] != "briefcase.files.create" {
                let proof = format!("obo_{}", Uuid::new_v4());
                let endpoint = body["endpoint_id"].as_str().unwrap_or_else(|| panic!("endpoint"));
                let digest = body["request"]["body_sha256"].as_str().unwrap_or_else(|| panic!("digest"));
                minted_reads.lock().unwrap_or_else(|_| panic!("ledger")).proofs.insert(proof.clone(), (endpoint.to_owned(), digest.to_owned()));
                return ResponseTemplate::new(201).set_body_json(json!({"access_proof":proof,"proof_id":Uuid::new_v4(),"expires_in":60,"expires_at":"2099-01-01T00:00:00Z"}));
            }
            assert_eq!(body["request"]["body_sha256"], silicon_iam_client::api::obo::body_sha256(expected_bytes));
            assert!(request.headers.contains_key("x-obo-signature"));
            exchange_names.lock().unwrap_or_else(|_| panic!("exchange lock")).push(body["metadata"]["name"].as_str().unwrap_or_else(|| panic!("upload name")).to_owned());
            ResponseTemplate::new(201).set_body_json(json!({"access_proof":"obo_paired_upload","proof_id":Uuid::new_v4(),"expires_in":60,"expires_at":"2099-01-01T00:00:00Z"}))
        }).expect(4).mount(&iam).await;
    let operations = briefcase_client::OPERATIONS
        .iter()
        .map(|o| json!({"id":o.id,"version":o.version,"method":o.method,"path":o.path}))
        .collect::<Vec<_>>();
    Mock::given(path("/api/version")).and(header("x-testing-environment-key", "B".repeat(32)))
        .respond_with(ResponseTemplate::new(200).insert_header("briefcase-api-version","v1").set_body_json(json!({
            "service":"silicon-briefcase", "selected_api_version":"v1", "supported_api_versions":["v1"], "contract_version":"1.0.0", "build":"local-test", "operations":operations
        }))).expect(3).mount(&storage).await;
    let stored_names = uploads.clone();
    Mock::given(method("POST")).and(path("/api/v1/obo/files"))
        .and(header("x-testing-environment-key", "B".repeat(32)))
        .and(header("x-iam-obo-access-proof", "obo_paired_upload"))
        .and(header("x-app-id", "tos>waveform"))
        .respond_with(move |request: &wiremock::Request| {
            assert!(!request.headers.contains_key("authorization"));
            assert_eq!(request.body, expected_bytes);
            let names = stored_names.lock().unwrap_or_else(|_| panic!("upload lock"));
            let name = names.last().unwrap_or_else(|| panic!("upload must follow exchange"));
            ResponseTemplate::new(201).set_body_json(json!({
                "id":Uuid::new_v4(), "org_id":"tos", "type":"file", "visibility":"full", "name":name,
                "path":format!("private/actor/apps/tos>waveform/{name}"), "root_type":"private", "content_type":"audio/mpeg", "size":request.body.len(),
                "permanent_url":format!("https://briefcase.example.test/org/tos/private/actor/apps/tos>waveform/{name}"), "origin_app_id":"tos>waveform", "effective_access":["read"],
                "created_at":"2026-09-08T00:00:00Z", "updated_at":"2026-09-08T00:00:00Z", "deleted_at":null
            }))
        }).expect(2).mount(&storage).await;
    let read_names = uploads.clone();
    let list_proofs = read_ledger.clone();
    let replay_entry_id = Uuid::new_v4();
    Mock::given(path("/api/v1/obo/entries/list"))
        .and(header("x-testing-environment-key", "B".repeat(32)))
        .respond_with(move |request: &wiremock::Request| {
            consume_read_proof(&list_proofs, request, "briefcase.entries.list");
            let names = read_names.lock().unwrap_or_else(|_| panic!("names"));
            let name = &names[0];
            ResponseTemplate::new(200).set_body_json(json!({"items":[{
                "id":replay_entry_id,"org_id":"tos","type":"file","visibility":"full","name":name,
                "path":format!("private/actor/apps/tos>waveform/{name}"),"root_type":"private",
                "permanent_url":format!("https://briefcase.example.test/org/tos/private/actor/apps/tos>waveform/{name}"),
                "effective_access":["read"],"created_at":null,"updated_at":null,"deleted_at":null
            }],"next_cursor":null}))
        }).expect(1).mount(&storage).await;
    let file_proofs = read_ledger.clone();
    Mock::given(path("/api/v1/obo/files/read"))
        .and(header("x-testing-environment-key", "B".repeat(32)))
        .respond_with(move |request: &wiremock::Request| {
            consume_read_proof(&file_proofs, request, "briefcase.files.read");
            let body: Value =
                serde_json::from_slice(&request.body).unwrap_or_else(|_| panic!("read JSON"));
            assert_eq!(body["range"], "bytes=0-0");
            assert_eq!(body["entry_id"], replay_entry_id.to_string());
            ResponseTemplate::new(206)
                .insert_header("content-type", "audio/mpeg")
                .insert_header("content-range", "bytes 0-0/53000")
                .set_body_bytes(vec![73])
        })
        .expect(1)
        .mount(&storage)
        .await;
    let first_key = Uuid::new_v4().to_string();
    let second_key = Uuid::new_v4().to_string();
    let mut first_result = Value::Null;
    for (index, key) in [&first_key, &second_key, &first_key]
        .into_iter()
        .enumerate()
    {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/tts")
            .header("content-type", "application/json")
            .header("authorization", "Bearer oat_fixture")
            .header("x-org-id", "tos")
            .header("x-testing-environment-key", root)
            .header("idempotency-key", key)
            .body(Body::from(
                json!({"text":"use the deterministic fixture"}).to_string(),
            ))?;
        let response = app.clone().oneshot(request).await?;
        let status = response.status();
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["temporary_url"].is_null());
        if index == 0 {
            first_result = body.clone();
        }
        if index == 2 {
            assert_eq!(body, first_result);
        }
        assert!(body["file_url"].as_str().is_some_and(|url| url.starts_with(
            "https://briefcase.example.test/org/tos/private/actor/apps/tos%3Ewaveform/"
        )));
    }
    let names = uploads
        .lock()
        .unwrap_or_else(|_| panic!("upload lock"))
        .clone();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM waveform_jobs WHERE plane_id=$1 AND status='completed'",
    )
    .bind(
        created["id"]
            .as_str()
            .ok_or("missing plane")?
            .parse::<Uuid>()?,
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 2);
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

async fn cli_command(
    home: &std::path::Path,
    url: &str,
    args: &[&str],
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cli/target/debug/waveform");
    let output = tokio::process::Command::new(binary)
        .arg("--url")
        .arg(url)
        .args(args)
        .env("HOME", home)
        .env("WAVEFORM_AUTO_UPDATE", "off")
        .env_remove("WAVEFORM_ORG")
        .kill_on_drop(true)
        .output()
        .await?;
    Ok(output)
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL and cargo build --manifest-path cli/Cargo.toml"]
#[allow(clippy::too_many_lines)]
async fn real_cli_keeps_production_and_test_logins_separate() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = router(state);
    let actor = Uuid::new_v4();
    let plane_id = Uuid::new_v4();
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(move |request: &wiremock::Request| {
            let mut value = snapshot(actor);
            if request.headers.contains_key("x-testing-environment-key") {
                assert!(String::from_utf8_lossy(&request.body).contains("token=oat_test"));
                value["authorization"]["testing_environment_id"] = json!(plane_id);
            } else {
                assert!(!String::from_utf8_lossy(&request.body).contains("token=oat_test"));
            }
            ResponseTemplate::new(200).set_body_json(value)
        })
        .mount(&iam)
        .await;
    Mock::given(path("/api/v1/app-auth/tokens"))
        .respond_with(move |request: &wiremock::Request| {
            let plane = if request.headers.contains_key("x-testing-environment-key") { "test" } else { "prod" };
            assert!(String::from_utf8_lossy(&request.body).contains(plane));
            ResponseTemplate::new(200).set_body_json(json!({
                "access_token":format!("oat_{plane}"), "refresh_token":format!("ort_{plane}"), "token_type":"Bearer", "expires_in":1800,
                "scope":"roles.read memberships.read", "actor":{"principal_id":actor,"type":"carbon","public_id":"12345678"}, "org_id":"tos"
            }))
        }).mount(&iam).await;
    Mock::given(path("/api/v1/oauth/revoke"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .and(body_string_contains("token=oat_test"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&iam)
        .await;
    let (status, created) = call(&app, "POST", "/api/v1/testing-environments", Some("oat_fixture"), json!({
        "name":"cli-sessions", "iam_environment_id":plane_id, "iam_environment_key":"I".repeat(32),
        "app_secret":"test-environment-app-secret", "briefcase_environment_key":"B".repeat(32)
    })).await?;
    assert_eq!(status, StatusCode::OK);
    let key = created["key"].as_str().ok_or("root key missing")?;
    let id = created["id"].as_str().ok_or("plane ID missing")?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let home = std::env::temp_dir().join(format!("waveform-cli-http-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&home)?;
    for args in [
        vec!["login", "oac_prod"],
        vec!["me"],
        vec!["--test", key, "login", "oac_test"],
        vec!["--test", key, "me"],
        vec!["me"],
        vec!["--test", key, "refresh"],
        vec!["--organization", "tos", "--test", id, "me"],
        vec!["--test", key, "logout"],
        vec!["me"],
    ] {
        let mut json_args = vec!["--json"];
        json_args.extend(args);
        let result = cli_command(&home, &url, &json_args).await?;
        assert!(
            result.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let output: serde_json::Value = serde_json::from_slice(&result.stdout)?;
        assert!(
            output.is_object(),
            "--json must cover session commands, too"
        );
    }
    let logged_out = cli_command(&home, &url, &["--test", key, "me"]).await?;
    assert!(!logged_out.status.success());
    assert!(String::from_utf8_lossy(&logged_out.stderr).contains("not logged in"));
    assert!(home.join(".waveform/dir").is_dir());
    server.abort();
    std::fs::remove_dir_all(home)?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

// Proof ledger models IAM's exact-body, single-use contract. It intentionally
// checks the actual SDK request bytes, rather than reconstructing a DTO hash.
#[derive(Default)]
struct ReadLedger {
    proofs: std::collections::HashMap<String, (String, String)>,
    read_ranges: Vec<Option<String>>,
    denied: bool,
    invalid_audio: bool,
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL and cargo build --manifest-path cli/Cargo.toml"]
async fn paired_stt_and_replays_work_for_carbons_and_silicons() -> TestResult {
    for kind in ["carbon", "silicon"] {
        paired_read_roundtrip(kind).await?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    clippy::expect_used,
    reason = "mock protocol assertions contain no live data"
)]
async fn paired_read_roundtrip(kind: &str) -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let storage = MockServer::start().await;
    let mut state = fixture(pool.clone(), &iam)?;
    let settings = &mut Arc::get_mut(&mut state)
        .ok_or("shared state")?
        .briefcase_settings;
    settings.base_url = storage.uri().parse()?;
    settings.permanent_origin = "https://briefcase.example.test".parse()?;
    settings.cdn_origin = settings.permanent_origin.clone();
    crate::infrastructure::testing::seed_audio(&pool).await?;
    let app = speech_app(state)?;
    let actor = Uuid::new_v4();
    let iam_plane = Uuid::new_v4();
    let mut actor_snapshot = snapshot(actor);
    actor_snapshot["authorization"]["actor_type"] = json!(kind);
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(actor_snapshot.clone()))
        .mount(&iam)
        .await;
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/testing-environments",
        Some("oat_fixture"),
        json!({
            "name":format!("{kind}-roundtrip"), "iam_environment_id":iam_plane,
            "iam_environment_key":"I".repeat(32), "app_secret":"test-environment-app-secret",
            "briefcase_environment_key":"B".repeat(32)
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{created}");
    let root = created["key"].as_str().ok_or("missing root")?;
    actor_snapshot["authorization"]["testing_environment_id"] = json!(iam_plane);
    Mock::given(path("/api/v1/oauth/introspect"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(ResponseTemplate::new(200).set_body_json(actor_snapshot))
        .with_priority(1)
        .mount(&iam)
        .await;
    Mock::given(path("/api/v1/obo-access/applications/tos%3Ebriefcase/endpoints"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "application":{"app_id":"tos>briefcase","org_id":"tos"}, "endpoints":[
                {"endpoint_id":"briefcase.entries.list","path":"/api/v1/obo/entries/list","metadata":{}},
                {"endpoint_id":"briefcase.files.read","path":"/api/v1/obo/files/read","metadata":{}}
            ]
        }))).mount(&iam).await;
    let ledger = Arc::new(Mutex::new(ReadLedger::default()));
    let mint = ledger.clone();
    Mock::given(path("/api/v1/obo-access/exchanges"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("exchange JSON");
            assert_eq!(body["subject_token"], "oat_fixture");
            assert_eq!(body["metadata"], json!({}));
            assert_eq!(body["request"]["method"], "POST");
            assert_eq!(body["audience"], "tos>briefcase");
            assert!(request.headers.contains_key("x-obo-signature"));
            let proof = format!("obo_{}", Uuid::new_v4());
            mint.lock().expect("proof ledger").proofs.insert(proof.clone(), (
                body["endpoint_id"].as_str().expect("endpoint").to_owned(),
                body["request"]["body_sha256"].as_str().expect("digest").to_owned(),
            ));
            ResponseTemplate::new(201).set_body_json(json!({"access_proof":proof,"proof_id":Uuid::new_v4(),"expires_in":60,"expires_at":"2099-01-01T00:00:00Z"}))
        }).mount(&iam).await;
    let operations: Vec<_> = briefcase_client::OPERATIONS
        .iter()
        .map(|o| json!({"id":o.id,"version":o.version,"method":o.method,"path":o.path}))
        .collect();
    Mock::given(path("/api/version"))
        .and(header("x-testing-environment-key", "B".repeat(32)))
        .respond_with(ResponseTemplate::new(200).insert_header("briefcase-api-version", "v1")
            .set_body_json(json!({"service":"silicon-briefcase","selected_api_version":"v1","supported_api_versions":["v1"],"contract_version":"1.0.0","build":"local-test","operations":operations})))
        .mount(&storage).await;
    let entry_id = Uuid::new_v4();
    let source_url = "https://briefcase.example.test/org/tos/private/actor/source.wav";
    let entry = json!({"id":entry_id,"org_id":"tos","type":"file","visibility":"full","name":"source.wav","path":"private/actor/source.wav","root_type":"private","content_type":"audio/wav","size":16044,"permanent_url":source_url,"effective_access":["read"],"created_at":null,"updated_at":null,"deleted_at":null});
    let listing = ledger.clone();
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(header("x-testing-environment-key", "B".repeat(32)))
        .and(header("x-app-id", "tos>waveform"))
        .respond_with(move |request: &wiremock::Request| {
            consume_read_proof(&listing, request, "briefcase.entries.list");
            let body: Value = serde_json::from_slice(&request.body).expect("list JSON");
            assert_eq!(body["path"], "private/actor");
            // An empty first page still needs a fresh proof for the next page.
            if body["cursor"].is_null() {
                ResponseTemplate::new(200)
                    .set_body_json(json!({"items":[],"next_cursor":"second-page"}))
            } else {
                assert_eq!(body["cursor"], "second-page");
                ResponseTemplate::new(200)
                    .set_body_json(json!({"items":[entry],"next_cursor":null}))
            }
        })
        .mount(&storage)
        .await;
    let reading = ledger.clone();
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/files/read"))
        .and(header("x-testing-environment-key", "B".repeat(32)))
        .and(header("x-app-id", "tos>waveform"))
        .respond_with(move |request: &wiremock::Request| {
            consume_read_proof(&reading, request, "briefcase.files.read");
            let body: Value = serde_json::from_slice(&request.body).expect("read JSON");
            assert_eq!(body["entry_id"], entry_id.to_string());
            assert_eq!(body["download"], false, "preserve Briefcase media type");
            let mut state = reading.lock().expect("read ledger");
            state
                .read_ranges
                .push(body["range"].as_str().map(str::to_owned));
            if state.denied {
                return ResponseTemplate::new(404)
                    .set_body_json(json!({"error":{"code":"not_found","message":"not found"}}));
            }
            if body["range"].is_null() {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "audio/wav")
                    .set_body_bytes(if state.invalid_audio {
                        b"not audio".to_vec()
                    } else {
                        one_second_wav()
                    })
            } else {
                assert_eq!(body["range"], "bytes=0-0");
                ResponseTemplate::new(206)
                    .insert_header("content-type", "audio/wav")
                    .insert_header("content-range", "bytes 0-0/16044")
                    .set_body_bytes(vec![73])
            }
        })
        .mount(&storage)
        .await;
    let key = Uuid::new_v4().to_string();
    let send = |request_key: &str| {
        Request::builder()
            .method("POST")
            .uri("/api/v1/stt")
            .header("content-type", "application/json")
            .header("authorization", "Bearer oat_fixture")
            .header("x-org-id", "tos")
            .header("x-testing-environment-key", root)
            .header("idempotency-key", request_key)
            .body(Body::from(json!({"file_url":source_url}).to_string()))
    };
    let mut first = Value::Null;
    for attempt in 0..2 {
        let response = app.clone().oneshot(send(&key)?).await?;
        let status = response.status();
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
        assert_eq!(status, StatusCode::OK, "{kind} attempt {attempt}: {body}");
        assert_eq!(
            body["transcript"],
            crate::infrastructure::testing::TEST_STT_TEXT
        );
        assert_eq!(body["duration_ms"], 1000);
        if attempt == 0 {
            first = body;
        } else {
            assert_eq!(body, first);
        }
    }
    Mock::given(path("/api/v1/app-auth/tokens"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token":"oat_fixture","refresh_token":"ort_fixture","token_type":"Bearer","expires_in":1800,
            "scope":"roles.read memberships.read","actor":{"principal_id":actor,"type":kind,"public_id":"12345678"},"org_id":"tos"
        }))).expect(1).mount(&iam).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server_app = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, server_app).await });
    let home = std::env::temp_dir().join(format!("waveform-cli-storage-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&home)?;
    let actor_text = actor.to_string();
    let job_id = first["request_id"].as_str().ok_or("job id")?;
    for args in [
        vec!["--test", root, "login", "oac_fixture"],
        vec![
            "--test",
            root,
            "preferences",
            "--org",
            "tos",
            "--actor",
            &actor_text,
        ],
        vec![
            "--test",
            root,
            "provider-key-set",
            "--org",
            "tos",
            "--actor",
            &actor_text,
            "gemini",
            "fixture-personal-key",
        ],
        vec![
            "--test",
            root,
            "provider-keys",
            "--org",
            "tos",
            "--actor",
            &actor_text,
        ],
        vec![
            "--test",
            root,
            "provider-key-delete",
            "--org",
            "tos",
            "--actor",
            &actor_text,
            "gemini",
        ],
        vec![
            "--test",
            root,
            "stt",
            source_url,
            "--org",
            "tos",
            "--actor",
            &actor_text,
            "--idempotency",
            &key,
        ],
        vec![
            "--test",
            root,
            "jobs",
            "--org",
            "tos",
            "--actor",
            &actor_text,
            "--job-id",
            job_id,
            "--wait",
        ],
    ] {
        let result = cli_command(&home, &url, &args).await?;
        assert!(
            result.status.success(),
            "{kind} CLI failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        if args.contains(&"stt") {
            let body: Value = serde_json::from_slice(&result.stdout)?;
            assert_eq!(body, first);
        }
    }
    server.abort();
    std::fs::remove_dir_all(home)?;
    ledger.lock().expect("ledger").denied = true;
    let denied = app.clone().oneshot(send(&key)?).await?;
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    let denied_body = to_bytes(denied.into_body(), 65536).await?;
    assert!(
        !String::from_utf8_lossy(&denied_body)
            .contains(crate::infrastructure::testing::TEST_STT_TEXT)
    );
    {
        let mut state = ledger.lock().expect("ledger");
        state.denied = false;
        state.invalid_audio = true;
    }
    let invalid = app
        .clone()
        .oneshot(send(&Uuid::new_v4().to_string())?)
        .await?;
    assert_eq!(invalid.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let invalid_body: Value = serde_json::from_slice(&to_bytes(invalid.into_body(), 65536).await?)?;
    assert_eq!(invalid_body["error"]["code"], "unsupported_media_type");
    let invalid_id = invalid_body["error"]["request_id"]
        .as_str()
        .ok_or("failed job id")?
        .parse::<Uuid>()?;
    let failed: (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT status,error_code,provider FROM waveform_jobs WHERE id=$1")
            .bind(invalid_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        failed,
        (
            "failed".to_owned(),
            Some("unsupported_media_type".to_owned()),
            None
        )
    );
    let plane = created["id"].as_str().ok_or("plane")?.parse::<Uuid>()?;
    let row: (String, String, i64) = sqlx::query_as(
        "SELECT actor_kind,status,duration_ms FROM waveform_jobs WHERE plane_id=$1 AND id=$2",
    )
    .bind(plane)
    .bind(job_id.parse::<Uuid>()?)
    .fetch_one(&pool)
    .await?;
    assert_eq!(row, (kind.to_owned(), "completed".to_owned(), 1000));
    {
        let state = ledger.lock().expect("ledger");
        assert!(
            state.proofs.is_empty(),
            "every proof must be consumed exactly once"
        );
        assert_eq!(
            state.read_ranges,
            vec![
                None,
                Some("bytes=0-0".to_owned()),
                Some("bytes=0-0".to_owned()),
                Some("bytes=0-0".to_owned()),
                None
            ]
        );
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

#[allow(
    clippy::expect_used,
    reason = "mock protocol assertions contain no live data"
)]
fn consume_read_proof(ledger: &Mutex<ReadLedger>, request: &wiremock::Request, endpoint: &str) {
    assert!(!request.headers.contains_key("authorization"));
    let proof = request
        .headers
        .get("x-iam-obo-access-proof")
        .expect("proof")
        .to_str()
        .expect("proof header");
    let binding = ledger
        .lock()
        .expect("proof ledger")
        .proofs
        .remove(proof)
        .expect("fresh single-use proof");
    assert_eq!(binding.0, endpoint);
    assert_eq!(
        binding.1,
        silicon_iam_client::api::obo::body_sha256(&request.body)
    );
}

fn one_second_wav() -> Vec<u8> {
    let mut wav = Vec::with_capacity(16_044);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&16_036_u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&8_000_u32.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.resize(16_044, 0);
    wav
}
