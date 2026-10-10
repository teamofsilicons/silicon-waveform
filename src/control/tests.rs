//! Control and speech API tests against PostgreSQL (`WAVEFORM_TEST_DATABASE_URL`),
//! a stubbed Silicon Accounts (real `EdDSA` tokens, JWKS, proofs, signed webhooks)
//! and a stubbed Briefcase. Each test gets its own schema.

#![allow(clippy::too_many_lines)]

use std::{sync::Arc, time::Duration};

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

use super::ControlState;
use crate::{
    api::{ApiState, ReadinessChecks},
    application::{
        ports::LeaseIdGenerator,
        service::{ServicePolicy, StorageBoundary, WaveformService},
    },
    config::{BriefcaseSettings, ServerSettings},
    domain::{
        idempotency::{IdempotencyLeaseId, RequestDigestKey},
        media::{BriefcaseOrigin, MediaSizeLimit},
    },
    infrastructure::{
        accounts::AccountsGateway,
        authenticator::AccountsAuthenticator,
        briefcase::BriefcaseHttp,
        directory::AccountDirectory,
        held_audio::PostgresHeldAudio,
        idempotency::PostgresIdempotencyStore,
        storage_proofs::StorageProofs,
        testing::{FixtureAudioNormalizer, TEST_STT_TEXT, provider_chains},
        vault::Vault,
    },
    test_support::{TestAccounts, WEBHOOK_SECRET, issued_proof, signed_delivery},
};

pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(super) const ADA: (&str, &str, &str) = ("zQo", "carbon", "c:ada");
pub(super) const SCOUT: (&str, &str, &str) = ("K1E", "silicon", "si:scout");
pub(super) const BOB: (&str, &str, &str) = ("B0b", "carbon", "c:bob");
pub(super) const ROVER: (&str, &str, &str) = ("R0v", "silicon", "si:rover");

/// A fresh schema in the test database with every migration applied.
pub(super) async fn database() -> Result<(PgPool, String), Box<dyn std::error::Error>> {
    database_with(None).await
}

/// A fresh schema migrated up to `up_to` (all migrations when `None`).
pub(super) async fn database_with(
    up_to: Option<i64>,
) -> Result<(PgPool, String), Box<dyn std::error::Error>> {
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
        .max_connections(8)
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
    let mut migrator = sqlx::migrate!();
    if let Some(up_to) = up_to {
        migrator.migrations = std::borrow::Cow::Owned(
            migrator
                .migrations
                .iter()
                .filter(|migration| migration.version <= up_to)
                .cloned()
                .collect(),
        );
    }
    migrator.run(&pool).await?;
    Ok((pool, schema))
}

pub(super) async fn drop_schema(pool: &PgPool, schema: &str) -> TestResult {
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(pool)
        .await?;
    pool.close().await;
    Ok(())
}

struct TestLeaseIds;

impl LeaseIdGenerator for TestLeaseIds {
    fn new_idempotency_lease_id(&self) -> IdempotencyLeaseId {
        IdempotencyLeaseId::new(Uuid::new_v4()).unwrap_or_else(|_| unreachable!())
    }
}

/// Everything a test needs: the real router over a real schema.
pub(super) struct Harness {
    pub(super) pool: PgPool,
    pub(super) schema: String,
    pub(super) accounts: TestAccounts,
    pub(super) briefcase: MockServer,
    pub(super) state: Arc<ControlState>,
    /// The full public router (with contract negotiation, which needs the database).
    pub(super) app: axum::Router,
    /// The control routes alone, without the negotiation layer.
    pub(super) control: axum::Router,
}

impl Harness {
    pub(super) async fn new(issuers: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let (pool, schema) = database().await?;
        Self::over(pool, schema, issuers).await
    }

    /// A harness whose database is never reached (routes that fail or answer first).
    pub(super) async fn without_database(
        issuers: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let pool = PgPoolOptions::new().connect_lazy("postgres://localhost/unused")?;
        Self::over(pool, String::new(), issuers).await
    }

    pub(super) async fn over(
        pool: PgPool,
        schema: String,
        issuers: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let accounts = TestAccounts::start().await;
        let briefcase = MockServer::start().await;
        let accounts_settings = accounts.settings(issuers);
        let gateway = Arc::new(AccountsGateway::new(&accounts_settings)?);
        let authenticator = AccountsAuthenticator::new(
            gateway.clone(),
            AccountDirectory::new(pool.clone(), gateway.clone()),
            accounts_settings.proof_issuers.clone(),
        );
        let vault = Vault::new(&SecretString::from("19".repeat(32)))?;
        let proofs = StorageProofs::new(
            pool.clone(),
            vault.clone(),
            gateway.clone(),
            "briefcase".to_owned(),
        );
        let briefcase_settings = BriefcaseSettings {
            base_url: briefcase.uri().parse()?,
            permanent_origin: briefcase.uri().parse()?,
            app_id: "briefcase".to_owned(),
            app_folder: "apps/waveform".to_owned(),
            timeout: Duration::from_secs(5),
            download_timeout: Duration::from_secs(5),
            max_download_bytes: 1_000_000,
        };
        let briefcase_http = Arc::new(BriefcaseHttp::new(&briefcase_settings, "waveform")?);
        let held_audio = PostgresHeldAudio::new(pool.clone(), Duration::from_hours(24));
        let state = Arc::new(ControlState {
            pool: pool.clone(),
            mail: None,
            station: None,
            app_id: "waveform".to_owned(),
            accounts_url: accounts.issuer(),
            public_url: "http://127.0.0.1:4161".parse()?,
            authenticator: authenticator.clone(),
            storage_proofs: Some(proofs.clone()),
            briefcase: briefcase_http.clone(),
            held_audio: held_audio.clone(),
            vault: Some(vault),
            webhook_secrets: vec![SecretString::from(WEBHOOK_SECRET.to_owned())],
        });
        let idempotency = Arc::new(PostgresIdempotencyStore::new(
            pool.clone(),
            Duration::from_secs(10),
            Duration::from_secs(2),
            Duration::from_hours(24),
        ));
        let (tts, stt) = provider_chains();
        let service = WaveformService::new(
            Arc::new(authenticator),
            StorageBoundary {
                authority: Arc::new(proofs),
                briefcase: briefcase_http,
                held_audio: Arc::new(held_audio),
            },
            idempotency.clone(),
            Arc::new(FixtureAudioNormalizer),
            Arc::new(TestLeaseIds),
            tts,
            stt,
            ServicePolicy::new(
                Duration::from_secs(600),
                MediaSizeLimit::default(),
                BriefcaseOrigin::new(briefcase.uri().parse()?)?,
                4_096,
                RequestDigestKey::new(b"control-tests-request-digest-key-0123")?,
                "http://127.0.0.1:4161".parse()?,
            )?,
        )?
        .with_voice_profiles(state.clone())
        .with_provider_orders(state.clone())
        .with_provider_keys(state.clone());
        let api = ApiState::new(
            Arc::new(service),
            ReadinessChecks::new(idempotency, Arc::new(FixtureAudioNormalizer), true, true),
            Duration::from_secs(300),
            Duration::from_secs(600),
        )
        .with_control(state.clone());
        let app = crate::api::router(
            api,
            &ServerSettings {
                bind_addr: "127.0.0.1:4161".parse()?,
                json_body_limit: 327_680,
                tts_deadline: Duration::from_secs(300),
                stt_deadline: Duration::from_secs(600),
                max_in_flight: std::num::NonZeroUsize::new(64).ok_or("in flight")?,
                shutdown_timeout: Duration::from_secs(5),
            },
        );
        let control = super::router(state.clone());
        Ok(Self {
            pool,
            schema,
            accounts,
            briefcase,
            state,
            app,
            control,
        })
    }

    /// A valid Waveform access token for one of the test accounts.
    pub(super) fn token(&self, account: (&str, &str, &str)) -> String {
        self.accounts.token(account.0, account.1, account.2)
    }

    pub(super) async fn finish(self) -> TestResult {
        drop_schema(&self.pool, &self.schema).await
    }
}

/// Sends one request through the router and decodes a JSON answer.
pub(super) async fn send(
    app: &axum::Router,
    verb: &str,
    route: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let (status, bytes, _) = send_raw(app, verb, route, headers, body).await?;
    Ok((
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        },
    ))
}

/// Sends one request and returns the raw answer.
pub(super) async fn send_raw(
    app: &axum::Router,
    verb: &str,
    route: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Result<(StatusCode, Vec<u8>, http::HeaderMap), Box<dyn std::error::Error>> {
    let mut request = Request::builder().method(verb).uri(route);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))?)
        .await?;
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 32 * 1_048_576).await?;
    Ok((status, bytes.to_vec(), response_headers))
}

