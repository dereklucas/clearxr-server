//! Persistent configuration for ClearXR (shared between layer and space).
//!
//! Config is stored as TOML at the platform config directory.

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub panel: PanelConfig,
    pub audio: AudioConfig,
    pub display: DisplayConfig,
    pub shell: ShellConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    pub position: [f32; 3],
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub anchor: AnchorMode,
    pub theater_distance: f32,
    pub theater_scale: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub volume: f32,
    pub output_device: String,
    pub mic_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub show_fps: bool,
    pub show_boundary: bool,
    pub debug_borders: bool,
    pub theme: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    pub default_view: String,
    pub haptics_enabled: bool,
}

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
            debug_borders: false,
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

impl Config {
    pub fn config_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "clearxr", "clearxr")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            log::warn!("[ClearXR Layer] Could not determine config directory, using defaults.");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => {
                    log::info!("[ClearXR Layer] Loaded config from {}", path.display());
                    config
                }
                Err(e) => {
                    log::warn!(
                        "[ClearXR Layer] Failed to parse config ({}), using defaults.",
                        e
                    );
                    Self::default()
                }
            },
            Err(_) => {
                log::info!(
                    "[ClearXR Layer] No config file at {}, using defaults.",
                    path.display()
                );
                Self::default()
            }
        }
    }

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

        log::info!("[ClearXR Layer] Config saved to {}", path.display());
        Ok(())
    }
}
