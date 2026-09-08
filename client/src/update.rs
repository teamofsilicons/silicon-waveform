//! Best-effort hourly maintenance for the Waveform client package.
//!
//! A Rust library cannot replace code already loaded in its process. When a
//! newer client release exists, this updater advances the nearest consuming
//! Cargo lockfile; the new code is used by the next build. Failures never
//! change the result of the Waveform request that triggered maintenance.

use semver::Version;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Crates.io package containing this client.
pub const CLIENT_CRATE: &str = "silicon-waveform-client";
/// Version compiled into this client.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Automatic update policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdatePolicy {
    /// Check crates.io and update the consuming lockfile when due.
    #[default]
    Automatic,
    /// Never contact crates.io or invoke Cargo.
    Disabled,
}

impl UpdatePolicy {
    /// Resolves `WAVEFORM_CLIENT_AUTO_UPDATE`; missing or unknown values stay on.
    #[must_use]
    pub fn from_environment() -> Self {
        match std::env::var("WAVEFORM_CLIENT_AUTO_UPDATE") {
            Ok(value)
                if matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                ) =>
            {
                Self::Disabled
            }
            _ => Self::Automatic,
        }
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

/// Shared, cancellation-safe automatic updater state.
pub(crate) struct AutomaticUpdater {
    policy: UpdatePolicy,
    running: AtomicBool,
    state: Mutex<(Option<Instant>, UpdateStatus)>,
}

impl AutomaticUpdater {
    pub(crate) fn new(policy: UpdatePolicy) -> Arc<Self> {
        Arc::new(Self {
            policy,
            running: AtomicBool::new(false),
            state: Mutex::new((None, UpdateStatus::default())),
        })
    }

    pub(crate) fn status(&self) -> UpdateStatus {
        self.state
            .lock()
            .map(|state| state.1.clone())
            .unwrap_or(UpdateStatus::Failed {
                reason: "updater state unavailable".into(),
            })
    }

    pub(crate) async fn after_request(self: &Arc<Self>) {
        if self.policy == UpdatePolicy::Disabled {
            if let Ok(mut state) = self.state.lock() {
                state.1 = UpdateStatus::Disabled;
            }
            return;
        }
        let Some(_run) = self.try_start() else {
            return;
        };
        let result = self.perform().await;
        if let Ok(mut state) = self.state.lock() {
            state.1 = result;
        }
    }

    fn try_start(self: &Arc<Self>) -> Option<UpdateRun> {
        let mut state = self.state.lock().ok()?;
        if state.0.is_some_and(|last| last.elapsed() < CHECK_INTERVAL)
            || self.running.swap(true, Ordering::AcqRel)
        {
            return None;
        }
        state.0 = Some(Instant::now());
        Some(UpdateRun(self.clone()))
    }

    async fn perform(&self) -> UpdateStatus {
        let release = match check(CLIENT_CRATE, CLIENT_VERSION).await {
            Ok(release) => release,
            Err(error) => {
                return UpdateStatus::Failed {
                    reason: error.to_string(),
                };
            }
        };
        if !release.update_available() {
            return UpdateStatus::Current {
                version: release.current,
            };
        }
        let Some(manifest) = find_manifest(&std::env::current_dir().unwrap_or_default()) else {
            return UpdateStatus::NoCargoProject;
        };
        match update_dependency(&manifest, CLIENT_CRATE, &release.latest).await {
            Ok(()) => UpdateStatus::Updated {
                from: release.current,
                to: release.latest,
            },
            Err(error) => UpdateStatus::Failed {
                reason: error.to_string(),
            },
        }
    }
}

struct UpdateRun(Arc<AutomaticUpdater>);
impl Drop for UpdateRun {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

/// Installs a published CLI release for the next invocation using Cargo.
/// The current command has already completed when this is called.
pub async fn install_binary(package: &str, version: &Version) -> Result<(), UpdateError> {
    let mut command =
        tokio::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .arg("install")
        .arg(package)
        .arg("--version")
        .arg(format!("={version}"))
        .arg("--locked")
        .arg("--force")
        .env("CARGO_HTTP_TIMEOUT", "10")
        .env("CARGO_NET_RETRY", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(300), command.status())
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
    #[test]
    fn cancellation_releases_single_flight_but_retains_hourly_throttle() {
        let updater = super::AutomaticUpdater::new(UpdatePolicy::Automatic);
        let run = updater.try_start().expect("first attempt");
        assert!(updater.try_start().is_none());
        drop(run);
        assert!(!updater.running.load(std::sync::atomic::Ordering::Acquire));
        assert!(updater.try_start().is_none());
        updater.state.lock().unwrap().0 = Some(std::time::Instant::now() - super::CHECK_INTERVAL);
        assert!(updater.try_start().is_some());
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
        assert_eq!(UpdatePolicy::Automatic, UpdatePolicy::default());
        assert_eq!(UpdateStatus::default(), UpdateStatus::NotChecked);
    }
}