pub(super) fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Posts a signed Accounts webhook delivery.
pub(super) async fn deliver(
    app: &axum::Router,
    event: &Value,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let (headers, body) = signed_delivery(WEBHOOK_SECRET, event);
    let mut request = Request::builder().method("POST").uri("/webhooks/accounts");
    for (name, value) in &headers {
        request = request.header(*name, value);
    }
    let response = app.clone().oneshot(request.body(Body::from(body))?).await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65_536).await?;
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn event(kind: &str, data: Value) -> Value {
    json!({
        "event_id": Uuid::now_v7().to_string(), "type": kind,
        "occurred_at": time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        "app_id": "waveform", "silicon": null, "data": data
    })
}

/// Mounts a Briefcase that accepts uploads into `apps/waveform` and lists the
/// last one, echoing the operation and file name it was given.
pub(super) async fn briefcase_accepts_uploads(briefcase: &MockServer) {
    let entry_id = Uuid::new_v4();
    let upload_id = Uuid::new_v4();
    let origin = briefcase.uri();
    let last = Arc::new(std::sync::Mutex::new(json!({})));
    let status = move |state: &str, operation: &Value| json!({"operation_id":operation,"upload_id":upload_id,"state":state,"expires_at":"2099-01-01T00:00:00Z","published_entry_id": if state == "committed" { Some(entry_id) } else { None }});
    let reserved = last.clone();
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/uploads/reserve"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let mut answer = status("reserved", &body["operation_id"]);
            answer["capability"] = json!("capability-1");
            if let Ok(mut last) = reserved.lock() {
                *last = body;
            }
            ResponseTemplate::new(200).set_body_json(answer)
        })
        .mount(briefcase)
        .await;
    let staged = last.clone();
    Mock::given(method("PUT"))
        .and(path(format!("/api/v1/obo/uploads/{upload_id}/content")))
        .respond_with(move |_request: &wiremock::Request| {
            let operation = staged
                .lock()
                .map_or(Value::Null, |last| last["operation_id"].clone());
            ResponseTemplate::new(200).set_body_json(status("staged", &operation))
        })
        .mount(briefcase)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/uploads/commit"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            ResponseTemplate::new(200).set_body_json(status("committed", &body["operation_id"]))
        })
        .mount(briefcase)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/files/read"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(b"I".to_vec()),
        )
        .mount(briefcase)
        .await;
    let listed = last;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path":"apps/waveform"})))
        .respond_with(move |_request: &wiremock::Request| {
            let name = listed.lock().map(|last| last["name"].as_str().unwrap_or_default().to_owned()).unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":entry_id,"type":"file","name":name,"path":format!("apps/waveform/{name}"),"permanent_url":format!("{origin}/zQo/apps/waveform/{name}"),"origin_app_id":"waveform"}],"next_cursor":null}))
        })
        .mount(briefcase)
        .await;
}

/// Mounts a Briefcase that lets the account read one source file.
pub(super) async fn briefcase_serves_source(briefcase: &MockServer) -> String {
    let entry_id = Uuid::new_v4();
    let url = format!("{}/zQo/apps/waveform/voice.mp3", briefcase.uri());
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path":"apps/waveform"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":entry_id,"type":"file","name":"voice.mp3","path":"apps/waveform/voice.mp3","permanent_url":url}],"next_cursor":null})))
        .mount(briefcase)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/files/read"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(b"voice-bytes".to_vec()),
        )
        .mount(briefcase)
        .await;
    url
}

#[tokio::test]
async fn discovery_is_public_and_retired_routes_say_what_replaced_them() -> TestResult {
    let harness = Harness::without_database("").await?;
    let (status, body) = send(&harness.control, "GET", "/api/v1/accounts", &[], None).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app_id"], "waveform");
    assert_eq!(body["accounts_url"], harness.accounts.issuer());
    assert_eq!(
        body["proof_scopes"],
        json!(["waveform.tts", "waveform.stt"])
    );
    for (verb, route) in [
        ("GET", "/api/v1/iam"),
        ("POST", "/api/v1/auth/login"),
        ("POST", "/api/v1/auth/refresh"),
        ("POST", "/api/v1/auth/logout"),
        ("GET", "/api/v1/testing-environment"),
        ("POST", "/webhooks/"),
        ("POST", "/webhook/"),
    ] {
        let (status, body) = send(&harness.control, verb, route, &[], None).await?;
        assert_eq!(status, StatusCode::GONE, "{verb} {route}");
        assert_eq!(body["error"]["code"], "route_retired");
        assert!(
            body["error"]["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("waveform login"))
        );
    }
    // No Accounts call was needed for any of this.
    assert!(
        harness
            .accounts
            .server
            .received_requests()
            .await
            .ok_or("requests")?
            .iter()
            .all(|request| request.url.path() != "/v1/proofs/verify")
    );
    Ok(())
}

#[tokio::test]
async fn credentials_are_refused_with_exact_reasons_before_any_data_access() -> TestResult {
    let harness = Harness::without_database("").await?;
    let mut wrong_audience = harness.accounts.claims(ADA.0, ADA.1, ADA.2);
    wrong_audience["aud"] = json!("briefcase");
    let wrong_audience = bearer(&harness.accounts.key.sign(&wrong_audience));
    let mut expired = harness.accounts.claims(ADA.0, ADA.1, ADA.2);
    expired["exp"] = json!(time::OffsetDateTime::now_utc().unix_timestamp() - 600);
    let expired = bearer(&harness.accounts.key.sign(&expired));
    for (headers, status, reason) in [
        (vec![], StatusCode::UNAUTHORIZED, "missing_credentials"),
        (
            vec![("authorization", "Basic d2F2ZWZvcm06eA==")],
            StatusCode::UNAUTHORIZED,
            "malformed_authorization",
        ),
        (
            vec![("authorization", "Proof sapr_refresh-token")],
            StatusCode::UNAUTHORIZED,
            "malformed_authorization",
        ),
        (
            vec![
                ("authorization", "Bearer x"),
                ("x-iam-obo-access-token", "oba_old"),
            ],
            StatusCode::UNAUTHORIZED,
            "retired_credentials",
        ),
        (
            vec![("authorization", wrong_audience.as_str())],
            StatusCode::UNAUTHORIZED,
            "wrong_audience",
        ),
        (
            vec![("authorization", expired.as_str())],
            StatusCode::UNAUTHORIZED,
            "token_expired",
        ),
        (
            vec![("authorization", "Bearer not.a.jwt")],
            StatusCode::UNAUTHORIZED,
            "invalid_token",
        ),
    ] {
        let (got, body) = send(&harness.control, "GET", "/api/v1/me", &headers, None).await?;
        assert_eq!(got, status, "{reason}");
        assert_eq!(body["error"]["reason"], reason);
        assert!(body["error"]["hint"].is_string(), "{reason}");
    }
    // A valid proof is still refused on a route that only takes the account's own token.
    harness
        .accounts
        .valid_proof("sap_for-settings", ADA, "interface", &["waveform.tts"])
        .await;
    let (status, body) = send(
        &harness.control,
        "GET",
        "/api/v1/preferences",
        &[("authorization", "Proof sap_for-settings")],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["reason"], "proof_not_accepted");
    Ok(())
}

/// Signs an account in once so Waveform knows it (and, for a Silicon, its custodian).
pub(super) async fn sign_in(
    harness: &Harness,
    account: (&str, &str, &str),
) -> Result<Value, Box<dyn std::error::Error>> {
    let token = harness.token(account);
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&token))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body)
}

