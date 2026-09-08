//! Opportunistic CLI updates after the requested command finishes.
use silicon_waveform_client::update;
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn set_enabled(directory: &Path, enabled: bool) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    fs::write(
        directory.join("auto-update.json"),
        serde_json::json!({"enabled":enabled}).to_string(),
    )
    .map_err(|e| e.to_string())
}

fn enabled(directory: &Path) -> bool {
    match std::env::var("WAVEFORM_AUTO_UPDATE").ok().as_deref() {
        Some("0" | "false" | "off" | "no") => false,
        Some("1" | "true" | "on" | "yes") => true,
        _ => fs::read(directory.join("auto-update.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value["enabled"].as_bool())
            .unwrap_or(true),
    }
}

fn due(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|last| last > now || now - last >= 3600)
}

pub async fn automatic(directory: &Path) -> Result<(), String> {
    if !enabled(directory) {
        return Ok(());
    }
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("update.lock"))
        .map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let state_path = directory.join("update-attempt.json");
    let last = fs::read(&state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<u64>(&bytes).ok());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    if !due(last, now) {
        return Ok(());
    }
    // Reserve before network work, including failed attempts; the OS lock is
    // released on process exit and does not leave a stale lock directory.
    fs::write(&state_path, now.to_string()).map_err(|e| e.to_string())?;
    let release = update::check("waveform-cli", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|e| e.to_string())?;
    if release.update_available() {
        update::install_binary("waveform-cli", &release.latest)
            .await
            .map_err(|e| e.to_string())?;
        eprintln!(
            "Waveform updated to {}; the next command uses the new release.",
            release.latest
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn attempts_are_hourly_including_clock_rollback() {
        assert!(super::due(None, 10000));
        assert!(!super::due(Some(10000), 13599));
        assert!(super::due(Some(10000), 13600));
        assert!(super::due(Some(10000), 9000));
    }
}
