//! Signing in and out against a stub Silicon Accounts: the Carbon device flow, a Silicon's
//! short-lived token, refresh rotation (single-flight across processes) and sign-out.
mod common;

use common::{
    Home, Stubs, assert_never_shown, json_any, json_out, mount_me, oauth_error, sign_in, text,
    tokens,
};
use serde_json::{Value, json};
#[cfg(unix)]
use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{
    Mock, Request, ResponseTemplate,
    matchers::{body_string_contains, method, path},
};

async fn device_authorize(stubs: &Stubs) {
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .and(body_string_contains("\"client_id\":\"waveform\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code":"sad_private_device","user_code":"WDJB-MJHT",
            "verification_uri":"http://localhost:9590/device",
            "verification_uri_complete":"http://localhost:9590/device?code=WDJB-MJHT",
            "expires_in":600,"interval":1
        })))
        .expect(1)
        .mount(&stubs.accounts)
        .await;
}

fn lines(output: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|_| panic!("not a JSON line: {line}"))
        })
        .collect()
}

#[tokio::test]
async fn a_carbon_signs_in_with_the_device_flow_through_pending_and_slow_down() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    device_authorize(&stubs).await;
    let polls = Arc::new(AtomicUsize::new(0));
    let seen = polls.clone();
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .and(body_string_contains("device_code=sad_private_device"))
        .and(body_string_contains("client_id=waveform"))
        .respond_with(
            move |_: &Request| match seen.fetch_add(1, Ordering::SeqCst) {
                0 => oauth_error("authorization_pending", "Not approved yet."),
                1 => oauth_error("slow_down", "Polling too fast."),
                _ => ResponseTemplate::new(200).set_body_json(tokens(
                    "eyJ-carbon-access",
                    "sar_carbon_refresh",
                    "carbon",
                    1800,
                )),
            },
        )
        .expect(3)
        .mount(&stubs.accounts)
        .await;
    mount_me(&stubs, "eyJ-carbon-access", "carbon").await;

    let output = stubs.run(&home, &["login", "--json"], None).await;
    assert!(output.status.success(), "{}", text(&output));
    let events = lines(&output);
    assert_eq!(events[0]["event"], "device_code");
    assert_eq!(events[0]["user_code"], "WDJB-MJHT");
    assert_eq!(
        events[0]["verification_uri"],
        "http://localhost:9590/device"
    );
    assert_eq!(events[1], json!({"event":"slow_down","interval":6}));
    let done = events.last().unwrap();
    assert_eq!(done["event"], "signed_in");
    assert_eq!(done["authenticated"], true);
    assert_eq!(done["uuid"], "aB1");
    assert_eq!(done["id"], "c:ada");
    assert_eq!(done["kind"], "carbon");
    assert_eq!(done["method"], "device");
    assert_eq!(done["verified"], true);
    assert_never_shown(
        &output,
        &[
            "eyJ-carbon-access",
            "sar_carbon_refresh",
            "sad_private_device",
        ],
    );
    let saved = home.session();
    assert_eq!(saved["refresh_token"], "sar_carbon_refresh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let file = home.session_files().remove(0);
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(home.state_dir()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let body =
        String::from_utf8(stubs.accounts_requests("/v1/device").await[0].body.clone()).unwrap();
    assert!(
        body.contains("client_label"),
        "the sign-in is named on the account site"
    );

    // The text form prints the page and the code on stderr, and the result on stdout.
    let status = json_out(&stubs.run(&home, &["login", "status", "--json"], None).await);
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["verified"], true);
    assert_eq!(status["display_name"], "Ada");
    assert!(status["expires_at"].is_string() && status["refresh_expires_at"].is_string());
}

#[tokio::test]
async fn the_device_flow_text_output_shows_the_page_and_code_and_opens_nothing() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    device_authorize(&stubs).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(tokens("eyJ-a", "sar_a", "carbon", 1800)),
        )
        .mount(&stubs.accounts)
        .await;
    let output = stubs.run(&home, &["login"], None).await;
    assert!(output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("http://localhost:9590/device?code=WDJB-MJHT"),
        "{stderr}"
    );
    assert!(stderr.contains("code: WDJB-MJHT"), "{stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Signed in to Waveform as c:ada"));
}

