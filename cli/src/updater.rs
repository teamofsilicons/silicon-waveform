//! Legacy preference compatibility; Honeycomb owns CLI updates.
use std::{fs, path::Path};
pub fn set_enabled(directory: &Path, enabled: bool) -> Result<(), String> {
    if enabled {
        return Err("Updates are managed by Honeycomb; run honeycomb update".into());
    }
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    fs::write(directory.join("auto-update.json"), br#"{"enabled":false}"#)
        .map_err(|e| e.to_string())
}