/// Waits until the background custodian lookup filled a Silicon's custodian.
pub(super) async fn wait_for_custodian(harness: &Harness, silicon: &str) -> TestResult {
    for _ in 0..100 {
        let custodian: Option<String> = sqlx::query_scalar(
            "SELECT custodian_uuid FROM waveform_accounts WHERE account_uuid=$1",
        )
        .bind(silicon)
        .fetch_optional(&harness.pool)
        .await?
        .flatten();
        if custodian.is_some() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("custodian was never looked up".into())
}

pub(super) async fn storage_key(
    harness: &Harness,
    account: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT storage_actor_id FROM waveform_account_keys WHERE account_uuid=$1",
    )
    .bind(account)
    .fetch_one(&harness.pool)
    .await?)
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn accounts_are_keyed_by_their_exact_uuid_and_silicons_learn_their_custodian() -> TestResult {
    let harness = Harness::new("").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    let me = sign_in(&harness, ADA).await?;
    assert_eq!(me["account"]["uuid"], "zQo");
    assert_eq!(me["account"]["id"], "c:ada");
    assert_eq!(me["account"]["kind"], "carbon");
    // A uuid differing only by case is a different account.
    let other = sign_in(&harness, ("zqo", "carbon", "c:zed")).await?;
    assert_eq!(other["account"]["uuid"], "zqo");
    assert_ne!(
        storage_key(&harness, "zQo").await?,
        storage_key(&harness, "zqo").await?
    );
    // Signing in again keeps the same private key.
    let first = storage_key(&harness, "zQo").await?;
    sign_in(&harness, ADA).await?;
    assert_eq!(storage_key(&harness, "zQo").await?, first);

    // A Silicon's first answer already names who looks after it.
    let scout_me = sign_in(&harness, SCOUT).await?;
    assert_eq!(
        scout_me["account"]["custodian"]["uuid"], "zQo",
        "{scout_me}"
    );
    wait_for_custodian(&harness, SCOUT.0).await?;
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/silicons",
        &[("authorization", &bearer(&harness.token(ADA)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["uuid"], "K1E");
    assert_eq!(body["items"][0]["custodian"]["uuid"], "zQo");
    // The circle helper: a Carbon's circle is itself and its Silicons.
    let ada = harness
        .state
        .directory()
        .load(&"zQo".parse()?)
        .await?
        .ok_or("ada")?;
    let circle = harness.state.directory().circle(&ada).await?;
    assert_eq!(
        circle.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["zQo", "K1E"]
    );
    let scout = harness
        .state
        .directory()
        .load(&"K1E".parse()?)
        .await?
        .ok_or("scout")?;
    let circle = harness.state.directory().circle(&scout).await?;
    assert_eq!(
        circle.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["K1E", "zQo"]
    );
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn a_custodian_manages_its_silicons_settings_keys_and_history_and_nobody_else_can()
-> TestResult {
    let harness = Harness::new("").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    harness
        .accounts
        .account(ROVER.0, ROVER.1, ROVER.2, Some((BOB.0, BOB.2)))
        .await;
    harness.accounts.account(BOB.0, BOB.1, BOB.2, None).await;
    harness.accounts.introspection(true).await;
    for account in [ADA, SCOUT, BOB, ROVER] {
        sign_in(&harness, account).await?;
    }
    wait_for_custodian(&harness, SCOUT.0).await?;
    let ada = bearer(&harness.token(ADA));
    let bob = bearer(&harness.token(BOB));
    let scout = bearer(&harness.token(SCOUT));

    // Ada changes Scout's settings and saves a key for it.
    let (status, body) = send(
        &harness.app,
        "PATCH",
        "/api/v1/preferences?account=si:scout",
        &[("authorization", &ada)],
        Some(json!({"voice_profile":"puck","tts_order":["openai"]})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["uuid"], "K1E");
    let (status, _) = send(
        &harness.app,
        "PUT",
        "/api/v1/provider-keys/openai?account=K1E",
        &[("authorization", &ada)],
        Some(json!({"api_key":"sk-custodian-pays"})),
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Scout sees them as its own; the key value is never returned.
    let (_, preferences) = send(
        &harness.app,
        "GET",
        "/api/v1/preferences",
        &[("authorization", &scout)],
        None,
    )
    .await?;
    assert_eq!(preferences["voice_profile"], "puck");
    assert_eq!(
        preferences["tts_order"],
        json!(["openai", "gemini", "elevenlabs"])
    );
    let (_, keys) = send(
        &harness.app,
        "GET",
        "/api/v1/provider-keys",
        &[("authorization", &scout)],
        None,
    )
    .await?;
    assert_eq!(keys["items"][0]["provider"], "openai");
    assert!(!keys.to_string().contains("sk-custodian-pays"));

    // Bob is not Scout's custodian; Scout cannot manage its custodian; unknown ids are named.
    for (caller, route) in [
        (&bob, "/api/v1/preferences?account=si:scout"),
        (&bob, "/api/v1/jobs?account=K1E"),
        (&bob, "/api/v1/provider-keys?account=si:scout"),
        (&scout, "/api/v1/preferences?account=c:ada"),
        (&scout, "/api/v1/jobs?account=si:rover"),
    ] {
        let (status, body) = send(
            &harness.app,
            "GET",
            route,
            &[("authorization", caller)],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{route}");
        assert_eq!(body["error"]["code"], "not_custodian");
    }
    Mock::given(method("GET"))
        .and(path("/v1/accounts/by-id/si:nobody"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error":{"code":"account_not_found","message":"no"}})),
        )
        .mount(&harness.accounts.server)
        .await;
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?account=si:nobody",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("si:nobody"))
    );

    // History: Scout's jobs are visible to Scout and Ada, never to Bob; another
    // account's job id answers like an unknown one.
    let scout_key = storage_key(&harness, SCOUT.0).await?;
    let job = Uuid::now_v7();
    sqlx::query("INSERT INTO waveform_jobs(id,plane_id,org_id,actor_id,actor_kind,operation,status,first_line,request_digest,idempotency_key_hash,issuing_app,created_at,finished_at,duration_ms,provider) VALUES($1,$2,'@',$3,'silicon','tts','completed','hello',$4,$5,'interface',now(),now(),1200,'gemini')")
        .bind(job).bind(Uuid::nil()).bind(scout_key).bind(vec![1_u8; 32]).bind(vec![2_u8; 32])
        .execute(&harness.pool).await?;
    // A legacy (pre-Accounts, org-scoped) job of the same key appears in the same history.
    let legacy_job = Uuid::now_v7();
    sqlx::query("INSERT INTO waveform_jobs(id,plane_id,org_id,actor_id,actor_kind,operation,status,first_line,request_digest,idempotency_key_hash,created_at,finished_at,error_code) VALUES($1,$2,'tos',$3,'silicon','stt','failed','',$4,$5,now()-interval '1 day',now()-interval '1 day','providers_exhausted')")
        .bind(legacy_job).bind(Uuid::nil()).bind(scout_key).bind(vec![3_u8; 32]).bind(vec![4_u8; 32])
        .execute(&harness.pool).await?;
    for caller in [&scout, &ada] {
        let route = if caller == &ada {
            "/api/v1/jobs?account=si:scout"
        } else {
            "/api/v1/jobs"
        };
        let (status, body) = send(
            &harness.app,
            "GET",
            route,
            &[("authorization", caller)],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ids = body["items"]
            .as_array()
            .ok_or("items")?
            .iter()
            .map(|item| item["id"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![job.to_string(), legacy_job.to_string()]);
        assert_eq!(body["items"][0]["issuing_app"], "interface");
    }
    let (status, _) = send(
        &harness.app,
        "GET",
        &format!("/api/v1/jobs/{job}"),
        &[("authorization", &bob)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = send(
        &harness.app,
        "GET",
        &format!("/api/v1/jobs/{job}?account=si:scout"),
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["first_line"], "hello");

    // Key writes see revocation at once: an inactive sign-in is refused.
    harness.accounts.server.reset().await;
    harness.accounts.introspection(false).await;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"keys":[harness.accounts.key.jwk()]})),
        )
        .mount(&harness.accounts.server)
        .await;
    let fresh = bearer(&harness.token(ADA));
    let (status, body) = send(
        &harness.app,
        "DELETE",
        "/api/v1/provider-keys/openai?account=si:scout",
        &[("authorization", &fresh)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["reason"], "token_inactive");
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn webhooks_are_verified_deduplicated_and_applied() -> TestResult {
    let harness = Harness::new("").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    for account in [ADA, SCOUT, BOB] {
        sign_in(&harness, account).await?;
    }
    wait_for_custodian(&harness, SCOUT.0).await?;

    // Signature, timestamp and body checks.
    let ping = event("ping", json!({}));
    let (headers, body) = signed_delivery("whsec_some-other-secret", &ping);
    let mut request = Request::builder().method("POST").uri("/webhooks/accounts");
    for (name, value) in &headers {
        request = request.header(*name, value);
    }
    let response = harness
        .app
        .clone()
        .oneshot(request.body(Body::from(body))?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let stale = time::OffsetDateTime::now_utc().unix_timestamp() - 3_600;
    let raw = serde_json::to_vec(&ping)?;
    let response = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/accounts")
                .header("x-accounts-timestamp", stale.to_string())
                .header(
                    "x-accounts-signature",
                    silicon_accounts_client::sign_webhook(WEBHOOK_SECRET, stale, &raw),
                )
                .body(Body::from(raw))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let (headers, _) = signed_delivery(WEBHOOK_SECRET, &ping);
    let mut request = Request::builder().method("POST").uri("/webhooks/accounts");
    for (name, value) in &headers {
        if *name == "x-accounts-signature" {
            let timestamp: i64 = headers[0].1.parse()?;
            request = request.header(
                *name,
                silicon_accounts_client::sign_webhook(WEBHOOK_SECRET, timestamp, b"not json"),
            );
        } else {
            request = request.header(*name, value);
        }
    }
    let response = harness
        .app
        .clone()
        .oneshot(request.body(Body::from("not json"))?)
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(deliver(&harness.app, &ping).await?.1["outcome"], "ping");
    assert_eq!(
        deliver(
            &harness.app,
            &event("account.mood_changed", json!({"uuid":"zQo"}))
        )
        .await?
        .1["outcome"],
        "ignored"
    );

    // account.id_changed, then a duplicate delivery of the same event.
    let renamed = event(
        "account.id_changed",
        json!({"uuid":"zQo","membership_id":"waveform:zQo","kind":"carbon","old_id":"c:ada","new_id":"c:ada-lovelace"}),
    );
    assert_eq!(
        deliver(&harness.app, &renamed).await?.1["outcome"],
        "id_updated"
    );
    let (status, body) = deliver(&harness.app, &renamed).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["duplicate"], true);
    let (_, me) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&harness.token(ADA)))],
        None,
    )
    .await?;
    assert_eq!(me["account"]["id"], "c:ada-lovelace");
    let custodian_id: Option<String> =
        sqlx::query_scalar("SELECT custodian_id FROM waveform_accounts WHERE account_uuid='K1E'")
            .fetch_one(&harness.pool)
            .await?;
    assert_eq!(custodian_id.as_deref(), Some("c:ada-lovelace"));
    // A lookup that may have been answered before the change (the stub still says
    // c:ada) never brings the old id back, for the Carbon or as the Silicon's custodian.
    harness.accounts.account(ADA.0, ADA.1, ADA.2, None).await;
    let directory = harness.state.directory();
    directory.refresh_from_accounts(&"K1E".parse()?).await?;
    directory.refresh_from_accounts(&"zQo".parse()?).await?;
    let ids: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT (SELECT current_id FROM waveform_accounts WHERE account_uuid='zQo'), (SELECT custodian_id FROM waveform_accounts WHERE account_uuid='K1E')",
    )
    .fetch_one(&harness.pool)
    .await?;
    assert_eq!(
        ids,
        (
            Some("c:ada-lovelace".to_owned()),
            Some("c:ada-lovelace".to_owned())
        )
    );

    // account.updated applies only newer versions.
    let updated = |version: i64, name: &str| {
        event(
            "account.updated",
            json!({"uuid":"zQo","membership_id":"waveform:zQo","changed":["display_name"],"account":{"uuid":"zQo","membership_id":"waveform:zQo","kind":"carbon","id":"c:ada-lovelace","display_name":name,"pfp_url":"https://accounts.example.test/v1/photos/1","version":version}}),
        )
    };
    assert_eq!(
        deliver(&harness.app, &updated(5, "Ada L")).await?.1["outcome"],
        "profile_updated"
    );
    assert_eq!(
        deliver(&harness.app, &updated(4, "Old name")).await?.1["outcome"],
        "stale_or_unknown_account"
    );
    let name: Option<String> =
        sqlx::query_scalar("SELECT display_name FROM waveform_accounts WHERE account_uuid='zQo'")
            .fetch_one(&harness.pool)
            .await?;
    assert_eq!(name.as_deref(), Some("Ada L"));

    // silicon.custodian_changed hands Scout to Bob.
    let moved = event(
        "silicon.custodian_changed",
        json!({"uuid":"K1E","membership_id":"waveform:K1E","from":{"uuid":"zQo","id":"c:ada-lovelace"},"to":{"uuid":"B0b","id":"c:bob"}}),
    );
    assert_eq!(
        deliver(&harness.app, &moved).await?.1["outcome"],
        "custodian_updated"
    );
    let (status, _) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?account=K1E",
        &[("authorization", &bearer(&harness.token(BOB)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((BOB.0, BOB.2)))
        .await;
    let (status, _) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?account=K1E",
        &[("authorization", &bearer(&harness.token(ADA)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // membership.signed_out: app_revoked keeps the account's other sign-ins.
    let old_token = harness.token(BOB);
    let app_revoked = event(
        "membership.signed_out",
        json!({"uuid":"B0b","membership_id":"waveform:B0b","reason":"app_revoked"}),
    );
    assert_eq!(
        deliver(&harness.app, &app_revoked).await?.1["outcome"],
        "ignored_app_revoked"
    );
    let (status, _) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&old_token))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    // Any other reason refuses tokens issued before the event.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let signed_out = event(
        "membership.signed_out",
        json!({"uuid":"B0b","membership_id":"waveform:B0b","reason":"stk_rotated"}),
    );
    assert_eq!(
        deliver(&harness.app, &signed_out).await?.1["outcome"],
        "sign_ins_ended"
    );
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&old_token))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["reason"], "sign_in_ended");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let (status, _) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&harness.token(BOB)))],
        None,
    )
    .await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "a token issued after the sign-out works"
    );
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn briefcase_access_is_approved_by_the_account_and_ended_by_events() -> TestResult {
    let harness = Harness::new("").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    harness.accounts.introspection(true).await;
    harness.accounts.issues_briefcase_proofs().await;
    for account in [ADA, SCOUT] {
        sign_in(&harness, account).await?;
    }
    wait_for_custodian(&harness, SCOUT.0).await?;
    let ada = bearer(&harness.token(ADA));
    let scout = bearer(&harness.token(SCOUT));

    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/storage-authorizations",
        &[("authorization", &scout)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["briefcase"]["status"], "missing");
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/storage-authorizations",
        &[("authorization", &scout)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["briefcase"]["status"], "active");
    assert_eq!(
        body["briefcase"]["scopes"],
        json!([
            "briefcase.uploads.reserve",
            "briefcase.uploads.commit",
            "briefcase.entries.list",
            "briefcase.files.read"
        ])
    );
    assert!(
        !body.to_string().contains("sap_"),
        "proof tokens never leave Waveform"
    );
    // The proof was issued from Scout's own token, for Briefcase, with Waveform's credentials.
    let issued = harness
        .accounts
        .server
        .received_requests()
        .await
        .ok_or("requests")?;
    let issue = issued
        .iter()
        .find(|request| request.url.path() == "/v1/proofs/user-verification")
        .ok_or("issue")?;
    let issue_body: Value = serde_json::from_slice(&issue.body)?;
    assert_eq!(issue_body["receiving_app"], "briefcase");
    assert_eq!(issue_body["subject_token"], harness_token_from(&scout));
    assert!(issue.headers.contains_key("idempotency-key"));
    // Stored encrypted.
    let cipher: Vec<u8> = sqlx::query_scalar(
        "SELECT token_cipher FROM waveform_storage_proofs WHERE storage_actor_id=$1",
    )
    .bind(storage_key(&harness, SCOUT.0).await?)
    .fetch_one(&harness.pool)
    .await?;
    assert!(!String::from_utf8_lossy(&cipher).contains("sap_briefcase-1"));

    // Ada can see and revoke it for Scout, but cannot approve for Scout.
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/storage-authorizations?account=si:scout",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["briefcase"]["status"], "active");
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/storage-authorizations?account=si:scout",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "approval_needs_the_account");
    let (status, body) = send(
        &harness.app,
        "DELETE",
        "/api/v1/storage-authorizations?account=si:scout",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["briefcase"]["status"], "revoked");
    assert_eq!(body["briefcase"]["revoked_reason"], "revoked_by_custodian");

    // membership.access_removed ends sign-ins and the proof.
    let (status, _) = send(
        &harness.app,
        "POST",
        "/api/v1/storage-authorizations",
        &[("authorization", &scout)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let removed = event(
        "membership.access_removed",
        json!({"uuid":"K1E","membership_id":"waveform:K1E"}),
    );
    assert_eq!(
        deliver(&harness.app, &removed).await?.1["outcome"],
        "sign_ins_ended"
    );
    let (_, body) = send(
        &harness.app,
        "GET",
        "/api/v1/storage-authorizations?account=K1E",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(body["briefcase"]["status"], "revoked");
    assert_eq!(body["briefcase"]["revoked_reason"], "access_removed");
    let (status, _) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &scout)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    harness.finish().await
}

/// The raw token inside an `Authorization: Bearer` value.
fn harness_token_from(header: &str) -> &str {
    header.trim_start_matches("Bearer ")
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn account_deletion_purges_the_accounts_data_and_keeps_a_tombstone() -> TestResult {
    let harness = Harness::new("").await?;
    harness.accounts.introspection(true).await;
    for account in [ADA, BOB] {
        sign_in(&harness, account).await?;
    }
    let ada = bearer(&harness.token(ADA));
    send(
        &harness.app,
        "PATCH",
        "/api/v1/preferences",
        &[("authorization", &ada)],
        Some(json!({"voice_profile":"puck"})),
    )
    .await?;
    send(
        &harness.app,
        "PUT",
        "/api/v1/provider-keys/gemini",
        &[("authorization", &ada)],
        Some(json!({"api_key":"AIza-ada"})),
    )
    .await?;
    send(
        &harness.app,
        "PUT",
        "/api/v1/provider-keys/gemini",
        &[("authorization", &bearer(&harness.token(BOB)))],
        Some(json!({"api_key":"AIza-bob"})),
    )
    .await?;
    send(
        &harness.app,
        "PATCH",
        "/api/v1/preferences",
        &[("authorization", &bearer(&harness.token(BOB)))],
        Some(json!({"voice_profile":"kore"})),
    )
    .await?;
    let ada_key = storage_key(&harness, ADA.0).await?;
    let bob_key = storage_key(&harness, BOB.0).await?;
    for key in [ada_key, bob_key] {
        sqlx::query("INSERT INTO waveform_jobs(id,plane_id,org_id,actor_id,actor_kind,operation,status,first_line,request_digest,idempotency_key_hash,created_at) VALUES($1,$2,'@',$3,'carbon','tts','running','x',$4,$5,now())")
            .bind(Uuid::now_v7()).bind(Uuid::nil()).bind(key).bind(vec![1_u8; 32]).bind(vec![2_u8; 32])
            .execute(&harness.pool).await?;
        sqlx::query("INSERT INTO waveform_held_audio(request_id, storage_actor_id, filename, audio, reason, expires_at) VALUES($1,$2,'a.mp3','\\x01'::bytea,'delegated_call',now()+interval '1 day')")
            .bind(Uuid::now_v7()).bind(key)
            .execute(&harness.pool).await?;
    }
    let deleted = event(
        "account.deleted",
        json!({"uuid":"zQo","membership_id":"waveform:zQo"}),
    );
    assert_eq!(
        deliver(&harness.app, &deleted).await?.1["outcome"],
        "account_deleted"
    );
    for table in [
        "waveform_provider_keys",
        "waveform_account_preferences",
        "waveform_jobs",
    ] {
        let ada_rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE actor_id=$1"
        )))
        .bind(ada_key)
        .fetch_one(&harness.pool)
        .await?;
        let bob_rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE actor_id=$1"
        )))
        .bind(bob_key)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(ada_rows, 0, "{table}");
        assert!(bob_rows > 0, "{table} keeps other accounts' data");
    }
    let held: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_held_audio WHERE storage_actor_id=$1")
            .bind(ada_key)
            .fetch_one(&harness.pool)
            .await?;
    assert_eq!(held, 0);
    let (id, deleted_at): (Option<String>, Option<time::OffsetDateTime>) = sqlx::query_as(
        "SELECT current_id, deleted_at FROM waveform_accounts WHERE account_uuid='zQo'",
    )
    .fetch_one(&harness.pool)
    .await?;
    assert!(id.is_none() && deleted_at.is_some());
    // The tombstone keeps refusing the deleted account's tokens, even new ones.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&harness.token(ADA)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["reason"], "account_deleted");
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn speech_runs_for_accounts_and_for_apps_with_proofs() -> TestResult {
    let harness = Harness::new("waveform.tts=interface;waveform.stt=interface,dm").await?;
    harness.accounts.issues_briefcase_proofs().await;
    briefcase_accepts_uploads(&harness.briefcase).await;
    let ada = bearer(&harness.token(ADA));

    // A direct request stores the MP3 in Ada's Briefcase files.
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/tts",
        &[
            ("authorization", &ada),
            ("idempotency-key", "tts-direct-0001"),
        ],
        Some(json!({"text":"Hello from Ada"})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["storage"]["status"], "stored");
    assert!(
        body["file_url"]
            .as_str()
            .is_some_and(|url| url.starts_with(&harness.briefcase.uri()))
    );
    assert!(body["audio_url"].is_null());
    let briefcase_requests = harness
        .briefcase
        .received_requests()
        .await
        .ok_or("requests")?;
    assert!(
        briefcase_requests
            .iter()
            .filter(|request| request.method == "POST")
            .all(|request| {
                request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Proof sap_briefcase-1")
            })
    );
    // Replaying the same key returns the same result without new provider work.
    let (status, replay) = send(
        &harness.app,
        "POST",
        "/api/v1/tts",
        &[
            ("authorization", &ada),
            ("idempotency-key", "tts-direct-0001"),
        ],
        Some(json!({"text":"Hello from Ada"})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["request_id"], body["request_id"]);

    // Interface calls for Ada with a proof: the audio is held by Waveform.
    harness
        .accounts
        .valid_proof("sap_interface-tts", ADA, "interface", &["waveform.tts"])
        .await;
    let (status, held) = send(
        &harness.app,
        "POST",
        "/api/v1/tts",
        &[
            ("authorization", "Proof sap_interface-tts"),
            ("idempotency-key", "tts-proof-0001"),
        ],
        Some(json!({"text":"Hello for Ada"})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(held["storage"]["status"], "held");
    assert_eq!(held["storage"]["reason"], "delegated_call");
    assert!(held["file_url"].is_null());
    let audio_url = held["audio_url"].as_str().ok_or("audio_url")?;
    let audio_path = audio_url.trim_start_matches("http://127.0.0.1:4161");
    let (status, bytes, headers) = send_raw(
        &harness.app,
        "GET",
        audio_path,
        &[("authorization", "Proof sap_interface-tts")],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "audio/mpeg");
    assert!(bytes.len() > 1_000);
    // Ada can fetch it too; Bob cannot (it answers like a missing file).
    let (status, _, _) = send_raw(
        &harness.app,
        "GET",
        audio_path,
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send_raw(
        &harness.app,
        "GET",
        audio_path,
        &[("authorization", &bearer(&harness.token(BOB)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The job records which app asked.
    let (_, jobs) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?operation=tts",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(jobs["items"][0]["issuing_app"], "interface");
    assert!(jobs["items"][1]["issuing_app"].is_null());

    // Proofs are refused outside their scope, issuer allow-list, or validity.
    harness
        .accounts
        .valid_proof("sap_stt-only", ADA, "interface", &["waveform.stt"])
        .await;
    harness
        .accounts
        .valid_proof("sap_from-dm", ADA, "dm", &["waveform.tts", "waveform.stt"])
        .await;
    harness.accounts.invalid_proof("sap_revoked").await;
    for (token, receiver, kind) in [
        ("sap_wrong-receiver", "briefcase", "user_verification"),
        ("sap_wrong-kind", "waveform", "app_verification"),
    ] {
        Mock::given(method("POST"))
            .and(path("/v1/proofs/verify"))
            .and(wiremock::matchers::body_partial_json(
                json!({"proof_token":token}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "valid":true,"proof_id":token,"kind":kind,"expires_at":"2099-01-01T00:00:00Z",
                "issuing_app":{"app_id":"interface","name":"Interface"},
                "receiving_app":{"app_id":receiver,"name":receiver},
                "user":{"uuid":ADA.0,"id":ADA.2,"kind":ADA.1,"membership_id":"interface:zQo"},
                "scopes":["waveform.tts"]
            })))
            .mount(&harness.accounts.server)
            .await;
    }
    for (proof, status, reason) in [
        (
            "Proof sap_stt-only",
            StatusCode::FORBIDDEN,
            "proof_scope_missing",
        ),
        (
            "Proof sap_from-dm",
            StatusCode::FORBIDDEN,
            "proof_issuer_not_allowed",
        ),
        (
            "Proof sap_revoked",
            StatusCode::UNAUTHORIZED,
            "invalid_proof",
        ),
        (
            "Proof sap_wrong-receiver",
            StatusCode::UNAUTHORIZED,
            "invalid_proof",
        ),
        (
            "Proof sap_wrong-kind",
            StatusCode::FORBIDDEN,
            "proof_without_account",
        ),
    ] {
        let (got, body) = send(
            &harness.app,
            "POST",
            "/api/v1/tts",
            &[
                ("authorization", proof),
                ("idempotency-key", "tts-proof-refused"),
            ],
            Some(json!({"text":"no"})),
        )
        .await?;
        assert_eq!(got, status, "{proof}");
        assert_eq!(body["error"]["reason"], reason);
    }

    // Briefcase down: a direct request still completes, held with the reason.
    harness.briefcase.reset().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/uploads/reserve"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&harness.briefcase)
        .await;
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/tts",
        &[
            ("authorization", &ada),
            ("idempotency-key", "tts-direct-0002"),
        ],
        Some(json!({"text":"Briefcase is down"})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["storage"]["reason"], "briefcase_unavailable");
    assert!(
        body["storage"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Briefcase is unavailable"))
    );

    // STT: a direct caller reads its source with Waveform's Briefcase proof;
    // DM may transcribe for Ada because Ada's Briefcase access already exists.
    let source = briefcase_serves_source(&harness.briefcase).await;
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/stt",
        &[
            ("authorization", &ada),
            ("idempotency-key", "stt-direct-0001"),
        ],
        Some(json!({"file_url": source})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["transcript"], TEST_STT_TEXT);
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/stt",
        &[
            ("authorization", "Proof sap_from-dm"),
            ("idempotency-key", "stt-proof-0001"),
        ],
        Some(json!({"file_url": source})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // A Silicon that never used Waveform directly has no Briefcase access to lend.
    harness
        .accounts
        .valid_proof("sap_for-scout", SCOUT, "interface", &["waveform.stt"])
        .await;
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/stt",
        &[
            ("authorization", "Proof sap_for-scout"),
            ("idempotency-key", "stt-proof-0002"),
        ],
        Some(json!({"file_url": source})),
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "storage_authorization_required");
    assert!(
        body["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("waveform storage approve"))
    );
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn voice_catalog_and_the_stt_folder_follow_the_credential() -> TestResult {
    let harness = Harness::new("waveform.stt=interface").await?;
    harness.accounts.issues_briefcase_proofs().await;
    let ada = bearer(&harness.token(ADA));

    let (status, voices) = send(
        &harness.app,
        "GET",
        "/api/v1/voice-profiles",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{voices}");
    assert!(
        voices["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty() && items.iter().all(|v| v["id"].is_string())),
        "{voices}"
    );
    // Account routes take no proofs, even with a scope Waveform honours.
    harness
        .accounts
        .valid_proof("sap_interface-stt", BOB, "interface", &["waveform.stt"])
        .await;
    let (status, refused) = send(
        &harness.app,
        "GET",
        "/api/v1/voice-profiles",
        &[("authorization", "Proof sap_interface-stt")],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["reason"], "proof_not_accepted");

    // Ada's own token: Waveform gets Briefcase access and returns her folder.
    let folder_id = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path":"apps"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[
            {"id":Uuid::new_v4(),"type":"folder","name":"other","path":"apps/other","origin_app_id":"other"},
            {"id":folder_id,"type":"folder","name":"waveform","path":"apps/waveform","origin_app_id":"waveform","deleted_at":null}
        ],"next_cursor":null})))
        .mount(&harness.briefcase)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[],"next_cursor":null})),
        )
        .mount(&harness.briefcase)
        .await;
    let (status, target) = send(
        &harness.app,
        "POST",
        "/api/v1/stt/source-target",
        &[("authorization", &ada)],
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{target}");
    assert_eq!(target["folder_id"], folder_id.to_string());
    assert_eq!(target["folder_path"], "apps/waveform");
    assert_eq!(target["account"]["uuid"], "zQo");
    let requests = harness
        .briefcase
        .received_requests()
        .await
        .ok_or("requests")?;
    assert!(!requests.is_empty());
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Proof sap_briefcase-1")
            && !request.headers.contains_key("x-org-id")
    }));

    // Interface acting for Bob, who never gave Waveform Briefcase access.
    let (status, refused) = send(
        &harness.app,
        "POST",
        "/api/v1/stt/source-target",
        &[("authorization", "Proof sap_interface-stt")],
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "storage_authorization_required");
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn contract_negotiation_and_operator_deprecation_are_durable() -> TestResult {
    let harness = Harness::new("").await?;
    let (status, body, headers) =
        send_raw(&harness.app, "GET", "/api/v1/accounts", &[], None).await?;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["x-waveform-api-version"], "v1");
    assert_eq!(headers["x-waveform-contract-version"], "2.0.0");
    assert_eq!(headers["x-waveform-protocol-version"], "1");
    for (route, header, value) in [
        ("/api/v2/accounts", "x-waveform-api-version", "v2"),
        ("/api/v1/accounts", "x-waveform-api-version", "v2"),
        ("/api/v1/accounts", "x-waveform-protocol-version", "2"),
    ] {
        let (status, _) = send(&harness.app, "GET", route, &[(header, value)], None).await?;
        assert_eq!(
            status,
            StatusCode::NOT_ACCEPTABLE,
            "{route} {header}={value}"
        );
    }
    let (status, discovery) = send(&harness.app, "GET", "/api/contracts", &[], None).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(discovery["contracts"][0]["contract_version"], "2.0.0");
    assert_eq!(discovery["compatibility"][0]["cli"], ">=0.6.0, <0.7.0");

    // Deprecation is an operator command now; it needs an active successor.
    assert!(
        super::contracts::deprecate(&harness.pool, "v1", "v2")
            .await
            .is_err()
    );
    sqlx::query("INSERT INTO waveform_api_contracts(version,contract_version,state) VALUES('v2','3.0.0','active')").execute(&harness.pool).await?;
    super::contracts::deprecate(&harness.pool, "v1", "v2").await?;
    sqlx::query("UPDATE waveform_api_contracts SET deprecated_at=now()-interval '8 days',last_request_at=now()-interval '6 days' WHERE version='v1'").execute(&harness.pool).await?;
    assert_eq!(
        harness.state.sunset_contracts().await?,
        0,
        "recent traffic keeps v1"
    );
    let (status, _, headers) = send_raw(&harness.app, "GET", "/api/v1/accounts", &[], None).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.contains_key("deprecation"));
    sqlx::query("UPDATE waveform_api_contracts SET last_request_at=now()-interval '7 days' WHERE version='v1'").execute(&harness.pool).await?;
    assert_eq!(harness.state.sunset_contracts().await?, 1);
    let (status, body) = send(&harness.app, "GET", "/api/v1/accounts", &[], None).await?;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"]["code"], "api_version_sunset");
    let (status, _) = send(&harness.app, "GET", "/api/contracts", &[], None).await?;
    assert_eq!(status, StatusCode::OK, "discovery survives retirement");
    harness.finish().await
}

