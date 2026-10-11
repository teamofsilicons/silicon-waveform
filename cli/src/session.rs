//! The saved sign-in: one private JSON file per profile and Silicon Accounts origin
//! (`accounts-session-<digest>.json` in the profile's state directory), written atomically and
//! rotated under a file lock so two processes never present the same refresh token (a reused
//! refresh token ends the whole sign-in at Silicon Accounts).
//!
//! Files from earlier Waveform versions (`session-<digest>.json`, signed in with the old
//! identity service) are never read as sign-ins: they are reported so the user signs in again,
//! and `waveform logout` deletes them.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use silicon_waveform_client::{Account, ExposeSecret as _, signin::Tokens};

use crate::home::{ensure_private_dir, write_private};
use crate::output::{CliError, CliResult, EXIT_FAILURE};

/// The session file format written by this version.
pub const FORMAT: u32 = 1;
/// Refresh when the access token has less than this many seconds left.
pub const REFRESH_MARGIN_SECONDS: i64 = 60;
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// An origin without its trailing slash, so `https://x/` and `https://x` are one session.
pub fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

/// One account's sign-in to Waveform.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub format: u32,
    pub app_id: String,
    pub accounts_url: String,
    pub access_token: String,
    pub refresh_token: String,
    /// When the access token expires (Unix seconds).
    pub expires_at: i64,
    /// When the sign-in ends (Unix seconds), when known.
    #[serde(default)]
    pub refresh_expires_at: Option<i64>,
    #[serde(default)]
    pub scope: String,
    /// `device` or `slt`.
    pub method: String,
    pub signed_in_at: i64,
    #[serde(default)]
    pub refreshed_at: Option<i64>,
    pub account: Account,
}

impl std::fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSession")
            .field("accounts_url", &self.accounts_url)
            .field("account", &self.account)
            .field("expires_at", &self.expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .finish_non_exhaustive()
    }
}

impl StoredSession {
    pub fn from_tokens(tokens: &Tokens, accounts_url: &str, method: &str) -> Self {
        Self {
            format: FORMAT,
            app_id: silicon_waveform_client::APP_ID.to_owned(),
            accounts_url: normalize_url(accounts_url),
            access_token: tokens.access_token.expose_secret().to_owned(),
            refresh_token: tokens.refresh_token.expose_secret().to_owned(),
            expires_at: tokens.expires_at,
            refresh_expires_at: tokens.refresh_expires_at,
            scope: tokens.scope.clone(),
            method: method.to_owned(),
            signed_in_at: now(),
            refreshed_at: None,
            account: tokens.account.clone(),
        }
    }

    /// The same sign-in after a refresh.
    pub fn rotated(&self, tokens: &Tokens) -> Self {
        Self {
            access_token: tokens.access_token.expose_secret().to_owned(),
            refresh_token: tokens.refresh_token.expose_secret().to_owned(),
            expires_at: tokens.expires_at,
            refresh_expires_at: tokens.refresh_expires_at.or(self.refresh_expires_at),
            scope: if tokens.scope.is_empty() {
                self.scope.clone()
            } else {
                tokens.scope.clone()
            },
            refreshed_at: Some(now()),
            account: tokens.account.clone(),
            ..self.clone()
        }
    }

    pub fn needs_refresh(&self, at: i64) -> bool {
        self.expires_at <= at.saturating_add(REFRESH_MARGIN_SECONDS)
    }

    pub fn ended(&self, at: i64) -> bool {
        self.refresh_expires_at.is_some_and(|end| end <= at)
    }
}

/// What the session file holds.
pub enum Loaded {
    Missing,
    Session {
        session: Box<StoredSession>,
        /// Set when the file was readable by others and has been made private again.
        warning: Option<String>,
    },
    Unreadable {
        path: PathBuf,
        why: String,
    },
}

/// The session file of one profile and Silicon Accounts origin.
pub struct SessionStore {
    dir: PathBuf,
    accounts_url: String,
}

/// Held while a sign-in is written or rotated; dropping it releases the lock.
pub struct SessionLock {
    _file: fs::File,
}

impl SessionStore {
    pub fn new(dir: PathBuf, accounts_url: &str) -> Self {
        Self {
            dir,
            accounts_url: normalize_url(accounts_url),
        }
    }

