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
