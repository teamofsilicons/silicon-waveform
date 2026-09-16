//! Compatibility commands for retiring the old independent updater.
use std::{fs, path::Path};
const GUIDANCE: &str = "Waveform updates are managed by Honeycomb. Run honeycomb update; remove any legacy waveform-updater service from your OS service manager.";
pub fn status(dir: &Path) -> Result<serde_json::Value, String> {
    let running = if dir.exists() {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("daemon.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock().is_err()
    } else {
        false
    };
    Ok(serde_json::json!({"running":running,"update_manager":"honeycomb","guidance":GUIDANCE}))
}
pub fn start(_: &Path) -> Result<(), String> {
    Err(GUIDANCE.into())
}
pub async fn run(_: &Path) -> Result<(), String> {
    Err(GUIDANCE.into())
}
pub fn install() -> Result<(), String> {
    Err(GUIDANCE.into())
}
pub fn stop(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("auto-update.json"), br#"{"enabled":false}"#).map_err(|e| e.to_string())?;
    fs::write(dir.join("daemon.stop"), b"stop").map_err(|e| e.to_string())
}
