use super::*;
use crate::control::tests::{database, fixture, snapshot};
use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt as _;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) fn pair(endpoint: &str, org: &str, actor: &str, testing: bool, expired: bool) -> Value {
    json!({"grant_id":Uuid::new_v4(),"token_id":Uuid::new_v4(),"access_token":format!("oba_{endpoint}"),"refresh_token":format!("obr_{endpoint}"),"token_type":"Bearer","expires_in":1800,"expires_at":if expired {"2000-01-01T00:00:00Z"}else{"2099-01-01T00:00:00Z"},"audience":"briefcase","endpoint_id":endpoint,"org_id":org,"actor":{"type":if actor.starts_with("si:"){"silicon"}else{"carbon"},"public_id":actor},"scope":"","testing_context":testing.then(||json!({"app_id":"briefcase","app_secret":format!("ask_{}","B".repeat(43)),"iam_test_key":"I".repeat(32)}))})
}
async fn call(
    app: &axum::Router,
    verb: &str,
    route: &str,
    org: &str,
    body: Value,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(verb)
                .uri(route)
                .header("authorization", "Bearer oat_fixture")
                .header("x-org-id", org)
                .header("idempotency-key", "storage-consent-stable-key")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))?,
        )
        .await?;
    let status = response.status();
    let value = serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
    Ok((status, value))
}
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    clippy::expect_used,
    reason = "isolated integration fixtures with explicit assertions"
)]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn feature_consent_is_bound_encrypted_replayable_and_refreshes_without_login_scope()
-> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = crate::control::router(state.clone());
    let mut authority = snapshot(Uuid::new_v4());
    authority["authorization"]["scopes"] = json!(["self.identity.read"]);
    let current = std::sync::Arc::new(std::sync::Mutex::new(authority.clone()));
    let active = current.clone();
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(active.lock().expect("fixture").clone())
        })
        .mount(&iam)
        .await;
    let actor_id =
        crate::infrastructure::actor_keys::resolve(&pool, Uuid::nil(), "c:12345678").await?;
    let store = state.storage_grants().map_err(|_| "store")?;
    assert!(matches!(
        store
            .token(
                &state.iam,
                Uuid::nil(),
                "tos",
                actor_id,
                "briefcase",
                ENDPOINTS[0]
            )
            .await,
        Err(IamError::StorageAuthorizationRequired)
    ));
    let request_id = Uuid::new_v4();
    Mock::given(method("POST")).and(path("/api/v1/obo-access/authorizations")).respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":request_id,"app_id":"waveform","app_name":"Waveform","actor":{"type":"carbon","public_id":"c:12345678"},"org_id":"tos","status":"pending","version":1,"expires_at":"2099-01-01T00:00:00Z","endpoints":[],"authorization_url":format!("{}/obo-consent?authorization_id={request_id}",iam.uri())}))).expect(1).mount(&iam).await;
    let (status, started) = call(
        &app,
        "POST",
        "/api/v1/storage-authorizations",
        "tos",
        json!({}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/storage-authorizations",
            "tos",
            json!({})
        )
        .await?
        .1,
        started
    );
    let route = format!(
        "/api/v1/storage-authorizations/{}/complete",
        started["authorization_id"].as_str().ok_or("id")?
    );
    let body = json!({"code":"obc_approved","state":started["state"]});
    assert_eq!(
        call(
            &app,
            "POST",
            &route,
            "tos",
            json!({"code":"obc_approved","state":"wrong"})
        )
        .await?
        .0,
        StatusCode::FORBIDDEN
    );
    let exchanges = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = exchanges.clone();
    Mock::given(path("/api/v1/obo-access/tokens")).respond_with(move |request: &wiremock::Request| {
        let body: Value = serde_json::from_slice(&request.body).expect("body");
        recorded.lock().expect("exchange keys").push(request.headers["idempotency-key"].to_str().expect("key").to_owned());
        if body["authorization_code"] == "obc_mistyped" {
            ResponseTemplate::new(401).set_body_json(json!({"error":{"code":"invalid_grant","message":"Invalid code"}}))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({"items":ENDPOINTS.map(|endpoint|pair(endpoint,"storage-org","si:storage-agent",false,true))}))
        }
    }).expect(3).mount(&iam).await;
    for _ in 0..2 {
        let (status, rejected) = call(
            &app,
            "POST",
            &route,
            "tos",
            json!({"code":"obc_mistyped","state":started["state"]}),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(rejected["error"]["code"], "invalid_storage_authorization");
    }
    let (status, completed) = call(&app, "POST", &route, "tos", body.clone()).await?;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["status"], "completed");
    {
        let keys = exchanges.lock().map_err(|_| "exchange keys")?;
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0], keys[1], "identical code retries keep their key");
        assert_ne!(
            keys[0], keys[2],
            "correcting a code gets a separate redemption key"
        );
    }
    assert_eq!(call(&app, "POST", &route, "tos", body).await?.1, completed);
    assert_eq!(
        call(
            &app,
            "POST",
            &route,
            "tos",
            json!({"code":"obc_different","state":started["state"]})
        )
        .await?
        .0,
        StatusCode::FORBIDDEN
    );
    let ciphers: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT token_cipher FROM waveform_storage_grants")
            .fetch_all(&pool)
            .await?;
    assert_eq!(ciphers.len(), 4);
    for cipher in ciphers {
        assert!(!String::from_utf8_lossy(&cipher).contains("obr_"));
    }
    assert!(matches!(
        store
            .token(
                &state.iam,
                Uuid::new_v4(),
                "tos",
                actor_id,
                "briefcase",
                ENDPOINTS[0]
            )
            .await,
        Err(IamError::StorageAuthorizationRequired)
    ));
    assert!(matches!(
        store
            .token(
                &state.iam,
                Uuid::nil(),
                "other-org",
                actor_id,
                "briefcase",
                ENDPOINTS[0]
            )
            .await,
        Err(IamError::StorageAuthorizationRequired)
    ));
    assert!(matches!(
        store
            .token(
                &state.iam,
                Uuid::nil(),
                "tos",
                Uuid::new_v4(),
                "briefcase",
                ENDPOINTS[0]
            )
            .await,
        Err(IamError::StorageAuthorizationRequired)
    ));
    iam.reset().await;
    let keys = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = keys.clone();
    Mock::given(path("/api/v1/obo-access/tokens"))
        .respond_with(move |r: &wiremock::Request| {
            record.lock().expect("keys").push(
                r.headers["idempotency-key"]
                    .to_str()
                    .expect("header")
                    .to_owned(),
            );
            ResponseTemplate::new(503)
                .set_body_json(json!({"error":{"code":"unavailable","message":"retry"}}))
        })
        .expect(1)
        .mount(&iam)
        .await;
    assert!(matches!(
        store
            .token(
                &state.iam,
                Uuid::nil(),
                "tos",
                actor_id,
                "briefcase",
                ENDPOINTS[0]
            )
            .await,
        Err(IamError::Unavailable)
    ));
    iam.reset().await;
    let record = keys.clone();
    Mock::given(path("/api/v1/obo-access/tokens"))
        .respond_with(move |r: &wiremock::Request| {
            record.lock().expect("keys").push(
                r.headers["idempotency-key"]
                    .to_str()
                    .expect("header")
                    .to_owned(),
            );
            ResponseTemplate::new(200).set_body_json(
                json!({"items":[pair(ENDPOINTS[0],"storage-org","si:storage-agent",false,false)]}),
            )
        })
        .expect(1)
        .mount(&iam)
        .await;
    let (one, two) = tokio::join!(
        store.token(
            &state.iam,
            Uuid::nil(),
            "tos",
            actor_id,
            "briefcase",
            ENDPOINTS[0]
        ),
        store.token(
            &state.iam,
            Uuid::nil(),
            "tos",
            actor_id,
            "briefcase",
            ENDPOINTS[0]
        )
    );
    assert_eq!(one?.org_id, "storage-org");
    assert_eq!(two?.actor.ok_or("actor")?.public_id, "si:storage-agent");
    {
        let keys = keys.lock().map_err(|_| "keys")?;
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], keys[1]);
    }
    iam.reset().await;
    Mock::given(path("/api/v1/obo-access/tokens"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"error":{"code":"obo_access_token_invalid","message":"revoked"}}),
        ))
        .expect(1)
        .mount(&iam)
        .await;
    for _ in 0..2 {
        assert!(matches!(
            store
                .token(
                    &state.iam,
                    Uuid::nil(),
                    "tos",
                    actor_id,
                    "briefcase",
                    ENDPOINTS[1]
                )
                .await,
            Err(IamError::StorageAuthorizationRequired)
        ));
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}
