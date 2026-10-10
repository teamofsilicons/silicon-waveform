//! The account cache against PostgreSQL and a stubbed Silicon Accounts: public
//! ids follow the newest verified credential, custodians are confirmed before a
//! stale cache is trusted, and naming accounts is bounded per caller.

#![allow(clippy::too_many_lines)]

use std::time::Duration;

use http::StatusCode;
use serde_json::{Value, json};
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path},
};

use super::tests::{
    ADA, BOB, Harness, ROVER, SCOUT, TestResult, bearer, deliver, event, send, sign_in,
    wait_for_custodian,
};

/// Accounts that use Waveform only through another app (Interface).
const HAVEN: (&str, &str, &str) = ("Hv5", "silicon", "si:haven");
const VERA: (&str, &str, &str) = ("V9x", "carbon", "c:vera");
/// Carbons Silicon Accounts has events about before they ever call Waveform.
const BEA: (&str, &str, &str) = ("KYh", "carbon", "c:bea");
const CAL: (&str, &str, &str) = ("nh0", "carbon", "c:cal");
const DOT: (&str, &str, &str) = ("D0t", "carbon", "c:dot");

/// A Waveform token for `account` carrying `id`, issued `age` ago.
fn token_with(harness: &Harness, account: (&str, &str, &str), id: &str, age: i64) -> String {
    let mut claims = harness.accounts.claims(account.0, account.1, id);
    let issued = time::OffsetDateTime::now_utc().unix_timestamp() - age;
    claims["iat"] = json!(issued);
    claims["nbf"] = json!(issued);
    bearer(&harness.accounts.key.sign(&claims))
}

async fn me(harness: &Harness, authorization: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", authorization)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body)
}

