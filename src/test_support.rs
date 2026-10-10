//! Test doubles for Silicon Accounts: a wiremock server publishing a JWKS for
//! an Ed25519 test key, `EdDSA` access tokens signed with it, proof endpoints and
//! signed webhook deliveries.

#![allow(clippy::missing_panics_doc, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{SigningKey, pkcs8::EncodePrivateKey as _};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

use crate::config::{AccountsSettings, ProofIssuers};

/// Webhook secret used by test deliveries.
pub const WEBHOOK_SECRET: &str = "whsec_waveform-test-webhook-secret";
/// Waveform's app secret in tests.
pub const APP_SECRET: &str = "sa_app_waveform_test-only-secret-value";

/// One Ed25519 signing key and its JWK.
pub struct TestKey {
    pub kid: String,
    encoding: EncodingKey,
    x: String,
}

impl TestKey {
    #[must_use]
    pub fn new(seed: u8, kid: &str) -> Self {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let der = signing.to_pkcs8_der().expect("pkcs8");
        Self {
            kid: kid.to_owned(),
            encoding: EncodingKey::from_ed_der(der.as_bytes()),
            x: URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes()),
        }
    }

    #[must_use]
    pub fn jwk(&self) -> Value {
        json!({"kty":"OKP","crv":"Ed25519","x":self.x,"kid":self.kid,"use":"sig","alg":"EdDSA"})
    }

    #[must_use]
    pub fn sign(&self, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(self.kid.clone());
        encode(&header, claims, &self.encoding).expect("sign")
    }
}

/// A stubbed Silicon Accounts.
pub struct TestAccounts {
    pub server: MockServer,
    pub key: TestKey,
}

impl TestAccounts {
    /// Starts the stub and publishes the JWKS.
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let key = TestKey::new(7, "test-1");
        Mock::given(method("GET"))
            .and(path("/.well-known/jwks.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"keys":[key.jwk()]})))
            .mount(&server)
            .await;
        Self { server, key }
    }

    /// The issuer tokens carry (the stub's origin).
    #[must_use]
    pub fn issuer(&self) -> String {
        self.server.uri()
    }

    /// Claims of a valid Waveform access token.
    #[must_use]
    pub fn claims(&self, uuid: &str, kind: &str, id: &str) -> Value {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        json!({
            "iss": self.issuer(), "sub": uuid, "aud": "waveform", "exp": now + 600, "iat": now,
            "nbf": now, "jti": uuid::Uuid::new_v4().to_string(), "kind": kind, "id": id,
            "mid": format!("waveform:{uuid}"), "fid": format!("fid-{uuid}"), "scope": "profile"
        })
    }

    /// A valid Waveform access token for an account.
    #[must_use]
    pub fn token(&self, uuid: &str, kind: &str, id: &str) -> String {
        self.key.sign(&self.claims(uuid, kind, id))
    }

    /// Accounts settings pointing at the stub.
    #[must_use]
    pub fn settings(&self, issuers: &str) -> AccountsSettings {
        let url = url::Url::parse(&self.server.uri()).expect("stub url");
        AccountsSettings {
            public_url: url.clone(),
            api_url: url,
            app_id: "waveform".to_owned(),
            app_secret: SecretString::from(APP_SECRET.to_owned()),
            webhook_secret: Some(SecretString::from(WEBHOOK_SECRET.to_owned())),
            previous_webhook_secret: None,
            timeout: Duration::from_secs(5),
            proof_issuers: ProofIssuers::parse(issuers).expect("issuers"),
        }
    }

    /// `POST /v1/proofs/verify` answers valid for this token.
    pub async fn valid_proof(
        &self,
        token: &str,
        user: (&str, &str, &str),
        issuer: &str,
        scopes: &[&str],
    ) {
        let (uuid, kind, id) = user;
        Mock::given(method("POST"))
            .and(path("/v1/proofs/verify"))
            .and(body_partial_json(json!({"proof_token": token})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "valid": true, "proof_id": format!("proof-{token}"), "kind": "user_verification",
                "expires_at": "2099-01-01T00:00:00.000Z",
                "issuing_app": {"app_id": issuer, "name": issuer},
                "receiving_app": {"app_id": "waveform", "name": "Waveform"},
                "user": {"uuid": uuid, "id": id, "kind": kind, "membership_id": format!("{issuer}:{uuid}")},
                "scopes": scopes,
            })))
            .mount(&self.server)
            .await;
    }

    /// `POST /v1/proofs/verify` answers invalid for this token.
    pub async fn invalid_proof(&self, token: &str) {
        Mock::given(method("POST"))
            .and(path("/v1/proofs/verify"))
            .and(body_partial_json(json!({"proof_token": token})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"valid": false, "expires_at": null})),
            )
            .mount(&self.server)
            .await;
    }

    /// `POST /v1/oauth/introspect` answers with `active`.
    pub async fn introspection(&self, active: bool) {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"active": active})))
            .mount(&self.server)
            .await;
    }

    /// `GET /v1/accounts/{uuid}` and `/v1/accounts/by-id/{id}` describe an account.
    pub async fn account(&self, uuid: &str, kind: &str, id: &str, custodian: Option<(&str, &str)>) {
        let body = json!({
            "uuid": uuid, "kind": kind, "id": id, "display_name": id, "pfp_url": "", "status": "active",
            "custodian": custodian.map(|(uuid, id)| json!({"uuid": uuid, "id": id})),
        });
        Mock::given(method("GET"))
            .and(path(format!("/v1/accounts/{uuid}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/accounts/by-id/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    /// `POST /v1/proofs/user-verification` issues a Briefcase proof.
    pub async fn issues_briefcase_proofs(&self) {
        Mock::given(method("POST"))
            .and(path("/v1/proofs/user-verification"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(issued_proof("sap_briefcase-1", "sapr_briefcase-1")),
            )
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/proofs/revoke"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&self.server)
            .await;
    }
}

/// An issued Briefcase proof body.
#[must_use]
pub fn issued_proof(token: &str, refresh: &str) -> Value {
    json!({
        "proof_id": "proof-briefcase-1", "kind": "user_verification", "proof_token": token,
        "expires_at": "2099-01-01T00:00:00.000Z", "proof_refresh_token": refresh,
        "refresh_expires_at": "2099-06-01T00:00:00.000Z", "issuing_app": "waveform",
        "receiving_app": "briefcase", "user": {"uuid": "zQo", "id": "c:ada", "kind": "carbon"},
        "scopes": ["briefcase.uploads.reserve","briefcase.uploads.commit","briefcase.entries.list","briefcase.files.read"]
    })
}

/// Headers and body of a signed webhook delivery.
#[must_use]
pub fn signed_delivery(secret: &str, body: &Value) -> (Vec<(&'static str, String)>, Vec<u8>) {
    let raw = serde_json::to_vec(body).expect("json");
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let signature = silicon_accounts_client::sign_webhook(secret, timestamp, &raw);
    (
        vec![
            ("x-accounts-timestamp", timestamp.to_string()),
            ("x-accounts-signature", signature),
            ("content-type", "application/json".to_owned()),
        ],
        raw,
    )
}
