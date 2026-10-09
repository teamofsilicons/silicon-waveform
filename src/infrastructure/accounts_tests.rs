use serde_json::json;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{header, header_exists, method, path},
};

use super::{AccountsGateway, ProofError};
use crate::{
    application::ports::AccountsError,
    domain::auth::AuthFailure,
    test_support::{TestAccounts, TestKey, issued_proof},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn access_tokens_are_verified_locally_with_exact_failures() -> TestResult {
    let accounts = TestAccounts::start().await;
    let gateway = AccountsGateway::new(&accounts.settings(""))?;
    let claims = gateway
        .verify_access_token(&accounts.token("zQo", "carbon", "c:ada"))
        .await?;
    assert_eq!(claims.sub, "zQo");
    assert_eq!(claims.id.as_deref(), Some("c:ada"));

    let mut wrong_audience = accounts.claims("zQo", "carbon", "c:ada");
    wrong_audience["aud"] = json!("briefcase");
    let mut wrong_issuer = accounts.claims("zQo", "carbon", "c:ada");
    wrong_issuer["iss"] = json!("https://accounts.attacker.test");
    let mut expired = accounts.claims("zQo", "carbon", "c:ada");
    expired["exp"] = json!(time::OffsetDateTime::now_utc().unix_timestamp() - 3_600);
    for (claims, failure) in [
        (wrong_audience, AuthFailure::WrongAudience),
        (wrong_issuer, AuthFailure::WrongIssuer),
        (expired, AuthFailure::TokenExpired),
    ] {
        assert_eq!(
            gateway
                .verify_access_token(&accounts.key.sign(&claims))
                .await,
            Err(AccountsError::Rejected(failure))
        );
    }
    assert_eq!(
        gateway.verify_access_token("not-a-jwt").await,
        Err(AccountsError::Rejected(AuthFailure::InvalidToken))
    );
    // Signed by a key that is not published under the token's kid.
    let forged = TestKey::new(9, "test-1").sign(&accounts.claims("zQo", "carbon", "c:ada"));
    assert_eq!(
        gateway.verify_access_token(&forged).await,
        Err(AccountsError::Rejected(AuthFailure::InvalidToken))
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_kid_refetches_the_key_set_at_most_once_per_interval() -> TestResult {
    let server = wiremock::MockServer::start().await;
    let first = TestKey::new(7, "k-old");
    let rotated = TestKey::new(8, "k-new");
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"keys":[first.jwk()]})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"keys":[first.jwk(), rotated.jwk()]})),
        )
        .mount(&server)
        .await;
    let accounts = TestAccounts {
        server,
        key: TestKey::new(7, "k-old"),
    };
    let gateway = AccountsGateway::new(&accounts.settings(""))?;
    gateway
        .verify_access_token(&first.sign(&accounts.claims("zQo", "carbon", "c:ada")))
        .await?;
    // A token from the rotated key triggers one refetch, then verifies.
    gateway
        .verify_access_token(&rotated.sign(&accounts.claims("zQo", "carbon", "c:ada")))
        .await?;
    // A key nobody publishes is refused without hammering Accounts.
    let unknown = TestKey::new(10, "k-unknown").sign(&accounts.claims("zQo", "carbon", "c:ada"));
    assert_eq!(
        gateway.verify_access_token(&unknown).await,
        Err(AccountsError::Rejected(AuthFailure::UnknownSigningKey))
    );
    assert_eq!(
        gateway.verify_access_token(&unknown).await,
        Err(AccountsError::Rejected(AuthFailure::UnknownSigningKey))
    );
    let fetches = accounts
        .server
        .received_requests()
        .await
        .ok_or("requests")?
        .iter()
        .filter(|request| request.url.path() == "/.well-known/jwks.json")
        .count();
    assert_eq!(
        fetches, 2,
        "one initial fetch and one refetch within the interval"
    );
    Ok(())
}

#[tokio::test]
async fn introspection_is_cached_and_uses_app_credentials() -> TestResult {
    let accounts = TestAccounts::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/introspect"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"active": false})))
        .expect(1)
        .mount(&accounts.server)
        .await;
    let gateway = AccountsGateway::new(&accounts.settings(""))?;
    assert!(!gateway.introspect_active("token-one").await?);
    assert!(!gateway.introspect_active("token-one").await?);
    Ok(())
}

#[tokio::test]
async fn proof_refresh_is_idempotent_and_classifies_refusals() -> TestResult {
    let accounts = TestAccounts::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/proofs/refresh"))
        .and(header("idempotency-key", "refresh-key-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issued_proof("sap_new", "sapr_new")))
        .mount(&accounts.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/proofs/refresh"))
        .and(header("idempotency-key", "reused"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error":{"code":"proof_refresh_token_reused","message":"reused"}}),
        ))
        .mount(&accounts.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/proofs/refresh"))
        .and(header("idempotency-key", "down"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&accounts.server)
        .await;
    let gateway = AccountsGateway::new(&accounts.settings(""))?;
    let refreshed = gateway.refresh_proof("sapr_old", "refresh-key-1").await?;
    assert_eq!(refreshed.proof_token.expose(), "sap_new");
    assert_eq!(
        gateway.refresh_proof("sapr_old", "reused").await.err(),
        Some(ProofError::Refused)
    );
    assert_eq!(
        gateway.refresh_proof("sapr_old", "down").await.err(),
        Some(ProofError::Unavailable)
    );
    Ok(())
}

#[tokio::test]
async fn lookups_and_revocations_tolerate_missing_records() -> TestResult {
    let accounts = TestAccounts::start().await;
    accounts
        .account("K1E", "silicon", "si:scout", Some(("zQo", "c:ada")))
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/accounts/nope"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error":{"code":"account_not_found","message":"no"}})),
        )
        .mount(&accounts.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/proofs/revoke"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error":{"code":"proof_not_found","message":"no"}})),
        )
        .mount(&accounts.server)
        .await;
    let gateway = AccountsGateway::new(&accounts.settings(""))?;
    let silicon = gateway.lookup("K1E").await?.ok_or("account")?;
    assert_eq!(
        silicon.custodian.map(|custodian| custodian.uuid),
        Some("zQo".to_owned())
    );
    assert!(gateway.lookup("nope").await?.is_none());
    assert_eq!(gateway.revoke_proof("proof-gone").await, Ok(()));
    Ok(())
}