#[tokio::test]
async fn a_denied_or_expired_device_code_fails_with_exit_3_and_a_precise_reason() {
    for (error, code) in [
        ("access_denied", "device_denied"),
        ("expired_token", "device_code_expired"),
    ] {
        let home = Home::new();
        let stubs = Stubs::start().await;
        device_authorize(&stubs).await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error(error, "no"))
            .mount(&stubs.accounts)
            .await;
        let output = stubs.run(&home, &["login", "--json"], None).await;
        assert_eq!(output.status.code(), Some(3), "{}", text(&output));
        let last = lines(&output).pop().unwrap();
        assert_eq!(last["error"]["code"], code);
        assert!(
            last["error"]["message"]
                .as_str()
                .unwrap()
                .contains("WDJB-MJHT")
        );
        assert!(home.session_files().is_empty());
    }
}

#[tokio::test]
async fn a_silicon_signs_in_with_a_short_lived_token_from_stdin_an_argument_or_the_flag() {
    for (args, stdin) in [
        (
            vec!["login", "--slt-stdin", "--json"],
            Some("slt_fixture\n"),
        ),
        (vec!["login", "slt_fixture", "--json"], None),
        (vec!["login", "--slt", "slt_fixture", "--json"], None),
    ] {
        let home = Home::new();
        let stubs = Stubs::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains(
                "grant_type=urn%3Asilicon%3Aparams%3Aoauth%3Agrant-type%3Aslt",
            ))
            .and(body_string_contains("slt=slt_fixture"))
            .and(body_string_contains("client_id=waveform"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens(
                "eyJ-silicon",
                "sar_silicon",
                "silicon",
                1800,
            )))
            .expect(1)
            .mount(&stubs.accounts)
            .await;
        mount_me(&stubs, "eyJ-silicon", "silicon").await;
        let output = stubs.run(&home, &args, stdin).await;
        let value = json_out(&output);
        assert_eq!(value["kind"], "silicon", "{args:?}");
        assert_eq!(value["id"], "si:scout");
        assert_eq!(value["custodian"]["id"], "c:ada");
        assert_eq!(value["method"], "slt");
        assert_never_shown(&output, &["slt_fixture", "eyJ-silicon", "sar_silicon"]);
        assert!(
            !home.all_files_text().contains("slt_fixture"),
            "the SLT is never stored"
        );
        let body = String::from_utf8(
            stubs.accounts_requests("/v1/oauth/token").await[0]
                .body
                .clone(),
        )
        .unwrap();
        assert!(
            !body.contains("client_secret"),
            "a public client never sends a secret"
        );
    }
}

#[tokio::test]
async fn every_short_lived_token_refusal_is_explained_and_names_the_fix() {
    for (description, code, hint) in [
        (
            "The short-lived token was already used; each one works once. Get a new one.",
            "slt_already_used",
            "work once",
        ),
        (
            "The short-lived token expired at 2026-10-10T00:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app waveform`.",
            "slt_expired",
            "2 minutes",
        ),
        (
            "The short-lived token was issued for the app 'remind', not for 'waveform'; get one for 'waveform' with `silicon-accounts login --app waveform`.",
            "slt_wrong_app",
            "minted for remind",
        ),
        (
            "The short-lived token is not known: it is mistyped or was never issued.",
            "slt_unknown",
            "whole token",
        ),
    ] {
        let home = Home::new();
        let stubs = Stubs::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error("invalid_grant", description))
            .mount(&stubs.accounts)
            .await;
        let output = stubs
            .run(
                &home,
                &["login", "--slt", "slt_refused_private", "--json"],
                None,
            )
            .await;
        assert_eq!(output.status.code(), Some(3), "{code}");
        let error = &json_any(&output)["error"];
        assert_eq!(error["code"], code);
        assert_eq!(
            error["message"]
                .as_str()
                .unwrap()
                .split(" (A short")
                .next()
                .unwrap(),
            description
        );
        let shown_hint = error["hint"].as_str().unwrap();
        assert!(shown_hint.contains(hint), "{shown_hint}");
        assert!(
            shown_hint.contains("silicon-accounts login --app waveform -q"),
            "{shown_hint}"
        );
        assert_never_shown(&output, &["slt_refused_private"]);
        assert!(home.session_files().is_empty());
    }
    // Something that is not a short-lived token is never sent.
    let home = Home::new();
    let stubs = Stubs::start().await;
    let output = stubs
        .run(&home, &["login", "--slt", "sar_pasted_refresh_token"], None)
        .await;
    assert_eq!(output.status.code(), Some(3));
    assert!(stubs.accounts_requests("/").await.is_empty());
    let empty = stubs.run(&home, &["login", "--slt-stdin"], Some("")).await;
    assert_eq!(empty.status.code(), Some(2));
}

