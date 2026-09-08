//! Exact-byte speech uploads using the official IAM and Briefcase clients.
//!
//! A proof is minted after normalization, immediately before the one upload.
//! Its digest and metadata cannot be computed earlier in the speech workflow.

use briefcase_client::{Client, Config, Entry, EnvironmentKey, OnBehalfOfUpload};
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
    /// IAM could not issue the one-request delegation.
    #[error("IAM did not authorize the Briefcase upload")]
    Delegation,
    /// The returned entry disagrees with the exact request that was authorized.
    #[error("Briefcase returned an inconsistent audio entry")]
    InvalidEntry,
}

/// All request-local authority for an upload; no tokens are cached or persisted.
pub struct AudioUpload<'a> {
    /// Waveform's canonical application ID.
    pub app_id: &'a str,
    /// Organization selected from the live IAM authorization snapshot.
    pub organization: &'a str,
    /// Fresh Briefcase-audience proof issued by IAM for this exact upload.
    pub delegated_authorization: &'a DelegatedAuthorization,
    /// Mandatory paired Briefcase root for a test-plane request.
    pub environment: Option<EnvironmentKey>,
    /// Collision-resistant final name used in the proof metadata.
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

    /// Consumes a prepared exact-byte OBO proof in one upload without retrying it.
    ///
    /// # Errors
    /// Returns a redacted error if discovery, exchange, upload or response
    /// binding validation fails. An uncertain upload is never blindly retried.
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
        if let Some(environment) = request.environment {
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
        let entry = briefcase
            .create_file_on_behalf_of(&OnBehalfOfUpload::bytes(
                request.app_id,
                request
                    .delegated_authorization
                    .proof
                    .expose_secret()
                    .to_owned(),
                request.audio.bytes().to_vec(),
            ))
            .await
            .map_err(|_| UploadError::Briefcase)?;
        if entry.org_id != request.organization
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
            application_id: "tos>waveform".parse()?,
            proof: crate::domain::auth::OboProof::new("obo_test_proof".to_owned())?,
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
        Mock::given(method("POST")).and(path("/api/v1/obo/files"))
            .and(header("x-app-id","tos>waveform")).and(header("x-iam-obo-access-proof","obo_test_proof"))
            .and(body_bytes(audio.bytes().to_vec()))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id":Uuid::new_v4(),"org_id":"tos","type":"file","visibility":"full","name":filename.as_str(),"path":format!("private/actor/apps/tos>waveform/{}",filename.as_str()),"root_type":"private","content_type":"audio/mpeg","size":audio.bytes().len(),"permanent_url":format!("{}/files/audio.mp3",storage.uri()),"origin_app_id":"tos>waveform","effective_access":["read"],"created_at":"2099-01-01T00:00:00Z","updated_at":"2099-01-01T00:00:00Z","deleted_at":null
            }))).expect(1).mount(&storage).await;
        let settings = BriefcaseSettings {
            base_url: storage.uri().parse()?,
            permanent_origin: storage.uri().parse()?,
            cdn_origin: storage.uri().parse()?,
            app_id: "tos>waveform".to_owned(),
            audience: "tos>briefcase".to_owned(),
            timeout: Duration::from_secs(5),
            download_timeout: Duration::from_secs(5),
            max_download_bytes: 1_000_000,
        };
        let result = BriefcaseSdkUploader::new(settings)
            .upload(AudioUpload {
                delegated_authorization: &delegated,
                app_id: "tos>waveform",
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
        Ok(())
    }
}
