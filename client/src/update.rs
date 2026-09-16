//! Explicit dependency maintenance helpers. Requests never update dependencies.
//! CLI installation and updates are owned by Honeycomb.

use semver::Version;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

/// Crates.io package containing this client.
pub const CLIENT_CRATE: &str = "silicon-waveform-client";
/// Version compiled into this client.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Automatic update policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdatePolicy {
    /// Legacy setting retained for source compatibility; runtime updates stay disabled.
    Automatic,
    /// Never contact crates.io or invoke Cargo.
    #[default]
    Disabled,
}

impl UpdatePolicy {
    /// Runtime dependency updates are disabled regardless of environment settings.
    #[must_use]
    pub fn from_environment() -> Self {
        Self::Disabled
    }
}

/// A stable crates.io release comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    /// Compiled client version.
    pub current: Version,
    /// Newest stable published version.
    pub latest: Version,
}

impl Release {
    /// Returns true when the registry has a newer stable release.
    #[must_use]
    pub fn update_available(&self) -> bool {
        self.latest > self.current
    }
}

/// Observable result of the last maintenance attempt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum UpdateStatus {
    /// No request has completed an update attempt yet.
    #[default]
    NotChecked,
    /// Updates were disabled.
    Disabled,
    /// No Cargo manifest was found from the current directory upward.
    NoCargoProject,
    /// The current build is already at the latest stable release.
    Current { version: Version },
    /// The lockfile was advanced for the next build.
    Updated { from: Version, to: Version },
    /// Best-effort maintenance failed.
    Failed { reason: String },
}

/// Error returned by an explicit update operation.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("CLI updates are managed by Honeycomb; run honeycomb update")]
    HoneycombManaged,
    #[error("invalid package version: {0}")]
    Version(#[from] semver::Error),
    #[error("cannot query crates.io: {0}")]
    Registry(#[from] reqwest::Error),
    #[error("crates.io returned no stable version for {0}")]
    MissingStableVersion(String),
    #[error("Cargo manifest {0} has no parent")]
    InvalidManifest(PathBuf),
    #[error("cannot run Cargo: {0}")]
    CargoIo(#[from] std::io::Error),
    #[error("Cargo failed while updating {package} to {version}")]
    CargoFailed { package: String, version: Version },
}

/// Looks up a package's newest stable release.
pub async fn check(package: &str, current: &str) -> Result<Release, UpdateError> {
    let current = Version::parse(current)?;
    let url = format!("https://crates.io/api/v1/crates/{package}");
    let response = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .user_agent(format!("{CLIENT_CRATE}/{CLIENT_VERSION} updater"))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<CratesIoResponse>()
        .await?;
    let latest = response
        .package
        .max_stable_version
        .ok_or_else(|| UpdateError::MissingStableVersion(package.to_owned()))?;
    Ok(Release {
        current,
        latest: Version::parse(&latest)?,
    })
}

/// Finds the nearest Cargo project at or above `start`.
#[must_use]
pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join("Cargo.toml"))
        .find(|path| path.is_file())
}

/// Advances a dependency in a consuming lockfile for the next build.
pub async fn update_dependency(
    manifest: &Path,
    package: &str,
    version: &Version,
) -> Result<(), UpdateError> {
    let Some(parent) = manifest.parent() else {
        return Err(UpdateError::InvalidManifest(manifest.to_owned()));
    };
    let mut command =
        tokio::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(parent)
        .arg("update")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("-p")
        .arg(package)
        .arg("--precise")
        .arg(version.to_string())
        .env("CARGO_HTTP_TIMEOUT", "10")
        .env("CARGO_NET_RETRY", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(120), command.status())
        .await
        .map_err(|_| UpdateError::CargoFailed {
            package: package.to_owned(),
            version: version.clone(),
        })??;
    if status.success() {
        Ok(())
    } else {
        Err(UpdateError::CargoFailed {
            package: package.to_owned(),
            version: version.clone(),
        })
    }
}

/// Compatibility shim. Dependency updates belong to the consuming project.
pub(crate) struct AutomaticUpdater;
impl AutomaticUpdater {
    pub(crate) fn new(_policy: UpdatePolicy) -> Arc<Self> {
        Arc::new(Self)
    }
    pub(crate) fn status(&self) -> UpdateStatus {
        UpdateStatus::Disabled
    }
    pub(crate) async fn after_request(&self) {}
}

/// Installs a published CLI release for the next invocation using Cargo.
/// The current command has already completed when this is called.
pub async fn install_binary(package: &str, version: &Version) -> Result<(), UpdateError> {
    let _ = (package, version);
    Err(UpdateError::HoneycombManaged)
}

#[derive(Debug, Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    package: CratesIoPackage,
}
#[derive(Debug, Deserialize)]
struct CratesIoPackage {
    max_stable_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{Release, UpdatePolicy, UpdateStatus};
    use semver::Version;
    #[tokio::test]
    async fn legacy_automatic_policy_never_runs_maintenance() {
        let updater = super::AutomaticUpdater::new(UpdatePolicy::Automatic);
        updater.after_request().await;
        assert_eq!(updater.status(), UpdateStatus::Disabled);
        assert_eq!(UpdatePolicy::from_environment(), UpdatePolicy::Disabled);
    }

    #[test]
    fn release_comparison_and_policy_are_bounded() {
        assert!(
            Release {
                current: Version::new(1, 0, 0),
                latest: Version::new(1, 0, 1)
            }
            .update_available()
        );
        assert!(
            !Release {
                current: Version::new(1, 0, 1),
                latest: Version::new(1, 0, 0)
            }
            .update_available()
        );
        assert_eq!(UpdatePolicy::Disabled, UpdatePolicy::default());
        assert_eq!(UpdateStatus::default(), UpdateStatus::NotChecked);
    }
}
