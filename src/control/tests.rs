//! Local PostgreSQL + HTTP tests for the durable control API.

use super::*;
use crate::application::ports::ProviderKeyStore as _;
use axum::body::{Body, to_bytes};
use hmac::{Hmac, Mac as _};
use http::Request;
use secrecy::SecretString;
use serde_json::Value;
use sha2::Sha256;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const SECRET: &str = "test-only-waveform-webhook-secret-123456";

async fn database() -> Result<(PgPool, String), Box<dyn std::error::Error>> {
    let url = std::env::var("WAVEFORM_TEST_DATABASE_URL")?;
    // Identifier is generated exclusively from a UUID, with no user input.
    let schema = format!("control_{}", Uuid::new_v4().simple());
    let admin = PgPool::connect(&url).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await?;
    admin.close().await;
    let search = format!("SET search_path TO {schema}");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .after_connect(move |connection, _| {
            let search = search.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(search))
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    Ok((pool, schema))
}

fn fixture(
    pool: PgPool,
    iam: &MockServer,
) -> Result<Arc<ControlState>, Box<dyn std::error::Error>> {
    Ok(Arc::new(ControlState {
        tts_scope: "obo:tos>briefcase:briefcase.files.create".into(),
        stt_scope: "obo:tos>briefcase:briefcase.files.read".into(),
        mail: None,
        station: None,
        pool,
        iam: Client::builder(&iam.uri())?
            .credential(Credential::application("tos>waveform", "test-app-secret"))
            .auto_update(false)
            .build()?,
        app_id: "tos>waveform".to_owned(),
        briefcase_settings: crate::config::BriefcaseSettings {
            base_url: iam.uri().parse()?,
            permanent_origin: iam.uri().parse()?,
            cdn_origin: iam.uri().parse()?,
            app_id: "tos>waveform".to_owned(),
            audience: "tos>briefcase".to_owned(),
            timeout: std::time::Duration::from_secs(5),
            download_timeout: std::time::Duration::from_secs(5),
            max_download_bytes: 1_000_000,
        },
        iam_timeout: std::time::Duration::from_secs(5),
        vault: Some(Vault::new(&SecretString::from("19".repeat(32)))?),
        verifier: Some(WebhookVerifier::new(WebhookSecretKeyring::new(
            1,
            WebhookSecret::new(SECRET)?,
        )?)),
    }))
}

async fn call(
    app: &Router,
    verb: &str,
    route: &str,
    token: Option<&str>,
    body: Value,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let mut request = Request::builder()
        .method(verb)
        .uri(route)
        .header("content-type", "application/json")
        .header("x-org-id", "tos");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string()))?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    Ok((
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        },
    ))
}

fn snapshot(actor: Uuid) -> Value {
    json!({"active": true, "authorization": {
        "principal_id": actor, "actor_type": "carbon", "public_id": "12345678",
        "organization_id": Uuid::from_u128(2), "org_id": "tos", "membership_id": Uuid::from_u128(3),
        "membership_version": 1, "authorization_epoch": 1, "audience": "tos>waveform",
        "testing_environment_id": null, "scopes": ["roles.read","memberships.read"], "org_role": "member", "tags": []
    }})
}