/// IAM-era rows as origin/main (migration 0015) stores them.
async fn seed_iam_era(pool: &PgPool, vault: &Vault, keys: &[(&str, Uuid)]) -> TestResult {
    let nil = Uuid::nil();
    for (public_id, key) in keys {
        sqlx::query("INSERT INTO waveform_actor_keys(plane_id, public_id, storage_actor_id) VALUES($1,$2,$3)")
            .bind(nil).bind(*public_id).bind(*key).execute(pool).await?;
    }
    let ada = keys[0].1;
    for (org, voice, age, openai) in [
        ("tos", "puck", "2 days", "sk-old-tos"),
        ("acme", "charon", "1 day", "sk-new-acme"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO waveform_account_preferences(plane_id, org_id, actor_id, voice_profile, tts_order, updated_at) VALUES($1,$2,$3,$4,'[\"openai\",\"gemini\",\"elevenlabs\"]', now()-interval '{age}')")))
            .bind(nil).bind(org).bind(ada).bind(voice).execute(pool).await?;
        let sealed = vault.seal(
            openai,
            &crate::control::legacy_key_context(org, ada, "openai"),
        )?;
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO waveform_provider_keys(plane_id, org_id, actor_id, provider, secret_cipher, updated_at) VALUES($1,$2,$3,'openai',$4, now()-interval '{age}')")))
            .bind(nil).bind(org).bind(ada).bind(sealed).execute(pool).await?;
        sqlx::query("INSERT INTO waveform_jobs(id,plane_id,org_id,actor_id,actor_kind,operation,status,first_line,request_digest,idempotency_key_hash,created_at,finished_at,duration_ms,provider) VALUES($1,$2,$3,$4,'carbon','tts','completed',$5,$6,$7,now()-interval '3 days',now()-interval '3 days',900,'gemini')")
            .bind(Uuid::now_v7()).bind(nil).bind(org).bind(ada).bind(format!("job in {org}")).bind(vec![1_u8; 32]).bind(vec![2_u8; 32])
            .execute(pool).await?;
    }
    let gemini = vault.seal(
        "AIza-tos-only",
        &crate::control::legacy_key_context("tos", ada, "gemini"),
    )?;
    sqlx::query("INSERT INTO waveform_provider_keys(plane_id, org_id, actor_id, provider, secret_cipher) VALUES($1,'tos',$2,'gemini',$3)")
        .bind(nil).bind(ada).bind(gemini).execute(pool).await?;
    sqlx::query("INSERT INTO waveform_storage_grants(plane_id, org_id, actor_id, endpoint_id, token_cipher, expires_at) VALUES($1,'tos',$2,'briefcase.files.read','\\x01'::bytea, now()+interval '1 hour')")
        .bind(nil).bind(ada).execute(pool).await?;
    sqlx::query("INSERT INTO waveform_bug_reports(id, plane_id, org_id, actor_id, idempotency_key, payload, status) VALUES($1,$2,'tos',$3,'report-1','{\"message\":\"old\"}','sent')")
        .bind(Uuid::now_v7()).bind(nil).bind(ada).execute(pool).await?;
    for (public_id, key) in &keys[1..] {
        let kind = if public_id.starts_with("si:") {
            "silicon"
        } else {
            "carbon"
        };
        sqlx::query("INSERT INTO waveform_jobs(id,plane_id,org_id,actor_id,actor_kind,operation,status,first_line,request_digest,idempotency_key_hash,created_at) VALUES($1,$2,'tos',$3,$4,'stt','running','',$5,$6,now())")
            .bind(Uuid::now_v7()).bind(nil).bind(*key).bind(kind).bind(vec![1_u8; 32]).bind(vec![2_u8; 32])
            .execute(pool).await?;
    }
    Ok(())
}

async fn count(pool: &PgPool, sql: &str) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await?)
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn iam_era_data_upgrades_and_links_to_accounts_without_loss() -> TestResult {
    use crate::operator::{LinkOptions, link_identities, parse_mapping};
    use secrecy::ExposeSecret as _;
    let (pool, schema) = database_with(Some(15)).await?;
    let vault = Vault::new(&SecretString::from("19".repeat(32)))?;
    let (ada, scout, gone, legacy) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    seed_iam_era(
        &pool,
        &vault,
        &[("c:ada", ada), ("si:scout", scout), ("c:gone", gone)],
    )
    .await?;
    // A pre-IAM 3 account kept its principal UUID as its key and has no public id row.
    sqlx::query("INSERT INTO waveform_jobs(id,plane_id,org_id,actor_id,actor_kind,operation,status,first_line,request_digest,idempotency_key_hash,created_at) VALUES($1,$2,'tos',$3,'silicon','tts','running','legacy',$4,$5,now())")
        .bind(Uuid::now_v7()).bind(Uuid::nil()).bind(legacy).bind(vec![1_u8; 32]).bind(vec![2_u8; 32])
        .execute(&pool).await?;
    let jobs_before = count(&pool, "SELECT count(*) FROM waveform_jobs").await?;

    // The upgrade itself (0016, 0017) keeps every byte and seeds the IAM mapping.
    sqlx::migrate!().run(&pool).await?;
    assert_eq!(
        count(&pool, "SELECT count(*) FROM waveform_jobs").await?,
        jobs_before
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM waveform_identity_links WHERE accounts_uuid IS NULL"
        )
        .await?,
        3
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM waveform_provider_keys").await?,
        3
    );
    let contract: String = sqlx::query_scalar(
        "SELECT contract_version FROM waveform_api_contracts WHERE version='v1'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(contract, "2.0.0");

    let mapping = parse_mapping(&format!(
        "iam_principal_id,accounts_uuid\nc:ada,zQo\nsi:scout,K1E\n{legacy},L3g\nc:nobody,N0b\n"
    ))
    .map_err(|errors| errors.join("; "))?;
    // Different legacy identities must never collapse into the same account,
    // even with --merge and even before either account has used the app.
    let ambiguous =
        parse_mapping("c:ada,Same1\nsi:scout,Same1\n").map_err(|errors| errors.join("; "))?;
    let refused = link_identities(
        &pool,
        Some(&vault),
        &ambiguous,
        LinkOptions {
            merge: true,
            ..LinkOptions::default()
        },
    )
    .await?;
    assert!(!refused.conflicts.is_empty());
    assert_eq!(
        count(&pool, "SELECT count(*) FROM waveform_account_keys").await?,
        0
    );

    // Dry run: a full report, nothing applied.
    let report = link_identities(
        &pool,
        Some(&vault),
        &mapping,
        LinkOptions {
            dry_run: true,
            ..LinkOptions::default()
        },
    )
    .await?;
    assert_eq!(
        (
            report.linked,
            report.unmatched.clone(),
            report.conflicts.len()
        ),
        (3, vec!["c:nobody".to_owned()], 0)
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM waveform_account_keys").await?,
        0
    );
    // Real run.
    let report = link_identities(&pool, Some(&vault), &mapping, LinkOptions::default()).await?;
    assert_eq!(report.linked, 3);
    assert_eq!(report.preferences_consolidated, 1);
    assert_eq!(report.provider_keys_resealed, 2);
    assert_eq!(report.storage_grants_invalidated, 1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM waveform_account_keys WHERE source='link_identities'"
        )
        .await?,
        3
    );
    let legacy_key: Uuid = sqlx::query_scalar(
        "SELECT storage_actor_id FROM waveform_account_keys WHERE account_uuid='L3g'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_key, legacy);
    let silicon_kind: String =
        sqlx::query_scalar("SELECT kind FROM waveform_account_keys WHERE account_uuid='L3g'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(silicon_kind, "silicon");
    // Nothing was deleted; legacy rows stay next to the consolidated ones.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM waveform_provider_keys").await?,
        5
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM waveform_account_preferences").await?,
        3
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM waveform_storage_grants").await?,
        1
    );
    // Idempotent.
    let again = link_identities(&pool, Some(&vault), &mapping, LinkOptions::default()).await?;
    assert_eq!(
        (
            again.linked,
            again.already_linked,
            again.provider_keys_resealed
        ),
        (0, 3, 0)
    );
    // A different account for linked data is a conflict, and nothing changes...
    let conflicting = parse_mapping("si:scout,S2s\n").map_err(|errors| errors.join("; "))?;
    let report = link_identities(&pool, Some(&vault), &conflicting, LinkOptions::default()).await?;
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM waveform_account_keys WHERE account_uuid='S2s'"
        )
        .await?,
        0
    );
    // A corrected identity must keep its earlier sign-out fence even when its
    // storage key is removed. It must not gain a new unrevoked first-use identity.
    sqlx::query("UPDATE waveform_accounts SET revoked_before=now()-interval '1 minute', revoked_reason='signed_out' WHERE account_uuid='K1E'")
        .execute(&pool).await?;
    // ...unless the operator explicitly relinks a link this command made.
    let report = link_identities(
        &pool,
        Some(&vault),
        &conflicting,
        LinkOptions {
            relink: true,
            ..LinkOptions::default()
        },
    )
    .await?;
    assert_eq!((report.linked, report.conflicts.len()), (1, 0));
    assert_eq!(count(&pool, "SELECT count(*) FROM waveform_account_pending_events WHERE account_uuid='K1E' AND revoked_before IS NOT NULL").await?, 1);
    let back = parse_mapping("si:scout,K1E\n").map_err(|errors| errors.join("; "))?;
    link_identities(
        &pool,
        Some(&vault),
        &back,
        LinkOptions {
            relink: true,
            ..LinkOptions::default()
        },
    )
    .await?;

    // The linked accounts sign in and see their pre-cutover data.
    let harness = Harness::over(pool.clone(), schema, "").await?;
    let ada_bearer = bearer(&harness.token(ADA));
    let (status, jobs) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs",
        &[("authorization", &ada_bearer)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{jobs}");
    assert_eq!(jobs["items"].as_array().map(Vec::len), Some(2));
    let (_, preferences) = send(
        &harness.app,
        "GET",
        "/api/v1/preferences",
        &[("authorization", &ada_bearer)],
        None,
    )
    .await?;
    assert_eq!(
        preferences["voice_profile"], "charon",
        "the newest legacy preference wins"
    );
    let actor = crate::domain::auth::AuthorizedActor {
        actor: crate::domain::identity::Actor::new(
            crate::domain::identity::ActorKind::Carbon,
            crate::domain::identity::ActorId::new(ada)?,
        ),
        account: crate::domain::identity::AccountRef {
            uuid: "zQo".parse()?,
            id: "c:ada".to_owned(),
        },
        originating_application: None,
        expires_at: None,
        scopes: Vec::new(),
        token_family: None,
    };
    let keys = crate::application::ports::ProviderKeyStore::load(harness.state.as_ref(), &actor)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        keys[&crate::domain::provider::ProviderName::OpenAi].expose_secret(),
        "sk-new-acme"
    );
    assert_eq!(
        keys[&crate::domain::provider::ProviderName::Gemini].expose_secret(),
        "AIza-tos-only"
    );

    // An account that signed in before its mapping ran can be merged when its new key is unused.
    sign_in(&harness, ("Fr3", "carbon", "c:gone")).await?;
    let late = parse_mapping("c:gone,Fr3\n").map_err(|errors| errors.join("; "))?;
    let report = link_identities(&pool, Some(&vault), &late, LinkOptions::default()).await?;
    assert_eq!(report.conflicts.len(), 1, "{:?}", report.conflicts);
    let report = link_identities(
        &pool,
        Some(&vault),
        &late,
        LinkOptions {
            merge: true,
            ..LinkOptions::default()
        },
    )
    .await?;
    assert_eq!((report.linked, report.conflicts.len()), (1, 0));
    let merged: Uuid = sqlx::query_scalar(
        "SELECT storage_actor_id FROM waveform_account_keys WHERE account_uuid='Fr3'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(merged, gone);
    // Once the linked identity changes its public id, correcting its ownership
    // would transfer new data. Refuse it and retain the current mapping.
    sqlx::query("UPDATE waveform_accounts SET id_changed_at=clock_timestamp()+interval '1 second' WHERE account_uuid='K1E'").execute(&pool).await?;
    let refused = link_identities(
        &pool,
        Some(&vault),
        &conflicting,
        LinkOptions {
            relink: true,
            ..LinkOptions::default()
        },
    )
    .await?;
    assert!(
        refused
            .conflicts
            .iter()
            .any(|message| message.contains("has used its linked data"))
    );
    harness.finish().await
}

