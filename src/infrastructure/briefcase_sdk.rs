//! Reserve exact normalized bytes, transfer with a narrow staging capability,
//! and publish only after Briefcase rechecks the commit endpoint token.

use briefcase_client::{
    ApplicationId, Client, Config, DelegatedCommitUpload, DelegatedListEntries,
    DelegatedReserveUpload, DelegatedUploadState, Entry, EnvironmentKey, OboProof, UploadSource,
};
use sha2::Digest as _;
use thiserror::Error;

use crate::{
    config::BriefcaseSettings,
    domain::{
        auth::DelegatedAuthorization,
        media::{GeneratedAudioFileName, NormalizedAudio},
    },
};

/// Safe failures reported without tokens, response bodies or media bytes.
#[derive(Debug, Error)]
pub enum UploadError {
    /// Configured upstream URL or organization is unusable.
    #[error("invalid Briefcase client configuration")]
    Configuration,
    /// The discovered service does not implement the expected contract.
    #[error("Briefcase is unavailable or incompatible")]
    Briefcase,
    /// IAM rejected the current endpoint authorization.
    #[error("IAM did not authorize the Briefcase upload")]
    Delegation,
    /// The returned entry disagrees with the exact request that was authorized.
    #[error("Briefcase returned an inconsistent audio entry")]
    InvalidEntry,
}

/// Request-local access tokens; refresh credentials remain in the encrypted broker.
pub struct AudioUpload<'a> {
    /// Stable speech operation ID for downstream reservation replay.
    pub operation_id: uuid::Uuid,
    /// Originating application namespace.
    pub app_id: &'a str,
    /// Organization selected from the live IAM authorization snapshot.
    pub organization: &'a str,
    /// Live Briefcase endpoint authority selected at feature consent.
    pub delegated_authorization: &'a DelegatedAuthorization,
    /// Paired Briefcase imported application secret for a test-plane request.
    pub environment: Option<EnvironmentKey>,
    /// Collision-resistant final name bound by the reservation.
    pub filename: &'a GeneratedAudioFileName,
    /// The validated final MP3 buffer.
    pub audio: &'a NormalizedAudio,
}

/// Configured audio uploader. Actual client credentials are request-local.
#[derive(Clone, Debug)]
pub struct BriefcaseSdkUploader {
    settings: BriefcaseSettings,
}

impl BriefcaseSdkUploader {
    /// Retains non-secret connection and origin policy.
    #[must_use]
    pub fn new(settings: BriefcaseSettings) -> Self {
        Self { settings }
    }