#[tokio::test]
async fn iam_discovery_is_public_and_exposes_only_public_configuration() -> TestResult {
    // Discovery in production needs neither a database connection nor an IAM call.
    let pool = PgPoolOptions::new().connect_lazy("postgres://localhost/unused")?;
    let iam = MockServer::start().await;
    let app = router(fixture(pool, &iam)?);
    let response = app
        .oneshot(Request::builder().uri("/api/v1/iam").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[http::header::CACHE_CONTROL], "no-store");
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
    assert_eq!(
        body,
        json!({
            "app_id":"tos>waveform", "iam_base_url":format!("{}/", iam.uri()),
            "testing_environment_id":null
        })
    );
    assert!(
        iam.received_requests()
            .await
            .ok_or("requests unavailable")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn session_mutations_reject_invalid_or_duplicate_keys_before_iam() -> TestResult {
    let pool = PgPoolOptions::new().connect_lazy("postgres://localhost/unused")?;
    let iam = MockServer::start().await;
    let app = router(fixture(pool, &iam)?);
    for (route, body) in [
        ("login", json!({"slt":"oac_fixture"})),
        ("refresh", json!({"refresh_token":"ort_fixture"})),
        ("logout", json!({"token":"ort_fixture"})),
    ] {
        for key in [
            "short".to_owned(),
            "a".repeat(256),
            "contains a space".to_owned(),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/auth/{route}"))
                        .header("content-type", "application/json")
                        .header("idempotency-key", key)
                        .body(Body::from(body.to_string()))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/auth/{route}"))
                    .header("content-type", "application/json")
                    .header("idempotency-key", "first-operation-key")
                    .header("idempotency-key", "second-operation-key")
                    .body(Body::from(body.to_string()))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(
        iam.received_requests()
            .await
            .ok_or("requests unavailable")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn session_retries_preserve_the_iam_receipt_in_both_planes() -> TestResult {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let app = router(fixture(pool.clone(), &iam)?);
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(Uuid::new_v4())))
        .mount(&iam)
        .await;
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/testing-environments",
        Some("oat_fixture"),
        json!({
            "name":"session-retries", "iam_environment_id":Uuid::new_v4(),
            "iam_environment_key":"I".repeat(32), "app_secret":"test-environment-app-secret",
            "briefcase_environment_key":"B".repeat(32)
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let root = created["key"].as_str().ok_or("environment key missing")?;
    for testing in [false, true] {
        for (route, body, upstream, success) in [
            (
                "login",
                json!({"slt":"oac_fixture"}),
                "/api/v1/app-auth/tokens",
                StatusCode::OK,
            ),
            (
                "refresh",
                json!({"refresh_token":"ort_fixture"}),
                "/api/v1/app-auth/tokens",
                StatusCode::OK,
            ),
            (
                "logout",
                json!({"token":"ort_fixture"}),
                "/api/v1/oauth/revoke",
                StatusCode::NO_CONTENT,
            ),
        ] {
            iam.reset().await;
            let key = format!(
                "session-{route}-{}-receipt",
                if testing { "test" } else { "production" }
            );
            let attempts = Arc::new(AtomicUsize::new(0));
            let counter = attempts.clone();
            let response = if success == StatusCode::NO_CONTENT {
                ResponseTemplate::new(204)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "access_token":"oat_saved", "refresh_token":"ort_saved", "token_type":"Bearer",
                    "expires_in":1800, "scope":"self.identity.read", "org_id":null,
                    "actor":{"principal_id":Uuid::new_v4(),"type":"carbon","public_id":"tester"}
                }))
            };
            // IAM has processed the operation, but the first response is unavailable.
            // A repeat with the same key returns its saved receipt.
            Mock::given(method("POST"))
                .and(path(upstream))
                .and(header("idempotency-key", key.as_str()))
                .respond_with(move |_: &wiremock::Request| {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        ResponseTemplate::new(502).set_body_json(
                            json!({"error":{"code":"unavailable","message":"response lost"}}),
                        )
                    } else {
                        response.clone()
                    }
                })
                .expect(2)
                .mount(&iam)
                .await;
            for expected in [StatusCode::SERVICE_UNAVAILABLE, success] {
                let mut request = Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/auth/{route}"))
                    .header("content-type", "application/json")
                    .header("idempotency-key", &key);
                if testing {
                    request = request.header("x-testing-environment-key", root);
                }
                let result = app
                    .clone()
                    .oneshot(request.body(Body::from(body.to_string()))?)
                    .await?;
                assert_eq!(result.status(), expected, "{route}, testing={testing}");
                if expected == StatusCode::OK {
                    let tokens: Value =
                        serde_json::from_slice(&to_bytes(result.into_body(), 65536).await?)?;
                    assert_eq!(tokens["refresh_token"], "ort_saved");
                }
            }
            let requests = iam
                .received_requests()
                .await
                .ok_or("requests unavailable")?;
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].body, requests[1].body);
            assert_eq!(requests[0].headers, requests[1].headers);
            assert_eq!(
                requests[0]
                    .headers
                    .contains_key("x-testing-environment-key"),
                testing
            );
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            iam.verify().await;
        }
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn login_settings_secrets_webhooks_and_online_revocation() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = router(state.clone());
    let actor = Uuid::new_v4();
    Mock::given(method("POST")).and(path("/api/v1/app-auth/tokens"))
        .and(header("authorization", "Basic dG9zPndhdmVmb3JtOnRlc3QtYXBwLXNlY3JldA=="))
        .and(body_string_contains("slt=oac_fixture"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "oat_fixture", "refresh_token": "ort_fixture", "token_type": "Bearer",
            "expires_in": 1800, "scope": "roles.read memberships.read", "actor": {"principal_id": actor,"type":"carbon","public_id":"12345678"}, "org_id": "tos"
        }))).expect(1).mount(&iam).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/oauth/introspect"))
        .and(body_string_contains("token=oat_fixture"))
        .and(header("x-org-id", "tos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(actor)))
        .mount(&iam)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/oauth/introspect"))
        .and(body_string_contains("token=oat_other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(Uuid::new_v4())))
        .mount(&iam)
        .await;

    let (status, tokens) = call(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({"slt":"oac_fixture"}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tokens["access_token"], "oat_fixture");
    let assigned: String = sqlx::query_scalar("SELECT voice_profile FROM waveform_account_preferences WHERE plane_id=$1 AND org_id='tos' AND actor_id=$2").bind(Uuid::nil()).bind(actor).fetch_one(&pool).await?;
    assert_eq!(assigned, "kore");
    let (status, catalog) = call(
        &app,
        "GET",
        "/api/v1/voice-profiles",
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["items"].as_array().ok_or("catalog")?.len(), 30);
    assert_eq!(
        call(&app, "GET", "/api/v1/voice-profiles", None, Value::Null)
            .await?
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, changed) = call(
        &app,
        "PATCH",
        "/api/v1/preferences",
        Some("oat_fixture"),
        json!({"voice_profile":"puck"}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["voice_profile"], "puck");
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/preferences",
            Some("oat_fixture"),
            json!({"voice_profile":"missing"})
        )
        .await?
        .0,
        StatusCode::BAD_REQUEST
    );

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({"slt":"ort_refresh-is-not-login"}),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        call(&app, "GET", "/api/v1/preferences", None, Value::Null)
            .await?
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, prefs) = call(
        &app,
        "GET",
        "/api/v1/preferences",
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(
        prefs["tts_order"],
        json!(["gemini", "elevenlabs", "openai"])
    );
    let (status, prefs) = call(
        &app,
        "PATCH",
        "/api/v1/preferences",
        Some("oat_fixture"),
        json!({"tts_order":["openai"]}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        prefs["tts_order"],
        json!(["openai", "gemini", "elevenlabs"])
    );
    let (_, other) = call(
        &app,
        "GET",
        "/api/v1/preferences",
        Some("oat_other"),
        Value::Null,
    )
    .await?;
    assert_eq!(other["tts_order"][0], "gemini");
    assert_eq!(other["voice_profile"], "kore");
    assert_eq!(prefs["voice_profile"], "puck");
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/preferences",
            Some("oat_fixture"),
            json!({"tts_order":["deepgram"]})
        )
        .await?
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v1/provider-keys/openai",
            Some("oat_fixture"),
            json!({"api_key":"personal-test-secret"})
        )
        .await?
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, keys) = call(
        &app,
        "GET",
        "/api/v1/provider-keys",
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(keys["items"][0]["provider"], "openai");
    assert!(!keys.to_string().contains("personal-test-secret"));
    let cipher: Vec<u8> =
        sqlx::query_scalar("SELECT secret_cipher FROM waveform_provider_keys WHERE actor_id=$1")
            .bind(actor)
            .fetch_one(&pool)
            .await?;
    assert!(!String::from_utf8_lossy(&cipher).contains("personal-test-secret"));
    let authorized = AuthorizedActor {
        actor: Actor::new(ActorKind::Carbon, ActorId::new(actor)?),
        organization_id: "tos".parse()?,
        originating_application: None,
        expires_at: None,
    };
    let loaded = state.load(Uuid::nil(), &authorized).await?;
    assert_eq!(
        loaded
            .get(&ProviderName::OpenAi)
            .ok_or("personal key missing")?
            .expose_secret(),
        "personal-test-secret"
    );
    assert!(state.load(Uuid::new_v4(), &authorized).await?.is_empty());
    let mut other = authorized.clone();
    other.organization_id = "other".parse()?;
    assert!(state.load(Uuid::nil(), &other).await?.is_empty());
    other = authorized.clone();
    other.actor = Actor::new(ActorKind::Carbon, ActorId::new(Uuid::new_v4())?);
    assert!(state.load(Uuid::nil(), &other).await?.is_empty());

    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/provider-keys",
            Some("oat_other"),
            Value::Null
        )
        .await?
        .1["items"],
        json!([])
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            "/api/v1/provider-keys/openai",
            Some("oat_fixture"),
            Value::Null
        )
        .await?
        .0,
        StatusCode::NO_CONTENT
    );

    assert!(state.load(Uuid::nil(), &authorized).await?.is_empty());

    let id = Uuid::new_v4();
    let event = json!({"spec_version":"1.0","event_id":id,"event_type":"session.revoked.v1","occurred_at":"2026-09-07T00:00:00Z","organization_id":Uuid::from_u128(2),"aggregate":{"type":"session","id":Uuid::from_u128(4),"version":1},"data":{"private":"not-persisted"}}).to_string();
    assert_eq!(
        webhook(&app, id, &event, false).await?,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        webhook(&app, id, &event, false).await?,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        webhook(&app, id, &event, true).await?,
        StatusCode::UNAUTHORIZED
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM waveform_webhook_events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 1);
    iam.reset().await;
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"active":false})))
        .mount(&iam)
        .await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/preferences",
            Some("oat_fixture"),
            Value::Null
        )
        .await?
        .0,
        StatusCode::UNAUTHORIZED
    );
    // Unknown test roots are never sent to production IAM.
    let request = Request::builder()
        .uri("/api/v1/auth/me")
        .header("authorization", "Bearer oat_fixture")
        .header("x-org-id", "tos")
        .header("x-testing-environment-key", "A".repeat(32))
        .body(Body::empty())?;
    assert_eq!(
        app.oneshot(request).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

async fn webhook(
    app: &Router,
    id: Uuid,
    body: &str,
    tamper: bool,
) -> Result<StatusCode, Box<dyn std::error::Error>> {
    use std::fmt::Write as _;
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
    let mut mac = <Hmac<Sha256> as hmac::Mac>::new_from_slice(SECRET.as_bytes())?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    let mut signature = String::from("v1=");
    for byte in mac.finalize().into_bytes() {
        write!(signature, "{byte:02x}")?;
    }
    let body = if tamper {
        format!("{body} ")
    } else {
        body.to_owned()
    };
    let request = Request::builder()
        .method("POST")
        .uri("/webhooks/")
        .header("x-silicon-iam-event-id", id.to_string())
        .header("x-silicon-iam-timestamp", timestamp)
        .header("x-silicon-iam-key-version", "1")
        .header("x-silicon-iam-signature", signature)
        .body(Body::from(body))?;
    Ok(app.clone().oneshot(request).await?.status())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn sandbox_creation_and_atomic_clean_preserve_the_environment() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = router(state.clone());
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(Uuid::new_v4())))
        .mount(&iam)
        .await;
    let iam_environment_id = Uuid::new_v4();
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/testing-environments",
        Some("oat_fixture"),
        json!({
            "name":"local-sandbox", "iam_environment_id":iam_environment_id,
            "iam_environment_key":"I".repeat(32), "app_secret":"test-environment-app-secret", "briefcase_environment_key":format!("ask_{}","B".repeat(43))
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let key = created["key"].as_str().ok_or("environment key missing")?;
    assert_eq!(key.len(), 32);
    for (root, expected) in [
        (key.to_owned(), StatusCode::OK),
        ("A".repeat(32), StatusCode::UNAUTHORIZED),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/iam")
                    .header("x-testing-environment-key", &root)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let value: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
            assert_eq!(
                value,
                json!({"app_id":"tos>waveform", "iam_base_url":format!("{}/", iam.uri()), "testing_environment_id":iam_environment_id})
            );
        }
    }
    let id: Uuid = created["id"]
        .as_str()
        .ok_or("environment id missing")?
        .parse()?;
    let defaults: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_provider_defaults WHERE plane_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(defaults, 1);
    let (status, environments) = call(
        &app,
        "GET",
        "/api/v1/testing-environments",
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(
        environments["items"]
            .as_array()
            .is_some_and(|items| { items.iter().any(|item| item["id"] == id.to_string()) })
    );
    let (status, detail) = call(
        &app,
        "GET",
        &format!("/api/v1/testing-environments/{id}"),
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["id"], id.to_string());
    let (status, retrieved) = call(
        &app,
        "GET",
        &format!("/api/v1/testing-environments/{id}/key"),
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retrieved["key"], key);
    let (status, rotated) = call(
        &app,
        "POST",
        &format!("/api/v1/testing-environments/{id}/rotate-key"),
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let rotated_key = rotated["key"].as_str().ok_or("rotated key missing")?;
    assert_ne!(rotated_key, key);
    let old_key_request = Request::builder()
        .method("GET")
        .uri("/api/v1/testing-environment")
        .header("x-testing-environment-key", key)
        .body(Body::empty())?;
    assert_eq!(
        app.clone().oneshot(old_key_request).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/testing-environments/{id}/delete"),
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/testing-environments/{id}/key"),
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/testing-environments/{id}/restore"),
        Some("oat_fixture"),
        Value::Null,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let mut clean_snapshot = snapshot(Uuid::new_v4());
    clean_snapshot["authorization"]["testing_environment_id"] =
        created["iam_environment_id"].clone();
    let iam_id: Uuid =
        sqlx::query_scalar("SELECT iam_environment_id FROM waveform_environments WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    clean_snapshot["authorization"]["testing_environment_id"] = json!(iam_id);
    clean_snapshot["authorization"]["org_role"] = json!("owner");
    Mock::given(path("/api/v1/oauth/introspect"))
        .and(header("x-testing-environment-key", "I".repeat(32)))
        .respond_with(ResponseTemplate::new(200).set_body_json(clean_snapshot))
        .with_priority(1)
        .mount(&iam)
        .await;
    for route in [
        "/api/v1/testing-environment",
        "/api/v1/testing-environment/clean",
    ] {
        let method = if route.ends_with("/clean") {
            "POST"
        } else {
            "GET"
        };
        let request = Request::builder()
            .method(method)
            .uri(route)
            .header("x-testing-environment-key", rotated_key)
            .header("x-org-id", "tos")
            .header("authorization", "Bearer oat_fixture")
            .body(Body::empty())?;
        assert!(app.clone().oneshot(request).await?.status().is_success());
    }
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_environments WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(remaining, 1);

    sqlx::query(
        "UPDATE waveform_environments SET last_activity_at=now() - interval '16 days' WHERE id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await?;
    assert_eq!(state.cleanup_environments().await?, 1);
    let deleted: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT deleted_at FROM waveform_environments WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert!(deleted.is_some());
    sqlx::query(
        "UPDATE waveform_environments SET deleted_at=now() - interval '31 days' WHERE id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO waveform_idempotency_records (plane_id,actor_type,actor_id,org_id,operation,idempotency_key,request_digest,request_id,state,lease_token,lease_expires_at,expires_at) VALUES ($1,'carbon',$2,'tos','tts','purge-key',decode(repeat('a',64),'hex'),$3,'pending',$4,now() + interval '1 hour',now() + interval '2 hours')")
        .bind(id)
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await?;
    assert_eq!(state.cleanup_environments().await?, 1);
    let purged: i64 = sqlx::query_scalar("SELECT count(*) FROM waveform_environments WHERE id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(purged, 0);
    let orphaned_idempotency: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_idempotency_records WHERE plane_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(orphaned_idempotency, 0);
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/testing-environment/clean")
        .body(Body::empty())?;
    assert_eq!(
        app.oneshot(request).await?.status(),
        StatusCode::BAD_REQUEST
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

#[path = "speech_tests.rs"]
mod speech_tests;

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn voice_profiles_isolate_accounts_organizations_and_test_catalogs() -> TestResult {
    use crate::application::ports::VoiceProfileStore as _;
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let actor = AuthorizedActor {
        actor: Actor::new(ActorKind::Silicon, ActorId::new(Uuid::new_v4())?),
        organization_id: "tos".parse()?,
        originating_application: None,
        expires_at: None,
    };
    let baseline = state.resolve(Uuid::nil(), &actor, None).await?;
    assert_eq!(baseline.id, "kore");
    let plane = Uuid::new_v4();
    sqlx::query("INSERT INTO waveform_environments(id,org_id,creator_id,name,root_key_hash,root_key_cipher,iam_key_cipher,briefcase_key_cipher,app_secret_cipher,iam_environment_id) VALUES($1,'tos',$2,'voice test',$3,$3,$3,$3,$3,$1)")
        .bind(plane).bind(actor.actor.id.as_uuid()).bind(plane.as_bytes().to_vec()).execute(&pool).await?;
    sqlx::query("INSERT INTO waveform_voice_profiles SELECT $1,id,profile FROM waveform_voice_profiles WHERE plane_id=$2").bind(plane).bind(Uuid::nil()).execute(&pool).await?;
    sqlx::query("INSERT INTO waveform_provider_defaults(plane_id,tts_order,stt_order,voice_profile) SELECT $1,tts_order,stt_order,'puck' FROM waveform_provider_defaults WHERE plane_id=$2").bind(plane).bind(Uuid::nil()).execute(&pool).await?;
    let test_default = state.resolve(plane, &actor, None).await?;
    assert_eq!(test_default.id, "puck");
    assert_eq!(
        state.resolve(plane, &actor, Some("sulafat")).await?.id,
        "sulafat"
    );
    assert_eq!(state.resolve(plane, &actor, None).await?.id, "puck");
    assert_eq!(state.resolve(Uuid::nil(), &actor, None).await?, baseline);
    // Catalog tuning in a test environment never changes production.
    sqlx::query("UPDATE waveform_voice_profiles SET profile=jsonb_set(profile,'{openai_voice}',to_jsonb('shimmer'::text)) WHERE plane_id=$1 AND id='kore'").bind(plane).execute(&pool).await?;
    assert_eq!(
        state
            .resolve(plane, &actor, Some("kore"))
            .await?
            .openai_voice,
        "shimmer"
    );
    assert_eq!(
        state.resolve(plane, &actor, Some("kore")).await?.revision,
        2
    );
    assert_eq!(state.resolve(Uuid::nil(), &actor, None).await?, baseline);
    // Same principal in a second organization gets an independent default.
    sqlx::query("UPDATE waveform_account_preferences SET voice_profile='sulafat' WHERE plane_id=$1 AND org_id='tos' AND actor_id=$2").bind(Uuid::nil()).bind(actor.actor.id.as_uuid()).execute(&pool).await?;
    let other = AuthorizedActor {
        organization_id: "other".parse()?,
        ..actor.clone()
    };
    assert_eq!(state.resolve(Uuid::nil(), &other, None).await?.id, "kore");
    assert_eq!(
        state.resolve(Uuid::nil(), &actor, None).await?.id,
        "sulafat"
    );
    assert!(state.resolve(plane, &actor, Some("missing")).await.is_err());
    sqlx::query("UPDATE waveform_voice_profiles SET profile=jsonb_set(profile,'{elevenlabs,voice_settings,speed}', '9') WHERE plane_id=$1 AND id='kore'").bind(plane).execute(&pool).await?;
    assert!(matches!(
        state.resolve(plane, &actor, Some("kore")).await,
        Err(crate::domain::error::WaveformError::DependencyUnavailable { .. })
    ));
    // Environment cleanup removes the choice; next use reassigns that plane's default.
    sqlx::query("DELETE FROM waveform_account_preferences WHERE plane_id=$1")
        .bind(plane)
        .execute(&pool)
        .await?;
    assert_eq!(state.resolve(plane, &actor, None).await?.id, "puck");
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn unscoped_identity_uses_iam_workspace_and_rejects_wrong_audience() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = router(state);
    let mut authority = snapshot(Uuid::new_v4())["authorization"].clone();
    authority["org_id"] = json!("workspace");
    for (token, audience, expected) in [
        ("oat_unscoped", "tos>waveform", StatusCode::OK),
        ("oat_wrong", "tos>other", StatusCode::FORBIDDEN),
    ] {
        authority["audience"] = json!(audience);
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .and(body_string_contains(format!("token={token}")))
            .and(|request: &wiremock::Request| !request.headers.contains_key("x-org-id"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"active":true,"authorizations":[authority.clone()]})),
            )
            .expect(1)
            .mount(&iam)
            .await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let value: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
            assert_eq!(value["org_id"], "workspace");
        }
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

async fn mock_discovery(
    iam: &MockServer,
    id: Uuid,
    secret: &str,
    version: i64,
    cleaned: Option<&str>,
) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    Mock::given(path("/api/v1/application/testing-context"))
      .and(header("x-testing-application",format!("Basic {}",STANDARD.encode(format!("tos>waveform:{secret}")))))
      .respond_with(ResponseTemplate::new(200).set_body_json(json!({"environment_id":id,"application":{"app_id":"tos>waveform","base_url":"https://backend.waveform.example","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},"environment":{"environment_id":id,"org_id":"tos","name":format!("Sandbox {version}"),"version":version,"key_generation":1,"cleaned_at":cleaned,"created_at":"2026-09-01T00:00:00Z","creator_type":"carbon","creator_id":"alice"},"webhook_key_digest":"00".repeat(32)}))).mount(iam).await;
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn app_secret_discovery_revalidates_isolates_and_cleans() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let id = Uuid::new_v4();
    let secret = format!("ask_{}", "a".repeat(43));
    mock_discovery(&iam, id, &secret, 1, None).await;
    assert_eq!(
        state
            .discover_plane(&secret)
            .await
            .map_err(|_| "discovery failed")?
            .id,
        id
    );
    let credentials: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT iam_key_cipher,briefcase_key_cipher FROM waveform_environments WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    assert!(credentials.0.is_empty() && credentials.1.is_empty());
    let actor = Uuid::new_v4();
    for plane in [Uuid::nil(), id] {
        sqlx::query("INSERT INTO waveform_account_preferences(plane_id,org_id,actor_id) VALUES($1,'tos',$2)").bind(plane).bind(actor).execute(&pool).await?;
    }
    iam.reset().await;
    mock_discovery(&iam, id, &secret, 2, Some("2026-09-13T00:00:00Z")).await;
    state
        .discover_plane(&secret)
        .await
        .map_err(|_| "clean discovery failed")?;
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_account_preferences WHERE plane_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(remaining, 0);
    let production: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_account_preferences WHERE plane_id=$1")
            .bind(Uuid::nil())
            .fetch_one(&pool)
            .await?;
    assert_eq!(production, 1);
    iam.reset().await;
    mock_discovery(&iam, id, &secret, 1, None).await;
    assert!(state.discover_plane(&secret).await.is_err());
    iam.reset().await;
    Mock::given(path("/api/v1/application/testing-context"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&iam)
        .await;
    assert!(state.discover_plane(&secret).await.is_err());
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

async fn sandbox_call(
    app: &Router,
    secret: &str,
    route: &str,
    body: Value,
    key: &str,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(route)
                .header("content-type", "application/json")
                .header("x-testing-environment-key", secret)
                .header("x-org-id", "tos")
                .header("authorization", "Bearer oat_sandbox")
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65536).await?;
    Ok((
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)))
        },
    ))
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn discovered_identity_reports_permissions_and_webhooks_are_isolated() -> TestResult {
    use sha2::Digest as _;
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = router(state.clone());
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let secret = format!("ask_{}", "R".repeat(43));
    mock_discovery(&iam, id, &secret, 1, None).await;
    let mut authority = snapshot(actor);
    authority["authorization"]["testing_environment_id"] = json!(id);
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(authority.clone()))
        .mount(&iam)
        .await;
    Mock::given(path("/api/v1/app-auth/tokens")).and(body_string_contains("slt=test-carbon"))
      .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token":"oat_sandbox","refresh_token":"ort_sandbox","token_type":"Bearer","expires_in":1800,"scope":"roles.read","actor":{"principal_id":actor,"type":"carbon","public_id":"test-carbon"},"org_id":"tos"}))).mount(&iam).await;
    Mock::given(path("/api/v1/app-auth/tokens"))
        .and(body_string_contains("slt=inactive"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error":"invalid_grant"})))
        .mount(&iam)
        .await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/login",
            None,
            json!({"slt":"test-carbon"})
        )
        .await?
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        sandbox_call(
            &app,
            &secret,
            "/api/v1/auth/login",
            json!({"slt":"test-carbon"}),
            "login"
        )
        .await?
        .0,
        StatusCode::OK
    );
    assert!(
        !sandbox_call(
            &app,
            &secret,
            "/api/v1/auth/login",
            json!({"slt":"inactive"}),
            "login"
        )
        .await?
        .0
        .is_success()
    );
    // The secret does not grant the missing speech scope or local lifecycle control.
    let mut headers = HeaderMap::new();
    headers.insert("x-testing-environment-key", secret.parse()?);
    headers.insert("x-org-id", "tos".parse()?);
    headers.insert("authorization", "Bearer oat_sandbox".parse()?);
    assert!(state.authorize_test_speech(&headers, "tts").await.is_err());
    assert_eq!(
        sandbox_call(
            &app,
            &secret,
            "/api/v1/testing-environment/clean",
            json!({}),
            "clean"
        )
        .await?
        .0,
        StatusCode::BAD_REQUEST
    );
    let report = json!({"message":"Sandbox report only","client_version":"test"});
    let (status, first) =
        sandbox_call(&app, &secret, "/api/v1/reports", report.clone(), "report-1").await?;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(first["notification"], "simulated");
    assert_eq!(
        sandbox_call(&app, &secret, "/api/v1/reports", report.clone(), "report-1")
            .await?
            .1,
        first
    );
    assert_eq!(
        sandbox_call(
            &app,
            &secret,
            "/api/v1/reports",
            json!({"message":"Different"}),
            "report-1"
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sandbox_call(
            &app,
            &secret,
            "/api/v1/reports",
            json!({"message":secret}),
            "secret"
        )
        .await?
        .0,
        StatusCode::BAD_REQUEST
    );
    for n in 2..=10 {
        assert_eq!(
            sandbox_call(
                &app,
                &secret,
                "/api/v1/reports",
                report.clone(),
                &format!("report-{n}")
            )
            .await?
            .0,
            StatusCode::ACCEPTED
        );
    }
    assert_eq!(
        sandbox_call(&app, &secret, "/api/v1/reports", report, "report-11")
            .await?
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert!(state.deliver_report().await.is_ok());
    assert_eq!(
        sandbox_call(
            &app,
            &secret,
            "/api/v1/telemetry",
            json!({"event":"page_view"}),
            "event"
        )
        .await?
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        sandbox_call(
            &app,
            &secret,
            "/api/v1/telemetry",
            json!({"event":"page_view","secret":"not-allowed"}),
            "event"
        )
        .await?
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let production_reports: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_bug_reports WHERE plane_id=$1")
            .bind(Uuid::nil())
            .fetch_one(&pool)
            .await?;
    assert_eq!(production_reports, 0);
    let test_root = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6";
    sqlx::query("UPDATE waveform_environments SET webhook_key_digest=$1 WHERE id=$2")
        .bind(format!("{:x}", Sha256::digest(test_root.as_bytes())))
        .bind(id)
        .execute(&pool)
        .await?;
    let event_id = Uuid::new_v4();
    let event=json!({"test":{"testing_key":test_root,"metadata":{"spec_version":"1.0","event_id":event_id,"event_type":"session.revoked.v1","occurred_at":"2026-09-07T00:00:00Z","organization_id":Uuid::from_u128(2),"aggregate":{"type":"session","id":actor,"version":2}},"data":{"private":"must-not-persist"}}}).to_string();
    assert_eq!(
        webhook(&app, event_id, &event, true).await?,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        webhook(&app, event_id, &event, false).await?,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        webhook(&app, event_id, &event, false).await?,
        StatusCode::NO_CONTENT
    );
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_webhook_events WHERE plane_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(events, 1);
    let other = event.replace(test_root, "Z1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6");
    assert_eq!(
        webhook(&app, event_id, &other, false).await?,
        StatusCode::UNAUTHORIZED
    );
    // Live IAM disagreement about the bearer plane is rejected, without production fallback.
    iam.reset().await;
    mock_discovery(&iam, id, &secret, 1, None).await;
    authority["authorization"]["testing_environment_id"] = Value::Null;
    Mock::given(path("/api/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(authority))
        .mount(&iam)
        .await;
    assert_eq!(
        sandbox_call(
            &app,
            &secret,
            "/api/v1/reports",
            json!({"message":"wrong world"}),
            "wrong"
        )
        .await?
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}
