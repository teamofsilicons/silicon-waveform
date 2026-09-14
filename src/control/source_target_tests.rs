//! Exact-request storage fixtures with real control-plane persistence.
use super::*;
use std::{collections::HashMap, sync::Mutex};

#[tokio::test]
#[ignore = "requires disposable WAVEFORM_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines, clippy::expect_used)]
async fn source_target_verifies_current_actor_world_and_actual_private_folder() -> TestResult {
    for testing in [false, true] {
        for kind in ["carbon", "silicon"] {
            let (pool, schema) = database().await?;
            let iam = MockServer::start().await;
            let storage = MockServer::start().await;
            let mut state = fixture(pool.clone(), &iam)?;
            let settings = &mut Arc::get_mut(&mut state)
                .ok_or("state shared")?
                .briefcase_settings;
            settings.base_url = storage.uri().parse()?;
            settings.permanent_origin = "https://briefcase.example.test".parse()?;
            let app = router(state);
            let actor = Uuid::new_v4();
            let authority = Arc::new(Mutex::new(snapshot(actor)));
            let current = authority.clone();
            Mock::given(path("/api/v1/oauth/introspect"))
                .respond_with(move |_: &wiremock::Request| {
                    ResponseTemplate::new(200)
                        .set_body_json(current.lock().expect("snapshot").clone())
                })
                .mount(&iam)
                .await;
            let world = testing.then(Uuid::new_v4);
            let root = if let Some(world) = world {
                let (status, created) = call(&app, "POST", "/api/v1/testing-environments", Some("oat_fixture"), json!({
                    "name":"source-target", "iam_environment_id":world, "iam_environment_key":"I".repeat(32),
                    "app_secret":"test-source-app-secret", "briefcase_environment_key":format!("ask_{}", "B".repeat(43))
                })).await?;
                assert_eq!(status, StatusCode::OK);
                Some(created["key"].as_str().ok_or("root missing")?.to_owned())
            } else {
                None
            };
            let mut active = snapshot(actor);
            active["authorization"]["testing_environment_id"] = json!(world);
            active["authorization"]["org_id"] = json!("client-workspace");
            active["authorization"]["actor_type"] = json!(kind);
            active["authorization"]["scopes"] =
                json!(["waveform.stt", "self.identity.read", "self.membership.read"]);
            *authority.lock().map_err(|_| "snapshot")? = active.clone();
            Mock::given(path("/api/v1/obo-access/applications/tos%3Ebriefcase/endpoints"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "application":{"app_id":"tos>briefcase","org_id":"tos"},
                    "endpoints":[{"critical":false,"endpoint_id":"briefcase.entries.list","path":"/api/v1/obo/entries/list","metadata":{}}]
                }))).mount(&iam).await;
            let ledger = Arc::new(Mutex::new(HashMap::<String, String>::new()));
            let issued = ledger.clone();
            Mock::given(path("/api/v1/obo-access/exchanges"))
                .respond_with(move |r: &wiremock::Request| {
                    let b: Value = serde_json::from_slice(&r.body).expect("exchange");
                    assert_eq!(b["org_id"], "client-workspace");
                    assert_eq!(b["subject_token"], "oat_fixture");
                    assert_eq!(b["audience"], "tos>briefcase");
                    assert_eq!(b["endpoint_id"], "briefcase.entries.list");
                    assert_eq!(b["request"]["method"], "POST");
                    assert!(r.headers.contains_key("x-obo-signature"));
                    assert_eq!(r.headers.contains_key("x-testing-environment-key"), testing);
                    let proof = format!("obo_{}", Uuid::new_v4());
                    issued.lock().expect("ledger").insert(proof.clone(), b["request"]["body_sha256"].as_str().expect("digest").to_owned());
                    ResponseTemplate::new(201).set_body_json(json!({"access_proof":proof,"proof_id":Uuid::new_v4(),"expires_in":60,"expires_at":"2099-01-01T00:00:00Z"}))
                }).mount(&iam).await;
            Mock::given(path("/api/version"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("briefcase-api-version", "v1")
                        .set_body_string(include_str!(
                            "../../tests/fixtures/briefcase-v1.1.0.json"
                        )),
                )
                .mount(&storage)
                .await;
            let folder_id = Uuid::new_v4();
            let folder = json!({"id":folder_id,"org_id":"client-workspace","type":"folder","visibility":"full","name":"12345678",
                "path":"apps/tos>waveform/private/12345678", "root_type":"private", "owner":{"type":kind,"id":"12345678"},
                "created_at":null,"updated_at":null,"deleted_at":null,"origin_app_id":"tos>waveform","effective_access":["read","write"],
                "permanent_url":"https://briefcase.example.test/org/client-workspace/apps/tos%3Ewaveform/private/12345678"});
            let returned = Arc::new(Mutex::new(folder.clone()));
            let entry = returned.clone();
            Mock::given(path("/api/v1/obo/entries/list"))
                .respond_with(move |r: &wiremock::Request| {
                    assert_eq!(r.headers.get("x-org-id").expect("org"), "client-workspace");
                    assert_eq!(r.headers.get("x-app-id").expect("app"), "tos>waveform");
                    assert!(!r.headers.contains_key("authorization"));
                    assert_eq!(r.headers.contains_key("x-briefcase-app-secret"), testing);
                    if testing {
                        assert_eq!(
                            r.headers.get("x-briefcase-app-secret").expect("selector"),
                            &format!("ask_{}", "B".repeat(43))
                        );
                    }
                    let proof = r
                        .headers
                        .get("x-iam-obo-access-proof")
                        .expect("proof")
                        .to_str()
                        .expect("proof text");
                    let digest = ledger
                        .lock()
                        .expect("ledger")
                        .remove(proof)
                        .expect("fresh proof");
                    assert_eq!(digest, silicon_iam_client::api::obo::body_sha256(&r.body));
                    let b: Value = serde_json::from_slice(&r.body).expect("list");
                    let items = if b["path"].is_null() {
                        json!([])
                    } else {
                        assert_eq!(b["path"], "apps/tos>waveform/private");
                        json!([entry.lock().expect("entry").clone()])
                    };
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"items":items,"next_cursor":null}))
                })
                .mount(&storage)
                .await;
            let invoke = || {
                let mut r = Request::builder()
                    .method("POST")
                    .uri("/api/v1/stt/source-target")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer oat_fixture")
                    .header("x-org-id", "client-workspace")
                    .header("idempotency-key", "source-target-stable-retry");
                if let Some(root) = &root {
                    r = r.header("x-testing-environment-key", root);
                }
                app.clone()
                    .oneshot(r.body(Body::from("{}")).expect("request"))
            };
            for _ in 0..2 {
                let response = invoke().await?;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "storage routes {:?}; IAM routes {:?}",
                    storage.received_requests().await.map(|r| r
                        .iter()
                        .map(|x| x.url.path().to_owned())
                        .collect::<Vec<_>>()),
                    iam.received_requests().await.map(|r| r
                        .iter()
                        .map(|x| x.url.path().to_owned())
                        .collect::<Vec<_>>())
                );
                assert_eq!(response.headers()[http::header::CACHE_CONTROL], "no-store");
                let data: Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
                assert_eq!(
                    data,
                    json!({"org_id":"client-workspace","app_id":"tos>waveform","actor_id":"12345678",
                    "folder_id":folder_id,"folder_path":"apps/tos>waveform/private/12345678","testing_environment_id":world})
                );
            }
            // Current authority is checked again on retries; stale snapshots never
            // become cached successful targets or invoke any storage operation.
            for (key, bad) in [
                ("scopes", json!(["self.identity.read"])),
                ("scopes", json!(["waveform.stt"])),
                ("public_id", Value::Null),
                ("actor_type", Value::Null),
                ("org_id", json!("other")),
                ("audience", json!("other>app")),
                ("testing_environment_id", json!(Uuid::new_v4())),
            ] {
                let before = storage.received_requests().await.ok_or("requests")?.len();
                let mut revoked = active.clone();
                revoked["authorization"][key] = bad;
                *authority.lock().map_err(|_| "snapshot")? = revoked;
                assert_eq!(invoke().await?.status(), StatusCode::FORBIDDEN);
                assert_eq!(
                    storage.received_requests().await.ok_or("requests")?.len(),
                    before
                );
            }
            *authority.lock().map_err(|_| "snapshot")? = active;
            for (key, bad) in [
                ("owner", json!({"type":kind,"id":"other"})),
                ("type", json!("file")),
                ("org_id", json!("other")),
                ("origin_app_id", json!("other>app")),
                ("root_type", json!("public")),
                ("effective_access", json!(["read"])),
                ("deleted_at", json!("2026-01-01T00:00:00Z")),
            ] {
                let mut invalid = folder.clone();
                invalid[key] = bad;
                *returned.lock().map_err(|_| "entry")? = invalid;
                assert_eq!(invoke().await?.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
                .execute(&pool)
                .await?;
            pool.close().await;
        }
    }
    Ok(())
}

#[tokio::test]
async fn source_target_rejects_ambiguous_headers_and_unknown_body_before_dependencies() -> TestResult
{
    let pool = PgPoolOptions::new().connect_lazy("postgres://localhost/unused")?;
    let iam = MockServer::start().await;
    let app = router(fixture(pool, &iam)?);
    for case in 0..4 {
        let mut r = Request::builder()
            .method("POST")
            .uri("/api/v1/stt/source-target")
            .header("content-type", "application/json")
            .header("authorization", "Bearer oat_fixture")
            .header("x-org-id", "client-workspace");
        if case != 0 {
            r = r.header("idempotency-key", "source-target-key");
        }
        if case == 1 {
            r = r.header("x-org-id", "other");
        }
        if case == 2 {
            r = r.header("x-iam-obo-access-proof", "obo_fixture");
        }
        let response = app
            .clone()
            .oneshot(r.body(Body::from(if case == 3 {
                "{\"folder_id\":\"caller-selected\"}"
            } else {
                "{}"
            }))?)
            .await?;
        assert!(response.status().is_client_error());
    }
    assert!(iam.received_requests().await.ok_or("requests")?.is_empty());
    Ok(())
}