    /// Publishes exact bytes using a stable reservation and separate commit authority.
    ///
    /// # Errors
    /// Returns a redacted error if discovery, exchange, upload or response
    /// binding validation fails. An uncertain upload retains its logical operation ID for reconciliation.
    #[allow(
        clippy::too_many_lines,
        reason = "ordered reserve, transfer, commit and response validation"
    )]
    pub async fn upload(&self, request: AudioUpload<'_>) -> Result<Entry, UploadError> {
        let mut api_base = self.settings.base_url.clone();
        if api_base.path() == "/" {
            api_base.set_path("/api/v1/");
        }
        let mut config = Config::new(api_base.as_str(), request.organization)
            .map_err(|_| UploadError::Configuration)?
            .with_auto_update(false)
            .with_request_timeout(self.settings.timeout)
            .with_transfer_timeout(self.settings.download_timeout);
        let environment = request
            .delegated_authorization
            .testing_secret
            .as_ref()
            .map(|secret| {
                EnvironmentKey::new(secrecy::ExposeSecret::expose_secret(secret))
                    .map_err(|_| UploadError::Configuration)
            })
            .transpose()?
            .or(request.environment);
        if let Some(environment) = environment {
            config = config.with_environment(environment);
        }
        // Check published API compatibility before presenting the proof.
        let briefcase = Client::connect(config)
            .await
            .map_err(|_| UploadError::Briefcase)?;
        if request.delegated_authorization.application_id.as_str() != request.app_id
            || request.delegated_authorization.purpose
                != crate::domain::auth::DelegationPurpose::StoreGeneratedAudio
            || request.delegated_authorization.expires_at <= time::OffsetDateTime::now_utc()
        {
            return Err(UploadError::Delegation);
        }
        let app = ApplicationId::new(request.app_id).map_err(|_| UploadError::Configuration)?;
        let proof = |token: &crate::domain::auth::OboProof| {
            OboProof::new(token.expose_secret()).map_err(|_| UploadError::Delegation)
        };
        let reserve = DelegatedReserveUpload {
            operation_id: request.operation_id,
            parent_path: String::new(),
            name: request.filename.as_str().to_owned(),
            content_type: "audio/mpeg".to_owned(),
            size: request.audio.bytes().len() as u64,
            sha256: format!("{:x}", sha2::Sha256::digest(request.audio.bytes())),
        }
        .prepare()
        .map_err(|_| UploadError::Configuration)?;
        let reservation = briefcase
            .reserve_delegated_upload(
                &app,
                proof(&request.delegated_authorization.proof)?,
                &reserve,
            )
            .await
            .map_err(|error| map_storage_error(&error))?;
        if reservation.status.operation_id != request.operation_id
            || reservation.status.upload_id.is_nil()
        {
            return Err(UploadError::InvalidEntry);
        }
        let upload_id = reservation.status.upload_id;
        let status = match reservation.status.state {
            DelegatedUploadState::Reserved => {
                let capability = reservation.capability.ok_or(UploadError::InvalidEntry)?;
                let staged = briefcase
                    .transfer_delegated_upload(
                        upload_id,
                        capability,
                        &UploadSource::Bytes(request.audio.bytes().to_vec()),
                    )
                    .await
                    .map_err(|error| map_storage_error(&error))?;
                if staged.operation_id != request.operation_id
                    || staged.upload_id != upload_id
                    || staged.state != DelegatedUploadState::Staged
                {
                    return Err(UploadError::InvalidEntry);
                }
                staged
            }
            DelegatedUploadState::Staged | DelegatedUploadState::Committed => reservation.status,
            _ => return Err(UploadError::Briefcase),
        };
        let committed = if status.state == DelegatedUploadState::Committed {
            status
        } else {
            let manifest = DelegatedCommitUpload {
                operation_id: request.operation_id,
                upload_id,
            }
            .prepare()
            .map_err(|_| UploadError::Configuration)?;
            briefcase
                .commit_delegated_upload(
                    &app,
                    proof(
                        request
                            .delegated_authorization
                            .commit_proof
                            .as_ref()
                            .ok_or(UploadError::Delegation)?,
                    )?,
                    &manifest,
                )
                .await
                .map_err(|error| map_storage_error(&error))?
        };
        if committed.operation_id != request.operation_id
            || committed.upload_id != upload_id
            || committed.state != DelegatedUploadState::Committed
        {
            return Err(UploadError::InvalidEntry);
        }
        let published = committed
            .published_entry_id
            .ok_or(UploadError::InvalidEntry)?;
        let actor = request
            .delegated_authorization
            .actor_id
            .as_deref()
            .ok_or(UploadError::Delegation)?;
        let parent = format!("apps/{}/private/{actor}", request.app_id);
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        let mut found = None;
        for _ in 0..100 {
            let manifest = DelegatedListEntries {
                path: Some(parent.clone()),
                cursor,
                limit: Some(100),
                ..Default::default()
            }
            .prepare()
            .map_err(|_| UploadError::Configuration)?;
            let page = briefcase
                .list_entries_on_behalf_of(
                    &app,
                    proof(
                        request
                            .delegated_authorization
                            .list_proof
                            .as_ref()
                            .ok_or(UploadError::Delegation)?,
                    )?,
                    &manifest,
                )
                .await
                .map_err(|error| map_storage_error(&error))?;
            if let Some(entry) = page.items.into_iter().find(|entry| entry.id == published) {
                found = Some(entry);
                break;
            }
            match page.next_cursor {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                None => break,
                _ => return Err(UploadError::InvalidEntry),
            }
        }
        let entry = found.ok_or(UploadError::InvalidEntry)?;
        if entry.org_id != request.organization
            || entry.path != format!("{parent}/{}", request.filename.as_str())
            || entry.name != request.filename.as_str()
            || entry.origin_app_id.as_deref() != Some(request.app_id)
            || entry.content_type.as_deref() != Some("audio/mpeg")
            || entry.size != u64::try_from(request.audio.bytes().len()).ok()
            || entry.permanent_url.origin() != self.settings.permanent_origin.origin()
        {
            return Err(UploadError::InvalidEntry);
        }
        Ok(entry)
    }
}