#[tokio::test]
async fn concurrent_commands_refresh_once_and_share_the_new_tokens() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    // The access token has 30 seconds left: every command must refresh first.
    sign_in(&home, &stubs, "carbon", "eyJ-old", "sar_old", 30).await;
    let refreshes = Arc::new(AtomicUsize::new(0));
    let seen = refreshes.clone();
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=sar_old"))
        .and(body_string_contains("client_id=waveform"))
        .respond_with(move |_: &Request| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(200)
                    .set_body_json(tokens("eyJ-new", "sar_new", "carbon", 1800))
            } else {
                oauth_error(
                    "invalid_grant",
                    "This refresh token was already used once; the sign-in was revoked.",
                )
            }
        })
        .mount(&stubs.accounts)
        .await;
    mount_me(&stubs, "eyJ-new", "carbon").await;
    let me = ["me", "--json"];
    let (a, b, c, d) = tokio::join!(
        stubs.run(&home, &me, None),
        stubs.run(&home, &me, None),
        stubs.run(&home, &me, None),
        stubs.run(&home, &me, None),
    );
    let outputs = [a, b, c, d];
    for output in &outputs {
        let value = json_out(output);
        assert_eq!(value["account"]["id"], "c:ada");
        assert_never_shown(output, &["eyJ-new", "sar_new", "sar_old"]);
    }
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "exactly one refresh across four processes"
    );
    assert_eq!(home.session()["refresh_token"], "sar_new");
}

#[tokio::test]
async fn a_token_waveform_calls_stale_is_refreshed_once_and_the_call_repeated() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-revoked", "sar_one", 1800).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/jobs"))
        .and(wiremock::matchers::header("authorization", "Bearer eyJ-revoked"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error":{
            "code":"unauthenticated","reason":"sign_in_ended",
            "message":"This sign-in ended (signed out, or Waveform's access was removed) after the token was issued."}})))
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/jobs"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer eyJ-two",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[],"next_cursor":null})),
        )
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("refresh_token=sar_one"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(tokens("eyJ-two", "sar_two", "carbon", 1800)),
        )
        .expect(1)
        .mount(&stubs.accounts)
        .await;
    let value = json_out(&stubs.run(&home, &["jobs", "--json"], None).await);
    assert_eq!(value["items"], json!([]));
    assert_eq!(home.session()["refresh_token"], "sar_two");
    // A refusal a refresh cannot cure is not retried.
    Mock::given(method("GET"))
        .and(path("/api/v1/preferences"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error":{
            "code":"not_custodian","message":"c:ada is not the custodian of si:other.",
            "hint":"Select a Silicon you look after (waveform silicons)."}})))
        .expect(1)
        .mount(&stubs.waveform)
        .await;
    let refused = stubs
        .run(&home, &["preferences", "--account", "si:other"], None)
        .await;
    assert_eq!(refused.status.code(), Some(3));
    let shown = text(&refused);
    assert!(
        shown.contains("not the custodian") && shown.contains("waveform silicons"),
        "{shown}"
    );
}

#[tokio::test]
async fn an_ended_sign_in_is_removed_and_reported_exactly() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "silicon", "eyJ-old", "sar_used", 10).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("refresh_token=sar_used"))
        .respond_with(oauth_error(
            "invalid_grant",
            "The account removed the access of the app 'waveform', so this sign-in has ended; the account has to sign in to the app again.",
        ))
        .mount(&stubs.accounts)
        .await;
    let output = stubs.run(&home, &["jobs", "--json"], None).await;
    assert_eq!(output.status.code(), Some(3));
    let error = &json_any(&output)["error"];
    assert_eq!(error["code"], "sign_in_ended");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("removed the access of the app 'waveform'")
    );
    assert!(error["hint"].as_str().unwrap().contains("--slt-stdin"));
    assert!(
        home.session_files().is_empty(),
        "the dead sign-in is deleted"
    );
    let status = json_out(&stubs.run(&home, &["login", "status", "--json"], None).await);
    assert_eq!(status, json!({"authenticated": false}));
}