/// How many times Waveform asked Silicon Accounts about `uuid`.
async fn lookups_of(harness: &Harness, uuid: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let wanted = format!("/v1/accounts/{uuid}");
    Ok(harness
        .accounts
        .server
        .received_requests()
        .await
        .ok_or("requests")?
        .iter()
        .filter(|request| request.url.path() == wanted)
        .count())
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn a_cached_id_follows_the_newest_verified_credential() -> TestResult {
    let harness = Harness::new("waveform.tts=interface").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, "c:ada")))
        .await;
    harness.accounts.introspection(true).await;
    // An earlier handle cached for the account, as a cutover link left it.
    sqlx::query("INSERT INTO waveform_account_keys(account_uuid, storage_actor_id, kind, source) VALUES('zQo', $1, 'carbon', 'link_identities')")
        .bind(uuid::Uuid::now_v7())
        .execute(&harness.pool)
        .await?;
    sqlx::query("INSERT INTO waveform_accounts(account_uuid, kind, current_id) VALUES('zQo', 'carbon', 'c:ada')")
        .execute(&harness.pool)
        .await?;
    sign_in(&harness, SCOUT).await?;
    wait_for_custodian(&harness, SCOUT.0).await?;

    // The token says c:ada-lovelace: /me shows it, and selecting it means the caller.
    let lovelace = token_with(&harness, ADA, "c:ada-lovelace", 30);
    assert_eq!(
        me(&harness, &lovelace).await?["account"]["id"],
        "c:ada-lovelace"
    );
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/preferences?account=c:ada-lovelace",
        &[("authorization", &lovelace)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["uuid"], "zQo");
    assert_eq!(body["account"]["id"], "c:ada-lovelace");
    // Its Silicon shows the custodian's new id too.
    let (_, scout) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&harness.token(SCOUT)))],
        None,
    )
    .await?;
    assert_eq!(scout["account"]["custodian"]["id"], "c:ada-lovelace");

    // A rename Silicon Accounts announces is not undone by an older token...
    let renamed = event(
        "account.id_changed",
        json!({"uuid":"zQo","membership_id":"waveform:zQo","kind":"carbon","old_id":"c:ada-lovelace","new_id":"c:ada-king"}),
    );
    assert_eq!(
        deliver(&harness.app, &renamed).await?.1["outcome"],
        "id_updated"
    );
    let older = token_with(&harness, ADA, "c:ada-lovelace", 60);
    assert_eq!(me(&harness, &older).await?["account"]["id"], "c:ada-king");
    // ...but a token issued after it, carrying a newer id, is.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let newer = token_with(&harness, ADA, "c:ada-queen", 0);
    assert_eq!(me(&harness, &newer).await?["account"]["id"], "c:ada-queen");
    assert_eq!(me(&harness, &older).await?["account"]["id"], "c:ada-queen");

    // A proof Silicon Accounts verifies just now carries the current id.
    harness
        .accounts
        .valid_proof(
            "sap_current-id",
            (ADA.0, ADA.1, "c:ada-empress"),
            "interface",
            &["waveform.tts"],
        )
        .await;
    let held: Option<String> =
        sqlx::query_scalar("SELECT current_id FROM waveform_accounts WHERE account_uuid='zQo'")
            .fetch_one(&harness.pool)
            .await?;
    assert_eq!(held.as_deref(), Some("c:ada-queen"));
    let (status, _) = send(
        &harness.app,
        "GET",
        &format!("/api/v1/audio/{}", uuid::Uuid::new_v4()),
        &[("authorization", "Proof sap_current-id")],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        me(&harness, &newer).await?["account"]["id"],
        "c:ada-empress"
    );
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn a_former_custodian_is_refused_once_the_cached_custodian_is_stale() -> TestResult {
    let harness = Harness::new("").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    harness.accounts.introspection(true).await;
    for account in [ADA, BOB, SCOUT] {
        sign_in(&harness, account).await?;
    }
    wait_for_custodian(&harness, SCOUT.0).await?;
    let ada = bearer(&harness.token(ADA));
    let bob = bearer(&harness.token(BOB));
    let (status, _) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?account=K1E",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    // Scout is handed to Bob at Silicon Accounts and no webhook arrives; Waveform's
    // cache still names Ada, last confirmed two days ago.
    harness.accounts.server.reset().await;
    remount_keys(&harness).await;
    harness.accounts.introspection(true).await;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((BOB.0, BOB.2)))
        .await;
    let make_stale = "UPDATE waveform_accounts SET custodian_uuid = 'zQo', custodian_id = 'c:ada', custodian_checked_at = now() - interval '2 days' WHERE account_uuid = 'K1E'";
    sqlx::query(make_stale).execute(&harness.pool).await?;
    // The list of Silicons asks again about the stale one and leaves it out.
    let (_, listed) = send(
        &harness.app,
        "GET",
        "/api/v1/silicons",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(listed, json!({"items": [], "complete": true}));
    assert_eq!(lookups_of(&harness, "K1E").await?, 1);
    // So does every route that names it.
    sqlx::query(make_stale).execute(&harness.pool).await?;
    let attempts: [(&str, &str, Option<Value>); 5] = [
        ("GET", "/api/v1/jobs?account=K1E", None),
        (
            "PATCH",
            "/api/v1/preferences?account=K1E",
            Some(json!({"voice_profile":"puck"})),
        ),
        (
            "PUT",
            "/api/v1/provider-keys/openai?account=K1E",
            Some(json!({"api_key":"sk-former-custodian"})),
        ),
        ("DELETE", "/api/v1/storage-authorizations?account=K1E", None),
        ("GET", "/api/v1/provider-keys?account=K1E", None),
    ];
    for (verb, route, body) in attempts {
        let (status, answer) =
            send(&harness.app, verb, route, &[("authorization", &ada)], body).await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{verb} {route}: {answer}");
        assert_eq!(answer["error"]["code"], "not_custodian", "{verb} {route}");
    }
    assert_eq!(
        lookups_of(&harness, "K1E").await?,
        2,
        "the first refusal asked Silicon Accounts; the rest used its fresh answer"
    );

    // Bob, the custodian now, reads from the refreshed cache without asking again...
    let before = lookups_of(&harness, "K1E").await?;
    let (status, _) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?account=K1E",
        &[("authorization", &bob)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lookups_of(&harness, "K1E").await?, before);
    let (_, listed) = send(
        &harness.app,
        "GET",
        "/api/v1/silicons",
        &[("authorization", &bob)],
        None,
    )
    .await?;
    assert_eq!(listed["items"][0]["uuid"], "K1E");
    assert_eq!(listed["complete"], true);
    // ...but every change he makes is confirmed with Silicon Accounts first.
    let (status, body) = send(
        &harness.app,
        "PATCH",
        "/api/v1/preferences?account=K1E",
        &[("authorization", &bob)],
        Some(json!({"voice_profile":"puck"})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(lookups_of(&harness, "K1E").await?, before + 1);

    // When Silicon Accounts cannot confirm a stale custodian, Waveform refuses.
    harness.accounts.server.reset().await;
    remount_keys(&harness).await;
    Mock::given(method("GET"))
        .and(path("/v1/accounts/K1E"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&harness.accounts.server)
        .await;
    sqlx::query("UPDATE waveform_accounts SET custodian_checked_at = now() - interval '11 minutes' WHERE account_uuid = 'K1E'")
        .execute(&harness.pool)
        .await?;
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?account=K1E",
        &[("authorization", &bob)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "dependency_unavailable");
    let (_, listed) = send(
        &harness.app,
        "GET",
        "/api/v1/silicons",
        &[("authorization", &bob)],
        None,
    )
    .await?;
    assert_eq!(listed, json!({"items": [], "complete": false}));
    harness.finish().await
}

async fn remount_keys(harness: &Harness) {
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"keys":[harness.accounts.key.jwk()]})),
        )
        .mount(&harness.accounts.server)
        .await;
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn naming_accounts_is_bounded_per_caller_and_never_caches_strangers() -> TestResult {
    let harness = Harness::new("").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    harness
        .accounts
        .account(ROVER.0, ROVER.1, ROVER.2, Some((BOB.0, BOB.2)))
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(
            "^/v1/accounts/by-id/si:probe-",
        ))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error":{"code":"account_not_found","message":"no"}})),
        )
        .mount(&harness.accounts.server)
        .await;
    for account in [ADA, BOB, SCOUT] {
        sign_in(&harness, account).await?;
    }
    wait_for_custodian(&harness, SCOUT.0).await?;
    let ada = bearer(&harness.token(ADA));
    let jobs = |selector: &str| format!("/api/v1/jobs?account={selector}");

    // Another Carbon's Silicon is refused and leaves nothing in Waveform's cache.
    let (status, _) = send(
        &harness.app,
        "GET",
        &jobs("R0v"),
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let cached: i64 =
        sqlx::query_scalar("SELECT count(*) FROM waveform_account_keys WHERE account_uuid='R0v'")
            .fetch_one(&harness.pool)
            .await?;
    assert_eq!(cached, 0);
    // An id nobody has is asked about once a minute, however often it is named.
    for _ in 0..3 {
        let (status, _) = send(
            &harness.app,
            "GET",
            &jobs("si:probe-0"),
            &[("authorization", &ada)],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let asked = harness
        .accounts
        .server
        .received_requests()
        .await
        .ok_or("requests")?
        .iter()
        .filter(|request| request.url.path() == "/v1/accounts/by-id/si:probe-0")
        .count();
    assert_eq!(asked, 1);
    // Ada has now made 2 lookups; 28 more distinct names are allowed this minute.
    for n in 1..=28 {
        let (status, _) = send(
            &harness.app,
            "GET",
            &jobs(&format!("si:probe-{n}")),
            &[("authorization", &ada)],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "probe {n}");
    }
    let (status, body, headers) = super::tests::send_raw(
        &harness.app,
        "GET",
        &jobs("si:probe-29"),
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(headers["retry-after"], "60");
    let body: Value = serde_json::from_slice(&body)?;
    assert_eq!(body["error"]["code"], "account_lookup_limit");
    // Her own Silicon, already known, still works; Bob has his own allowance.
    let (status, _) = send(
        &harness.app,
        "GET",
        &jobs("K1E"),
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(
        &harness.app,
        "GET",
        &jobs("si:rover"),
        &[("authorization", &bearer(&harness.token(BOB)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["uuid"], "R0v");
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn a_stored_record_of_the_other_kind_shows_no_data() -> TestResult {
    let harness = Harness::new("").await?;
    // A wrong cutover mapping linked a Silicon's earlier data to Ada's uuid.
    sqlx::query("INSERT INTO waveform_account_keys(account_uuid, storage_actor_id, kind, source) VALUES('zQo', $1, 'silicon', 'link_identities')")
        .bind(uuid::Uuid::now_v7())
        .execute(&harness.pool)
        .await?;
    sqlx::query("INSERT INTO waveform_accounts(account_uuid, kind, current_id) VALUES('zQo', 'silicon', 'si:scout-earlier')")
        .execute(&harness.pool)
        .await?;
    for route in ["/api/v1/me", "/api/v1/jobs", "/api/v1/preferences"] {
        let (status, body) = send(
            &harness.app,
            "GET",
            route,
            &[("authorization", &bearer(&harness.token(ADA)))],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{route}: {body}");
        assert_eq!(
            body["error"]["reason"], "account_record_conflict",
            "{route}"
        );
    }
    let kept: (String, Option<String>) =
        sqlx::query_as("SELECT kind, current_id FROM waveform_accounts WHERE account_uuid='zQo'")
            .fetch_one(&harness.pool)
            .await?;
    assert_eq!(
        kept,
        ("silicon".to_owned(), Some("si:scout-earlier".to_owned())),
        "the record is left for the operator to correct"
    );
    harness.finish().await
}

/// Silicon Accounts answers that `uuid` was deleted.
async fn deleted_at_accounts(harness: &Harness, uuid: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/v1/accounts/{uuid}")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error":{
            "code":"account_deleted","message":format!("The account {uuid} was deleted at 2026-10-10T00:00:00.000Z.")}})))
        .mount(&harness.accounts.server)
        .await;
}

/// Rows Waveform holds for an account, table by table.
async fn holdings(harness: &Harness, uuid: &str) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    let key = super::tests::storage_key(harness, uuid).await?;
    let mut counts = Vec::new();
    for sql in [
        "SELECT count(*) FROM waveform_jobs WHERE actor_id=$1",
        "SELECT count(*) FROM waveform_idempotency_records WHERE actor_id=$1",
        "SELECT count(*) FROM waveform_held_audio WHERE storage_actor_id=$1",
        "SELECT count(*) FROM waveform_account_preferences WHERE actor_id=$1",
    ] {
        counts.push(
            sqlx::query_scalar(sql)
                .bind(key)
                .fetch_one(&harness.pool)
                .await?,
        );
    }
    Ok(counts)
}

async fn speak_with_proof(
    harness: &Harness,
    proof: &str,
    key: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let (status, body) = send(
        &harness.app,
        "POST",
        "/api/v1/tts",
        &[("authorization", proof), ("idempotency-key", key)],
        Some(json!({"text": "A private line spoken through another app."})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body)
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn accounts_no_webhook_reaches_are_checked_daily_and_purged_once_deleted() -> TestResult {
    let harness = Harness::new("waveform.tts=interface").await?;
    harness
        .accounts
        .account(HAVEN.0, HAVEN.1, HAVEN.2, Some((ADA.0, ADA.2)))
        .await;
    harness.accounts.account(VERA.0, VERA.1, VERA.2, None).await;
    harness.accounts.account(ADA.0, ADA.1, ADA.2, None).await;
    harness.accounts.introspection(true).await;
    // Haven and Vera use Waveform only through Interface; Ada looks after Haven.
    for (who, proof) in [(HAVEN, "sap_for-haven"), (VERA, "sap_for-vera")] {
        harness
            .accounts
            .valid_proof(proof, who, "interface", &["waveform.tts"])
            .await;
        let held = speak_with_proof(
            &harness,
            &format!("Proof {proof}"),
            &format!("proof-only-{}", who.0),
        )
        .await?;
        assert_eq!(held["storage"]["reason"], "delegated_call");
    }
    wait_for_custodian(&harness, HAVEN.0).await?;
    sign_in(&harness, ADA).await?;
    let (status, _) = send(
        &harness.app,
        "PATCH",
        "/api/v1/preferences?account=Hv5",
        &[("authorization", &bearer(&harness.token(ADA)))],
        Some(json!({"voice_profile":"puck"})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(holdings(&harness, HAVEN.0).await?, vec![1, 1, 1, 1]);
    assert_eq!(holdings(&harness, VERA.0).await?, vec![1, 1, 1, 1]);

    // Both delete their accounts; Silicon Accounts sends Waveform no webhook (they
    // were never members), and later says so when asked. Ada still exists.
    harness.accounts.server.reset().await;
    remount_keys(&harness).await;
    deleted_at_accounts(&harness, HAVEN.0).await;
    deleted_at_accounts(&harness, VERA.0).await;
    harness.accounts.account(ADA.0, ADA.1, ADA.2, None).await;
    // Haven was looked up minutes ago (its custodian's check), so it is not due
    // before tomorrow; the accounts never looked up are due at once.
    let report = harness.state.sweep_accounts().await.map_err(|_| "sweep")?;
    assert_eq!((report.checked, report.purged), (1, 1), "Ada and Vera");
    assert_eq!(holdings(&harness, HAVEN.0).await?, vec![1, 1, 1, 1]);
    // A day later Haven is due too.
    sqlx::query("UPDATE waveform_accounts SET looked_up_at = looked_up_at - interval '25 hours' WHERE account_uuid = 'Hv5'")
        .execute(&harness.pool)
        .await?;
    let report = harness.state.sweep_accounts().await.map_err(|_| "sweep")?;
    assert_eq!(
        (
            report.checked,
            report.purged,
            report.unknown,
            report.stopped
        ),
        (0, 1, 0, false)
    );
    for uuid in [HAVEN.0, VERA.0] {
        assert_eq!(holdings(&harness, uuid).await?, vec![0, 0, 0, 0], "{uuid}");
        let (id, deleted): (Option<String>, Option<time::OffsetDateTime>) = sqlx::query_as(
            "SELECT current_id, deleted_at FROM waveform_accounts WHERE account_uuid=$1",
        )
        .bind(uuid)
        .fetch_one(&harness.pool)
        .await?;
        assert!(
            id.is_none() && deleted.is_some(),
            "{uuid} keeps only a tombstone"
        );
    }
    // Everything was asked about today, so the next run has nothing to do.
    assert_eq!(
        harness.state.sweep_accounts().await.map_err(|_| "sweep")?,
        crate::infrastructure::directory::SweepReport::default()
    );

    // "No such account" deletes nothing: it means Waveform asks the wrong deployment.
    sign_in(&harness, BOB).await?;
    sqlx::query("INSERT INTO waveform_account_preferences(plane_id, org_id, actor_id, voice_profile) VALUES($1, '@', $2, 'kore')")
        .bind(uuid::Uuid::nil())
        .bind(super::tests::storage_key(&harness, BOB.0).await?)
        .execute(&harness.pool)
        .await?;
    Mock::given(method("GET"))
        .and(path("/v1/accounts/B0b"))
        .respond_with(ResponseTemplate::new(404).set_body_json(
            json!({"error":{"code":"account_not_found","message":"No account has the uuid 'B0b'."}}),
        ))
        .mount(&harness.accounts.server)
        .await;
    let report = harness.state.sweep_accounts().await.map_err(|_| "sweep")?;
    assert_eq!((report.purged, report.unknown), (0, 1));
    assert_eq!(holdings(&harness, BOB.0).await?[3], 1);

    // When Silicon Accounts cannot be asked, the batch stops and is retried later.
    sqlx::query("UPDATE waveform_accounts SET looked_up_at = now() - interval '2 days' WHERE account_uuid = 'zQo'")
        .execute(&harness.pool)
        .await?;
    harness.accounts.server.reset().await;
    remount_keys(&harness).await;
    Mock::given(method("GET"))
        .and(path("/v1/accounts/zQo"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&harness.accounts.server)
        .await;
    let report = harness.state.sweep_accounts().await.map_err(|_| "sweep")?;
    assert!(report.stopped && report.purged == 0);
    let retry_in: f64 = sqlx::query_scalar("SELECT extract(epoch FROM looked_up_at + interval '24 hours' - now())::float8 FROM waveform_accounts WHERE account_uuid = 'zQo'")
        .fetch_one(&harness.pool)
        .await?;
    assert!(
        (3_000.0..=3_700.0).contains(&retry_in),
        "retried in about an hour, not {retry_in} s"
    );
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn a_custodian_check_that_finds_the_silicon_deleted_purges_it() -> TestResult {
    let harness = Harness::new("waveform.tts=interface").await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    harness
        .accounts
        .valid_proof("sap_for-scout", SCOUT, "interface", &["waveform.tts"])
        .await;
    speak_with_proof(&harness, "Proof sap_for-scout", "scout-before-deletion").await?;
    wait_for_custodian(&harness, SCOUT.0).await?;
    sign_in(&harness, ADA).await?;
    harness.accounts.server.reset().await;
    remount_keys(&harness).await;
    deleted_at_accounts(&harness, SCOUT.0).await;
    sqlx::query("UPDATE waveform_accounts SET custodian_checked_at = now() - interval '1 hour' WHERE account_uuid = 'K1E'")
        .execute(&harness.pool)
        .await?;
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/jobs?account=K1E",
        &[("authorization", &bearer(&harness.token(ADA)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "account_not_found");
    assert_eq!(holdings(&harness, SCOUT.0).await?, vec![0, 0, 0, 0]);
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn sign_outs_and_deletions_before_the_first_request_are_honoured() -> TestResult {
    let harness = Harness::new("").await?;
    // Tokens Silicon Accounts issued before the events; Waveform has seen none of them.
    let bea_before = token_with(&harness, BEA, BEA.2, 5);
    let cal_before = token_with(&harness, CAL, CAL.2, 5);
    let dot_before = token_with(&harness, DOT, DOT.2, 5);
    for (kind, data, outcome) in [
        (
            "membership.signed_out",
            json!({"uuid":"KYh","membership_id":"waveform:KYh","reason":"refresh_token_reuse"}),
            "sign_ins_ended_before_first_use",
        ),
        (
            "account.deleted",
            json!({"uuid":"nh0","membership_id":"waveform:nh0"}),
            "deleted_before_first_use",
        ),
        (
            "membership.signed_out",
            json!({"uuid":"D0t","membership_id":"waveform:D0t","reason":"app_revoked"}),
            "ignored_app_revoked",
        ),
    ] {
        let (status, body) = deliver(&harness.app, &event(kind, data)).await?;
        assert_eq!(
            (status, body["outcome"].as_str()),
            (StatusCode::OK, Some(outcome))
        );
    }
    for (token, reason) in [
        (&bea_before, "sign_in_ended"),
        (&cal_before, "account_deleted"),
    ] {
        let (status, body) = send(
            &harness.app,
            "GET",
            "/api/v1/me",
            &[("authorization", token)],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(body["error"]["reason"], reason);
    }
    let (status, _) = send(
        &harness.app,
        "POST",
        "/api/v1/tts",
        &[
            ("authorization", &cal_before),
            ("idempotency-key", "after-deletion-1"),
        ],
        Some(json!({"text": "Nothing after deletion."})),
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let tombstone: (Option<time::OffsetDateTime>, i64) = sqlx::query_as("SELECT a.deleted_at, (SELECT count(*) FROM waveform_jobs j WHERE j.actor_id = k.storage_actor_id) FROM waveform_accounts a JOIN waveform_account_keys k USING (account_uuid) WHERE a.account_uuid = 'nh0'")
        .fetch_one(&harness.pool)
        .await?;
    assert!(tombstone.0.is_some() && tombstone.1 == 0);
    // Waveform's own sign-out of one machine ends nothing else.
    me(&harness, &dot_before).await?;
    // A token issued after the sign-out works.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    me(&harness, &token_with(&harness, BEA, BEA.2, 0)).await?;
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn a_silicon_found_deleted_on_its_first_request_is_refused() -> TestResult {
    let harness = Harness::new("").await?;
    deleted_at_accounts(&harness, SCOUT.0).await;
    let (status, body) = send(
        &harness.app,
        "GET",
        "/api/v1/me",
        &[("authorization", &bearer(&harness.token(SCOUT)))],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["reason"], "account_deleted");
    let deleted: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT deleted_at FROM waveform_accounts WHERE account_uuid='K1E'")
            .fetch_one(&harness.pool)
            .await?;
    assert!(deleted.is_some(), "the lookup left a tombstone");
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn the_background_check_fills_a_linked_silicons_custodian() -> TestResult {
    let harness = Harness::new("").await?;
    // A Silicon linked at cutover: Waveform knows its kind, not its custodian.
    sqlx::query("INSERT INTO waveform_account_keys(account_uuid, storage_actor_id, kind, source) VALUES('K1E', $1, 'silicon', 'link_identities')")
        .bind(uuid::Uuid::now_v7())
        .execute(&harness.pool)
        .await?;
    sqlx::query("INSERT INTO waveform_accounts(account_uuid, kind) VALUES('K1E', 'silicon')")
        .execute(&harness.pool)
        .await?;
    harness
        .accounts
        .account(SCOUT.0, SCOUT.1, SCOUT.2, Some((ADA.0, ADA.2)))
        .await;
    harness.accounts.account(ADA.0, ADA.1, ADA.2, None).await;
    sign_in(&harness, ADA).await?;
    let ada = bearer(&harness.token(ADA));
    let (_, listed) = send(
        &harness.app,
        "GET",
        "/api/v1/silicons",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(listed["items"], json!([]), "not known to be Ada's yet");
    let report = harness.state.sweep_accounts().await.map_err(|_| "sweep")?;
    assert_eq!((report.checked, report.purged), (2, 0));
    let (_, listed) = send(
        &harness.app,
        "GET",
        "/api/v1/silicons",
        &[("authorization", &ada)],
        None,
    )
    .await?;
    assert_eq!(listed["items"][0]["uuid"], "K1E", "{listed}");
    assert_eq!(listed["items"][0]["id"], "si:scout");
    assert_eq!(listed["complete"], true);
    harness.finish().await
}

#[tokio::test]
#[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
async fn canonical_128_bit_accounts_reach_tokens_custodians_and_pending_events() -> TestResult {
    const OWNER: (&str, &str, &str) = (
        "bd5d6e33-3eec-470f-9a28-2efb2c1ef1c9",
        "carbon",
        "c:uuid-owner",
    );
    const CHILD: (&str, &str, &str) = (
        "3ab13a61-b343-40c1-ad13-54b1a071a10a",
        "silicon",
        "si:uuid-child",
    );
    let harness = Harness::new("").await?;
    harness
        .accounts
        .account(OWNER.0, OWNER.1, OWNER.2, None)
        .await;
    harness
        .accounts
        .account(CHILD.0, CHILD.1, CHILD.2, Some((OWNER.0, OWNER.2)))
        .await;
    harness.accounts.introspection(true).await;
    for account in [OWNER, CHILD] {
        sign_in(&harness, account).await?;
    }
    wait_for_custodian(&harness, CHILD.0).await?;
    let auth = bearer(&harness.token(OWNER));
    assert_eq!(me(&harness, &auth).await?["account"]["uuid"], OWNER.0);
    let (status, body) = send(
        &harness.app,
        "GET",
        &format!("/api/v1/preferences?account={}", CHILD.0),
        &[("authorization", &auth)],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["uuid"], CHILD.0);
    let missing = "fbe924cb-f88a-4191-bac3-240bc88c5ce0";
    let deleted = event(
        "account.deleted",
        json!({"uuid":missing,"kind":"carbon","id":"c:uuid-gone"}),
    );
    assert_eq!(deliver(&harness.app, &deleted).await?.0, StatusCode::OK);
    let pending:i64=sqlx::query_scalar("SELECT count(*) FROM waveform_account_pending_events WHERE account_uuid=$1 AND deleted_at IS NOT NULL").bind(missing).fetch_one(&harness.pool).await?;
    assert_eq!(pending, 1);
    harness.finish().await
}
