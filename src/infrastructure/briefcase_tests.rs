use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use sha2::Digest as _;
use time::macros::datetime;
use url::Url;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{body_bytes, body_partial_json, header, method, path},
};

use super::BriefcaseHttp;
use crate::{
    application::ports::{
        BriefcaseError, BriefcaseFileAccessRequest, BriefcasePort, ReadSourceMediaRequest,
        StoreGeneratedAudioRequest,
    },
    config::BriefcaseSettings,
    domain::{
        auth::{BriefcaseProof, ProofToken},
        identity::RequestId,
        media::{
            BriefcaseFileUrl, GeneratedAudioFileName, MediaDuration, MediaSizeLimit,
            NormalizedAudio,
        },
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn settings(server: &MockServer) -> Result<BriefcaseSettings, url::ParseError> {
    Ok(BriefcaseSettings {
        base_url: server.uri().parse()?,
        permanent_origin: server.uri().parse()?,
        app_id: "briefcase".to_owned(),
        app_folder: "apps/waveform".to_owned(),
        timeout: Duration::from_secs(5),
        download_timeout: Duration::from_secs(5),
        max_download_bytes: 1_000_000,
    })
}

fn proof() -> Result<BriefcaseProof, Box<dyn std::error::Error>> {
    Ok(BriefcaseProof {
        token: ProofToken::new("sap_briefcase-proof".to_owned())?,
        expires_at: datetime!(2099-01-01 00:00:00 UTC),
    })
}

fn assert_no_retired_headers(requests: &[Request]) {
    for request in requests {
        for retired in [
            "x-org-id",
            "x-app-id",
            "x-iam-obo-access-token",
            "x-iam-obo-access-proof",
        ] {
            assert!(
                !request.headers.contains_key(retired),
                "{retired} sent to {}",
                request.url
            );
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn generated_audio_is_reserved_transferred_and_committed_with_a_proof() -> TestResult {
    let storage = MockServer::start().await;
    let request_id = RequestId::new(Uuid::new_v4())?;
    let filename =
        GeneratedAudioFileName::for_request(datetime!(2026-10-10 01:02:03 UTC), request_id);
    let audio = NormalizedAudio::new(
        Bytes::from_static(b"normalized-mp3-fixture"),
        MediaDuration::from_millis(1000),
    )?;
    let operation_id = request_id.as_uuid();
    let upload_id = Uuid::new_v4();
    let entry_id = Uuid::new_v4();
    let status = |state: &str| json!({"operation_id":operation_id,"upload_id":upload_id,"state":state,"expires_at":"2099-01-01T00:00:00Z","published_entry_id": if state == "committed" { Some(entry_id) } else { None }});
    let mut reservation = status("reserved");
    reservation["capability"] = json!("upload_capability");
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/uploads/reserve"))
        .and(header("authorization", "Proof sap_briefcase-proof"))
        .and(body_partial_json(json!({"operation_id":operation_id,"parent_path":"","name":filename.as_str(),"content_type":"audio/mpeg","size":audio.bytes().len(),"sha256":format!("{:x}", sha2::Sha256::digest(audio.bytes()))})))
        .respond_with(ResponseTemplate::new(200).set_body_json(reservation))
        .expect(1)
        .mount(&storage)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v1/obo/uploads/{upload_id}/content")))
        .and(header("x-briefcase-upload-capability", "upload_capability"))
        .and(body_bytes(audio.bytes().to_vec()))
        .respond_with(ResponseTemplate::new(200).set_body_json(status("staged")))
        .expect(1)
        .mount(&storage)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/uploads/commit"))
        .and(header("authorization", "Proof sap_briefcase-proof"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status("committed")))
        .expect(1)
        .mount(&storage)
        .await;
    let permanent = format!("{}/zQo/apps/waveform/{}", storage.uri(), filename.as_str());
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path":"apps/waveform"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":entry_id,"type":"file","name":filename.as_str(),"path":format!("apps/waveform/{}", filename.as_str()),"content_type":"audio/mpeg","size":audio.bytes().len(),"permanent_url":permanent,"origin_app_id":"waveform","deleted_at":null}],"next_cursor":null})))
        .expect(1)
        .mount(&storage)
        .await;
    let adapter = BriefcaseHttp::new(&settings(&storage)?, "waveform")?;
    let url = adapter
        .store_generated_audio(StoreGeneratedAudioRequest {
            proof: proof()?,
            filename: filename.clone(),
            audio,
            request_id,
        })
        .await?;
    assert_eq!(url.to_string(), permanent);
    let requests = storage.received_requests().await.ok_or("requests")?;
    assert_no_retired_headers(&requests);
    let transfer = requests
        .iter()
        .find(|request| request.method == "PUT")
        .ok_or("transfer")?;
    assert!(
        !transfer.headers.contains_key("authorization"),
        "the byte transfer carries only its capability"
    );
    Ok(())
}

