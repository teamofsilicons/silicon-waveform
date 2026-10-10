//! Briefcase delegated operations, authorized by the User verification proof
//! Waveform holds for the account (`Authorization: Proof sap_…`).
//!
//! Briefcase's Silicon Accounts contract is being published in parallel, so this
//! adapter keeps Briefcase 3.0's delegated routes and JSON manifests and changes
//! only what the migration settles: a proof replaces `X-App-ID` +
//! `X-IAM-OBO-Access-Token`, and there is no organization header. Assumptions
//! (recorded in docs/migration/decisions.md for the integration stage):
//! - an empty `parent_path` puts an upload in Waveform's folder of the account's
//!   drive, `WAVEFORM_BRIEFCASE_APP_FOLDER` (default `apps/waveform`);
//! - list paths are drive-relative, and listing with no parent lists that
//!   folder (Briefcase creates it on first use);
//! - an app sees only `apps/{app}/` and what is inside it (not the drive's top
//!   or `apps/` itself), so the folder's own entry is learnt from its children;
//! - a permanent URL is `{origin}/{owner}/{drive path}`; legacy
//!   `{origin}/org/{org}/{path}` URLs still resolve.

use std::{collections::HashSet, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Digest as _;
use url::{Origin, Url};
use uuid::Uuid;

use crate::{
    application::ports::{
        BriefcaseError, BriefcaseFileAccessRequest, BriefcasePort, ReadSourceMediaRequest,
        StoreGeneratedAudioRequest,
    },
    config::BriefcaseSettings,
    domain::{
        auth::BriefcaseProof,
        identity::RequestId,
        media::{BriefcaseFileUrl, SourceMedia},
    },
};

const USER_AGENT: &str = concat!("silicon-waveform/", env!("CARGO_PKG_VERSION"));
const MAX_PAGES: usize = 100;
const PAGE_SIZE: u16 = 100;

/// Construction failure for the Briefcase adapter.
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum BriefcaseAdapterBuildError {
    /// A configured URL is not a credential-free HTTP(S) origin.
    #[error("invalid Briefcase URL configuration")]
    InvalidUrl,
    /// The HTTP client could not be built.
    #[error("failed to build the Briefcase HTTP client")]
    HttpClient,
}

/// Waveform's folder in an account's drive, as Briefcase reports it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceTarget {
    /// Briefcase entry id of the folder.
    pub folder_id: Uuid,
    /// Drive-relative folder path.
    pub folder_path: String,
}

#[derive(Serialize)]
struct ReserveBody<'a> {
    operation_id: Uuid,
    parent_path: &'a str,
    name: &'a str,
    content_type: &'a str,
    size: u64,
    sha256: String,
}

#[derive(Serialize)]
struct CommitBody {
    operation_id: Uuid,
    upload_id: Uuid,
}

#[derive(Serialize)]
struct ListBody {
    parent_id: Option<Uuid>,
    path: Option<String>,
    filter: Option<String>,
    cursor: Option<String>,
    limit: Option<u16>,
}