/// Proof issuance that answers a new proof for every new Idempotency-Key
/// (`sap_briefcase-N`, `proof-N`) and the same proof for a key it saw before, as
/// Silicon Accounts does; answers the keys it was given.
async fn issues_numbered_proofs(accounts: &TestAccounts) -> Arc<std::sync::Mutex<Vec<String>>> {
    let keys = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = keys.clone();
    Mock::given(method("POST"))
        .and(path("/v1/proofs/user-verification"))
        .respond_with(move |request: &wiremock::Request| {
            let key = request
                .headers
                .get("idempotency-key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let number = seen.lock().map_or(0, |mut keys| {
                keys.iter().position(|known| *known == key).map_or_else(
                    || {
                        keys.push(key.clone());
                        keys.len()
                    },
                    |index| index + 1,
                )
            });
            let mut body = issued_proof(
                &format!("sap_briefcase-{number}"),
                &format!("sapr_briefcase-{number}"),
            );
            body["proof_id"] = json!(format!("proof-{number}"));
            ResponseTemplate::new(201).set_body_json(body)
        })
        .mount(&accounts.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/proofs/revoke"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&accounts.server)
        .await;
    keys
}

async fn speak(
    harness: &Harness,
    authorization: &str,
    key: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/tts",
        &[("authorization", authorization), ("idempotency-key", key)],
        Some(json!({"text": "Hello from the storage tests."})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body)
}