#[tokio::test]
async fn upload_refusals_and_missing_routes_are_classified() -> TestResult {
    for (status, expected) in [
        (401, BriefcaseError::Unauthorized),
        (403, BriefcaseError::Forbidden),
        (404, BriefcaseError::ContractUnavailable),
        (503, BriefcaseError::Unavailable),
    ] {
        let storage = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/obo/uploads/reserve"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&storage)
            .await;
        let adapter = BriefcaseHttp::new(&settings(&storage)?, "waveform")?;
        let request_id = RequestId::new(Uuid::new_v4())?;
        let result = adapter
            .store_generated_audio(StoreGeneratedAudioRequest {
                proof: proof()?,
                filename: GeneratedAudioFileName::for_request(
                    datetime!(2026-10-10 01:02:03 UTC),
                    request_id,
                ),
                audio: NormalizedAudio::new(
                    Bytes::from_static(b"mp3"),
                    MediaDuration::from_millis(1),
                )?,
                request_id,
            })
            .await;
        assert_eq!(result, Err(expected), "HTTP {status}");
    }
    Ok(())
}

#[tokio::test]
async fn sources_resolve_through_their_folder_and_are_read_with_a_proof() -> TestResult {
    let storage = MockServer::start().await;
    let entry_id = Uuid::new_v4();
    let url = format!("{}/zQo/apps/waveform/voice%20note.mp3", storage.uri());
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path":"apps/waveform"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[
            {"id":Uuid::new_v4(),"type":"file","name":"other.mp3","path":"apps/waveform/other.mp3"},
            {"id":entry_id,"type":"file","name":"voice note.mp3","path":"apps/waveform/voice note.mp3","permanent_url":url}
        ],"next_cursor":null})))
        .mount(&storage)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/files/read"))
        .and(header("authorization", "Proof sap_briefcase-proof"))
        .and(body_partial_json(
            json!({"entry_id":entry_id,"download":false}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(b"source-bytes".to_vec()),
        )
        .mount(&storage)
        .await;
    let adapter = BriefcaseHttp::new(&settings(&storage)?, "waveform")?;
    let file_url = BriefcaseFileUrl::new(Url::parse(&url)?)?;
    let media = adapter
        .read_source_media(ReadSourceMediaRequest {
            proof: proof()?,
            source_url: file_url.clone(),
            size_limit: MediaSizeLimit::default(),
            request_id: RequestId::new(Uuid::new_v4())?,
        })
        .await?;
    assert_eq!(media.bytes().as_ref(), b"source-bytes");
    adapter
        .verify_file_read_access(BriefcaseFileAccessRequest {
            proof: proof()?,
            file_url,
            request_id: RequestId::new(Uuid::new_v4())?,
        })
        .await?;
    let requests = storage.received_requests().await.ok_or("requests")?;
    assert_no_retired_headers(&requests);
    let probe = requests
        .iter()
        .filter(|request| request.url.path() == "/api/v1/obo/files/read")
        .nth(1)
        .ok_or("probe")?;
    let probe_body: serde_json::Value = serde_json::from_slice(&probe.body)?;
    assert_eq!(probe_body["range"], "bytes=0-0");
    Ok(())
}

#[test]
fn permanent_urls_map_to_drive_paths_for_both_url_schemes() -> TestResult {
    let storage = BriefcaseSettings {
        base_url: "https://backend.briefcase.test".parse()?,
        permanent_origin: "https://briefcase.test".parse()?,
        app_id: "briefcase".to_owned(),
        app_folder: "apps/waveform".to_owned(),
        timeout: Duration::from_secs(1),
        download_timeout: Duration::from_secs(1),
        max_download_bytes: 1024,
    };
    let adapter = BriefcaseHttp::new(&storage, "waveform")?;
    let path = |url: &str| -> Result<String, Box<dyn std::error::Error>> {
        Ok(adapter
            .relative_path(&BriefcaseFileUrl::new(url.parse()?)?)
            .map_err(|error| format!("{error}"))?)
    };
    assert_eq!(
        path("https://briefcase.test/zQo/apps/waveform/a%20b.mp3")?,
        "apps/waveform/a b.mp3"
    );
    assert_eq!(
        path("https://briefcase.test/org/tos/private/c:ada/x.mp3")?,
        "private/c:ada/x.mp3"
    );
    for refused in [
        "https://attacker.test/zQo/apps/waveform/a.mp3",
        "https://briefcase.test/a.mp3",
        "https://briefcase.test/zQo/apps%2Fwaveform/a.mp3",
        "https://briefcase.test/zQo/apps/%00a.mp3",
    ] {
        assert!(path(refused).is_err(), "{refused}");
    }
    Ok(())
}

#[tokio::test]
async fn the_stt_folder_is_found_through_what_is_inside_it() -> TestResult {
    // Briefcase shows an app only apps/{app}/ and its contents: listing with no
    // parent lists the folder, and listing apps/ itself is refused.
    let storage = MockServer::start().await;
    let folder = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(header("authorization", "Proof sap_briefcase-proof"))
        .and(body_partial_json(json!({"path": null, "parent_id": null})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":Uuid::new_v4(),"type":"file","name":"note.mp3","path":"apps/waveform/note.mp3","parent_id":folder,"permanent_url":format!("{}/c:ada/apps/waveform/note.mp3", storage.uri()),"origin_app_id":"waveform","deleted_at":null}],"next_cursor":null})))
        .expect(1)
        .mount(&storage)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path": "apps"})))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&storage)
        .await;
    let adapter = BriefcaseHttp::new(&settings(&storage)?, "waveform")?;
    let target = adapter
        .prepare_source_target(&proof()?, RequestId::new(Uuid::new_v4())?)
        .await?;
    assert_eq!(target.folder_id, folder);
    assert_eq!(target.folder_path, "apps/waveform");
    Ok(())
}

#[tokio::test]
async fn an_empty_stt_folder_hidden_from_waveform_is_not_found_and_a_listed_one_is_read()
-> TestResult {
    let storage = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path": null})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[],"next_cursor":null})),
        )
        .mount(&storage)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path": "apps"})))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&storage)
        .await;
    let adapter = BriefcaseHttp::new(&settings(&storage)?, "waveform")?;
    let refused = adapter
        .prepare_source_target(&proof()?, RequestId::new(Uuid::new_v4())?)
        .await;
    assert_eq!(refused.err(), Some(BriefcaseError::NotFound));

    // A Briefcase that lists the apps/ container to the app: the folder entry is read there.
    let legacy = MockServer::start().await;
    let folder = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path": null})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[],"next_cursor":null})),
        )
        .mount(&legacy)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path": "apps"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":folder,"type":"folder","name":"waveform","path":"apps/waveform","origin_app_id":"waveform","deleted_at":null}],"next_cursor":null})))
        .mount(&legacy)
        .await;
    let adapter = BriefcaseHttp::new(&settings(&legacy)?, "waveform")?;
    let target = adapter
        .prepare_source_target(&proof()?, RequestId::new(Uuid::new_v4())?)
        .await?;
    assert_eq!(target.folder_id, folder);
    Ok(())
}

#[tokio::test]
async fn an_empty_stt_folder_is_identified_by_its_authorized_listing_metadata() -> TestResult {
    let storage = MockServer::start().await;
    let folder = Uuid::new_v4();
    Mock::given(method("POST")).and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path":null})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[],"folder":{"id":folder,"type":"folder","path":"apps/waveform","origin_app_id":"waveform"}})))
        .expect(1).mount(&storage).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/obo/entries/list"))
        .and(body_partial_json(json!({"path":"apps"})))
        .respond_with(ResponseTemplate::new(403))
        .expect(0)
        .mount(&storage)
        .await;
    let adapter = BriefcaseHttp::new(&settings(&storage)?, "waveform")?;
    let target = adapter
        .prepare_source_target(&proof()?, RequestId::new(Uuid::new_v4())?)
        .await?;
    assert_eq!(target.folder_id, folder);
    Ok(())
}
