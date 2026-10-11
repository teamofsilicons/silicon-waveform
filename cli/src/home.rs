//! Where the CLI keeps its state: `{home}/.waveform/dir`, where `{home}` is the directory set
//! with `waveform config home DIR` (remembered in `<default home>/.waveform/config.json`), else
//! `$SILICON_HOME`, else `$HOME`. Named profiles live in `profiles/<name>/` below it.
//! Directories are `0700` and files `0600` on Unix. Reading never creates anything.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use crate::output::{CliError, CliResult};

/// `$SILICON_HOME`, else `$HOME` (`%USERPROFILE%` on Windows), else the current directory.
pub fn default_home() -> PathBuf {
    ["SILICON_HOME", "HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

pub fn config_path() -> PathBuf {
    default_home().join(".waveform").join("config.json")
}

/// The directory set with `config home`, when the config file names one.
pub fn configured_home() -> Option<PathBuf> {
    let text = fs::read_to_string(config_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("home")?.as_str().map(PathBuf::from)
}

/// `{home}/.waveform/dir`.
pub fn state_dir() -> PathBuf {
    configured_home()
        .unwrap_or_else(default_home)
        .join(".waveform")
        .join("dir")
}

/// Validates a profile name and returns its directory.
pub fn profile_dir(profile: &str) -> CliResult<PathBuf> {
    if profile.is_empty()
        || profile.len() > 64
        || !profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(CliError::invalid(
            format!("The profile name `{profile}` is not valid."),
            "Use 1-64 letters, digits, underscores or hyphens (--profile or WAVEFORM_PROFILE).",
        ));
    }
    let root = state_dir();
    Ok(if profile == "default" {
        root
    } else {
        root.join("profiles").join(profile)
    })
}

/// Creates a private directory (and its parents).
pub fn ensure_private_dir(dir: &Path) -> CliResult<()> {
    fs::create_dir_all(dir).map_err(|error| {
        CliError::local(
            format!("Cannot create {}: {error}.", dir.display()),
            "Check that the state home exists and is writable (waveform config show).",
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
            CliError::local(
                format!("Cannot make {} private: {error}.", dir.display()),
                "Check the directory's owner and permissions.",
            )
        })?;
    }
    Ok(())
}

/// Writes a private file atomically: a new `0600` temporary file in the same directory,
/// synced, then renamed over the target, then the directory synced.
pub fn write_private(path: &Path, bytes: &[u8]) -> CliResult<()> {
    let dir = path.parent().ok_or_else(|| {
        CliError::local(format!("{} has no parent directory.", path.display()), "")
    })?;
    ensure_private_dir(dir)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let failure = |what: &str, error: std::io::Error| {
        let _ = fs::remove_file(&tmp);
        CliError::local(
            format!("Cannot {what} {}: {error}.", path.display()),
            "Check free space and the permissions of the state directory (waveform config show).",
        )
    };
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|e| failure("create", e))?;
    file.write_all(bytes).map_err(|e| failure("write", e))?;
    file.sync_all().map_err(|e| failure("sync", e))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| failure("replace", e))?;
    #[cfg(unix)]
    if let Ok(directory) = fs::File::open(dir) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// `config home DIR`: remembers an existing directory as the state home.
pub fn configure_home(location: &Path) -> CliResult<PathBuf> {
    configure_home_at(&config_path(), location)
}

pub fn configure_home_at(config_path: &Path, location: &Path) -> CliResult<PathBuf> {
    if !location.is_dir() {
        return Err(CliError::invalid(
            format!("not a directory: {}", location.display()),
            "Pass an existing directory; Waveform keeps its state in <DIR>/.waveform/dir.",
        ));
    }
    let location = fs::canonicalize(location).map_err(|error| {
        CliError::invalid(
            format!("Cannot resolve {}: {error}.", location.display()),
            "Pass an existing, accessible directory.",
        )
    })?;
    let body = serde_json::to_vec_pretty(&serde_json::json!({ "home": location }))
        .map_err(|error| CliError::local(format!("Cannot encode the settings: {error}."), ""))?;
    write_private(config_path, &body)?;
    Ok(location)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waveform-home-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn config_home_requires_a_directory_and_persists_the_canonical_path() {
        let root = scratch();
        let state = root.join("state");
        fs::create_dir_all(&state).unwrap();
        let config = root.join("config").join("home.json");
        let error = configure_home_at(&config, &root.join("missing")).unwrap_err();
        assert!(error.message.starts_with("not a directory"));
        configure_home_at(&config, &state).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            value["home"].as_str(),
            fs::canonicalize(&state).unwrap().to_str()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn private_writes_are_atomic_and_owner_only() {
        let root = scratch();
        let path = root.join("nested").join("file.json");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let leftovers = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0, "no temporary file is left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn profile_names_are_checked() {
        assert!(profile_dir("work-2").is_ok());
        for bad in ["", "../x", "a b", &"x".repeat(65)] {
            assert!(profile_dir(bad).is_err(), "{bad}");
        }
    }
}
