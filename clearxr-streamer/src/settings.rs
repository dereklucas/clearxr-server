use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SETTINGS_DIR_NAME: &str = "StreamingSession";
const SETTINGS_FILE_NAME: &str = "streaming-session-settings.json";
const DEFAULT_CLEARXR_EXE_PATH: &str = "clear-xr.exe";
const DEFAULT_LAUNCH_DELAY_SECONDS: u64 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StreamingSessionSettings {
    pub launch_default_app: bool,
    pub clearxr_exe_path: String,
    pub clearxr_launch_delay_seconds: u64,
}

impl Default for StreamingSessionSettings {
    fn default() -> Self {
        Self {
            launch_default_app: true,
            clearxr_exe_path: DEFAULT_CLEARXR_EXE_PATH.to_string(),
            clearxr_launch_delay_seconds: DEFAULT_LAUNCH_DELAY_SECONDS,
        }
    }
}

pub fn ensure_settings_file() -> Result<PathBuf> {
    let path = settings_path()?;
    let parent = path
        .parent()
        .context("the settings file does not have a parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    if !path.exists() {
        write_settings_file(&path, &StreamingSessionSettings::default())?;
    }

    Ok(path)
}

pub fn load_settings() -> Result<(StreamingSessionSettings, PathBuf)> {
    let path = ensure_settings_file()?;
    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let settings: StreamingSessionSettings = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok((settings, path))
}

fn write_settings_file(path: &Path, settings: &StreamingSessionSettings) -> Result<()> {
    let contents = serde_json::to_string_pretty(settings)?;
    fs::write(path, format!("{contents}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn settings_path() -> Result<PathBuf> {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data)
            .join(SETTINGS_DIR_NAME)
            .join(SETTINGS_FILE_NAME));
    }

    if let Some(app_data) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(app_data)
            .join(SETTINGS_DIR_NAME)
            .join(SETTINGS_FILE_NAME));
    }

    let exe_dir = std::env::current_exe()
        .context("failed to determine the executable path for settings")?
        .parent()
        .context("the executable path does not have a parent directory")?
        .to_path_buf();

    Ok(exe_dir.join(SETTINGS_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{write_settings_file, StreamingSessionSettings};

    #[test]
    fn settings_file_uses_expected_json_keys() {
        let root = std::env::temp_dir().join(format!(
            "streaming-session-settings-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");

        write_settings_file(&path, &StreamingSessionSettings::default()).unwrap();
        let contents = fs::read_to_string(&path).unwrap();

        assert!(contents.contains("\"launchDefaultApp\": true"));
        assert!(contents.contains("\"clearxrExePath\": \"clear-xr.exe\""));
        assert!(contents.contains("\"clearxrLaunchDelaySeconds\": 3"));

        fs::remove_dir_all(root).unwrap();
    }
}