fn map_storage_error(error: &briefcase_client::Error) -> UploadError {
    if error.is_unauthenticated() {
        UploadError::Delegation
    } else {
        UploadError::Briefcase
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{identity::RequestId, media::MediaDuration};
    use bytes::Bytes;
    use serde_json::json;
    use std::time::Duration;
    use time::OffsetDateTime;
    use uuid::Uuid;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_bytes, header, method, path},
    };

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn official_clients_bind_final_bytes_and_never_send_bearer_to_storage()
    -> Result<(), Box<dyn std::error::Error>> {
        let storage = MockServer::start().await;
        let delegated = crate::domain::auth::DelegatedAuthorization {
            organization_id: "tos".parse()?,
            commit_proof: Some(crate::domain::auth::OboProof::new("oba_commit".into())?),
            list_proof: Some(crate::domain::auth::OboProof::new("oba_list".into())?),
            actor_id: Some("c:alice".into()),
            testing_secret: None,
            application_id: "waveform".parse()?,
            proof: crate::domain::auth::OboProof::new("oba_reserve".to_owned())?,
            purpose: crate::domain::auth::DelegationPurpose::StoreGeneratedAudio,
            expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(1),
        };
        let audio = NormalizedAudio::new(
            Bytes::from_static(b"normalized-mp3-fixture"),
            MediaDuration::from_millis(1000),
        )?;
        let filename = GeneratedAudioFileName::for_request(
            OffsetDateTime::now_utc(),
            RequestId::new(Uuid::new_v4())?,
        );
        let operations = briefcase_client::OPERATIONS.iter().map(|operation| json!({"id":operation.id,"version":operation.version,"method":operation.method,"path":operation.path})).collect::<Vec<_>>();
        Mock::given(path("/api/version"))
            .respond_with(ResponseTemplate::new(200).insert_header("briefcase-api-version","v1").set_body_json(json!({
                "service":"silicon-briefcase", "selected_api_version":"v1", "supported_api_versions":["v1"], "contract_version":"1.0.0", "build":"local-test", "operations":operations
            }))).expect(1).mount(&storage).await;
        let operation_id = Uuid::new_v4();
        let upload_id = Uuid::new_v4();
        let entry_id = Uuid::new_v4();
        let status = |state: &str| json!({"operation_id":operation_id,"upload_id":upload_id,"state":state,"expires_at":"2099-01-01T00:00:00Z","published_entry_id":if state=="committed"{Some(entry_id)}else{None}});
        let mut reservation = status("reserved");
        reservation["capability"] = json!("upload_capability");
        Mock::given(method("POST")).and(path("/api/v1/obo/uploads/reserve")).and(header("x-app-id","waveform")).and(header("x-iam-obo-access-token","oba_reserve")).and(wiremock::matchers::body_partial_json(json!({"operation_id":operation_id,"size":audio.bytes().len(),"sha256":format!("{:x}",sha2::Sha256::digest(audio.bytes())),"name":filename.as_str()}))).respond_with(ResponseTemplate::new(200).set_body_json(reservation)).expect(1).mount(&storage).await;
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
            .and(header("x-iam-obo-access-token", "oba_commit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(status("committed")))
            .expect(1)
            .mount(&storage)
            .await;
        Mock::given(method("POST")).and(path("/api/v1/obo/entries/list")).and(header("x-iam-obo-access-token","oba_list")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":entry_id,"org_id":"tos","type":"file","visibility":"full","name":filename.as_str(),"path":format!("apps/waveform/private/c:alice/{}",filename.as_str()),"root_type":"private","content_type":"audio/mpeg","size":audio.bytes().len(),"permanent_url":format!("{}/org/tos/apps/waveform/private/c:alice/audio.mp3",storage.uri()),"origin_app_id":"waveform","effective_access":["read"],"created_at":"2099-01-01T00:00:00Z","updated_at":"2099-01-01T00:00:00Z","deleted_at":null}],"next_cursor":null}))).expect(1).mount(&storage).await;
        let settings = BriefcaseSettings {
            base_url: storage.uri().parse()?,
            permanent_origin: storage.uri().parse()?,
            cdn_origin: storage.uri().parse()?,
            app_id: "waveform".to_owned(),
            audience: "briefcase".to_owned(),
            timeout: Duration::from_secs(5),
            download_timeout: Duration::from_secs(5),
            max_download_bytes: 1_000_000,
        };
        let result = BriefcaseSdkUploader::new(settings)
            .upload(AudioUpload {
                operation_id,
                delegated_authorization: &delegated,
                app_id: "waveform",
                organization: "tos",
                environment: None,
                filename: &filename,
                audio: &audio,
            })
            .await;
        let entry = result?;
        assert_eq!(entry.name, filename.as_str());
        let requests = storage
            .received_requests()
            .await
            .ok_or("mock request log unavailable")?;
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization"))
        );
        let bytes = requests
            .iter()
            .find(|request| request.method == "PUT")
            .ok_or("transfer missing")?;
        assert!(!bytes.headers.contains_key("x-iam-obo-access-token"));
        assert!(
            !requests
                .iter()
                .any(|r| r.headers.contains_key("x-iam-obo-access-proof"))
        );
        Ok(())
    }
}
