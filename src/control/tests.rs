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

pub(super) async fn database() -> Result<(PgPool, String), Box<dyn std::error::Error>> {
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

pub(super) fn fixture(
    pool: PgPool,
    iam: &MockServer,
) -> Result<Arc<ControlState>, Box<dyn std::error::Error>> {
    Ok(Arc::new(ControlState {
        fence_slots: Arc::new(tokio::sync::Semaphore::new(8)),
        honeycomb_token: Some(SecretString::from(
            "test-only-honeycomb-service-token-123456",
        )),
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

pub(super) async fn call(
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
async fn environment_management_is_owned_by_honeycomb() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = router(state.clone());
    for route in [
        "/api/v1/testing-environments".to_owned(),
        "/api/v1/testing-environment/clean".to_owned(),
        format!("/api/v1/testing-environments/{}/restore", Uuid::new_v4()),
    ] {
        let (status, body) = call(&app, "POST", &route, Some("oat_fixture"), json!({})).await?;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "manage_environment_in_honeycomb");
    }
    let legacy = legacy_plane(&state, Uuid::new_v4()).await?;
    let id: Uuid = legacy["id"].as_str().ok_or("id")?.parse()?;
    sqlx::query("UPDATE waveform_environments SET last_activity_at=now()-interval '90 days',deleted_at=now()-interval '40 days' WHERE id=$1").bind(id).execute(&pool).await?;
    assert_eq!(state.cleanup_environments().await?, 0);
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM waveform_environments WHERE id=$1)"
        )
        .bind(id)
        .fetch_one(&pool)
        .await?
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

// Existing paired environments remain readable after management moves to Honeycomb.
async fn legacy_plane(
    state: &ControlState,
    iam_id: Uuid,
) -> Result<Value, Box<dyn std::error::Error>> {
    let id = Uuid::new_v4();
    let key = Uuid::new_v4().simple().to_string();
    let vault = state.vault().map_err(|_| "vault")?;
    sqlx::query("INSERT INTO waveform_environments(id,org_id,creator_id,name,root_key_hash,root_key_cipher,iam_key_cipher,app_secret_cipher,briefcase_key_cipher,iam_environment_id) VALUES($1,'tos',$2,'legacy sandbox',$3,$4,$5,$6,$7,$8)")
        .bind(id).bind(Uuid::new_v4()).bind(vault.digest(&key,"environment-root")?)
        .bind(vault.seal(&key,&format!("{id}/root-key"))?)
        .bind(vault.seal(&"I".repeat(32),&format!("{id}/iam-key"))?)
        .bind(vault.seal("test-environment-app-secret",&format!("{id}/app-secret"))?)
        .bind(vault.seal(&format!("ask_{}","B".repeat(43)),&format!("{id}/briefcase-key"))?)
        .bind(iam_id).execute(&state.pool).await?;
    sqlx::query("INSERT INTO waveform_voice_profiles(plane_id,id,profile) SELECT $1,id,profile FROM waveform_voice_profiles WHERE plane_id=$2").bind(id).bind(Uuid::nil()).execute(&state.pool).await?;
    sqlx::query("INSERT INTO waveform_provider_defaults(plane_id,tts_order,stt_order,voice_profile) SELECT $1,tts_order,stt_order,voice_profile FROM waveform_provider_defaults WHERE plane_id=$2").bind(id).bind(Uuid::nil()).execute(&state.pool).await?;
    Ok(json!({"id":id,"key":key}))
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
        StatusCode::CONFLICT
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

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
#[allow(
    clippy::too_many_lines,
    clippy::unwrap_used,
    reason = "Sequential lifecycle integration assertions on local fixtures"
)]
async fn honeycomb_lifecycle_replays_fences_cleans_and_keeps_tombstones() -> TestResult {
    let (pool, schema) = database().await?;
    let iam = MockServer::start().await;
    let state = fixture(pool.clone(), &iam)?;
    let app = router(state.clone());
    let id = Uuid::new_v4();
    let secret = format!("ask_{}", "H".repeat(43));
    let mut op = json!({"operation_id":Uuid::new_v4(),"environment_id":id,"org_id":"tos","app_id":"tos>waveform","environment_revision":1,"generation":1,"key_version":1,"action":"prepare","testing_key":"never-persist-this-root-key","snapshot":{}});
    let route = |op: &Value| {
        format!(
            "/internal/honeycomb/organizations/tos/testing-environments/{id}/operations/{}",
            op["operation_id"].as_str().unwrap()
        )
    };
    let token = "test-only-honeycomb-service-token-123456";
    assert_eq!(
        call(&app, "PUT", &route(&op), Some(&secret), op.clone())
            .await?
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, receipt) = call(&app, "PUT", &route(&op), Some(token), op.clone()).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["state"], "completed");
    assert!(!receipt.to_string().contains("root-key"));
    assert_eq!(
        call(&app, "PUT", &route(&op), Some(token), op.clone())
            .await?
            .1,
        receipt
    );
    let mut changed = op.clone();
    changed["reason"] = json!("changed body");
    assert_eq!(
        call(&app, "PUT", &route(&changed), Some(token), changed.clone())
            .await?
            .0,
        StatusCode::CONFLICT
    );
    mock_discovery(&iam, id, &secret, 1, None).await;
    let plane = state
        .discover_plane(&secret)
        .await
        .map_err(|_| "discovery failed")?;
    drop(plane);
    let actor = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO waveform_account_preferences(plane_id,org_id,actor_id) VALUES($1,'tos',$2)",
    )
    .bind(id)
    .bind(actor)
    .execute(&pool)
    .await?;
    let fence = state.test_fence(id).await.map_err(|_| "fence failed")?;
    op["operation_id"] = json!(Uuid::new_v4());
    op["environment_revision"] = json!(2);
    op["generation"] = json!(2);
    op["action"] = json!("clean");
    let clean = op.clone();
    let app_clone = app.clone();
    let url = route(&op);
    let cleaning = tokio::spawn(async move {
        call(&app_clone, "PUT", &url, Some(token), clean)
            .await
            .map_err(|e| e.to_string())
    });
    // Wait for the durable pending barrier, without releasing the running request.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let pending: bool = sqlx::query_scalar(
                "SELECT state='pending' FROM waveform_lifecycle WHERE environment_id=$1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            if pending {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(state.test_fence(id).await.is_err());
    assert!(!cleaning.is_finished());
    drop(fence);
    assert_eq!(cleaning.await??.1["state"], "completed");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_account_preferences WHERE plane_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 0);
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM waveform_environments WHERE id=$1)"
        )
        .bind(id)
        .fetch_one(&pool)
        .await?
    );
    // Retrying clean does not erase new data written after its completed receipt.
    sqlx::query(
        "INSERT INTO waveform_account_preferences(plane_id,org_id,actor_id) VALUES($1,'tos',$2)",
    )
    .bind(id)
    .bind(actor)
    .execute(&pool)
    .await?;
    assert_eq!(
        call(&app, "PUT", &route(&op), Some(token), op.clone())
            .await?
            .1["state"],
        "completed"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM waveform_account_preferences WHERE plane_id=$1"
        )
        .bind(id)
        .fetch_one(&pool)
        .await?,
        1
    );
    let older_operation = op.clone();
    for (revision, action) in [
        (3, "disable"),
        (4, "restore"),
        (5, "rotate-key"),
        (6, "purge"),
    ] {
        op["operation_id"] = json!(Uuid::new_v4());
        op["environment_revision"] = json!(revision);
        op["action"] = json!(action);
        if action == "rotate-key" {
            op["key_version"] = json!(2);
        }
        assert_eq!(
            call(&app, "PUT", &route(&op), Some(token), op.clone())
                .await?
                .1["state"],
            "completed"
        );
        if action == "disable" || action == "purge" {
            assert!(state.discover_plane(&secret).await.is_err());
        }
        if action == "restore" {
            assert!(state.discover_plane(&secret).await.is_ok());
        }
        if action == "rotate-key" {
            assert!(state.discover_plane(&secret).await.is_err());
        }
    }
    assert_eq!(
        call(&app, "PUT", &route(&op), Some(token), op.clone())
            .await?
            .1["state"],
        "completed"
    );
    let mut new_stale = older_operation;
    new_stale["operation_id"] = json!(Uuid::new_v4());
    assert_eq!(
        call(
            &app,
            "PUT",
            &route(&new_stale),
            Some(token),
            new_stale.clone()
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM waveform_environments WHERE id=$1)"
        )
        .bind(id)
        .fetch_one(&pool)
        .await?
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}