#[tokio::test]
async fn logout_revokes_the_refresh_token_and_deletes_the_sign_in() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-bye", "sar_bye", 1800).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_bye"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .and(body_string_contains("client_id=waveform"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&stubs.accounts)
        .await;
    let value = json_out(&stubs.run(&home, &["logout", "--json"], None).await);
    assert_eq!(value["signed_out"], true);
    assert_eq!(value["revoked"], true);
    assert_eq!(value["id"], "c:ada");
    assert!(home.session_files().is_empty());
    let again = json_out(&stubs.run(&home, &["logout", "--json"], None).await);
    assert_eq!(again["signed_out"], false);
}

#[tokio::test]
async fn logout_still_deletes_the_sign_in_when_accounts_cannot_be_reached() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-x", "sar_x", 1800).await;
    let output = home
        .run(
            &[
                "--url",
                &stubs.waveform.uri(),
                "--accounts-url",
                "http://127.0.0.1:9",
                "logout",
                "--json",
            ],
            None,
        )
        .await;
    // The sign-in was made at another Silicon Accounts, so this one has nothing saved.
    assert_eq!(json_out(&output)["signed_out"], false);
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&stubs.accounts)
        .await;
    let output = stubs.run(&home, &["logout", "--json"], None).await;
    let value = json_out(&output);
    assert_eq!(value["revoked"], false);
    assert!(String::from_utf8_lossy(&output.stderr).contains("deleted from this machine anyway"));
    assert!(home.session_files().is_empty());
}

#[tokio::test]
async fn signing_in_again_ends_the_replaced_sign_in() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-first", "sar_first", 1800).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_first"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&stubs.accounts)
        .await;
    sign_in(&home, &stubs, "silicon", "eyJ-second", "sar_second", 1800).await;
    assert_eq!(home.session()["account"]["id"], "si:scout");
}

#[tokio::test]
async fn status_offline_reads_only_the_file_and_online_tells_refusals_from_outages() {
    let home = Home::new();
    let stubs = Stubs::start().await;
    sign_in(&home, &stubs, "carbon", "eyJ-status", "sar_status", 1800).await;
    let before = stubs.waveform_requests().await.len();
    let offline = json_out(
        &stubs
            .run(&home, &["login", "status", "--offline", "--json"], None)
            .await,
    );
    assert_eq!(offline["authenticated"], true);
    assert_eq!(offline["verified"], false);
    assert_eq!(
        stubs.waveform_requests().await.len(),
        before,
        "--offline sends nothing"
    );

    // Waveform refuses the token for a reason a refresh cannot cure: signed out, with why.
    let other = Home::new();
    let refusing = Stubs::start().await;
    sign_in(&other, &refusing, "carbon", "eyJ-wrong", "sar_wrong", 1800).await;
    refusing.waveform.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error":{
            "code":"unauthenticated","reason":"wrong_issuer",
            "message":"The access token was issued by a different Silicon Accounts deployment than the one Waveform trusts."}})))
        .mount(&refusing.waveform)
        .await;
    let refused = json_out(
        &refusing
            .run(&other, &["login", "status", "--json"], None)
            .await,
    );
    assert_eq!(refused["authenticated"], false);
    assert_eq!(refused["reason"], "wrong_issuer");
    let refused_text = refusing.run(&other, &["login", "status"], None).await;
    assert_eq!(refused_text.status.code(), Some(1));

    // Waveform unreachable: the saved sign-in is reported, unverified, with a warning.
    let unreachable = home
        .run(
            &[
                "--url",
                "http://127.0.0.1:9",
                "--accounts-url",
                &stubs.accounts.uri(),
                "login",
                "status",
                "--json",
            ],
            None,
        )
        .await;
    let value = json_out(&unreachable);
    assert_eq!(value["authenticated"], true);
    assert_eq!(value["verified"], false);
    assert!(
        value["warning"]
            .as_str()
            .unwrap()
            .contains("Could not reach Waveform")
    );
}
