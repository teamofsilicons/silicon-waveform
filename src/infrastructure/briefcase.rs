//! Official Briefcase uploads, delegated source reads, and current replay access checks.

use std::str::FromStr as _;

use async_trait::async_trait;
use thiserror::Error;
use time::OffsetDateTime;
use url::{Origin, Url};

use crate::{
    application::ports::{
        BriefcaseError, BriefcaseFileAccessRequest, BriefcasePort, ReadSourceMediaRequest,
        StoreGeneratedAudioRequest,
    },
    config::BriefcaseSettings,
    domain::{
        auth::{DelegatedAuthorization, DelegationPurpose},
        identity::ApplicationId,
        media::{SourceMedia, StoredAudio, TemporaryMediaUrl},
    },
};

/// Construction failure for the fail-closed Briefcase boundary.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BriefcaseAdapterBuildError {
    /// The permanent origin is not a credential-free HTTP(S) origin.
    #[error("invalid Briefcase permanent origin configuration")]
    InvalidPermanentOrigin,
    /// The Briefcase application identifier is not a valid scoped ID.
    #[error("invalid Briefcase application identity configuration")]
    InvalidApplicationIdentity,
}

/// Briefcase port backed by the published SDK and fresh per-operation IAM proofs.
#[derive(Clone, Debug)]
pub struct FailClosedBriefcaseStore {
    permanent_origin: Origin,
    application_id: ApplicationId,
    uploader: Option<super::briefcase_sdk::BriefcaseSdkUploader>,
    environment: Option<briefcase_client::EnvironmentKey>,
    reader: Option<super::briefcase_reader::BriefcaseSdkReader>,
}

impl FailClosedBriefcaseStore {
    /// Builds the boundary and freezes the exact trusted permanent-URL origin.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the configured value contains credentials,
    /// a non-root path, query, fragment, or invalid application identifier.
    pub fn new(settings: &BriefcaseSettings) -> Result<Self, BriefcaseAdapterBuildError> {
        validate_origin(&settings.permanent_origin)?;
        let application_id = ApplicationId::from_str(&settings.app_id)
            .map_err(|_| BriefcaseAdapterBuildError::InvalidApplicationIdentity)?;
        if application_id.as_str() != settings.app_id.as_str() {
            return Err(BriefcaseAdapterBuildError::InvalidApplicationIdentity);
        }
        Ok(Self {
            permanent_origin: settings.permanent_origin.origin(),
            application_id,
            uploader: None,
            environment: None,
            reader: None,
        })
    }

    /// Enables the published one-shot upload contract with official SDK transport.
    #[must_use]
    pub fn with_uploads(mut self, settings: &BriefcaseSettings) -> Self {
        self.uploader = Some(super::briefcase_sdk::BriefcaseSdkUploader::new(
            settings.clone(),
        ));
        self
    }

    /// Binds every SDK storage request to the paired Briefcase test plane.
    #[must_use]
    pub fn with_environment(mut self, key: briefcase_client::EnvironmentKey) -> Self {
        self.environment = Some(key);
        self
    }

    /// Enables current-file reads using a fresh proof for each storage operation.
    #[must_use]
    pub fn with_reads(
        mut self,
        settings: &BriefcaseSettings,
        iam: std::sync::Arc<dyn crate::application::ports::IamPort>,
    ) -> Self {
        self.reader = Some(super::briefcase_reader::BriefcaseSdkReader::new(
            settings.clone(),
            iam,
        ));
        self
    }

    fn validate_delegation(
        &self,
        authorization: &DelegatedAuthorization,
        purpose: DelegationPurpose,
    ) -> Result<(), BriefcaseError> {
        if authorization.application_id != self.application_id
            || authorization.purpose != purpose
            || authorization.expires_at <= OffsetDateTime::now_utc()
        {
            return Err(BriefcaseError::Unauthorized);
        }
        Ok(())
    }

    fn validate_file_access(
        &self,
        request: &BriefcaseFileAccessRequest,
    ) -> Result<(), BriefcaseError> {
        if request.file_url.as_url().origin() != self.permanent_origin {
            return Err(BriefcaseError::Forbidden);
        }
        if request
            .authorization
            .expires_at
            .is_some_and(|expiry| expiry <= OffsetDateTime::now_utc())
        {
            return Err(BriefcaseError::Unauthorized);
        }
        Ok(())
    }
}

