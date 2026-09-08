//! Current, actor-authorized reads through Briefcase's published delegated API.
use std::{collections::HashSet, fmt, sync::Arc};

use briefcase_client::{
    ApplicationId, Client, Config, DelegatedListEntries, DelegatedManifest, DelegatedReadFile,
    EnvironmentKey, OboProof, delegated::DelegatedOperation,
};
use bytes::Bytes;

use crate::{
    application::ports::{
        BriefcaseError, BriefcaseFileAccessRequest, IamPort, ReadSourceMediaRequest,
    },
    config::BriefcaseSettings,
    domain::{
        auth::{
            AccessToken, AuthorizedActor, DelegatedManifestBinding, DelegationPurpose,
            DelegationRequest,
        },
        identity::RequestId,
        media::{BriefcaseFileUrl, SourceMedia},
    },
};

#[derive(Clone)]
pub(super) struct BriefcaseSdkReader {
    settings: BriefcaseSettings,
    iam: Arc<dyn IamPort>,
}

impl fmt::Debug for BriefcaseSdkReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BriefcaseSdkReader").finish_non_exhaustive()
    }
}

struct ReadAuthority<'a> {
    actor: &'a AuthorizedActor,
    token: &'a Option<AccessToken>,
    request_id: RequestId,
}

impl BriefcaseSdkReader {
    pub(super) fn new(settings: BriefcaseSettings, iam: Arc<dyn IamPort>) -> Self {
        Self { settings, iam }
    }

    async fn connect(
        &self,
        org: &str,
        environment: Option<EnvironmentKey>,
    ) -> Result<Client, BriefcaseError> {
        let mut base = self.settings.base_url.clone();
        if base.path() == "/" {
            base.set_path("/api/v1/");
        }
        let mut config = Config::new(base.as_str(), org)
            .map_err(map_sdk)?
            .with_auto_update(false)
            .with_request_timeout(self.settings.timeout)
            .with_transfer_timeout(self.settings.download_timeout);
        if let Some(key) = environment {
            config = config.with_environment(key);
        }
        Client::connect(config).await.map_err(map_sdk)
    }

    async fn proof<T: DelegatedOperation>(
        &self,
        authority: &ReadAuthority<'_>,
        manifest: &DelegatedManifest<T>,
    ) -> Result<OboProof, BriefcaseError> {
        let result = self
            .iam
            .delegate(DelegationRequest {
                authorization: authority.actor.clone(),
                purpose: DelegationPurpose::ReadBriefcaseFile,
                request_id: authority.request_id,
                subject_token: authority.token.clone(),
                upload: None,
                manifest: Some(DelegatedManifestBinding {
                    endpoint_id: manifest.endpoint_id().to_owned(),
                    path: manifest.path().to_owned(),
                    body_sha256: manifest.body_sha256().to_owned(),
                }),
            })
            .await
            .map_err(|error| match error {
                crate::application::ports::IamError::Forbidden
                | crate::application::ports::IamError::OrganizationMismatch => {
                    BriefcaseError::Forbidden
                }
                crate::application::ports::IamError::InvalidCredential => {
                    BriefcaseError::Unauthorized
                }
                crate::application::ports::IamError::Timeout => BriefcaseError::Timeout,
                crate::application::ports::IamError::ContractUnavailable => {
                    BriefcaseError::ContractUnavailable
                }
                _ => BriefcaseError::Unavailable,
            })?;
        if result.application_id.as_str() != self.settings.app_id
            || result.purpose != DelegationPurpose::ReadBriefcaseFile
            || result.expires_at <= time::OffsetDateTime::now_utc()
        {
            return Err(BriefcaseError::Unauthorized);
        }
        OboProof::new(result.proof.expose_secret()).map_err(map_sdk)
    }