async fn briefcase_access(
    harness: &Harness,
    authorization: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/storage-authorizations",
        &[("authorization", authorization)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body["briefcase"].clone())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn revoked_briefcase_access_stays_revoked_until_approved_and_approval_gets_a_new_proof()
-> TestResult {
    let harness = Harness::new("").await?;
    harness.accounts.introspection(true).await;
    let keys = issues_numbered_proofs(&harness.accounts).await;
    briefcase_accepts_uploads(&harness.briefcase).await;
    sign_in(&harness, ADA).await?;
    let ada = bearer(&harness.token(ADA));

    // The first direct speech gives Waveform access and lands in Briefcase.
    assert_eq!(
        speak(&harness, &ada, "speech-first").await?["storage"]["status"],
        "stored"
    );
    assert_eq!(
        briefcase_access(&harness, &ada).await?["proof_id"],
        "proof-1"
    );
    // The account revokes it: speech is held and nothing is issued behind its back.
    let (status, body) = send(
        &harness.app,
        "DELETE",
        "/api/v1/storage-authorizations",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(
        (status, body["briefcase"]["revoked_reason"].clone()),
        (StatusCode::OK, json!("revoked_by_account"))
    );
    let held = speak(&harness, &ada, "speech-after-revoke").await?;
    assert_eq!(
        held["storage"]["reason"], "storage_not_authorized",
        "{held}"
    );
    assert_eq!(briefcase_access(&harness, &ada).await?["status"], "revoked");
    assert_eq!(keys.lock().map_or(0, |keys| keys.len()), 1);
    // Approving again gets a new proof, not the revoked one replayed for the same token.
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/storage-authorizations",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["briefcase"]["proof_id"], "proof-2");
    assert!(
        keys.lock()
            .is_ok_and(|keys| keys.len() == 2 && keys[0] != keys[1])
    );
    assert_eq!(
        speak(&harness, &ada, "speech-approved").await?["storage"]["status"],
        "stored"
    );

    // An end nobody decided (the account's sign-ins ended) is replaced by the next direct speech.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let (status, body) = deliver(
        &harness.app,
        &event(
            "membership.signed_out",
            json!({"uuid":"zQo","membership_id":"waveform:zQo","reason":"stk_rotated"}),
        ),
    )
    .await?;
    assert_eq!(
        (status, body["outcome"].clone()),
        (StatusCode::OK, json!("sign_ins_ended"))
    );
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let fresh = bearer(&harness.token(ADA));
    assert_eq!(
        briefcase_access(&harness, &fresh).await?["revoked_reason"],
        "stk_rotated"
    );
    assert_eq!(
        speak(&harness, &fresh, "speech-after-sign-out").await?["storage"]["status"],
        "stored"
    );
    assert_eq!(
        briefcase_access(&harness, &fresh).await?["proof_id"],
        "proof-3"
    );
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn a_proof_briefcase_refuses_is_replaced_when_it_ended_but_an_account_revocation_is_kept()
-> TestResult {
    let harness = Harness::new("").await?;
    harness.accounts.introspection(true).await;
    let keys = issues_numbered_proofs(&harness.accounts).await;
    let reason = Arc::new(std::sync::Mutex::new("sign_in_revoked".to_owned()));
    let answer = reason.clone();
    Mock::given(method("POST"))
        .and(path("/v1/proofs/refresh"))
        .respond_with(move |_request: &wiremock::Request| {
            let reason = answer.lock().map(|reason| reason.clone()).unwrap_or_default();
            ResponseTemplate::new(410).set_body_json(json!({"error":{"code":"proof_revoked",
                "message":"Proof can't be refreshed.","details":{"proof_id":"proof-n","reason":reason}}}))
        })
        .mount(&harness.accounts.server)
        .await;
    // Briefcase refuses proof-1: it ended with the sign-in it was issued under.
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/uploads/reserve"))
        .and(header("authorization", "Proof sap_briefcase-1"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error":{"code":"proof_invalid"}})),
        )
        .with_priority(1)
        .mount(&harness.briefcase)
        .await;
    briefcase_accepts_uploads(&harness.briefcase).await;
    sign_in(&harness, ADA).await?;
    let ada = bearer(&harness.token(ADA));

    let stored = speak(&harness, &ada, "replaced").await?;
    assert_eq!(stored["storage"]["status"], "stored", "{stored}");
    let access = briefcase_access(&harness, &ada).await?;
    assert_eq!(
        (access["status"].clone(), access["proof_id"].clone()),
        (json!("active"), json!("proof-2"))
    );
    assert_eq!(keys.lock().map_or(0, |keys| keys.len()), 2);

    // The account revokes proof-2 at Silicon Accounts: Briefcase refuses it, and
    // Silicon Accounts says why; Waveform keeps it revoked instead of re-issuing.
    if let Ok(mut reason) = reason.lock() {
        "revoked_by_account".clone_into(&mut reason);
    }
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/uploads/reserve"))
        .and(header("authorization", "Proof sap_briefcase-2"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error":{"code":"proof_invalid"}})),
        )
        .with_priority(1)
        .mount(&harness.briefcase)
        .await;
    for key in ["revoked-at-accounts", "still-revoked"] {
        let held = speak(&harness, &ada, key).await?;
        assert_eq!(
            held["storage"]["reason"], "storage_not_authorized",
            "{key}: {held}"
        );
    }
    let access = briefcase_access(&harness, &ada).await?;
    assert_eq!(
        (access["status"].clone(), access["revoked_reason"].clone()),
        (json!("revoked"), json!("revoked_at_accounts"))
    );
    assert_eq!(keys.lock().map_or(0, |keys| keys.len()), 2);
    // Until the account approves again.
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/storage-authorizations",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(
        (status, body["briefcase"]["proof_id"].clone()),
        (StatusCode::CREATED, json!("proof-3"))
    );
    assert_eq!(
        speak(&harness, &ada, "approved-again").await?["storage"]["status"],
        "stored"
    );
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn me_shows_the_accounts_profile_from_userinfo_once() -> TestResult {
    let harness = Harness::new("").await?;
    Mock::given(method("GET"))
        .and(path("/v1/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"uuid":"zQo","membership_id":"waveform:zQo",
            "kind":"carbon","id":"c:ada","display_name":"Ada King","pfp_url":"https://accounts.example.test/pfp/zQo",
            "version":3,"sub":"zQo","name":"Ada King"})))
        .expect(1)
        .mount(&harness.accounts.server)
        .await;
    let ada = bearer(&harness.token(ADA));
    for _ in 0..2 {
        let (status, body) = send(
            &harness.app,
            "GET",
            "/api/v1/me",
            &[("authorization", &ada)],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["account"]["display_name"], "Ada King");
        assert_eq!(
            body["account"]["pfp_url"],
            "https://accounts.example.test/pfp/zQo"
        );
    }
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn speech_never_takes_an_account_selector() -> TestResult {
    let harness = Harness::new("").await?;
    let ada = bearer(&harness.token(ADA));
    let audio = format!("/api/v1/audio/{}?account=si:scout", Uuid::new_v4());
    for (verb, route, body) in [
        (
            "POST",
            "/api/v1/tts?account=si:scout",
            Some(json!({"text": "Not as the Silicon."})),
        ),
        (
            "POST",
            "/api/v1/stt?account=si:scout",
            Some(
                json!({"file_url": format!("{}/zQo/apps/waveform/a.mp3", harness.briefcase.uri())}),
            ),
        ),
        (
            "POST",
            "/api/v1/stt/source-target?account=si:scout",
            Some(json!({})),
        ),
        ("GET", audio.as_str(), None),
    ] {
        let (status, answer) = send(
            &harness.app,
            verb,
            route,
            &[
                ("authorization", &ada),
                ("idempotency-key", "selector-key-1"),
            ],
            body,
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{route}: {answer}");
        assert_eq!(
            answer["error"]["code"], "speech_runs_as_yourself",
            "{route}"
        );
        assert!(answer["error"]["hint"].is_string(), "{route}");
    }
    harness.finish().await
}