    fn stem(&self) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(self.accounts_url.as_bytes());
        let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        format!("accounts-session-{hex}")
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.stem()))
    }

    pub fn lock_path(&self) -> PathBuf {
        self.dir.join(format!("{}.lock", self.stem()))
    }

    /// Reads the sign-in without creating anything.
    pub fn load(&self) -> Loaded {
        let path = self.path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
            Err(error) => {
                return Loaded::Unreadable {
                    path,
                    why: format!("it cannot be read ({error})"),
                };
            }
        };
        let session: StoredSession = match serde_json::from_slice(&bytes) {
            Ok(session) => session,
            Err(error) => {
                return Loaded::Unreadable {
                    path,
                    why: format!("it is not a sign-in this CLI wrote ({error})"),
                };
            }
        };
        if session.format != FORMAT
            || session.app_id != silicon_waveform_client::APP_ID
            || normalize_url(&session.accounts_url) != self.accounts_url
            || session.access_token.is_empty()
            || session.refresh_token.is_empty()
            || session.account.uuid.is_empty()
        {
            return Loaded::Unreadable {
                path,
                why: "it was written for another format, app or Silicon Accounts, or it is incomplete".into(),
            };
        }
        Loaded::Session {
            session: Box::new(session),
            warning: tighten(&path),
        }
    }

    pub fn save(&self, session: &StoredSession) -> CliResult<()> {
        let body = serde_json::to_vec_pretty(session)
            .map_err(|error| CliError::local(format!("Cannot encode the sign-in: {error}."), ""))?;
        write_private(&self.path(), &body)
    }

    /// Deletes the sign-in; true when there was one.
    pub fn remove(&self) -> CliResult<bool> {
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(CliError::local(
                format!("Cannot delete {}: {error}.", self.path().display()),
                "Delete it by hand; it holds this machine's Waveform sign-in.",
            )),
        }
    }

    /// Takes the exclusive sign-in lock, waiting up to 30 seconds for another `waveform`
    /// process (signing in, refreshing or signing out) to finish.
    pub async fn lock(&self) -> CliResult<SessionLock> {
        ensure_private_dir(&self.dir)?;
        let path = self.lock_path();
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|error| {
            CliError::local(
                format!("Cannot open the sign-in lock {}: {error}.", path.display()),
                "Check the permissions of the state directory (waveform config show).",
            )
        })?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(SessionLock { _file: file }),
                Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(CliError::new(
                        EXIT_FAILURE,
                        "sign_in_locked",
                        format!(
                            "Another waveform process has held the sign-in lock {} for 30 seconds (signing in, refreshing or signing out).",
                            path.display()
                        ),
                        "Wait for it to finish and retry; if no waveform process is running, retry (the lock is released when a process exits).",
                    ));
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(CliError::local(
                        format!("Cannot lock {}: {error}.", path.display()),
                        "Check that the state directory is on a local file system.",
                    ));
                }
            }
        }
    }

    fn files_matching(&self, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(&keep)
            })
            .collect();
        files.sort();
        files
    }

    /// Sign-ins saved by earlier Waveform versions in this profile directory
    /// (`session-<digest>.json`). They cannot be used any more.
    pub fn legacy_files(&self) -> Vec<PathBuf> {
        self.files_matching(|name| name.starts_with("session-") && name.ends_with(".json"))
    }

    /// Everything earlier versions kept here that `logout` deletes: their sign-ins, their lock
    /// files, and their cached test-environment secrets (`test-environments-<digest>.json`).
    pub fn legacy_state(&self) -> Vec<PathBuf> {
        self.files_matching(|name| {
            (name.starts_with("session-") || name.starts_with("test-environments-"))
                && (name.ends_with(".json") || name.ends_with(".lock"))
        })
    }

    /// Silicon Accounts origins this profile is signed in to, other than the current one.
    pub fn other_origins(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mine = self.path();
        let mut origins: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path != &mine
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("accounts-session-") && name.ends_with(".json")
                        })
            })
            .filter_map(|path| {
                let value: serde_json::Value =
                    serde_json::from_slice(&fs::read(path).ok()?).ok()?;
                value["accounts_url"].as_str().map(str::to_owned)
            })
            .collect();
        origins.sort();
        origins.dedup();
        origins
    }
}