#[derive(Serialize)]
struct ReadBody {
    entry_id: Uuid,
    range: Option<String>,
    download: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum UploadState {
    Reserved,
    Receiving,
    Staged,
    Committed,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct UploadStatus {
    operation_id: Uuid,
    upload_id: Uuid,
    state: UploadState,
    #[serde(default)]
    published_entry_id: Option<Uuid>,
    #[serde(default)]
    capability: Option<String>,
    #[serde(default)]
    entry: Option<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
struct Entry {
    id: Uuid,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    permanent_url: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    origin_app_id: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

impl Entry {
    fn is_folder(&self) -> bool {
        self.kind == "folder"
    }

    fn is_live(&self) -> bool {
        self.deleted_at.is_none() && !self.id.is_nil()
    }
}

#[derive(Deserialize)]
struct EntryPage {
    items: Vec<Entry>,
    #[serde(default)]
    folder: Option<Entry>,
    #[serde(default)]
    next_cursor: Option<String>,
}

/// Briefcase port speaking the delegated routes with a proof.
#[derive(Clone, Debug)]
pub struct BriefcaseHttp {
    http: reqwest::Client,
    api_root: Url,
    permanent_origin: Origin,
    waveform_app_id: String,
    app_folder: String,
    timeout: Duration,
    download_timeout: Duration,
    max_download_bytes: usize,
}

impl BriefcaseHttp {
    /// Builds the adapter from validated settings.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for unusable URLs or HTTP client settings.
    pub fn new(
        settings: &BriefcaseSettings,
        waveform_app_id: &str,
    ) -> Result<Self, BriefcaseAdapterBuildError> {
        let mut api_root = settings.base_url.clone();
        if !matches!(api_root.scheme(), "http" | "https")
            || api_root.host_str().is_none()
            || !api_root.username().is_empty()
            || api_root.password().is_some()
        {
            return Err(BriefcaseAdapterBuildError::InvalidUrl);
        }
        let path = api_root.path().trim_end_matches('/').to_owned();
        api_root.set_path(&if path.is_empty() {
            "/api/v1/".to_owned()
        } else {
            format!("{path}/")
        });
        let permanent = &settings.permanent_origin;
        if !matches!(permanent.scheme(), "http" | "https") || permanent.host_str().is_none() {
            return Err(BriefcaseAdapterBuildError::InvalidUrl);
        }
        let http = reqwest::Client::builder()
            .connect_timeout(settings.timeout.min(Duration::from_secs(5)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| BriefcaseAdapterBuildError::HttpClient)?;
        Ok(Self {
            http,
            api_root,
            permanent_origin: permanent.origin(),
            waveform_app_id: waveform_app_id.to_owned(),
            app_folder: settings.app_folder.clone(),
            timeout: settings.timeout,
            download_timeout: settings.download_timeout,
            max_download_bytes: settings.max_download_bytes,
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, BriefcaseError> {
        self.api_root
            .join(path)
            .map_err(|_| BriefcaseError::InvalidResponse)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        timeout: Duration,
        upload_route: bool,
    ) -> Result<reqwest::Response, BriefcaseError> {
        let response = request.timeout(timeout).send().await.map_err(|error| {
            if error.is_timeout() {
                BriefcaseError::Timeout
            } else {
                BriefcaseError::Unavailable
            }
        })?;
        let status = response.status().as_u16();
        if response.status().is_success() {
            return Ok(response);
        }
        Err(match status {
            401 => BriefcaseError::Unauthorized,
            403 => BriefcaseError::Forbidden,
            404 if upload_route => BriefcaseError::ContractUnavailable,
            404 | 410 => BriefcaseError::NotFound,
            405 | 501 => BriefcaseError::ContractUnavailable,
            408 | 504 => BriefcaseError::Timeout,
            413 => BriefcaseError::MediaTooLarge,
            415 => BriefcaseError::UnsupportedMediaType,
            _ => BriefcaseError::Unavailable,
        })
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        proof: &BriefcaseProof,
        body: &impl Serialize,
        request_id: RequestId,
        upload_route: bool,
    ) -> Result<T, BriefcaseError> {
        let request = self
            .http
            .post(self.endpoint(path)?)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Proof {}", proof.token.expose_secret()),
            )
            .header("x-request-id", request_id.to_string())
            .json(body);
        self.send(request, self.timeout, upload_route)
            .await?
            .json::<T>()
            .await
            .map_err(|_| BriefcaseError::InvalidResponse)
    }

    async fn list(
        &self,
        proof: &BriefcaseProof,
        path: Option<String>,
        cursor: Option<String>,
        limit: u16,
        request_id: RequestId,
    ) -> Result<EntryPage, BriefcaseError> {
        self.post_json(
            "obo/entries/list",
            proof,
            &ListBody {
                parent_id: None,
                path,
                filter: None,
                cursor,
                limit: Some(limit),
            },
            request_id,
            false,
        )
        .await
    }

    /// Pages through one folder until `matches` finds an entry.
    async fn find(
        &self,
        proof: &BriefcaseProof,
        path: Option<String>,
        request_id: RequestId,
        matches: impl Fn(&Entry) -> bool,
    ) -> Result<Entry, BriefcaseError> {
        let mut cursor = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_PAGES {
            let page = self
                .list(proof, path.clone(), cursor, PAGE_SIZE, request_id)
                .await?;
            if let Some(entry) = page.items.into_iter().find(|entry| matches(entry)) {
                return Ok(entry);
            }
            match page.next_cursor {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => return Err(BriefcaseError::InvalidResponse),
                None => return Err(BriefcaseError::NotFound),
            }
        }
        Err(BriefcaseError::InvalidResponse)
    }

    /// Drive-relative path behind a permanent URL. Segments are decoded once;
    /// the result is a lookup key, never a URL to fetch.
    fn relative_path(&self, url: &BriefcaseFileUrl) -> Result<String, BriefcaseError> {
        if url.as_url().origin() != self.permanent_origin {
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
        if segments.iter().any(|segment| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.contains(['/', '\\'])
                || segment.chars().any(char::is_control)
        }) {
            return Err(BriefcaseError::NotFound);
        }
        // Legacy: /org/{org}/{path…}. Accounts era: /{owner}/{path…}.
        let skip = if segments.first().is_some_and(|first| first == "org") && segments.len() >= 4 {
            2
        } else {
            1
        };
        if segments.len() <= skip {
            return Err(BriefcaseError::NotFound);
        }
        Ok(segments[skip..].join("/"))
    }

    async fn resolve(
        &self,
        proof: &BriefcaseProof,
        url: &BriefcaseFileUrl,
        request_id: RequestId,
    ) -> Result<Uuid, BriefcaseError> {
        let path = self.relative_path(url)?;
        let parent = path.rsplit_once('/').map(|(parent, _)| parent.to_owned());
        let entry = self
            .find(proof, parent, request_id, |entry| {
                entry.path == path && !entry.is_folder() && entry.is_live()
            })
            .await?;
        if let Some(listed) = &entry.permanent_url {
            let listed = Url::parse(listed)
                .ok()
                .and_then(|listed| BriefcaseFileUrl::new(listed).ok())
                .ok_or(BriefcaseError::InvalidResponse)?;
            if self.relative_path(&listed)? != path {
                return Err(BriefcaseError::InvalidResponse);
            }
        }
        Ok(entry.id)
    }

    async fn open(
        &self,
        proof: &BriefcaseProof,
        url: &BriefcaseFileUrl,
        request_id: RequestId,
        probe: bool,
    ) -> Result<reqwest::Response, BriefcaseError> {
        let entry_id = self.resolve(proof, url, request_id).await?;
        let request = self
            .http
            .post(self.endpoint("obo/files/read")?)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Proof {}", proof.token.expose_secret()),
            )
            .header("x-request-id", request_id.to_string())
            .json(&ReadBody {
                entry_id,
                // Briefcase serves downloads as octet-stream; inline keeps the
                // declared media type for codec validation.
                range: probe.then(|| "bytes=0-0".to_owned()),
                download: false,
            });
        self.send(request, self.download_timeout, false).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "reserve, transfer, commit and published-entry validation form one ordered upload"
    )]
    async fn upload(
        &self,
        request: &StoreGeneratedAudioRequest,
    ) -> Result<BriefcaseFileUrl, BriefcaseError> {
        let bytes = request.audio.bytes();
        let operation_id = request.request_id.as_uuid();
        let reservation: UploadStatus = self
            .post_json(
                "obo/uploads/reserve",
                &request.proof,
                &ReserveBody {
                    operation_id,
                    parent_path: "",
                    name: request.filename.as_str(),
                    content_type: "audio/mpeg",
                    size: u64::try_from(bytes.len()).map_err(|_| BriefcaseError::MediaTooLarge)?,
                    sha256: format!("{:x}", sha2::Sha256::digest(bytes)),
                },
                request.request_id,
                true,
            )
            .await?;
        if reservation.operation_id != operation_id || reservation.upload_id.is_nil() {
            return Err(BriefcaseError::InvalidResponse);
        }
        let upload_id = reservation.upload_id;
        let status = match reservation.state {
            UploadState::Reserved | UploadState::Receiving => {
                let capability = reservation
                    .capability
                    .ok_or(BriefcaseError::InvalidResponse)?;
                let transfer = self
                    .http
                    .put(self.endpoint(&format!("obo/uploads/{upload_id}/content"))?)
                    .header("x-briefcase-upload-capability", capability)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(bytes.clone());
                let staged: UploadStatus = self
                    .send(transfer, self.download_timeout, true)
                    .await?
                    .json()
                    .await
                    .map_err(|_| BriefcaseError::InvalidResponse)?;
                if staged.operation_id != operation_id
                    || staged.upload_id != upload_id
                    || !matches!(staged.state, UploadState::Staged | UploadState::Committed)
                {
                    return Err(BriefcaseError::InvalidResponse);
                }
                staged
            }
            UploadState::Staged | UploadState::Committed => reservation,
            UploadState::Other => return Err(BriefcaseError::Unavailable),
        };
        let committed = if status.state == UploadState::Committed {
            status
        } else {
            self.post_json(
                "obo/uploads/commit",
                &request.proof,
                &CommitBody {
                    operation_id,
                    upload_id,
                },
                request.request_id,
                true,
            )
            .await?
        };
        if committed.operation_id != operation_id
            || committed.upload_id != upload_id
            || committed.state != UploadState::Committed
        {
            return Err(BriefcaseError::InvalidResponse);
        }
        let published = committed
            .published_entry_id
            .ok_or(BriefcaseError::InvalidResponse)?;
        let entry = match committed.entry {
            Some(entry) if entry.id == published => entry,
            _ => {
                self.find(
                    &request.proof,
                    Some(self.app_folder.clone()),
                    request.request_id,
                    |entry| entry.id == published,
                )
                .await?
            }
        };
        let size = u64::try_from(bytes.len()).map_err(|_| BriefcaseError::MediaTooLarge)?;
        if entry.is_folder()
            || !entry.is_live()
            || entry.name != request.filename.as_str()
            || entry
                .content_type
                .as_deref()
                .is_some_and(|value| value != "audio/mpeg")
            || entry.size.is_some_and(|value| value != size)
            || entry
                .origin_app_id
                .as_deref()
                .is_some_and(|app| app != self.waveform_app_id)
        {
            return Err(BriefcaseError::InvalidResponse);
        }
        let url = entry
            .permanent_url
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
            .and_then(|value| BriefcaseFileUrl::new(value).ok())
            .ok_or(BriefcaseError::InvalidResponse)?;
        if url.as_url().origin() != self.permanent_origin {
            return Err(BriefcaseError::InvalidResponse);
        }
        Ok(url)
    }

    /// Makes sure Waveform's folder exists in the account's drive and returns it,
    /// so a caller can put STT source audio there.
    ///
    /// Briefcase shows an app only what is inside `apps/{app}/`, not `apps/`
    /// itself. Current Briefcase includes the listed folder as metadata, including
    /// when empty. Earlier responses are supported through a child's `parent_id`
    /// or an explicitly accessible apps/ container.
    ///
    /// # Errors
    ///
    /// [`BriefcaseError`] by category; `NotFound` when Briefcase does not let
    /// Waveform see its folder's entry (the folder is empty and the container is
    /// not listed to Waveform).
    pub async fn prepare_source_target(
        &self,
        proof: &BriefcaseProof,
        request_id: RequestId,
    ) -> Result<SourceTarget, BriefcaseError> {
        // Listing with no parent opens Waveform's folder, creating it on first use.
        let page = self.list(proof, None, None, 1, request_id).await?;
        if let Some(folder) = page.folder.as_ref().filter(|folder| {
            folder.path == self.app_folder
                && folder.is_folder()
                && folder.is_live()
                && folder
                    .origin_app_id
                    .as_deref()
                    .is_none_or(|app| app == self.waveform_app_id)
        }) {
            return Ok(SourceTarget {
                folder_id: folder.id,
                folder_path: self.app_folder.clone(),
            });
        }
        let prefix = format!("{}/", self.app_folder);
        if let Some(folder_id) = page
            .items
            .iter()
            .find(|entry| {
                entry.path.starts_with(&prefix) && !entry.path[prefix.len()..].contains('/')
            })
            .and_then(|entry| entry.parent_id)
            .filter(|id| !id.is_nil())
        {
            return Ok(SourceTarget {
                folder_id,
                folder_path: self.app_folder.clone(),
            });
        }
        let parent = self
            .app_folder
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_owned());
        let folder = self
            .find(proof, parent, request_id, |entry| {
                entry.path == self.app_folder && entry.is_folder() && entry.is_live()
            })
            .await
            .map_err(|error| match error {
                BriefcaseError::Forbidden => BriefcaseError::NotFound,
                other => other,
            })?;
        if folder
            .origin_app_id
            .as_deref()
            .is_some_and(|app| app != self.waveform_app_id)
        {
            return Err(BriefcaseError::InvalidResponse);
        }
        Ok(SourceTarget {
            folder_id: folder.id,
            folder_path: folder.path,
        })
    }
}

#[async_trait]
impl BriefcasePort for BriefcaseHttp {
    async fn verify_file_read_access(
        &self,
        request: BriefcaseFileAccessRequest,
    ) -> Result<(), BriefcaseError> {
        tokio::time::timeout(self.download_timeout, async {
            // Successful read headers establish current access; the body is dropped.
            self.open(&request.proof, &request.file_url, request.request_id, true)
                .await
                .map(drop)
        })
        .await
        .map_err(|_| BriefcaseError::Timeout)?
    }

    async fn read_source_media(
        &self,
        request: ReadSourceMediaRequest,
    ) -> Result<SourceMedia, BriefcaseError> {
        tokio::time::timeout(self.download_timeout, async {
            let mut response = self
                .open(
                    &request.proof,
                    &request.source_url,
                    request.request_id,
                    false,
                )
                .await?;
            let limit = request.size_limit.get().min(
                u64::try_from(self.max_download_bytes)
                    .map_err(|_| BriefcaseError::MediaTooLarge)?,
            );
            if response
                .content_length()
                .is_some_and(|length| length > limit)
            {
                return Err(BriefcaseError::MediaTooLarge);
            }
            if response
                .headers()
                .contains_key(reqwest::header::CONTENT_RANGE)
            {
                return Err(BriefcaseError::InvalidResponse);
            }
            let media_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .ok_or(BriefcaseError::UnsupportedMediaType)?
                .parse()
                .map_err(|_| BriefcaseError::UnsupportedMediaType)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| BriefcaseError::Unavailable)?
            {
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

    async fn store_generated_audio(
        &self,
        request: StoreGeneratedAudioRequest,
    ) -> Result<BriefcaseFileUrl, BriefcaseError> {
        tokio::time::timeout(
            self.download_timeout.max(self.timeout),
            self.upload(&request),
        )
        .await
        .map_err(|_| BriefcaseError::Timeout)?
    }
}

#[cfg(test)]
#[path = "briefcase_tests.rs"]
mod tests;
