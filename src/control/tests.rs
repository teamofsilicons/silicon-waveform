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
            "iam_environment_key":"I".repeat(32), "app_secret":"test-environment-app-secret", "briefcase_environment_key":"B".repeat(32)
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