/// Makes a session file readable by its owner only again; says so when it was not.
fn tighten(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path).ok()?.permissions().mode();
        if mode & 0o077 != 0 {
            let fixed = fs::set_permissions(path, fs::Permissions::from_mode(0o600)).is_ok();
            return Some(format!(
                "{} was readable by other users (mode {:o}){}; anyone who read it could act as you until you sign out (waveform logout) and sign in again.",
                path.display(),
                mode & 0o777,
                if fixed {
                    " and has been made private"
                } else {
                    ""
                }
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use silicon_waveform_client::SecretString;

    pub(crate) fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waveform-session-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn tokens(access: &str, refresh: &str, expires_in: i64) -> Tokens {
        Tokens {
            access_token: SecretString::from(access.to_owned()),
            refresh_token: SecretString::from(refresh.to_owned()),
            expires_at: now() + expires_in,
            refresh_expires_at: Some(now() + 86_400),
            scope: "profile".into(),
            account: Account {
                uuid: "aB1".into(),
                id: Some("c:ada".into()),
                kind: "carbon".into(),
                display_name: Some("Ada".into()),
                pfp_url: None,
                custodian: None,
            },
        }
    }

    #[test]
    fn sessions_are_private_atomic_and_keyed_by_the_accounts_origin() {
        let dir = scratch();
        let store = SessionStore::new(dir.clone(), "https://accounts.example/");
        assert!(matches!(store.load(), Loaded::Missing));
        assert!(!dir.join(".waveform").exists());
        let session = StoredSession::from_tokens(
            &tokens("eyJ-a", "sar_a", 1800),
            "https://accounts.example",
            "slt",
        );
        store.save(&session).unwrap();
        let Loaded::Session {
            session: loaded,
            warning,
        } = store.load()
        else {
            panic!("saved session must load")
        };
        assert!(warning.is_none());
        assert_eq!(loaded.refresh_token, "sar_a");
        assert!(
            !format!("{loaded:?}").contains("sar_a"),
            "Debug never prints tokens"
        );
        // The same origin with a trailing slash is the same file; another origin is another.
        assert_eq!(
            SessionStore::new(dir.clone(), "https://accounts.example").path(),
            store.path()
        );
        let local = SessionStore::new(dir.clone(), "http://localhost:9590");
        assert_ne!(local.path(), store.path());
        assert!(matches!(local.load(), Loaded::Missing));
        assert_eq!(
            local.other_origins(),
            vec!["https://accounts.example".to_owned()]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
            let Loaded::Session { warning, .. } = store.load() else {
                panic!()
            };
            assert!(warning.unwrap().contains("readable by other users"));
            assert_eq!(
                fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(store.remove().unwrap());
        assert!(!store.remove().unwrap());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupted_or_foreign_files_are_reported_not_used() {
        let dir = scratch();
        let store = SessionStore::new(dir.clone(), "https://accounts.example");
        fs::write(store.path(), b"{not json").unwrap();
        assert!(matches!(store.load(), Loaded::Unreadable { .. }));
        let mut session = StoredSession::from_tokens(
            &tokens("eyJ-a", "sar_a", 1800),
            "https://accounts.example",
            "slt",
        );
        session.app_id = "remind".into();
        fs::write(store.path(), serde_json::to_vec(&session).unwrap()).unwrap();
        let Loaded::Unreadable { why, .. } = store.load() else {
            panic!()
        };
        assert!(why.contains("another format, app"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn files_from_earlier_versions_are_found_but_never_read_as_sign_ins() {
        let dir = scratch();
        let store = SessionStore::new(dir.clone(), "https://accounts.example");
        fs::write(
            dir.join("session-0123abcd.json"),
            br#"{"access_token":"oat_old","refresh_token":"ort_old","identity":{"org_id":"tos"}}"#,
        )
        .unwrap();
        fs::write(dir.join("session-0123abcd.lock"), b"").unwrap();
        fs::write(dir.join("session-99.lock"), b"").unwrap();
        fs::write(
            dir.join("test-environments-0123abcd.json"),
            br#"{"id":"ask_old"}"#,
        )
        .unwrap();
        fs::write(dir.join("config.json"), b"{}").unwrap();
        assert!(matches!(store.load(), Loaded::Missing));
        // Only a saved sign-in counts as one; a lock left by an earlier sign-out does not.
        assert_eq!(store.legacy_files().len(), 1);
        assert_eq!(store.legacy_state().len(), 4);
        fs::remove_file(dir.join("session-0123abcd.json")).unwrap();
        assert!(store.legacy_files().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refresh_and_end_times_are_judged_with_a_margin() {
        let fresh =
            StoredSession::from_tokens(&tokens("a", "r", 1800), "https://a.example", "device");
        assert!(!fresh.needs_refresh(now()));
        let soon = StoredSession::from_tokens(&tokens("a", "r", 30), "https://a.example", "device");
        assert!(soon.needs_refresh(now()));
        let mut ended = fresh.clone();
        ended.refresh_expires_at = Some(now() - 1);
        assert!(ended.ended(now()) && !fresh.ended(now()));
        let rotated = fresh.rotated(&tokens("b", "r2", 1800));
        assert_eq!(rotated.refresh_token, "r2");
        assert_eq!(rotated.signed_in_at, fresh.signed_in_at);
        assert!(rotated.refreshed_at.is_some());
    }

    #[tokio::test]
    async fn the_sign_in_lock_admits_one_process_at_a_time() {
        let dir = scratch();
        let store = SessionStore::new(dir.clone(), "https://accounts.example");
        let first = store.lock().await.unwrap();
        let second_store = SessionStore::new(dir.clone(), "https://accounts.example");
        let waiter = tokio::spawn(async move {
            let started = Instant::now();
            let _second = second_store.lock().await.unwrap();
            started.elapsed()
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(first);
        let waited = waiter.await.unwrap();
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(store.lock_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(dir);
    }
}