    // Decode URL segments exactly once. A path is a lookup key, never a URL to fetch.
    fn path(&self, url: &BriefcaseFileUrl, org: &str) -> Result<String, BriefcaseError> {
        if url.as_url().origin() != self.settings.permanent_origin.origin() {
            return Err(BriefcaseError::Forbidden);
        }
        let segments = url
            .as_url()
            .path_segments()
            .ok_or(BriefcaseError::NotFound)?
            .map(|part| {
                percent_encoding::percent_decode_str(part)
                    .decode_utf8()
                    .map(std::borrow::Cow::into_owned)
                    .map_err(|_| BriefcaseError::NotFound)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if segments.len() < 4
            || segments[0] != "org"
            || segments[1] != org
            || segments.iter().any(|s| {
                s.is_empty()
                    || s == "."
                    || s == ".."
                    || s.contains(['/', '\\'])
                    || s.chars().any(char::is_control)
            })
        {
            return Err(BriefcaseError::NotFound);
        }
        Ok(segments[2..].join("/"))
    }

    async fn resolve(
        &self,
        client: &Client,
        authority: &ReadAuthority<'_>,
        url: &BriefcaseFileUrl,
    ) -> Result<uuid::Uuid, BriefcaseError> {
        let file_path = self.path(url, authority.actor.organization_id.as_str())?;
        let (parent, _) = file_path.rsplit_once('/').ok_or(BriefcaseError::NotFound)?;
        let app = ApplicationId::new(&self.settings.app_id).map_err(map_sdk)?;
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let manifest = DelegatedListEntries {
                path: Some(parent.to_owned()),
                cursor,
                limit: Some(100),
                ..Default::default()
            }
            .prepare()
            .map_err(map_sdk)?;
            let proof = self.proof(authority, &manifest).await?;
            let page = client
                .list_entries_on_behalf_of(&app, proof, &manifest)
                .await
                .map_err(map_sdk)?;
            for entry in page.items {
                if entry.path != file_path {
                    continue;
                }
                if entry.org_id != authority.actor.organization_id.as_str()
                    || entry.is_folder()
                    || entry.id.is_nil()
                    || entry.deleted_at.is_some()
                    || self.path(
                        &BriefcaseFileUrl::new(entry.permanent_url)
                            .map_err(|_| BriefcaseError::InvalidResponse)?,
                        &entry.org_id,
                    )? != file_path
                {
                    return Err(BriefcaseError::InvalidResponse);
                }
                return Ok(entry.id);
            }
            match page.next_cursor {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => return Err(BriefcaseError::InvalidResponse),
                None => return Err(BriefcaseError::NotFound),
            }
        }
    }

    async fn open(
        &self,
        authority: &ReadAuthority<'_>,
        url: &BriefcaseFileUrl,
        environment: Option<EnvironmentKey>,
        probe: bool,
    ) -> Result<briefcase_client::ContentStream, BriefcaseError> {
        // Validate before either discovery or IAM, and never connect to a caller URL.
        self.path(url, authority.actor.organization_id.as_str())?;
        if authority
            .actor
            .expires_at
            .is_some_and(|expiry| expiry <= time::OffsetDateTime::now_utc())
        {
            return Err(BriefcaseError::Unauthorized);
        }
        let client = self
            .connect(authority.actor.organization_id.as_str(), environment)
            .await?;
        let entry_id = self.resolve(&client, authority, url).await?;
        let manifest = DelegatedReadFile {
            entry_id,
            range: probe.then(|| "bytes=0-0".to_owned()),
            // Briefcase intentionally serves downloads as application/octet-stream.
            // Render intent preserves the declared media type for codec validation.
            download: false,
        }
        .prepare()
        .map_err(map_sdk)?;
        let proof = self.proof(authority, &manifest).await?;
        let app = ApplicationId::new(&self.settings.app_id).map_err(map_sdk)?;
        client
            .read_file_on_behalf_of(&app, proof, &manifest)
            .await
            .map_err(map_sdk)
    }

    pub(super) async fn verify(
        &self,
        request: BriefcaseFileAccessRequest,
        environment: Option<EnvironmentKey>,
    ) -> Result<(), BriefcaseError> {
        let authority = ReadAuthority {
            actor: &request.authorization,
            token: &request.subject_token,
            request_id: request.request_id,
        };
        tokio::time::timeout(self.settings.download_timeout, async {
            // Successful read headers establish current access. Drop the stream immediately;
            // no complete audio is transferred and no temporary bearer URL is invented.
            self.open(&authority, &request.file_url, environment, true)
                .await
                .map(drop)
        })
        .await
        .map_err(|_| BriefcaseError::Timeout)?
    }

    pub(super) async fn read(
        &self,
        request: ReadSourceMediaRequest,
        environment: Option<EnvironmentKey>,
    ) -> Result<SourceMedia, BriefcaseError> {
        let authority = ReadAuthority {
            actor: &request.authorization,
            token: &request.subject_token,
            request_id: request.request_id,
        };
        tokio::time::timeout(self.settings.download_timeout, async {
            let mut stream = self
                .open(&authority, &request.source_url, environment, false)
                .await?;
            let limit = request.size_limit.get().min(
                u64::try_from(self.settings.max_download_bytes)
                    .map_err(|_| BriefcaseError::MediaTooLarge)?,
            );
            if stream.content_length().is_some_and(|length| length > limit) {
                return Err(BriefcaseError::MediaTooLarge);
            }
            if stream.content_range().is_some() {
                return Err(BriefcaseError::InvalidResponse);
            }
            let media_type = stream
                .content_type()
                .ok_or(BriefcaseError::UnsupportedMediaType)?
                .parse()
                .map_err(|_| BriefcaseError::UnsupportedMediaType)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.chunk().await.map_err(map_sdk)? {
                let size = bytes
                    .len()
                    .checked_add(chunk.len())
                    .ok_or(BriefcaseError::MediaTooLarge)?;
                if u64::try_from(size).map_err(|_| BriefcaseError::MediaTooLarge)? > limit {
                    return Err(BriefcaseError::MediaTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            SourceMedia::new(media_type, Bytes::from(bytes), request.size_limit)
                .map_err(|_| BriefcaseError::InvalidResponse)
        })
        .await
        .map_err(|_| BriefcaseError::Timeout)?
    }
}

fn map_sdk(error: briefcase_client::Error) -> BriefcaseError {
    if let briefcase_client::Error::Api(api) = error {
        return match api.status {
            401 => BriefcaseError::Unauthorized,
            403 => BriefcaseError::Forbidden,
            404 => BriefcaseError::NotFound,
            413 => BriefcaseError::MediaTooLarge,
            415 => BriefcaseError::UnsupportedMediaType,
            _ => BriefcaseError::Unavailable,
        };
    }
    BriefcaseError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permanent_lookup_is_bound_to_origin_org_and_decoded_path()
    -> Result<(), Box<dyn std::error::Error>> {
        let settings = BriefcaseSettings {
            base_url: "https://backend.briefcase.test".parse()?,
            permanent_origin: "https://briefcase.test".parse()?,
            cdn_origin: "https://cdn.briefcase.test".parse()?,
            app_id: "tos>waveform".to_owned(),
            audience: "tos>briefcase".to_owned(),
            timeout: std::time::Duration::from_secs(1),
            download_timeout: std::time::Duration::from_secs(1),
            max_download_bytes: 1024,
        };
        let iam = Arc::new(crate::infrastructure::testing::FixtureIam::new(
            crate::domain::identity::ActorKind::Carbon,
            uuid::Uuid::new_v4(),
            "tos>waveform".parse()?,
        )?);
        let reader = BriefcaseSdkReader::new(settings, iam);
        let valid = BriefcaseFileUrl::new(
            "https://briefcase.test/org/tos/private/actor/hello%20world.mp3".parse()?,
        )?;
        assert_eq!(reader.path(&valid, "tos")?, "private/actor/hello world.mp3");
        for url in [
            "https://attacker.test/org/tos/private/actor/file.mp3",
            "https://briefcase.test/org/other/private/actor/file.mp3",
            "https://briefcase.test/files/file.mp3",
            "https://briefcase.test/org/tos/private/actor%2Fother/file.mp3",
            "https://briefcase.test/org/tos/private/actor%5Cother/file.mp3",
            "https://briefcase.test/org/tos/private/actor/%00file.mp3",
        ] {
            let url = BriefcaseFileUrl::new(url.parse()?)?;
            assert!(reader.path(&url, "tos").is_err());
        }
        Ok(())
    }
}
