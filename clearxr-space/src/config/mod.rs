//! Persistent configuration for ClearXR Shell.
//!
//! Config is stored as TOML at `~/.clearxr/config.toml`.
//! Missing fields use defaults; unknown fields are preserved.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AnchorMode {
    #[default]
    World,
    Controller,
    Wrist,
    Theater,
    Head,
}

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub panel: PanelConfig,
    pub audio: AudioConfig,
    pub display: DisplayConfig,
    pub shell: ShellConfig,
}

/// Panel placement and behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    /// Panel center position in world space [x, y, z].
    pub position: [f32; 3],
    /// Panel width in world units.
    pub width: f32,
    /// Panel height in world units.
    pub height: f32,
    /// Panel opacity (0.0 - 1.0).
    pub opacity: f32,
    /// Pinning mode.
    pub anchor: AnchorMode,
    /// Theater mode distance.
    pub theater_distance: f32,
    /// Theater mode scale multiplier.
    pub theater_scale: f32,
}

/// Audio settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Master volume (0.0 - 1.0).
    pub volume: f32,
    /// Output device name (empty = system default).
    pub output_device: String,
    /// Microphone enabled.
    pub mic_enabled: bool,
}

/// Display settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Show FPS counter.
    pub show_fps: bool,
    /// Show boundary/guardian.
    pub show_boundary: bool,
    /// UI theme name.
    pub theme: String,
}

/// Shell behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// Default view on startup: "launcher", "desktop".
    pub default_view: String,
    /// Enable haptic feedback.
    pub haptics_enabled: bool,
}

// ---- Defaults ----

impl Default for Config {
    fn default() -> Self {
        Self {
            panel: PanelConfig::default(),
            audio: AudioConfig::default(),
            display: DisplayConfig::default(),
            shell: ShellConfig::default(),
        }
    }
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            position: [0.0, 1.6, -2.5],
            width: 1.6,
            height: 1.0,
            opacity: 0.95,
            anchor: AnchorMode::default(),
            theater_distance: 5.0,
            theater_scale: 3.0,
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            volume: 0.8,
            output_device: String::new(),
            mic_enabled: false,
        }
    }
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            show_fps: true,
            show_boundary: true,
            theme: "default".into(),
        }
    }
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            default_view: "launcher".into(),
            haptics_enabled: true,
        }
    }
}

// ---- Persistence ----

impl Config {
    /// Get the config file path: ~/.clearxr/config.toml
    pub fn config_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "clearxr", "clearxr")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Load config from disk, or return defaults if file doesn't exist.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            log::warn!("Could not determine config directory, using defaults.");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => {
                    log::info!("Loaded config from {}", path.display());
                    config
                }
                Err(e) => {
                    log::warn!("Failed to parse config ({}), using defaults.", e);
                    Self::default()
                }
            },
            Err(_) => {
                log::info!("No config file at {}, using defaults.", path.display());
                Self::default()
            }
        }
    }

    /// Save config to disk.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::config_path() else {
            return Err("Could not determine config directory.".into());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }

        let contents = toml::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        std::fs::write(&path, contents)
            .map_err(|e| format!("Failed to write config: {}", e))?;

        log::info!("Config saved to {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        assert_eq!(config.panel.position, [0.0, 1.6, -2.5]);
        assert_eq!(config.audio.volume, 0.8);
        assert!(config.display.show_fps);
        assert_eq!(config.shell.default_view, "launcher");
    }

    #[test]
    fn config_round_trip_toml() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.panel.width, config.panel.width);
        assert_eq!(loaded.audio.volume, config.audio.volume);
        assert_eq!(loaded.display.theme, config.display.theme);
        assert_eq!(loaded.shell.haptics_enabled, config.shell.haptics_enabled);
    }

    #[test]
    fn config_missing_fields_use_defaults() {
        let partial = r#"
[panel]
width = 2.0

[audio]
volume = 0.5
"#;
        let config: Config = toml::from_str(partial).unwrap();
        assert_eq!(config.panel.width, 2.0);       // overridden
        assert_eq!(config.panel.height, 1.0);       // default
        assert_eq!(config.audio.volume, 0.5);       // overridden
        assert!(config.display.show_fps);            // default
    }

    #[test]
    fn config_extra_fields_ignored() {
        let with_extra = r#"
[panel]
width = 1.6
some_future_field = "hello"

[audio]
volume = 0.8
"#;
        // Should not error - unknown fields are ignored with #[serde(default)]
        let config: Config = toml::from_str(with_extra).unwrap();
        assert_eq!(config.panel.width, 1.6);
    }

    #[test]
    fn config_path_is_some() {
        // directories crate should return a path on all platforms
        assert!(Config::config_path().is_some());
    }
}
