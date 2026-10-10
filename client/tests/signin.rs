//! Waveform's public-client sign-in against a stub Silicon Accounts: device flow, short-lived
//! token exchange (and every refusal), refresh rotation and revocation.
use serde_json::json;
use silicon_waveform_client::{
    ExposeSecret as _,
    signin::{DeviceProgress, SignIn, SignInError, SltRefusal},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{body_string_contains, method, path},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn tokens(access: &str, refresh: &str) -> serde_json::Value {
    json!({
        "access_token": access, "token_type": "Bearer", "expires_in": 1800,
        "refresh_token": refresh, "refresh_token_expires_at": "2029-01-01T00:00:00.000Z",
        "scope": "profile", "membership_id": "waveform:aB1",
        "account": {"uuid":"aB1","membership_id":"waveform:aB1","kind":"carbon","id":"c:ada",
            "display_name":"Ada","pfp_url":"","version":3}
    })
}

fn oauth_error(error: &str, description: &str) -> ResponseTemplate {
    ResponseTemplate::new(400)
        .set_body_json(json!({"error": error, "error_description": description}))
}

async fn device_authorize(server: &MockServer, interval: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .and(body_string_contains("\"client_id\":\"waveform\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code":"sad_private_device","user_code":"WDJB-MJHT",
            "verification_uri":"http://127.0.0.1/device",
            "verification_uri_complete":"http://127.0.0.1/device?code=WDJB-MJHT",
            "expires_in":600,"interval":interval
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn device_flow_waits_through_pending_and_slow_down_then_signs_the_carbon_in() -> TestResult {
    let server = MockServer::start().await;
    device_authorize(&server, 2).await;
    let polls: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let seen = polls.clone();
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .and(body_string_contains("client_id=waveform"))
        .and(body_string_contains("device_code=sad_private_device"))
        .respond_with(move |_: &Request| {
            let mut seen = seen.lock().unwrap();
            seen.push(Instant::now());
            match seen.len() {
                1 => oauth_error("authorization_pending", "Not approved yet."),
                2 => oauth_error("slow_down", "Too fast."),
                _ => ResponseTemplate::new(200)
                    .set_body_json(tokens("eyJ-device-access", "sar_device_refresh")),
            }
        })
        .expect(3)
        .mount(&server)
        .await;
    let unit = Duration::from_millis(20);
    let sign_in = SignIn::new(&server.uri())?.with_poll_unit(unit);
    let device = sign_in.start_device(Some("waveform CLI test")).await?;
    assert_eq!(device.user_code, "WDJB-MJHT");
    assert_eq!(
        device.browser_url(),
        "http://127.0.0.1/device?code=WDJB-MJHT"
    );
    assert!(!format!("{device:?}").contains("sad_private_device"));
    let mut progress = Vec::new();
    let signed_in = sign_in
        .wait_for_device(&device, |event| progress.push(event.clone()))
        .await?;
    assert_eq!(signed_in.access_token.expose_secret(), "eyJ-device-access");
    assert_eq!(
        signed_in.refresh_token.expose_secret(),
        "sar_device_refresh"
    );
    assert_eq!(signed_in.account.label(), "c:ada");
    assert_eq!(signed_in.account.display_name.as_deref(), Some("Ada"));
    assert_eq!(signed_in.account.pfp_url, None);
    assert!(signed_in.refresh_expires_at.is_some());
    assert_eq!(
        progress,
        vec![
            DeviceProgress::Pending,
            DeviceProgress::SlowDown { interval: 7 }
        ]
    );
    // After slow_down the wait grows from 2 to 7 units.
    let polls = polls.lock().unwrap();
    assert!(
        polls[2].duration_since(polls[1]) >= unit * 7,
        "{:?}",
        polls[2] - polls[1]
    );
    assert!(polls[1].duration_since(polls[0]) >= unit * 2);
    Ok(())
}

#[tokio::test]
async fn a_denied_or_expired_device_code_ends_with_a_precise_error() -> TestResult {
    for (error, expected) in [
        ("access_denied", "device_denied"),
        ("expired_token", "device_code_expired"),
    ] {
        let server = MockServer::start().await;
        device_authorize(&server, 1).await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error(error, "no"))
            .expect(1)
            .mount(&server)
            .await;
        let sign_in = SignIn::new(&server.uri())?.with_poll_unit(Duration::from_millis(5));
        let device = sign_in.start_device(None).await?;
        let failure = sign_in.wait_for_device(&device, |_| {}).await.unwrap_err();
        assert_eq!(failure.code(), expected);
        assert!(failure.message().contains("WDJB-MJHT"));
        assert!(failure.hint().contains("waveform login"));
    }
    Ok(())
}

#[tokio::test]
async fn a_device_flow_not_enabled_for_waveform_says_so() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .respond_with(oauth_error(
            "unauthorized_client",
            "The app 'waveform' has not turned on device_flow.",
        ))
        .mount(&server)
        .await;
    let failure = SignIn::new(&server.uri())?
        .start_device(None)
        .await
        .unwrap_err();
    assert!(matches!(failure, SignInError::NotEnabled { .. }));
    assert!(failure.message().contains("device_flow"));
    Ok(())
}

#[tokio::test]
async fn a_short_lived_token_is_exchanged_with_the_client_id_alone() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Asilicon%3Aparams%3Aoauth%3Agrant-type%3Aslt",
        ))
        .and(body_string_contains("slt=slt_good"))
        .and(body_string_contains("client_id=waveform"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(tokens("eyJ-silicon", "sar_silicon")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let signed_in = SignIn::new(&server.uri())?
        .exchange_slt(" slt_good\n")
        .await?;
    assert_eq!(signed_in.access_token.expose_secret(), "eyJ-silicon");
    let body = String::from_utf8(server.received_requests().await.unwrap()[0].body.clone())?;
    assert!(
        !body.contains("client_secret"),
        "a public client never sends a secret"
    );
    Ok(())
}

#[tokio::test]
async fn every_short_lived_token_refusal_is_explained_exactly() -> TestResult {
    let cases = [
        (
            "The short-lived token was already used; each one works once. Get a new one.",
            SltRefusal::AlreadyUsed,
        ),
        (
            "The short-lived token expired at 2026-10-10T00:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app waveform`.",
            SltRefusal::Expired,
        ),
        (
            "The short-lived token was issued for the app 'remind', not for 'waveform'; get one for 'waveform' with `silicon-accounts login --app waveform`.",
            SltRefusal::WrongApp {
                app: Some("remind".into()),
            },
        ),
        (
            "The short-lived token is not known: it is mistyped or was never issued.",
            SltRefusal::Unknown,
        ),
        (
            "si:scout removed the access of the app 'waveform' at 2026-10-10T00:00:00.000Z, after this short-lived token was issued at 2026-10-09T23:59:00.000Z, so it can't be exchanged; …",
            SltRefusal::AccessRemoved,
        ),
        (
            "The short-lived token was issued at … by a sign-in of si:scout that ended when its custodian rotated its STK at …",
            SltRefusal::SignInEnded,
        ),
    ];
    for (description, reason) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error("invalid_grant", description))
            .mount(&server)
            .await;
        let failure = SignIn::new(&server.uri())?
            .exchange_slt("slt_refused")
            .await
            .unwrap_err();
        match &failure {
            SignInError::SltRefused {
                reason: got,
                description: text,
            } => {
                assert_eq!(got, &reason);
                assert_eq!(text, description);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            failure
                .hint()
                .contains("silicon-accounts login --app waveform -q"),
            "{}",
            failure.hint()
        );
        assert!(failure.code().starts_with("slt_"));
    }
    Ok(())
}

#[tokio::test]
async fn values_that_are_not_short_lived_tokens_are_never_sent() -> TestResult {
    let server = MockServer::start().await;
    let sign_in = SignIn::new(&server.uri())?;
    for value in [
        "",
        "sar_refresh_token_pasted_by_mistake",
        "eyJhbGciOi",
        "slt_with space",
    ] {
        assert!(sign_in.exchange_slt(value).await.is_err(), "{value}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn refresh_rotates_and_a_reused_refresh_token_ends_the_sign_in() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=sar_one"))
        .and(body_string_contains("client_id=waveform"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens("eyJ-two", "sar_two")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("refresh_token=sar_one"))
        .respond_with(oauth_error(
            "invalid_grant",
            "The refresh token was already used; the sign-in was revoked.",
        ))
        .mount(&server)
        .await;
    let sign_in = SignIn::new(&server.uri())?;
    let rotated = sign_in.refresh("sar_one").await?;
    assert_eq!(rotated.refresh_token.expose_secret(), "sar_two");
    let failure = sign_in.refresh("sar_one").await.unwrap_err();
    assert!(matches!(failure, SignInError::SignInEnded { .. }));
    assert!(
        failure
            .message()
            .starts_with("Your Waveform sign-in ended:")
    );
    assert!(failure.hint().contains("waveform login"));
    assert!(!failure.is_transient());
    Ok(())
}

#[tokio::test]
async fn revoke_posts_the_refresh_token_with_the_client_id() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_bye"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .and(body_string_contains("client_id=waveform"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    SignIn::new(&server.uri())?.revoke("sar_bye").await?;
    Ok(())
}

#[tokio::test]
async fn unreachable_accounts_and_bad_urls_are_reported_precisely() -> TestResult {
    let error = SignIn::new("http://accounts.example.com").unwrap_err();
    assert_eq!(error.code(), "sign_in_configuration");
    assert!(
        error.message().contains("plain http"),
        "{}",
        error.message()
    );
    let sign_in = SignIn::new("http://127.0.0.1:9")?;
    let failure = sign_in.exchange_slt("slt_unreachable").await.unwrap_err();
    assert!(failure.is_transient());
    assert!(
        failure.message().contains("127.0.0.1:9"),
        "{}",
        failure.message()
    );
    Ok(())
}