#[async_trait]
impl BriefcasePort for FailClosedBriefcaseStore {
    async fn verify_file_read_access(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<(), BriefcaseError> {
        self.validate_file_access(&request)?;
        self.reader
            .as_ref()
            .ok_or(BriefcaseError::ContractUnavailable)?
            .verify(request, self.environment.clone())
            .await
    }

    async fn issue_temporary_url(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<TemporaryMediaUrl, BriefcaseError> {
        self.validate_file_access(&request)?;
        Err(BriefcaseError::ContractUnavailable)
    }

    async fn read_source_media(
        &self,
        request: ReadSourceMediaRequest,
    ) -> Result<SourceMedia, BriefcaseError> {
        if request.source_url.as_url().origin() != self.permanent_origin {
            return Err(BriefcaseError::Forbidden);
        }
        self.reader
            .as_ref()
            .ok_or(BriefcaseError::ContractUnavailable)?
            .read(request, self.environment.clone())
            .await
    }

    async fn store_generated_audio(
        &self,
        request: StoreGeneratedAudioRequest,
    ) -> Result<StoredAudio, BriefcaseError> {
        self.validate_delegation(
            &request.delegated_authorization,
            DelegationPurpose::StoreGeneratedAudio,
        )?;
        let uploader = self
            .uploader
            .as_ref()
            .ok_or(BriefcaseError::ContractUnavailable)?;
        let entry = uploader
            .upload(super::briefcase_sdk::AudioUpload {
                app_id: self.application_id.as_str(),
                organization: request.authorization.organization_id.as_str(),
                delegated_authorization: &request.delegated_authorization,
                environment: self.environment.clone(),
                filename: &request.filename,
                audio: &request.audio,
            })
            .await
            .map_err(|error| match error {
                super::briefcase_sdk::UploadError::InvalidEntry => BriefcaseError::InvalidResponse,
                super::briefcase_sdk::UploadError::Delegation => BriefcaseError::Unauthorized,
                _ => BriefcaseError::Unavailable,
            })?;
        Ok(StoredAudio {
            permanent_url: crate::domain::media::BriefcaseFileUrl::new(entry.permanent_url)
                .map_err(|_| BriefcaseError::InvalidResponse)?,
            temporary_url: None,
        })
    }
}

fn validate_origin(value: &Url) -> Result<(), BriefcaseAdapterBuildError> {
    if !matches!(value.scheme(), "http" | "https")
        || value.host_str().is_none()
        || !value.username().is_empty()
        || value.password().is_some()
        || !matches!(value.path(), "" | "/")
        || value.query().is_some()
        || value.fragment().is_some()
    {
        return Err(BriefcaseAdapterBuildError::InvalidPermanentOrigin);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr as _, time::Duration};

    use bytes::Bytes;
    use time::OffsetDateTime;
    use url::Url;
    use uuid::Uuid;

    use super::FailClosedBriefcaseStore;
    use crate::{
        application::ports::{
            BriefcaseError, BriefcaseFileAccessRequest, BriefcasePort, ReadSourceMediaRequest,
            StoreGeneratedAudioRequest,
        },
        config::BriefcaseSettings,
        domain::{
            auth::{AuthorizedActor, DelegatedAuthorization, DelegationPurpose, OboProof},
            idempotency::IdempotencyKey,
            identity::{Actor, ActorId, ActorKind, ApplicationId, OrganizationId, RequestId},
            media::{
                BriefcaseFileUrl, GeneratedAudioFileName, MediaDuration, MediaSizeLimit,
                NormalizedAudio,
            },
        },
    };

    fn settings() -> Result<BriefcaseSettings, url::ParseError> {
        Ok(BriefcaseSettings {
            base_url: Url::parse("https://briefcase.example.test/api/v1")?,
            permanent_origin: Url::parse("https://briefcase.example.test")?,
            cdn_origin: Url::parse("https://cdn.example.test")?,
            app_id: "waveform".to_owned(),
            audience: "tos>briefcase".to_owned(),
            timeout: Duration::from_secs(10),
            download_timeout: Duration::from_secs(30),
            max_download_bytes: 25 * 1_024 * 1_024,
        })
    }

    fn request_id() -> Result<RequestId, crate::domain::identity::IdentityError> {
        RequestId::new(Uuid::new_v4())
    }

    fn authorized_actor() -> Result<AuthorizedActor, Box<dyn std::error::Error>> {
        Ok(AuthorizedActor {
            actor: Actor::new(ActorKind::Carbon, ActorId::new(Uuid::new_v4())?),
            organization_id: OrganizationId::from_str("acme")?,
            originating_application: None,
            expires_at: None,
        })
    }

    fn delegation(
        purpose: DelegationPurpose,
    ) -> Result<DelegatedAuthorization, Box<dyn std::error::Error>> {
        Ok(DelegatedAuthorization {
            application_id: ApplicationId::from_str("waveform")?,
            proof: OboProof::new("obo_downstream-proof".to_owned())?,
            purpose,
            expires_at: OffsetDateTime::now_utc() + time::Duration::seconds(60),
        })
    }

    #[tokio::test]
    async fn trusted_source_origin_reaches_explicit_contract_gap()
    -> Result<(), Box<dyn std::error::Error>> {
        let adapter = FailClosedBriefcaseStore::new(&settings()?)?;
        let source_url = BriefcaseFileUrl::new(Url::parse(
            "https://briefcase.example.test/api/v1/entries/one",
        )?)?;
        let result = adapter
            .read_source_media(ReadSourceMediaRequest {
                authorization: authorized_actor()?,
                source_url,
                subject_token: None,
                size_limit: MediaSizeLimit::default(),
                request_id: request_id()?,
            })
            .await;

        assert_eq!(result, Err(BriefcaseError::ContractUnavailable));
        Ok(())
    }

    #[tokio::test]
    async fn untrusted_origin_is_rejected_before_contract_gap()
    -> Result<(), Box<dyn std::error::Error>> {
        let adapter = FailClosedBriefcaseStore::new(&settings()?)?;
        let source_url = BriefcaseFileUrl::new(Url::parse(
            "https://attacker.example.test/api/v1/entries/one",
        )?)?;
        let result = adapter
            .read_source_media(ReadSourceMediaRequest {
                authorization: authorized_actor()?,
                source_url,
                subject_token: None,
                size_limit: MediaSizeLimit::default(),
                request_id: request_id()?,
            })
            .await;

        assert_eq!(result, Err(BriefcaseError::Forbidden));
        Ok(())
    }

    #[tokio::test]
    async fn replay_access_checks_reach_the_explicit_contract_gap()
    -> Result<(), Box<dyn std::error::Error>> {
        let adapter = FailClosedBriefcaseStore::new(&settings()?)?;
        let file_url = BriefcaseFileUrl::new(Url::parse(
            "https://briefcase.example.test/api/v1/entries/one",
        )?)?;
        let access_request = BriefcaseFileAccessRequest {
            authorization: authorized_actor()?,
            subject_token: None,
            file_url,
            request_id: request_id()?,
        };

        assert_eq!(
            adapter
                .verify_file_read_access(access_request.clone())
                .await,
            Err(BriefcaseError::ContractUnavailable)
        );
        assert_eq!(
            adapter.issue_temporary_url(access_request).await,
            Err(BriefcaseError::ContractUnavailable)
        );
        Ok(())
    }

    #[tokio::test]
    async fn replay_access_rejects_untrusted_origins_and_expired_delegation()
    -> Result<(), Box<dyn std::error::Error>> {
        let adapter = FailClosedBriefcaseStore::new(&settings()?)?;
        let untrusted = BriefcaseFileAccessRequest {
            authorization: authorized_actor()?,
            subject_token: None,
            file_url: BriefcaseFileUrl::new(Url::parse(
                "https://attacker.example.test/api/v1/entries/one",
            )?)?,
            request_id: request_id()?,
        };
        assert_eq!(
            adapter.issue_temporary_url(untrusted).await,
            Err(BriefcaseError::Forbidden)
        );

        let mut expired_actor = authorized_actor()?;
        expired_actor.expires_at = Some(OffsetDateTime::now_utc() - time::Duration::seconds(1));
        let expired = BriefcaseFileAccessRequest {
            authorization: expired_actor,
            subject_token: None,
            file_url: BriefcaseFileUrl::new(Url::parse(
                "https://briefcase.example.test/api/v1/entries/one",
            )?)?,
            request_id: request_id()?,
        };
        assert_eq!(
            adapter.verify_file_read_access(expired).await,
            Err(BriefcaseError::Unauthorized)
        );
        Ok(())
    }

    #[tokio::test]
    async fn generated_audio_storage_never_invents_an_app_folder_route()
    -> Result<(), Box<dyn std::error::Error>> {
        let adapter = FailClosedBriefcaseStore::new(&settings()?)?;
        let request_id = request_id()?;
        let result = adapter
            .store_generated_audio(StoreGeneratedAudioRequest {
                authorization: authorized_actor()?,
                delegated_authorization: delegation(DelegationPurpose::StoreGeneratedAudio)?,
                filename: GeneratedAudioFileName::for_request(
                    OffsetDateTime::now_utc(),
                    request_id,
                ),
                audio: NormalizedAudio::new(
                    Bytes::from_static(b"bounded-mp3"),
                    MediaDuration::from_millis(1),
                )?,
                idempotency_key: IdempotencyKey::for_briefcase_upload(request_id),
                request_id,
            })
            .await;

        assert_eq!(result, Err(BriefcaseError::ContractUnavailable));
        Ok(())
    }

    #[test]
    fn constructor_rejects_path_bearing_origins() -> Result<(), url::ParseError> {
        let mut settings = settings()?;
        settings.permanent_origin = Url::parse("https://briefcase.example.test/api/v1")?;

        assert!(FailClosedBriefcaseStore::new(&settings).is_err());
        Ok(())
    }
}
